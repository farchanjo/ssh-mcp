// SPDX-License-Identifier: ISC
//! Block-match path — Adler32 rolling hashtable + MD4/MD5 verification.
//!
//! Ported from openrsync's `blocks.c::blk_match` + `blocks.c::blk_find`
//! and the hashtable helpers `blkhash_alloc` / `blkhash_set`. Originally
//! Tridgell's algorithm; this is the openrsync C implementation
//! translated to async Rust.
//!
//! # Slice 6 scope (v7.0.0-alpha.X)
//!
//! Implements the **incremental sync** path for files where the
//! receiver's generator emits `block_count > 0` per-block signatures.
//! The sender slides a rolling-hash window over the local source bytes,
//! hashes each window through the receiver-supplied hashtable, verifies
//! candidate matches with the strong checksum (MD4 + seed at proto < 30
//! / MD4 + seed for the per-block strong even at proto >= 30 — see
//! [`crate::adapters::rsync::wire::hash::hash_slow_into`]), and emits
//! either a literal+bytes pair or a match-token sentinel per match.
//!
//! Wire output (per file):
//!
//! ```text
//! +N <N bytes>      : literal — N bytes of dirty data
//! -(K+1)            : match — block index K (no payload)
//! 0                 : end-of-file marker
//! <16-byte trailer> : per-file transfer digest (MD4-with-seed at proto
//!                     < 30, plain MD5 at proto >= 30)
//! ```
//!
//! See also: [`super::tokens::emit_whole_file_tokens`] for the
//! `block_count == 0` fast path that ships every byte as a literal.
//!
//! # Lock-free contract
//!
//! Every value lives on the per-task stack: the hashtable is built
//! once per file and dropped at end-of-file; the rolling window owns a
//! `(s1, s2, offs)` triple as plain locals; the [`MplexWriter`] is
//! exclusively borrowed from the caller. No `Mutex`, no shared cell,
//! no `Arc`. The matcher's worst-case state is the `BlockHashtable`
//! alongside whatever the caller already pinned — always per-task.

use tokio::io::AsyncWrite;
use tokio::task::yield_now;

use crate::adapters::rsync::wire::blocks::{BlockSet, MAX_CHUNK};
use crate::adapters::rsync::wire::hash::{
    FileHasher, combine_s1_s2, hash_roll, hash_slow_into, hash_split,
};
use crate::adapters::rsync::wire::io::MplexWriter;
use crate::adapters::rsync::wire::session::WireSession;
use crate::domain::error::DomainError;

/// Bucket count for the per-file rolling-hash hashtable. Mirrors
/// openrsync's `BLKTAB_SZ = 65536` from `blocks.c` line 57. Power of
/// two so `idx % BLKTAB_SZ` reduces to a bitmask under the optimiser
/// — same trade-off the C source makes ("Use the same that GPL rsync
/// uses").
const BLKTAB_SZ: usize = 65_536;

/// Slide-loop yield cadence for [`drive_match_loop`]. Every this-many
/// non-matching slide steps, the loop yields to the tokio scheduler via
/// `tokio::task::yield_now().await` so a long non-matching stretch of a
/// large file cannot starve co-scheduled tasks on the same worker
/// thread. Purely a scheduling nicety — it changes neither the emitted
/// tokens nor the byte offsets the matcher visits.
const MATCH_LOOP_YIELD_INTERVAL: u32 = 65_536;

/// Block hashtable keyed by Adler32 rolling-hash modulo
/// [`BLKTAB_SZ`].
///
/// Every bucket holds the indices of every block whose `rolling`
/// hash maps to that bucket. Collisions are common — at protocol 31
/// the receiver caps `block_count` around `2^16` typical, so a bucket
/// can hold one entry on average but several in the tail. We carry
/// the indices as `u32` rather than full [`BlockSig`] copies so the
/// matcher can re-borrow `&BlockSig` from the source-of-truth slice
/// without lifetime hand-waving.
///
/// Mirrors openrsync's `blktab` + `blkhash` pair (`blocks.c` lines
/// 34..50). The `Vec<Vec<u32>>` shape is the Rust equivalent of the
/// `TAILQ` chain — bucketed linked list.
#[derive(Debug)]
pub struct BlockHashtable {
    buckets: Vec<Vec<u32>>,
}

impl BlockHashtable {
    /// Build a hashtable from a [`BlockSet`].
    ///
    /// Mirrors `blocks.c::blkhash_set` (lines 89..117). Each block's
    /// `rolling` hash is reduced modulo [`BLKTAB_SZ`] and the block's
    /// index is appended to the bucket. Returns an empty hashtable
    /// when `blockset.blocks` is empty (the `blksz == 0` branch in
    /// upstream is no-op so we mirror that).
    #[must_use]
    pub fn build(blockset: &BlockSet) -> Self {
        let mut table = Self::empty();
        table.reset_for(blockset);
        table
    }

