// SPDX-License-Identifier: ISC
//! Ported from OpenBSD's openrsync — `blocks.c`.
//!
//! Original copyright: Kristaps Dzonsons; ISC license. See
//! `LICENSES/openrsync-ISC.txt` for the full notice.
//!
//! # Slice 3 scope (v7.0.0-alpha.X)
//!
//! - [`BlockSet`] / [`BlockSig`] — value-object mirror of openrsync's
//!   `struct blkset` + `struct blk` (`extern.h` lines 192..243).
//! - [`read_blockset`] — port of `blocks.c::blk_recv` (lines 349..442).
//!   Drains the per-file signature stream off an [`MplexReader`].
//! - [`write_blockset_header`] — emits the receiver-side ack header the
//!   sender echoes back per `blocks.c::blk_recv_ack` (lines 326..343),
//!   minus the leading `idx` (the caller emits the index separately so
//!   callers can wire `idx == -1` phase markers cleanly).
//!
//! # Lock-free contract (CRITICAL)
//!
//! All state lives on the per-task stack — no `Mutex`, no shared cell.
//! The [`MplexReader`] / [`MplexWriter`] arguments are owned by their
//! caller; this module only borrows mutably for the duration of one
//! file's signature exchange.

use tokio::io::{AsyncRead, AsyncWrite};

use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
use crate::adapters::rsync::wire::session::WireSession;
use crate::domain::error::DomainError;

/// Maximum chunk of literal file data the sender may emit per token.
///
/// Mirrors openrsync's `MAX_CHUNK = 32 * 1024` from `extern.h` line 37.
/// rsync 3.2.x tolerates this value at protocol 27/31; the upstream
/// client's per-token literal cap is identical.
pub const MAX_CHUNK: u32 = 32 * 1024;

/// Strong checksum length (in bytes) — full MD4 digest. Mirrors
/// `extern.h::CSUM_LENGTH_PHASE2 = 16`.
pub const CSUM_LENGTH_PHASE2: usize = 16;

/// One block signature exchanged between sender and receiver.
///
/// Mirrors openrsync's `struct blk` (`extern.h` lines 192..199).
///
/// We always carry the full 16-byte digest in [`Self::strong`]
/// regardless of the on-wire `strong_len` so the matcher does not have
/// to track the truncation length per byte access. The wire layer only
/// reads / writes `BlockSet::strong_len` bytes when parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSig {
    /// Block index (`b->idx` in `blocks.c::blk_recv`). Sequential from
    /// `0`, the high-byte of the negative match-token wire encoding is
    /// `idx + 1` (see [`MAX_CHUNK`] doc-comment).
    pub idx: u32,
    /// Adler32-style fast checksum (`b->chksum_short`). Read off the
    /// wire as a little-endian `i32`; reinterpreted as `u32` here so
    /// the hashtable bucket modulus operation does not have to redo
    /// the bit-cast on every probe.
    pub rolling: u32,
    /// Strong checksum (`b->chksum_long`). The wire stream contains
    /// only `BlockSet::strong_len` bytes; we zero-pad the trailing
    /// slot so the value type stays `Copy`.
    pub strong: [u8; CSUM_LENGTH_PHASE2],
}

/// Block-set metadata exchanged on a per-file basis between the
/// receiver and the sender.
///
/// Mirrors openrsync's `struct blkset` (`extern.h` lines 236..243) —
/// we collapse the C nesting into a flat value type because Rust's
/// borrow checker has no use for the inner `struct blk *blks; size_t
/// blksz;` separation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSet {
    /// File size from the receiver's perspective. Zero on the
    /// whole-file fast path (`blksz == 0`) — the field is informational
    /// for tracing.
    pub size: u64,
    /// Number of complete-length blocks (`blksz` in `blk_recv`). Zero
    /// triggers the whole-file path; positive triggers the block-match
    /// path (slice-4 work — slice 3 still emits one big literal).
    pub block_count: u32,
    /// Bytes per non-terminal block (`s->len`). Receiver default is
    /// `max(BLOCK_SIZE_MIN, sqrt(file_size))` rounded up to 1 KiB.
    pub block_len: u32,
    /// Bytes in the terminal partial block (`s->rem`). Zero when the
    /// last block is full-length.
    pub remainder: u32,
    /// Strong checksum truncation in bytes (`s->csum`). Always
    /// `<= CSUM_LENGTH_PHASE2`. rsync 3.2.7 sends 16 bytes per block;
    /// we accept anything in `1..=16` per `blk_send_ack`'s sanity
    /// check.
    pub strong_len: u32,
    /// Per-block signatures in receipt order. The slice is empty when
    /// `block_count == 0` (whole-file path).
    pub blocks: Vec<BlockSig>,
}

