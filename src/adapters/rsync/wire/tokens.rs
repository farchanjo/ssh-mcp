// SPDX-License-Identifier: ISC
//! Sender-side token stream emission.
//!
//! Ported from openrsync's `sender.c::send_up_fsm` (lines 100..273) and
//! `blocks.c::blk_match` (lines 243..318). The token stream is the
//! per-file inner-protocol payload the sender writes after echoing the
//! receiver's blockset header back. Three token forms exist:
//!
//! ```text
//! +N    : positive int32 — N literal bytes follow
//! -K    : negative int32 — match block index (K-1) (no payload)
//! 0     : zero int32      — end-of-file marker; followed by 16-byte MD4
//! ```
//!
//! ## Slice 3 scope
//!
//! Slice 3 ships only the **whole-file fast path** (the
//! `BlockSet::is_whole_file` branch). The block-match path that
//! requires Adler32 + MD4 hashtable scanning lands in slice 4. The
//! current emit chunks the whole file into [`MAX_CHUNK`]-sized literals
//! and finalises with a `0` token + 16-byte MD4 trailer driven by
//! [`crate::adapters::rsync::wire::hash::FileHasher`].
//!
//! ## Lock-free contract
//!
//! Every write goes through the caller's exclusively-owned
//! [`MplexWriter`]. There is no shared output buffer, no `Mutex`. The
//! `FileHasher` is consumed by value when finalising — no shared digest
//! context.

use tokio::io::AsyncWrite;

use crate::adapters::rsync::wire::blocks::{BlockSet, MAX_CHUNK};
use crate::adapters::rsync::wire::hash::FileHasher;
use crate::adapters::rsync::wire::io::MplexWriter;
use crate::adapters::rsync::wire::match_path::emit_block_match_tokens;
use crate::adapters::rsync::wire::session::WireSession;
use crate::domain::error::DomainError;

/// Dispatch on `blockset.is_whole_file()`.
///
/// Routes through [`emit_whole_file_tokens`] when the receiver
/// streamed `count == 0` (the fast path — every byte goes out as a
/// literal), otherwise routes through
/// [`emit_block_match_tokens`](crate::adapters::rsync::wire::match_path::emit_block_match_tokens)
/// (slice 6 — Adler32 rolling-hash matcher).
///
/// Both paths return the 16-byte per-file transfer digest written
/// straight after the EOF token. Callers do not need to know which
/// branch ran.
///
/// `seed` is the rsync session seed; `negotiated` selects the digest
/// algorithm (MD5 at proto >= 30 / MD4-with-seed at proto < 30 — see
/// [`FileHasher::for_protocol`]).
///
/// # Errors
///
/// - [`DomainError::RsyncProtocolError`] when the underlying writer
///   fails or the blockset is structurally invalid for either branch.
pub async fn emit_token_stream<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    blockset: &BlockSet,
    bytes: &[u8],
    seed: i32,
    negotiated: i32,
) -> Result<[u8; 16], DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    if blockset.is_whole_file() {
        emit_whole_file_tokens(writer, sess, blockset, bytes, seed, negotiated).await
    } else {
        emit_block_match_tokens(writer, sess, blockset, bytes, seed, negotiated).await
    }
}

/// Stream a whole local file through the sender token wire format.
///
/// `bytes` is the full contents of the source file (slice 3
/// simplification — slice 4+ chunks via `mmap` / streaming). The
/// function chunks the buffer into `MAX_CHUNK`-sized literal tokens,
/// emits a final `0` end-of-file token, and writes the per-file
/// transfer digest as a 16-byte trailer.
///
/// `blockset` is consulted for the `is_whole_file()` predicate. Slice
/// 3 only supports the whole-file path; if `blockset` carries
/// per-block signatures the function returns
/// [`DomainError::RsyncProtocolError`] so the slice-4 work cannot
/// silently drop bytes.
///
/// `negotiated` selects the digest algorithm — see
/// [`FileHasher::for_protocol`]. At protocol >= 30 we ship plain
/// `MD5(file_bytes)`; below that we fall back to the openrsync
/// `MD4(seed_le || file_bytes)` shape. Both produce 16-byte trailers.
///
/// # Errors
///
/// - [`DomainError::RsyncProtocolError`] when `blockset.is_whole_file()`
///   returns false (slice-4 territory) or the underlying writer fails.
pub async fn emit_whole_file_tokens<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    blockset: &BlockSet,
    bytes: &[u8],
    seed: i32,
    negotiated: i32,
) -> Result<[u8; 16], DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    if !blockset.is_whole_file() {
        return Err(DomainError::RsyncProtocolError(
            "tokens: block-match path is slice 4 territory; got non-empty blockset".to_string(),
        ));
    }
    let mut hasher = FileHasher::for_protocol(seed, negotiated);
    emit_literal_chunks(writer, sess, bytes, &mut hasher).await?;
    write_token(writer, sess, 0).await?;
    let digest = hasher.finish();
    writer.write_buf(sess, &digest).await?;
    Ok(digest)
}