    /// Create a hashtable with no buckets allocated yet. The outer
    /// `Vec<Vec<u32>>` is only materialised on the first
    /// [`Self::reset_for`] call — cheap to construct up front so a
    /// caller driving multiple files in one session can allocate the
    /// pool once (see [`Self::reset_for`]).
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            buckets: Vec::new(),
        }
    }

    /// (Re)populate the hashtable from `blockset`, reusing the
    /// previously allocated buckets when this is not the first call.
    ///
    /// First call: allocates the [`BLKTAB_SZ`]-bucket outer `Vec`
    /// (mirrors [`Self::build`]'s one-shot allocation). Subsequent
    /// calls: instead of reallocating the ~1.5 MiB outer `Vec` of
    /// bucket headers per file, `clear()`s every bucket that was
    /// populated by the previous file (leaving its heap capacity
    /// intact for reuse) and then re-populates from `blockset`. A
    /// cleared bucket is fully equivalent to a fresh `Vec::new()`
    /// bucket for matching purposes — `Vec::clear` drops every
    /// element and sets `len == 0`, so no stale index from a prior
    /// file can leak into this file's candidate list. The bucket
    /// selection (mask), chaining, and lookup semantics are unchanged
    /// from [`Self::build`].
    pub fn reset_for(&mut self, blockset: &BlockSet) {
        if self.buckets.is_empty() {
            self.buckets.resize_with(BLKTAB_SZ, Vec::new);
        } else {
            for bucket in &mut self.buckets {
                if !bucket.is_empty() {
                    bucket.clear();
                }
            }
        }
        for sig in &blockset.blocks {
            let bucket_idx = bucket_for(sig.rolling);
            if let Some(bucket) = self.buckets.get_mut(bucket_idx) {
                bucket.push(sig.idx);
            }
        }
    }

    /// Return the indices stored for `rolling`'s bucket. Empty slice
    /// when the bucket is empty (no candidate matches at this
    /// rolling-hash modulus).
    #[must_use]
    pub fn lookup(&self, rolling: u32) -> &[u32] {
        self.buckets
            .get(bucket_for(rolling))
            .map_or(&[], Vec::as_slice)
    }
}

#[expect(
    clippy::as_conversions,
    reason = "bucket_for narrows u32 to the low 16 bits via `& (BLKTAB_SZ - 1)`. The narrowing is intentional and `as usize` keeps the function `const fn` (u32::try_into into usize is not const-stable on 1.95). Mirrors openrsync `blocks.c` line 111: `idx = bset->blks[i].chksum_short % p->qsz`."
)]
const fn bucket_for(rolling: u32) -> usize {
    // BLKTAB_SZ = 65_536 = 2^16. Reduce modulo via the low 16 bits;
    // saves a mod op + makes the function `const fn`.
    (rolling as usize) & (BLKTAB_SZ - 1)
}

/// Stream the local source bytes through the block-match state
/// machine, emitting the rsync token wire format.
///
/// Direct port of openrsync's `blocks.c::blk_match` (lines 243..318)
/// driven by `blocks.c::blk_find` (lines 136..235). Three loops live
/// inside:
///
/// 1. **Initial hash** — compute `(s1, s2)` over `bytes[0..block_len]`.
///    Mirrors the `recomp` branch of `blk_find` (line 161).
/// 2. **Slide loop** — at each window position, look up the rolling
///    hash in [`BlockHashtable`]. For each candidate block, verify
///    the strong checksum via [`hash_slow_into`]. On match: flush the
///    accumulated literal run, emit a `-(idx + 1)` match token,
///    advance by `block_len` bytes, and recompute `(s1, s2)` from the
///    new window. On miss: roll the window one byte forward via
///    [`hash_roll`].
/// 3. **Tail flush** — emit the final literal run (bytes that never
///    found a match), the `0` end-of-file token, and the 16-byte MD5
///    or MD4 trailer.
///
/// `seed` selects the per-block strong checksum; `negotiated`
/// selects the per-file trailer digest (MD5 at proto >= 30 / MD4 at
/// proto < 30 — see [`FileHasher::for_protocol`]).
///
/// `table` is the caller-owned [`BlockHashtable`] pool for this
/// session — reset in place via [`BlockHashtable::reset_for`] instead
/// of allocating a fresh 65536-bucket table per file. Passing the
/// same `table` across every file in a session's `send_files` loop
/// avoids a per-file ~1.5 MiB allocation without changing which
/// candidate blocks are found or which tokens are emitted.
///
/// # Errors
///
/// - [`DomainError::RsyncProtocolError`] when the underlying writer
///   fails or the blockset's `block_count > 0` but `block_len == 0`.
pub async fn emit_block_match_tokens<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    blockset: &BlockSet,
    bytes: &[u8],
    seed: i32,
    negotiated: i32,
    table: &mut BlockHashtable,
) -> Result<[u8; 16], DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    validate_blockset(blockset)?;
    // File smaller than block_len → single literal + EOF + digest.
    // Mirrors the "file's empty or no blocks" branch of `blk_match`
    // (lines 252..256) collapsed for the case `mapsz < blksz` where
    // the slide loop's `end` would be <= 0 anyway.
    if bytes.len() < usize::try_from(blockset.block_len).unwrap_or(usize::MAX) {
        return emit_single_literal_tail(writer, sess, bytes, seed, negotiated).await;
    }

    table.reset_for(blockset);
    let mut hasher = FileHasher::for_protocol(seed, negotiated);
    let mut state = MatchState::new(blockset, bytes);
    drive_match_loop(
        writer,
        sess,
        blockset,
        bytes,
        table,
        &mut hasher,
        &mut state,
    )
    .await?;
    flush_tail(writer, sess, bytes, &mut hasher, &state).await?;
    write_token(writer, sess, 0).await?;
    let digest = hasher.finish();
    writer.write_buf(sess, &digest).await?;
    Ok(digest)
}