impl BlockSet {
    /// Build an empty (whole-file) block set. Used by the sender when
    /// the server signals "I have no checksums for this file" via a
    /// `count == 0` blockset header.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            size: 0,
            block_count: 0,
            block_len: 0,
            remainder: 0,
            strong_len: u32_as_strong_len_default(),
            blocks: Vec::new(),
        }
    }

    /// `true` when this is the whole-file fast path (no per-block
    /// matching). Mirrors openrsync's `blks->blksz == 0` branch in
    /// `blocks.c::blk_match` (line 259).
    #[must_use]
    pub const fn is_whole_file(&self) -> bool {
        self.block_count == 0
    }
}

/// Default strong-length when the receiver-supplied value is missing.
///
/// Mirrors openrsync's hard-coded 16-byte digest used everywhere
/// `csum` would otherwise be zero. `CSUM_LENGTH_PHASE2` is 16 — fits a
/// `u32` trivially.
#[expect(
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "CSUM_LENGTH_PHASE2 is the compile-time constant 16; the cast to u32 cannot truncate. `const fn` cannot use u32::try_from so the `as` keyword is the only option."
)]
const fn u32_as_strong_len_default() -> u32 {
    CSUM_LENGTH_PHASE2 as u32
}

// =====================================================================
// read_blockset — port of blocks.c::blk_recv (lines 349..442).
// =====================================================================

/// Drain the receiver's per-file block signature stream off the wire.
///
/// Direct port of `blocks.c::blk_recv`.
///
/// The wire shape per file is:
///
/// ```text
/// block_count: u32 LE     (s->blksz)
/// block_len:   u32 LE     (s->len)
/// strong_len:  u32 LE     (s->csum)
/// remainder:   u32 LE     (s->rem)
/// loop block_count times:
///   rolling:        u32 LE  (b->chksum_short)
///   strong[strong_len bytes]
/// ```
///
/// Returns the parsed [`BlockSet`]. The `size` field is computed from
/// the per-block `len` accumulation (mirrors `s->size = offs;` at
/// `blocks.c` line 434).
///
/// # Errors
///
/// - [`DomainError::RsyncProtocolError`] when the prologue's `rem >=
///   block_len` (mirrors openrsync's "block remainder is greater than
///   block size" guard at line 381..383), when `strong_len` exceeds
///   16 bytes, or when transport / EOF surfaces.
pub async fn read_blockset<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
) -> Result<BlockSet, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let prologue = read_prologue(reader, sess).await?;
    let blocks = read_block_signatures(reader, sess, &prologue).await?;
    let size = blocks_total_size(&prologue, blocks.len());
    Ok(BlockSet {
        size,
        block_count: prologue.block_count,
        block_len: prologue.block_len,
        remainder: prologue.remainder,
        strong_len: prologue.strong_len,
        blocks,
    })
}

/// Parsed prologue (4 little-endian u32 fields) — internal helper for
/// [`read_blockset`]. Kept separate so the inner `read_block_signatures`
/// loop can borrow only the metadata it actually needs.
#[derive(Debug, Clone, Copy)]
struct BlockSetPrologue {
    block_count: u32,
    block_len: u32,
    strong_len: u32,
    remainder: u32,
}

async fn read_prologue<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
) -> Result<BlockSetPrologue, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let block_count = read_u32(reader, sess).await?;
    let block_len = read_u32(reader, sess).await?;
    let strong_len = read_u32(reader, sess).await?;
    let remainder = read_u32(reader, sess).await?;
    tracing::debug!(
        target: "rsync.wire.blocks",
        block_count,
        block_len,
        strong_len,
        remainder,
        "read_prologue"
    );
    validate_prologue_invariants(block_count, block_len, strong_len, remainder)?;
    Ok(BlockSetPrologue {
        block_count,
        block_len,
        strong_len,
        remainder,
    })
}

/// Validate the four-field prologue invariants. Mirrors openrsync's
/// `blk_recv` guards plus the upstream `null_sum` (count = 0 → all
/// fields 0) tolerance per `generator.c::generate_and_send_sums` lines
/// 1948..1958.
fn validate_prologue_invariants(
    block_count: u32,
    block_len: u32,
    strong_len: u32,
    remainder: u32,
) -> Result<(), DomainError> {
    if remainder != 0 && remainder >= block_len {
        return Err(DomainError::RsyncProtocolError(format!(
            "blk_recv: remainder {remainder} >= block_len {block_len}"
        )));
    }
    let max_strong = u32_as_strong_len_default();
    if block_count != 0 && (strong_len == 0 || strong_len > max_strong) {
        return Err(DomainError::RsyncProtocolError(format!(
            "blk_recv: strong_len {strong_len} not in 1..={CSUM_LENGTH_PHASE2}"
        )));
    }
    if strong_len > max_strong {
        return Err(DomainError::RsyncProtocolError(format!(
            "blk_recv: strong_len {strong_len} > {CSUM_LENGTH_PHASE2}"
        )));
    }
    Ok(())
}