/// Drain the file into [`MAX_CHUNK`]-sized literal tokens. Each chunk
/// is preceded by a positive int32 length token and followed by the
/// raw bytes. Mirrors the `BLKSTAT_DATA` branch of
/// `sender.c::send_up_fsm` (lines 112..137).
async fn emit_literal_chunks<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    bytes: &[u8],
    hasher: &mut FileHasher,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    let max_chunk = usize::try_from(MAX_CHUNK).unwrap_or(usize::MAX);
    let mut start = 0_usize;
    while start < bytes.len() {
        let take = bytes.len().saturating_sub(start).min(max_chunk);
        let end = start.saturating_add(take);
        let Some(chunk) = bytes.get(start..end) else {
            return Err(DomainError::RsyncProtocolError(format!(
                "tokens: literal chunk slice [{start}..{end}] out of range (len={})",
                bytes.len()
            )));
        };
        let take_i32 = i32::try_from(take).map_err(|err| {
            DomainError::RsyncProtocolError(format!("tokens: chunk size {take} > i32::MAX: {err}"))
        })?;
        write_token(writer, sess, take_i32).await?;
        writer.write_buf(sess, chunk).await?;
        hasher.update(chunk);
        start = end;
    }
    Ok(())
}

/// Emit a single int32 token. Direct port of the `io_lowbuffer_int`
/// call inside the `BLKSTAT_TOK` arm of `sender.c::send_up_fsm` (line
/// 152). Note that `+N` for `N > 0` is a literal length, `0` is the
/// end-of-file sentinel, and `-K` for `K > 0` would be a match — but
/// the slice-3 whole-file path never emits matches.
async fn write_token<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    token: i32,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    writer.write_int(sess, token).await
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code uses unwrap/expect for brevity per project convention"
)]
mod tests {
    use super::{MAX_CHUNK, emit_whole_file_tokens};
    use crate::adapters::rsync::wire::blocks::{BlockSet, BlockSig};
    use crate::adapters::rsync::wire::hash::FileHasher;
    use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
    use crate::adapters::rsync::wire::session::WireSession;
    use tokio::io::duplex;

    fn drain_to_buf() -> WireSession {
        // helper alias — not strictly needed but mirrors local style.
        WireSession::new()
    }

    #[tokio::test]
    async fn whole_file_path_emits_literal_then_eof_then_md4() {
        let bytes = b"hello world";
        let seed = 0x1234_5678_i32;
        let blockset = BlockSet::empty();

        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut writer = MplexWriter::new(lw);
        let mut sess_w = drain_to_buf();
        // Pin negotiated = 27 to keep the legacy MD4 + seed shape so
        // the assertion against `FileHasher::start(seed)` (which is
        // MD4-with-seed) keeps holding.
        let digest = emit_whole_file_tokens(&mut writer, &mut sess_w, &blockset, bytes, seed, 27)
            .await
            .expect("emit");

        // Drain back through MplexReader: literal-len token, the bytes,
        // then EOF token, then 16-byte digest.
        let mut reader = MplexReader::new(rr);
        let mut sess_r = drain_to_buf();
        let lit_len = reader.read_int(&mut sess_r).await.expect("lit");
        assert_eq!(lit_len, bytes.len() as i32);
        let mut got_payload = vec![0_u8; bytes.len()];
        reader
            .read_buf(&mut sess_r, &mut got_payload)
            .await
            .expect("payload");
        assert_eq!(got_payload, bytes);
        let eof = reader.read_int(&mut sess_r).await.expect("eof");
        assert_eq!(eof, 0);
        let mut got_digest = [0_u8; 16];
        reader
            .read_buf(&mut sess_r, &mut got_digest)
            .await
            .expect("digest");
        assert_eq!(got_digest, digest);

        // Independently re-derive the digest and confirm.
        let mut h = FileHasher::start(seed);
        h.update(bytes);
        let want_digest = h.finish();
        assert_eq!(digest, want_digest);
    }