/// Validate a non-whole-file [`BlockSet`] before the matcher
/// starts. Mirrors the early-out guards at the head of openrsync's
/// `blk_match` (lines 252..258) plus the upstream `blk_send_ack`
/// `csum == 0` rejection.
fn validate_blockset(blockset: &BlockSet) -> Result<(), DomainError> {
    if blockset.is_whole_file() {
        return Err(DomainError::RsyncProtocolError(
            "match: whole-file path called on block-match dispatcher".to_string(),
        ));
    }
    if blockset.block_len == 0 {
        return Err(DomainError::RsyncProtocolError(
            "match: blockset has block_count > 0 but block_len == 0".to_string(),
        ));
    }
    Ok(())
}

/// Write the entire file as one literal then EOF + digest. Used by
/// [`emit_block_match_tokens`] when the source file is smaller than
/// `block_len` (no rolling window can fit so the matcher cannot run).
async fn emit_single_literal_tail<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    bytes: &[u8],
    seed: i32,
    negotiated: i32,
) -> Result<[u8; 16], DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    let mut hasher = FileHasher::for_protocol(seed, negotiated);
    write_literal_run(writer, sess, bytes, &mut hasher).await?;
    write_token(writer, sess, 0).await?;
    let digest = hasher.finish();
    writer.write_buf(sess, &digest).await?;
    Ok(digest)
}

/// Per-file matcher state. Carries the rolling-hash partial sums plus
/// the cursors openrsync's `blk_match` threads through `st->offs` /
/// `last`. Held by value on the per-task stack — no `Arc`, no `Mutex`.
struct MatchState {
    /// Current head of the rolling window inside `bytes`. Starts at 0
    /// and advances by 1 on every miss / by `block_len` (or
    /// `remainder` for the tail block) on every match. Mirrors
    /// `st->offs`.
    offs: usize,
    /// Last position the matcher emitted a literal up to. The next
    /// literal flush covers `bytes[last..offs]`. Mirrors `last`.
    last: usize,
    /// `s1` accumulator (low 16 bits of the rolling hash). Valid only
    /// when `offs + block_len <= bytes.len()` (slide-window invariant).
    s1: u32,
    /// `s2` accumulator (high 16 bits of the rolling hash). See
    /// [`Self::s1`].
    s2: u32,
    /// Cached `block_len` as `usize` — the slide loop uses it dozens
    /// of times per byte, no point widening on every iter.
    block_len: usize,
    /// `mapsz - block_len + 1` — exclusive upper bound for the
    /// rolling-window starting offset. Mirrors `end` from `blk_match`
    /// line 268.
    end: usize,
}

impl MatchState {
    fn new(blockset: &BlockSet, bytes: &[u8]) -> Self {
        let block_len = usize::try_from(blockset.block_len).unwrap_or(usize::MAX);
        // openrsync `blk_match` line 268:
        //   end = mapsz + 1 - blks->blks[blks->blksz - 1].len
        // The last block's len is `remainder` when set, else the full
        // `block_len`. Saturating-sub guards against underflow when
        // mapsz < that value (single-literal tail path handles this
        // earlier).
        let last_len = if blockset.remainder == 0 {
            block_len
        } else {
            usize::try_from(blockset.remainder).unwrap_or(usize::MAX)
        };
        let end = bytes.len().saturating_add(1).saturating_sub(last_len);
        Self {
            offs: 0,
            last: 0,
            s1: 0,
            s2: 0,
            block_len,
            end,
        }
    }

    /// Recompute the rolling hash for the current window from
    /// scratch. Used after a match (when the window has jumped
    /// forward) — mirrors openrsync's "recomp" branch in `blk_find`
    /// (lines 158..164).
    fn recompute_window(&mut self, bytes: &[u8]) {
        let end = self.offs.saturating_add(self.block_len).min(bytes.len());
        let window = bytes.get(self.offs..end).unwrap_or(&[]);
        let (s1, s2) = hash_split(window);
        self.s1 = s1;
        self.s2 = s2;
    }
}