async fn read_block_signatures<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    prologue: &BlockSetPrologue,
) -> Result<Vec<BlockSig>, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let mut blocks: Vec<BlockSig> =
        Vec::with_capacity(usize::try_from(prologue.block_count).unwrap_or(0));
    let strong_bytes = usize::try_from(prologue.strong_len).unwrap_or(0);
    for idx in 0..prologue.block_count {
        let rolling_i32 = reader.read_int(sess).await?;
        let rolling = u32::from_ne_bytes(rolling_i32.to_ne_bytes());
        let mut strong = [0_u8; CSUM_LENGTH_PHASE2];
        if let Some(slot) = strong.get_mut(..strong_bytes) {
            reader.read_buf(sess, slot).await?;
        }
        blocks.push(BlockSig {
            idx,
            rolling,
            strong,
        });
    }
    Ok(blocks)
}

/// Compute the total file size implied by a block prologue.
///
/// Mirrors the `s->size = offs;` accumulator in `blocks.c::blk_recv`
/// (line 434). The last block carries the remainder when non-zero;
/// everything before it has the full length.
fn blocks_total_size(prologue: &BlockSetPrologue, block_count: usize) -> u64 {
    if block_count == 0 {
        return 0;
    }
    let last_idx = block_count.saturating_sub(1);
    let last_len = if prologue.remainder == 0 {
        prologue.block_len
    } else {
        prologue.remainder
    };
    let body_len =
        u64::from(prologue.block_len).saturating_mul(u64::try_from(last_idx).unwrap_or(0));
    body_len.saturating_add(u64::from(last_len))
}

async fn read_u32<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
) -> Result<u32, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let val = reader.read_int(sess).await?;
    if val < 0 {
        return Err(DomainError::RsyncProtocolError(format!(
            "blk_recv: negative u32 wire value {val}"
        )));
    }
    Ok(u32::from_ne_bytes(val.to_ne_bytes()))
}

// =====================================================================
// write_blockset_header — port of blocks.c::blk_recv_ack tail (no idx).
// =====================================================================

/// Emit the four-field block-set header the sender writes back to the
/// receiver after consuming a per-file signature request.
///
/// Mirrors the payload portion of `blocks.c::blk_recv_ack` (lines
/// 326..343), minus the leading `idx` field — callers emit the file
/// index separately so the same helper covers both the live file path
/// and the `idx == -1` phase-end marker.
///
/// Wire shape:
///
/// ```text
/// block_count: u32 LE
/// block_len:   u32 LE
/// strong_len:  u32 LE
/// remainder:   u32 LE
/// ```
///
/// # Errors
///
/// Returns [`DomainError::RsyncProtocolError`] if the underlying writer
/// fails.
pub async fn write_blockset_header<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    blockset: &BlockSet,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    write_u32(writer, sess, blockset.block_count).await?;
    write_u32(writer, sess, blockset.block_len).await?;
    write_u32(writer, sess, blockset.strong_len).await?;
    write_u32(writer, sess, blockset.remainder).await?;
    Ok(())
}

async fn write_u32<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    val: u32,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    let val_i32 = i32::from_ne_bytes(val.to_ne_bytes());
    writer.write_int(sess, val_i32).await
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code uses unwrap/expect for brevity per project convention"
)]
mod tests {
    use super::{
        BlockSet, BlockSig, CSUM_LENGTH_PHASE2, MAX_CHUNK, read_blockset, write_blockset_header,
    };
    use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
    use crate::adapters::rsync::wire::session::WireSession;
    use tokio::io::duplex;

    #[test]
    fn max_chunk_matches_openrsync_extern_h() {
        // openrsync extern.h line 37: #define MAX_CHUNK (32 * 1024)
        assert_eq!(MAX_CHUNK, 32 * 1024);
    }

    #[test]
    fn csum_length_phase2_matches_openrsync_extern_h() {
        // openrsync extern.h line 60: #define CSUM_LENGTH_PHASE2 (16)
        assert_eq!(CSUM_LENGTH_PHASE2, 16);
    }

    #[test]
    fn empty_blockset_is_whole_file() {
        let bs = BlockSet::empty();
        assert!(bs.is_whole_file());
        assert_eq!(bs.block_count, 0);
        assert_eq!(bs.strong_len, 16);
    }