    #[tokio::test]
    async fn whole_file_path_chunks_at_max_chunk() {
        // File 2.5x MAX_CHUNK in size — expect three literal tokens.
        let one_chunk = MAX_CHUNK as usize;
        let total = one_chunk * 2 + (one_chunk / 2);
        let bytes = vec![0xab_u8; total];
        let seed = 0_i32;
        let blockset = BlockSet::empty();

        let (left, right) = duplex(8 * 1024 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut writer = MplexWriter::new(lw);
        let mut sess_w = drain_to_buf();
        // Pin negotiated = 27 — keep the legacy MD4-with-seed shape
        // so the local `FileHasher::start(seed)` reference matches.
        emit_whole_file_tokens(&mut writer, &mut sess_w, &blockset, &bytes, seed, 27)
            .await
            .expect("emit");

        let mut reader = MplexReader::new(rr);
        let mut sess_r = drain_to_buf();
        let mut accumulated_chunks = 0_usize;
        let mut accumulated_bytes = 0_usize;
        loop {
            let tok = reader.read_int(&mut sess_r).await.expect("tok");
            if tok == 0 {
                break;
            }
            assert!(tok > 0, "slice 3 only emits positive literals or EOF");
            let take = tok as usize;
            let mut chunk_buf = vec![0_u8; take];
            reader
                .read_buf(&mut sess_r, &mut chunk_buf)
                .await
                .expect("chunk");
            for b in &chunk_buf {
                assert_eq!(*b, 0xab);
            }
            accumulated_chunks += 1;
            accumulated_bytes += take;
        }
        assert_eq!(accumulated_bytes, total);
        assert_eq!(accumulated_chunks, 3, "2.5x MAX_CHUNK → 3 literal tokens");
        let mut digest_back = [0_u8; 16];
        reader
            .read_buf(&mut sess_r, &mut digest_back)
            .await
            .expect("digest");
        // Re-derive locally.
        let mut h = FileHasher::start(seed);
        h.update(&bytes);
        assert_eq!(digest_back, h.finish());
    }

    #[tokio::test]
    async fn empty_file_emits_only_eof_then_md4() {
        let bytes: &[u8] = b"";
        let seed = 7_i32;
        let blockset = BlockSet::empty();

        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut writer = MplexWriter::new(lw);
        let mut sess_w = drain_to_buf();
        // Pin negotiated = 27 to exercise the legacy MD4 + seed path.
        emit_whole_file_tokens(&mut writer, &mut sess_w, &blockset, bytes, seed, 27)
            .await
            .expect("emit");

        let mut reader = MplexReader::new(rr);
        let mut sess_r = drain_to_buf();
        let eof = reader.read_int(&mut sess_r).await.expect("eof");
        assert_eq!(eof, 0);
        let mut digest = [0_u8; 16];
        reader
            .read_buf(&mut sess_r, &mut digest)
            .await
            .expect("digest");
        // Empty file with seed=7 → MD4 of seed-LE only.
        use md4::{Digest, Md4};
        let mut h = Md4::new();
        h.update(7_i32.to_le_bytes());
        assert_eq!(&digest[..], h.finalize().as_slice());
    }

    #[tokio::test]
    async fn whole_file_path_proto_31_emits_md5_no_seed_trailer() {
        // At negotiated >= 30 the trailer must be plain MD5 of the
        // file bytes — no seed prefix. This is the wire shape every
        // modern rsync 3.x server expects in `receiver.c::receive_data`
        // line 411 (`memcmp(file_sum1, sender_file_sum, xfer_sum_len)`).
        let bytes = b"hello-from-a";
        let seed = 0xdead_beef_u32 as i32;
        let blockset = BlockSet::empty();

        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut writer = MplexWriter::new(lw);
        let mut sess_w = drain_to_buf();
        emit_whole_file_tokens(&mut writer, &mut sess_w, &blockset, bytes, seed, 31)
            .await
            .expect("emit");

        let mut reader = MplexReader::new(rr);
        let mut sess_r = drain_to_buf();
        // Skip the literal token + bytes + EOF to land on the digest.
        let lit_len = reader.read_int(&mut sess_r).await.expect("lit");
        assert_eq!(lit_len, bytes.len() as i32);
        let mut got_payload = vec![0_u8; bytes.len()];
        reader
            .read_buf(&mut sess_r, &mut got_payload)
            .await
            .expect("payload");
        assert_eq!(got_payload, bytes);
        let eof = reader.read_int(&mut sess_r).await.expect("eof");
        assert_eq!(eof, 0);
        let mut digest = [0_u8; 16];
        reader
            .read_buf(&mut sess_r, &mut digest)
            .await
            .expect("digest");

        // Reference: plain MD5 of the file bytes — no seed mixed in.
        use md5::{Digest as Md5Digest, Md5};
        let mut want = Md5::new();
        Md5Digest::update(&mut want, bytes);
        assert_eq!(&digest[..], want.finalize().as_slice());
    }

    #[tokio::test]
    async fn block_match_path_returns_slice4_error() {
        let bytes = b"data";
        let seed = 0_i32;
        let blockset = BlockSet {
            size: 4,
            block_count: 1,
            block_len: 4,
            remainder: 0,
            strong_len: 16,
            blocks: vec![BlockSig {
                idx: 0,
                rolling: 0,
                strong: [0_u8; 16],
            }],
        };

        let (left, _right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        drop(lr);
        let mut writer = MplexWriter::new(lw);
        let mut sess_w = drain_to_buf();
        let err = emit_whole_file_tokens(&mut writer, &mut sess_w, &blockset, bytes, seed, 31)
            .await
            .expect_err("must err");
        assert!(format!("{err}").contains("slice 4"));
    }
}