/// Drive the slide-and-match loop until `state.offs >= state.end`.
/// Each iteration either emits a match (recomputing the window from
/// scratch on the new offset) or rolls the window one byte forward.
///
/// Every [`MATCH_LOOP_YIELD_INTERVAL`] non-matching slide steps the
/// loop calls `tokio::task::yield_now().await` so a long
/// non-matching stretch (a large file with few matching blocks)
/// cannot monopolise its worker thread and starve co-scheduled tasks.
/// This is a scheduling-only change: it neither reorders nor skips
/// any byte the matcher visits, and emits the exact same token
/// stream as running the loop with no yield points at all.
async fn drive_match_loop<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    blockset: &BlockSet,
    bytes: &[u8],
    table: &BlockHashtable,
    hasher: &mut FileHasher,
    state: &mut MatchState,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    // Seed the initial (s1, s2) over bytes[0..block_len].
    state.recompute_window(bytes);
    let mut steps_since_yield: u32 = 0;
    while state.offs < state.end {
        let rolling = combine_s1_s2(state.s1, state.s2);
        tracing::trace!(
            target: "rsync.wire.match",
            offs = state.offs,
            rolling = format_args!("0x{rolling:08x}"),
            candidates = table.lookup(rolling).len(),
            "slide-loop probe"
        );
        if let Some(matched) = try_match(blockset, bytes, table, rolling, sess.seed, state) {
            // Found a match: flush bytes[last..offs] as literal, then
            // emit -(idx+1), then advance.
            handle_match(writer, sess, bytes, hasher, state, &matched).await?;
            // Re-seed (s1, s2) over the new window.
            state.recompute_window(bytes);
            continue;
        }
        // No match: slide the window by one byte. We need the
        // outgoing byte (at offs) and the incoming byte (at offs +
        // block_len) when available.
        slide_window_one_byte(bytes, state);
        steps_since_yield = steps_since_yield.saturating_add(1);
        if steps_since_yield >= MATCH_LOOP_YIELD_INTERVAL {
            steps_since_yield = 0;
            yield_now().await;
        }
    }
    Ok(())
}

/// Emit a match: flush the accumulated literal run (`bytes[last..offs]`)
/// then the `-(idx + 1)` match token. Updates `state.last` and bumps
/// `state.offs` past the matched block.
async fn handle_match<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    bytes: &[u8],
    hasher: &mut FileHasher,
    state: &mut MatchState,
    matched: &MatchedBlock,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    flush_pending_literal(writer, sess, bytes, hasher, state).await?;
    write_token(writer, sess, match_token_for(matched.idx)?).await?;
    consume_matched_window(bytes, hasher, state, matched)?;
    Ok(())
}

/// Flush `bytes[state.last..state.offs]` as a literal run when a
/// match is about to fire. No-op when `state.offs == state.last`.
async fn flush_pending_literal<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    bytes: &[u8],
    hasher: &mut FileHasher,
    state: &MatchState,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    if state.offs <= state.last {
        return Ok(());
    }
    let Some(literal) = bytes.get(state.last..state.offs) else {
        return Err(DomainError::RsyncProtocolError(format!(
            "match: literal slice [{}..{}] out of range (len={})",
            state.last,
            state.offs,
            bytes.len()
        )));
    };
    write_literal_run(writer, sess, literal, hasher).await
}

/// Encode a successful block match as the negative wire token
/// `-(idx + 1)`. Mirrors openrsync's `blk_match` line 283:
/// `tok = -(blk->idx + 1)`. Returns a protocol error when the
/// arithmetic overflows `i32` (only possible for an absurdly large
/// blockset > 2^31 entries).
fn match_token_for(idx: u32) -> Result<i32, DomainError> {
    let idx_plus_one = u64::from(idx).saturating_add(1);
    i64::try_from(idx_plus_one)
        .ok()
        .and_then(|v| i32::try_from(-v).ok())
        .ok_or_else(|| {
            DomainError::RsyncProtocolError(format!(
                "match: block idx {idx} does not fit i32 negative range"
            ))
        })
}

/// Feed the matched window into the per-file digest and bump the
/// matcher's cursors past it. Mirrors the post-token bookkeeping at
/// openrsync `blk_match` lines 285..299.
fn consume_matched_window(
    bytes: &[u8],
    hasher: &mut FileHasher,
    state: &mut MatchState,
    matched: &MatchedBlock,
) -> Result<(), DomainError> {
    let new_offs = state.offs.saturating_add(matched.matched_len);
    let Some(matched_bytes) = bytes.get(state.offs..new_offs) else {
        return Err(DomainError::RsyncProtocolError(format!(
            "match: matched slice [{}..{new_offs}] out of range (len={})",
            state.offs,
            bytes.len()
        )));
    };
    // Even though the sender does not transmit matched bytes as
    // literals, the receiver reconstructs the file from them and
    // verifies the digest, so the sender must include them in its
    // own running hash. Mirrors `blk_match` line 285:
    //   hash_file_buf(&st->ctx, st->map + last, sz + blk->len);
    hasher.update(matched_bytes);
    state.offs = new_offs;
    state.last = state.offs;
    Ok(())
}