    #[tokio::test]
    async fn read_blockset_count_zero_round_trips() {
        // Header only: count=0 len=0 csum=16 rem=0.
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut writer = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        writer.write_int(&mut sess_w, 0).await.expect("count");
        writer.write_int(&mut sess_w, 0).await.expect("len");
        writer.write_int(&mut sess_w, 16).await.expect("csum");
        writer.write_int(&mut sess_w, 0).await.expect("rem");
        let mut reader = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let bs = read_blockset(&mut reader, &mut sess_r).await.expect("read");
        assert!(bs.is_whole_file());
        assert_eq!(bs.block_count, 0);
        assert_eq!(bs.block_len, 0);
        assert_eq!(bs.strong_len, 16);
        assert_eq!(bs.remainder, 0);
        assert_eq!(bs.size, 0);
    }

    #[tokio::test]
    async fn read_blockset_with_two_blocks_decodes_each() {
        let (left, right) = duplex(2048);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut writer = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        // Header: 2 blocks, 700 byte block_len, 16-byte csum, 50-byte
        // remainder (last block is 50 bytes).
        writer.write_int(&mut sess_w, 2).await.expect("count");
        writer.write_int(&mut sess_w, 700).await.expect("len");
        writer.write_int(&mut sess_w, 16).await.expect("csum");
        writer.write_int(&mut sess_w, 50).await.expect("rem");
        // Block 0: rolling=0xdeadbeef, strong=[1; 16].
        writer
            .write_int(
                &mut sess_w,
                i32::from_ne_bytes(0xdead_beef_u32.to_ne_bytes()),
            )
            .await
            .expect("r0");
        writer
            .write_buf(&mut sess_w, &[1_u8; 16])
            .await
            .expect("s0");
        // Block 1: rolling=0xfeedface, strong=[2; 16].
        writer
            .write_int(
                &mut sess_w,
                i32::from_ne_bytes(0xfeed_face_u32.to_ne_bytes()),
            )
            .await
            .expect("r1");
        writer
            .write_buf(&mut sess_w, &[2_u8; 16])
            .await
            .expect("s1");

        let mut reader = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let bs = read_blockset(&mut reader, &mut sess_r).await.expect("read");
        assert_eq!(bs.block_count, 2);
        assert_eq!(bs.block_len, 700);
        assert_eq!(bs.remainder, 50);
        assert_eq!(bs.strong_len, 16);
        assert_eq!(bs.size, 750);
        assert_eq!(bs.blocks.len(), 2);
        assert_eq!(bs.blocks[0].rolling, 0xdead_beef_u32);
        assert_eq!(bs.blocks[0].strong, [1_u8; 16]);
        assert_eq!(bs.blocks[1].rolling, 0xfeed_face_u32);
        assert_eq!(bs.blocks[1].strong, [2_u8; 16]);
    }

    #[tokio::test]
    async fn read_blockset_rejects_remainder_ge_block_len() {
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut writer = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        writer.write_int(&mut sess_w, 1).await.expect("count");
        writer.write_int(&mut sess_w, 100).await.expect("len");
        writer.write_int(&mut sess_w, 16).await.expect("csum");
        writer.write_int(&mut sess_w, 100).await.expect("rem"); // rem == len → reject
        let mut reader = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let err = read_blockset(&mut reader, &mut sess_r)
            .await
            .expect_err("must err");
        assert!(format!("{err}").contains("remainder"));
    }

    #[tokio::test]
    async fn read_blockset_rejects_invalid_strong_len() {
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut writer = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        writer.write_int(&mut sess_w, 0).await.expect("count");
        writer.write_int(&mut sess_w, 0).await.expect("len");
        writer.write_int(&mut sess_w, 17).await.expect("csum"); // > 16 → reject
        writer.write_int(&mut sess_w, 0).await.expect("rem");
        let mut reader = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let err = read_blockset(&mut reader, &mut sess_r)
            .await
            .expect_err("must err");
        assert!(format!("{err}").contains("strong_len"));
    }

    #[tokio::test]
    async fn write_blockset_header_emits_four_le_u32() {
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut writer = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        let bs = BlockSet {
            size: 0,
            block_count: 0,
            block_len: 0,
            remainder: 0,
            strong_len: 16,
            blocks: Vec::new(),
        };
        write_blockset_header(&mut writer, &mut sess_w, &bs)
            .await
            .expect("header");

        let mut reader = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        // Round-trip: read four u32s back.
        assert_eq!(reader.read_int(&mut sess_r).await.expect("c"), 0);
        assert_eq!(reader.read_int(&mut sess_r).await.expect("l"), 0);
        assert_eq!(reader.read_int(&mut sess_r).await.expect("s"), 16);
        assert_eq!(reader.read_int(&mut sess_r).await.expect("r"), 0);
    }

    #[test]
    fn block_sig_value_type_is_copy() {
        let s = BlockSig {
            idx: 0,
            rolling: 0,
            strong: [0_u8; 16],
        };
        let _copy = s; // assert Copy
        let _again = s; // re-use after move proves Copy
    }
}