/// Slide the rolling-hash window forward by one byte. Mirrors the
/// fall-through branch of `blk_find` (lines 224..234).
fn slide_window_one_byte(bytes: &[u8], state: &mut MatchState) {
    let out_byte = bytes.get(state.offs).copied().unwrap_or(0);
    let in_idx = state.offs.saturating_add(state.block_len);
    let in_byte = bytes.get(in_idx).copied();
    let block_len_u32 = u32::try_from(state.block_len).unwrap_or(u32::MAX);
    let (s1, s2) = hash_roll(state.s1, state.s2, out_byte, in_byte, block_len_u32);
    state.s1 = s1;
    state.s2 = s2;
    state.offs = state.offs.saturating_add(1);
}

/// Result of a successful candidate verification — carries the
/// matched block's index and the actual byte length consumed (which
/// is `block_len` for non-tail blocks and `remainder` for the tail
/// block when shorter).
#[derive(Debug, Clone, Copy)]
struct MatchedBlock {
    idx: u32,
    matched_len: usize,
}

/// Probe the hashtable for a candidate block matching `rolling` at
/// the current window position. Verifies via [`hash_slow_into`] (the
/// strong MD4-with-seed checksum) and returns the matched index +
/// length on success; `None` on miss.
///
/// Mirrors the bucket scan + slow-hash verification block of
/// openrsync's `blk_find` (lines 187..216).
fn try_match(
    blockset: &BlockSet,
    bytes: &[u8],
    table: &BlockHashtable,
    rolling: u32,
    seed: i32,
    state: &MatchState,
) -> Option<MatchedBlock> {
    let candidates = table.lookup(rolling);
    if candidates.is_empty() {
        return None;
    }
    let strong_len = usize::try_from(blockset.strong_len).ok()?;
    if strong_len == 0 || strong_len > 16 {
        return None;
    }
    let mut md = [0_u8; 16];
    let mut have_md = false;
    for &idx in candidates {
        if let Some(matched) = verify_candidate(
            blockset,
            bytes,
            rolling,
            seed,
            state,
            idx,
            strong_len,
            &mut md,
            &mut have_md,
        ) {
            return Some(matched);
        }
    }
    None
}

/// Verify one candidate block index from a rolling-hash bucket.
///
/// Returns `Some(MatchedBlock)` when the rolling hash, window length,
/// and strong checksum all agree. The caller owns `md` + `have_md`
/// across the whole bucket scan so the slow MD4 is computed at most
/// once per scan (mirrors `blk_find` line 192 lazy `have_md`
/// pattern).
#[expect(
    clippy::too_many_arguments,
    reason = "the helper is the inner kernel of the bucket scan; \
              every parameter is per-loop state that the caller owns. \
              Bundling them into a struct just to pacify the lint \
              would cost a heap alloc per slide step on the hot path."
)]
fn verify_candidate(
    blockset: &BlockSet,
    bytes: &[u8],
    rolling: u32,
    seed: i32,
    state: &MatchState,
    idx: u32,
    strong_len: usize,
    md: &mut [u8; 16],
    have_md: &mut bool,
) -> Option<MatchedBlock> {
    let idx_usize = usize::try_from(idx).ok()?;
    let sig = blockset.blocks.get(idx_usize)?;
    // Pre-filter: rolling must match exactly (the bucket only
    // confirmed `rolling % BLKTAB_SZ == sig.rolling % BLKTAB_SZ`,
    // not full equality). Mirrors `blk_find` line 197.
    if sig.rolling != rolling {
        return None;
    }
    let candidate_len = candidate_block_len(blockset, idx, bytes.len(), state)?;
    // Window length filter: openrsync `blk_find` lines 197..199 skip
    // entries whose `len` differs from the current window size.
    if candidate_len != state.block_len.min(bytes.len().saturating_sub(state.offs)) {
        return None;
    }
    let window_end = state.offs.saturating_add(candidate_len);
    let window = bytes.get(state.offs..window_end)?;
    if !*have_md {
        hash_slow_into(window, seed, md);
        *have_md = true;
    }
    if md.get(..strong_len) == sig.strong.get(..strong_len) {
        return Some(MatchedBlock {
            idx,
            matched_len: candidate_len,
        });
    }
    None
}

/// Compute a candidate block's effective length given the
/// blockset's prologue (full `block_len` for non-tail blocks,
/// remainder for the tail when set, or full `block_len` when
/// remainder == 0). Returns `None` when the math underflows
/// (defensive — should never happen for a well-formed blockset).
fn candidate_block_len(
    blockset: &BlockSet,
    idx: u32,
    map_len: usize,
    state: &MatchState,
) -> Option<usize> {
    let block_len_u = state.block_len;
    // The tail-block length is `remainder` when set, else `block_len`.
    let tail_len = if blockset.remainder == 0 {
        block_len_u
    } else {
        usize::try_from(blockset.remainder).ok()?
    };
    let last_idx = blockset.block_count.checked_sub(1)?;
    let len = if idx == last_idx {
        tail_len
    } else {
        block_len_u
    };
    // Defensive: the matcher only enters with offs <= end so the
    // slice access is in bounds. Confirm anyway.
    if state.offs.saturating_add(len) > map_len {
        return None;
    }
    Some(len)
}

/// Flush the trailing literal run (bytes the matcher never matched)
/// after the slide loop has terminated. The trailing run starts at
/// `state.last` and runs to the end of the buffer (everything beyond
/// `state.offs` is bytes the rolling window never reached because the
/// slide loop's `end` excludes the last block's interior).
async fn flush_tail<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    bytes: &[u8],
    hasher: &mut FileHasher,
    state: &MatchState,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    let tail = bytes.get(state.last..).unwrap_or(&[]);
    if tail.is_empty() {
        return Ok(());
    }
    write_literal_run(writer, sess, tail, hasher).await
}

/// Stream a literal run as one or more `+N <bytes>` tokens, chunking
/// at [`MAX_CHUNK`]. Mirrors the `BLKSTAT_DATA` arm of
/// openrsync's `sender.c::send_up_fsm` (lines 112..137).
async fn write_literal_run<W>(
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
                "match: literal chunk [{start}..{end}] out of range (len={})",
                bytes.len()
            )));
        };
        let take_i32 = i32::try_from(take).map_err(|err| {
            DomainError::RsyncProtocolError(format!("match: chunk size {take} > i32::MAX: {err}"))
        })?;
        write_token(writer, sess, take_i32).await?;
        writer.write_buf(sess, chunk).await?;
        hasher.update(chunk);
        start = end;
    }
    Ok(())
}

/// Emit a single int32 token (literal length, `0` for EOF, or
/// `-(idx + 1)` for a match).
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
    clippy::too_many_lines,
    reason = "test code uses unwrap/expect for brevity per project convention; longer table-driven tests document the wire shape verbatim"
)]
mod tests {
    use super::{BlockHashtable, emit_block_match_tokens};
    use crate::adapters::rsync::wire::blocks::{BlockSet, BlockSig};
    use crate::adapters::rsync::wire::hash::{FileHasher, hash_fast, hash_slow_into};
    use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
    use crate::adapters::rsync::wire::session::WireSession;
    use tokio::io::duplex;

    /// Build a [`BlockSet`] from `bytes` with `block_len`-sized blocks.
    /// Replicates what the receiver's generator would emit so the
    /// matcher can be exercised against a known-good signature stream.
    fn blockset_from_bytes(bytes: &[u8], block_len: u32, seed: i32) -> BlockSet {
        let block_len_u = block_len as usize;
        let mut blocks = Vec::new();
        let mut idx = 0_u32;
        let mut offset = 0_usize;
        let mut remainder = 0_u32;
        while offset < bytes.len() {
            let take = bytes.len().saturating_sub(offset).min(block_len_u);
            let end = offset + take;
            let chunk = &bytes[offset..end];
            let rolling = hash_fast(chunk);
            let mut strong = [0_u8; 16];
            hash_slow_into(chunk, seed, &mut strong);
            blocks.push(BlockSig {
                idx,
                rolling,
                strong,
            });
            if take < block_len_u {
                remainder = take as u32;
            }
            idx += 1;
            offset = end;
        }
        BlockSet {
            size: bytes.len() as u64,
            block_count: blocks.len() as u32,
            block_len,
            remainder,
            strong_len: 16,
            blocks,
        }
    }

    #[test]
    fn block_hashtable_buckets_collisions_into_single_slot() {
        // Two BlockSigs with the same rolling hash → both indices in
        // the same bucket. Exercises the collision path of
        // `BlockHashtable::lookup`.
        let blockset = BlockSet {
            size: 0,
            block_count: 2,
            block_len: 4,
            remainder: 0,
            strong_len: 16,
            blocks: vec![
                BlockSig {
                    idx: 0,
                    rolling: 0x4242,
                    strong: [0_u8; 16],
                },
                BlockSig {
                    idx: 1,
                    rolling: 0x4242,
                    strong: [1_u8; 16],
                },
            ],
        };
        let table = BlockHashtable::build(&blockset);
        let bucket = table.lookup(0x4242);
        assert_eq!(bucket.len(), 2);
        assert!(bucket.contains(&0));
        assert!(bucket.contains(&1));
    }

    #[test]
    fn block_hashtable_lookup_empty_for_unknown_rolling() {
        let blockset = BlockSet {
            size: 0,
            block_count: 1,
            block_len: 4,
            remainder: 0,
            strong_len: 16,
            blocks: vec![BlockSig {
                idx: 0,
                rolling: 0x1234,
                strong: [0_u8; 16],
            }],
        };
        let table = BlockHashtable::build(&blockset);
        assert!(table.lookup(0xdead_beef).is_empty());
    }

    /// Drive the matcher writer-side and decode the resulting token
    /// stream off the duplex pair. Returns the list of decoded
    /// (token, payload) tuples plus the final 16-byte digest.
    async fn run_matcher(
        bytes: &[u8],
        blockset: &BlockSet,
        seed: i32,
        negotiated: i32,
    ) -> (Vec<(i32, Vec<u8>)>, [u8; 16]) {
        let (left, right) = duplex(8 * 1024 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut writer = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        // The matcher reads seed off `sess.seed` (matches the live
        // sender path which uses `sess.seed` from the rsync handshake
        // for strong-block checksums). Pin it to the same seed the
        // blockset was hashed under.
        sess_w.seed = seed;
        let mut table = BlockHashtable::empty();
        let _expected_digest = emit_block_match_tokens(
            &mut writer,
            &mut sess_w,
            blockset,
            bytes,
            seed,
            negotiated,
            &mut table,
        )
        .await
        .expect("emit");
        drop(writer);

        let mut reader = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let mut tokens = Vec::new();
        loop {
            let tok = reader.read_int(&mut sess_r).await.expect("tok");
            if tok == 0 {
                break;
            }
            if tok > 0 {
                let mut payload = vec![0_u8; tok as usize];
                reader
                    .read_buf(&mut sess_r, &mut payload)
                    .await
                    .expect("literal payload");
                tokens.push((tok, payload));
            } else {
                tokens.push((tok, Vec::new()));
            }
        }
        let mut digest = [0_u8; 16];
        reader
            .read_buf(&mut sess_r, &mut digest)
            .await
            .expect("digest");
        (tokens, digest)
    }

    #[tokio::test]
    async fn identical_files_emit_only_match_tokens() {
        // Local source identical to remote signatures → every block
        // matches, zero literal bytes emitted.
        let bytes = vec![0xab_u8; 4 * 1024]; // 4 KiB of constant data
        let block_len = 1024_u32;
        let seed = 0x1234_5678_i32;
        let blockset = blockset_from_bytes(&bytes, block_len, seed);

        let (tokens, digest) = run_matcher(&bytes, &blockset, seed, 27).await;

        // 4 blocks, all match → 4 negative tokens, no literals.
        assert!(
            tokens.iter().all(|(t, _)| *t < 0),
            "expected only match tokens, got {tokens:?}"
        );
        assert_eq!(tokens.len(), 4, "4 blocks → 4 match tokens");

        // Digest must equal MD4(seed_le || bytes) at proto < 30.
        let mut h = FileHasher::start(seed);
        h.update(&bytes);
        assert_eq!(digest, h.finish());
    }

    #[tokio::test]
    async fn appended_bytes_emit_match_tokens_then_literal() {
        // Remote has prefix; local has prefix + suffix. Matcher should
        // match the whole prefix as blocks then emit the suffix as one
        // literal.
        let prefix = vec![0xab_u8; 4 * 1024];
        let mut bytes = prefix.clone();
        bytes.extend_from_slice(b"appended-tail-bytes");
        let block_len = 1024_u32;
        let seed = 7_i32;
        // Generate the blockset against the prefix only (= remote's
        // view of the file).
        let blockset = blockset_from_bytes(&prefix, block_len, seed);

        let (tokens, digest) = run_matcher(&bytes, &blockset, seed, 27).await;

        // 4 match tokens (one per prefix block) + 1 literal for the
        // appended bytes.
        let neg_count = tokens.iter().filter(|(t, _)| *t < 0).count();
        let lit_count = tokens.iter().filter(|(t, _)| *t > 0).count();
        assert_eq!(neg_count, 4, "4 prefix blocks → 4 match tokens");
        assert_eq!(lit_count, 1, "1 appended-tail literal");
        let lit_total: usize = tokens
            .iter()
            .filter(|(t, _)| *t > 0)
            .map(|(_, p)| p.len())
            .sum();
        assert_eq!(lit_total, b"appended-tail-bytes".len());

        // Digest must equal MD4(seed_le || full local file).
        let mut h = FileHasher::start(seed);
        h.update(&bytes);
        assert_eq!(digest, h.finish());
    }

    #[tokio::test]
    async fn prepended_bytes_emit_literal_then_match_tokens() {
        // Remote has suffix; local has prefix + suffix. Matcher slides
        // window across the prefix (no matches → byte-by-byte literal
        // accumulation), then matches each suffix block as a block
        // token.
        let suffix = vec![0xab_u8; 4 * 1024];
        let mut bytes = b"prepended-head-bytes".to_vec();
        bytes.extend_from_slice(&suffix);
        let block_len = 1024_u32;
        let seed = 9_i32;
        // Blockset is keyed on the suffix (= remote's view).
        let blockset = blockset_from_bytes(&suffix, block_len, seed);

        let (tokens, digest) = run_matcher(&bytes, &blockset, seed, 27).await;

        let neg_count = tokens.iter().filter(|(t, _)| *t < 0).count();
        let lit_count = tokens.iter().filter(|(t, _)| *t > 0).count();
        assert_eq!(neg_count, 4, "4 suffix blocks → 4 match tokens");
        assert!(lit_count >= 1, "at least 1 prepended-head literal");
        let lit_total: usize = tokens
            .iter()
            .filter(|(t, _)| *t > 0)
            .map(|(_, p)| p.len())
            .sum();
        assert_eq!(lit_total, b"prepended-head-bytes".len());

        let mut h = FileHasher::start(seed);
        h.update(&bytes);
        assert_eq!(digest, h.finish());
    }

    #[tokio::test]
    async fn file_smaller_than_block_len_emits_single_literal() {
        // Local is shorter than block_len → matcher routes to
        // emit_single_literal_tail.
        let bytes = b"tiny";
        let blockset = BlockSet {
            size: 1024,
            block_count: 1,
            block_len: 1024,
            remainder: 0,
            strong_len: 16,
            blocks: vec![BlockSig {
                idx: 0,
                rolling: 0,
                strong: [0_u8; 16],
            }],
        };
        let seed = 0_i32;
        let (tokens, digest) = run_matcher(bytes, &blockset, seed, 27).await;
        assert_eq!(tokens.len(), 1, "single literal");
        assert_eq!(tokens[0].0, bytes.len() as i32, "literal length");
        assert_eq!(tokens[0].1, bytes);

        let mut h = FileHasher::start(seed);
        h.update(bytes);
        assert_eq!(digest, h.finish());
    }

    #[tokio::test]
    async fn empty_blockset_returns_protocol_error() {
        // Whole-file path called on the block-match dispatcher → must
        // error so the dispatcher cannot silently corrupt the stream.
        let bytes = b"any";
        let blockset = BlockSet::empty(); // is_whole_file → true

        let (left, _right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        drop(lr);
        let mut writer = MplexWriter::new(lw);
        let mut sess = WireSession::new();
        let mut table = BlockHashtable::empty();
        let err =
            emit_block_match_tokens(&mut writer, &mut sess, &blockset, bytes, 0, 31, &mut table)
                .await
                .expect_err("must err");
        assert!(format!("{err}").contains("whole-file"));
    }

    #[tokio::test]
    async fn proto_31_uses_md5_trailer_no_seed() {
        // Identical files at proto >= 30 → MD5 trailer (no seed
        // prefix). Matches the wire shape every modern rsync 3.x
        // server expects.
        let bytes = vec![0xcd_u8; 2 * 1024];
        let block_len = 1024_u32;
        let seed = 0xdead_beef_u32 as i32;
        let blockset = blockset_from_bytes(&bytes, block_len, seed);

        let (_tokens, digest) = run_matcher(&bytes, &blockset, seed, 31).await;

        use md5::{Digest as Md5Digest, Md5};
        let mut want = Md5::new();
        Md5Digest::update(&mut want, &bytes);
        assert_eq!(&digest[..], want.finalize().as_slice());
    }

    #[tokio::test]
    async fn middle_modification_emits_split_match_literal_match() {
        // Local = [block0_orig | corrupted_middle | block2_orig].
        // Remote signatures cover block0 + block1 + block2 of the
        // original file. After the local middle is corrupted, matcher
        // should match block0, miss block1 (emit as literal), match
        // block2.
        let block_len = 1024_u32;
        let block_len_u = block_len as usize;
        let block0 = vec![0x11_u8; block_len_u];
        let block1 = vec![0x22_u8; block_len_u];
        let block2 = vec![0x33_u8; block_len_u];
        let mut original = Vec::new();
        original.extend_from_slice(&block0);
        original.extend_from_slice(&block1);
        original.extend_from_slice(&block2);
        let seed = 0_i32;
        let blockset = blockset_from_bytes(&original, block_len, seed);

        // Local version: corrupt the middle block.
        let corrupted_middle = vec![0xff_u8; block_len_u];
        let mut local = Vec::new();
        local.extend_from_slice(&block0);
        local.extend_from_slice(&corrupted_middle);
        local.extend_from_slice(&block2);

        let (tokens, digest) = run_matcher(&local, &blockset, seed, 27).await;

        let neg_count = tokens.iter().filter(|(t, _)| *t < 0).count();
        let lit_total: usize = tokens
            .iter()
            .filter(|(t, _)| *t > 0)
            .map(|(_, p)| p.len())
            .sum();
        assert_eq!(neg_count, 2, "block0 + block2 match");
        assert_eq!(lit_total, block_len_u, "corrupted middle = 1 block literal");

        // Digest reproduces over the local bytes (sender's view).
        let mut h = FileHasher::start(seed);
        h.update(&local);
        assert_eq!(digest, h.finish());
    }
}
