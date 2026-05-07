// SPDX-License-Identifier: ISC
//! Sender state machine — port of openrsync `sender.c::rsync_sender`.
//!
//! Original copyright: Kristaps Dzonsons; ISC license. See
//! `LICENSES/openrsync-ISC.txt` for the full notice.
//!
//! # Slice 3 scope (v7.0.0-alpha.X)
//!
//! Drives the push direction of the rsync v31 transfer once the
//! handshake + filter list + flist exchange has completed (slices 1 +
//! 2). The state machine is a simplified Rust translation of
//! `sender.c::rsync_sender` (lines 357..688) operating in
//! synchronous-await fashion:
//!
//! 1. Read int32 — file index from the receiver's generator.
//!    - `>= 0` → process this file.
//!    - `-1` → end of phase. We then emit the closing `-1` index back
//!      (sender done) and return.
//!    - other negative → protocol error.
//! 2. Read the per-file blockset signature stream (whole-file path:
//!    `count == 0`).
//! 3. Echo the file index back to the receiver.
//! 4. Re-emit the four-field blockset header (`count, len, csum, rem`).
//! 5. Open the local source file, read it whole into memory (slice 3
//!    simplification), emit the literal/EOF token stream + 16-byte MD4
//!    digest trailer via [`tokens::emit_whole_file_tokens`].
//! 6. Push a [`RsyncProgressEvent::FileCompleted`] beacon onto the
//!    progress lane.
//! 7. Loop back to (1).
//!
//! # Block-match path (slice 4+)
//!
//! When the receiver streams `count > 0` block signatures, the sender
//! must build an Adler32 hashtable, slide a rolling window across the
//! local file, emit match tokens for hits and literal tokens for
//! misses. Slice 3 only ships the whole-file path; if the receiver
//! ever sends `count > 0` we surface a clean `RsyncProtocolError`
//! pointing at slice 4 instead of silently corrupting the transfer.
//!
//! # Lock-free contract (CRITICAL)
//!
//! - The state machine is one async fn driven by a single tokio task.
//! - The `MplexReader` / `MplexWriter` halves are owned exclusively
//!   by that task — no shared cell, no `Mutex`.
//! - The `WireSession` is threaded as `&mut` and never wrapped in
//!   `Arc<Mutex<...>>`.
//! - The progress `mpsc::Sender` is the only cross-task hook; receive
//!   side is the lane consumer in `super::WireRsyncTransport`.

use std::path::Path;

use tokio::fs;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

use crate::adapters::rsync::types::{RsyncProgressEvent, SkipReason};
use crate::adapters::rsync::wire::blocks::{read_blockset, write_blockset_header};
use crate::adapters::rsync::wire::flist::Flist;
use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
use crate::adapters::rsync::wire::ndx::{NDX_DONE, NdxState, read_ndx, write_ndx};
use crate::adapters::rsync::wire::session::WireSession;
use crate::adapters::rsync::wire::tokens::emit_token_stream;
use crate::domain::error::DomainError;
use crate::domain::rsync::RsyncStats;

/// Group of per-session channel-half + cancel + progress refs the
/// sender state machine threads through every helper. Bundling them
/// into one borrowable value avoids the `too_many_arguments` lint
/// (Layer B `clippy::all`) on the helper fns.
struct SenderCtx<'a, R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    reader: &'a mut MplexReader<R>,
    writer: &'a mut MplexWriter<W>,
    sess: &'a mut WireSession,
    flist: &'a [Flist],
    source_root: &'a Path,
    progress_tx: &'a Sender<RsyncProgressEvent>,
    cancel: &'a CancellationToken,
    /// Codec state for `read_ndx` (proto-30+ index byte-reduction).
    /// Per-direction; reader and writer keep separate cursors per
    /// upstream rsync 3.2.7's `static int32 prev_*` initialisation.
    ndx_in: &'a mut NdxState,
    /// Codec state for `write_ndx`. See [`Self::ndx_in`] for invariants.
    ndx_out: &'a mut NdxState,
    /// `--dry-run` mode flag. When set, the sender mirrors upstream
    /// rsync 3.2.7's `sender.c::send_files` lines 343..347 dry-run
    /// branch: per-file `ITEM_TRANSFER` frames echo `ndx + iflags` only
    /// (no blockset read, no token stream, no digest trailer) because
    /// the generator's `do_xfers = 0` path
    /// (`generator.c::recv_generator` line 1939) skips the
    /// `write_sum_head` write. Surfacing `FileSkipped { DryRun }` keeps
    /// the lane symmetric with the SFTP transport.
    dry_run: bool,
}

/// Drive the sender state machine to completion.
///
/// Pulls per-file requests off the `MplexReader`, opens each local
/// file, emits its token stream + MD4 trailer through `MplexWriter`,
/// and emits progress beacons through `progress_tx`. Returns the final
/// stats once the receiver has acked the final `idx == -1` "sender
/// done" sentinel.
///
/// `cancel.is_cancelled()` is consulted before every blocking await
/// boundary so callers can tear down a hung transfer cleanly.
///
/// # Errors
///
/// - [`DomainError::RsyncProtocolError`] on transport failure, EOF in
///   the wrong place, or a non-`-1` negative file index from the
///   receiver.
/// - [`DomainError::RsyncProtocolError`] when the receiver requests a
///   file index outside the flist range.
/// - [`DomainError::RsyncProtocolError`] when the receiver ships a
///   `count > 0` blockset (slice-4 territory).
#[expect(
    clippy::too_many_arguments,
    reason = "drive_sender threads channel halves + session + flist + source root + progress lane + cancel + dry_run; bundling into a struct just for one call site is more obscuring than helpful and the signature mirrors upstream rsync 3.2.7 sender.c::rsync_sender's parameter shape."
)]
pub async fn drive_sender<R, W>(
    reader: &mut MplexReader<R>,
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    flist: &[Flist],
    source_root: &Path,
    progress_tx: &Sender<RsyncProgressEvent>,
    cancel: &CancellationToken,
    dry_run: bool,
) -> Result<RsyncStats, DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let mut stats = ZeroStats::new(flist);
    let mut ndx_in = NdxState::new();
    let mut ndx_out = NdxState::new();
    let mut ctx = SenderCtx {
        reader,
        writer,
        sess,
        flist,
        source_root,
        progress_tx,
        cancel,
        ndx_in: &mut ndx_in,
        ndx_out: &mut ndx_out,
        dry_run,
    };
    drive_send_files_loop(&mut ctx, &mut stats).await?;
    finalise_sender(&mut ctx).await?;
    Ok(stats.finalize())
}

/// Emit the post-loop `NDX_DONE` and read the receiver goodbye pair.
///
/// Direct port of upstream rsync 3.2.7's `sender.c::send_files` lines
/// 462..464 (`write_ndx(f_out, NDX_DONE);` immediately before the
/// function returns) plus `main.c::read_final_goodbye` (lines
/// 875..906). Without the post-loop `NDX_DONE` the server-side
/// `recv_files` never breaks out of its own phase loop and the
/// generator never reaches the post-phase wait that emits the final
/// goodbye sentinel — sender would hang in `read_goodbye` forever.
async fn finalise_sender<R, W>(ctx: &mut SenderCtx<'_, R, W>) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let negotiated = ctx.sess.negotiated;
    write_ndx(ctx.writer, ctx.sess, ctx.ndx_out, negotiated, NDX_DONE).await?;
    // best-effort: tolerate channel close mid-goodbye since the
    // receiver-side rsync may already have shut down its mplex out.
    let _ = read_goodbye(
        ctx.reader,
        ctx.writer,
        ctx.sess,
        ctx.ndx_in,
        ctx.ndx_out,
        ctx.cancel,
        negotiated,
    )
    .await;
    Ok(())
}

/// Drive the multi-phase `send_files` loop.
///
/// Direct port of upstream rsync 3.2.7's `sender.c::send_files` lines
/// 225..258 — the loop bumps a `phase` counter on every received
/// `NDX_DONE`, **writes `NDX_DONE` back to the receiver**, and exits
/// once `phase > max_phase` (`max_phase = 2` at protocol >= 29).
///
/// rsync's protocol distinguishes:
///
/// - **Phase 0** — initial generator pass. Receiver requests files
///   (positive ndx + iflags + `sum_struct`) or itemize-only frames.
///   Terminates with `NDX_DONE`.
/// - **Phase 1** — redo phase. Receiver re-requests files whose MD5
///   trailer mismatched in phase 0. Terminates with `NDX_DONE`.
/// - **Phase 2** — flush phase. Generator emits one final `NDX_DONE`
///   so the sender can write its own `NDX_DONE` pair and exit.
///   Receiving `idx > 0` here is a fatal protocol error per upstream
///   `sender.c::send_files` lines 308..313.
///
/// At every `NDX_DONE` we **must** echo `NDX_DONE` back so the
/// generator can advance its own phase counter. Skipping the echo
/// deadlocks the generator (it waits forever for the sender's reply
/// before emitting the next phase boundary).
async fn drive_send_files_loop<R, W>(
    ctx: &mut SenderCtx<'_, R, W>,
    stats: &mut ZeroStats,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let negotiated = ctx.sess.negotiated;
    let max_phase = if negotiated >= 29 { 2_u8 } else { 1_u8 };
    let mut phase = 0_u8;
    loop {
        if ctx.cancel.is_cancelled() {
            return Err(DomainError::RsyncProtocolError(
                "sender: cancelled mid-phase".to_string(),
            ));
        }
        let idx = read_ndx(ctx.reader, ctx.sess, ctx.ndx_in, negotiated).await?;
        if idx == NDX_DONE {
            if handle_phase_boundary(ctx, &mut phase, max_phase).await? {
                return Ok(());
            }
            continue;
        }
        validate_positive_idx(idx, phase)?;
        process_one_file(ctx, idx, stats).await?;
    }
}

/// Handle one received `NDX_DONE` boundary inside the `send_files`
/// loop. Returns `true` when the caller should break out of the
/// loop (`++phase > max_phase`), `false` to continue.
///
/// Mirrors the `if (ndx == NDX_DONE) { ... }` branch of upstream
/// rsync 3.2.7 `sender.c::send_files` (lines 236..258). The break
/// check happens BEFORE the echo write, so the final phase-boundary
/// read does NOT emit an echo — see [`drive_sender`] for the
/// post-loop `NDX_DONE` write that closes the handshake.
async fn handle_phase_boundary<R, W>(
    ctx: &mut SenderCtx<'_, R, W>,
    phase: &mut u8,
    max_phase: u8,
) -> Result<bool, DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let negotiated = ctx.sess.negotiated;
    *phase = phase.saturating_add(1);
    if *phase > max_phase {
        tracing::debug!(
            target: "rsync.wire.sender",
            phase = *phase,
            max_phase,
            "phases complete — exiting send_files loop"
        );
        return Ok(true);
    }
    tracing::debug!(
        target: "rsync.wire.sender",
        phase = *phase,
        max_phase,
        "phase-end NDX_DONE — echoing back to generator"
    );
    write_ndx(ctx.writer, ctx.sess, ctx.ndx_out, negotiated, NDX_DONE).await?;
    Ok(false)
}

/// Validate a positive index from the generator. Mirrors the
/// `phase == 2` guard in upstream rsync 3.2.7 `sender.c::send_files`
/// lines 308..313 (`got transfer request in phase 2`) plus the
/// negative-index trap.
fn validate_positive_idx(idx: i32, phase: u8) -> Result<(), DomainError> {
    if idx < 0 {
        return Err(DomainError::RsyncProtocolError(format!(
            "sender: invalid negative file index {idx}"
        )));
    }
    if phase > 0 {
        tracing::debug!(
            target: "rsync.wire.sender",
            idx,
            phase,
            "redo-phase request from generator (phase > 0)"
        );
    }
    Ok(())
}

/// Process a single file-update request from the receiver.
///
/// Mirrors the `BLKSTAT_NEXT` arm of `sender.c::send_up_fsm` (line
/// 194..207) plus the `up.cur->idx >= 0` post-prime work in lines
/// 247..270. Slice 3 special-cases the whole-file path; the block-match
/// path errors clean. Slice 5 distinguishes itemize-only frames (no
/// `ITEM_TRANSFER`) from full transfer frames so directory / metadata
/// itemization does not consume bytes meant for the next file.
async fn process_one_file<R, W>(
    ctx: &mut SenderCtx<'_, R, W>,
    idx: i32,
    stats: &mut ZeroStats,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let entry = lookup_flist_entry(ctx.flist, idx)?;
    let negotiated = ctx.sess.negotiated;
    // At protocol >= 29, the receiver sends a 2-byte `iflags` short
    // (`read_shortint`) immediately after the file index. Mirrors
    // upstream rsync 3.2.7's `rsync.c::read_ndx_and_attrs` line 384.
    let iflags = read_iflags(ctx.reader, ctx.sess, negotiated).await?;
    consume_iflags_attrs(ctx.reader, ctx.sess, iflags).await?;
    if iflags & ITEM_TRANSFER == 0 {
        return process_itemize_only(ctx, idx, iflags, entry).await;
    }
    if ctx.dry_run {
        return process_dry_run_transfer(ctx, idx, iflags, entry).await;
    }
    process_full_transfer(ctx, idx, iflags, entry, stats).await
}

/// Itemize-only frame (directory entries, metadata-only updates,
/// skipped files). Mirrors the `!(iflags & ITEM_TRANSFER)` arm of
/// upstream rsync 3.2.7's `sender.c::send_files` (lines 288..307):
/// the sender just echoes `write_ndx_and_attrs` and continues —
/// no `read_sum_head`, no token stream, no MD4 trailer.
async fn process_itemize_only<R, W>(
    ctx: &mut SenderCtx<'_, R, W>,
    idx: i32,
    iflags: u16,
    entry: &Flist,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let negotiated = ctx.sess.negotiated;
    write_ndx(ctx.writer, ctx.sess, ctx.ndx_out, negotiated, idx).await?;
    write_iflags(ctx.writer, ctx.sess, negotiated, iflags).await?;
    tracing::debug!(
        target: "rsync.wire.sender",
        idx,
        iflags = format_args!("0x{iflags:04x}"),
        path = %entry.path.display(),
        "itemize-only frame (no ITEM_TRANSFER) — echoed and skipped"
    );
    Ok(())
}

/// Dry-run `ITEM_TRANSFER` frame. Direct port of upstream rsync 3.2.7's
/// `sender.c::send_files` lines 343..347 (`if (!do_xfers) { ... continue; }`):
///
/// - the generator emits `ndx + iflags` only — `do_xfers = 0` skips
///   the `write_sum_head` call at `generator.c::recv_generator` lines
///   1949..1960 (the `if (!do_xfers)` short-circuit at line 1939
///   `goto cleanup`s past the blockset write);
/// - the sender therefore must NOT call `read_blockset` (it would
///   deadlock waiting for bytes the generator never sent);
/// - the sender just echoes `write_ndx + write_iflags` and emits a
///   `FileSkipped { reason: DryRun }` beacon to keep the push lane
///   symmetric with the SFTP transport's dry-run reporting shape.
async fn process_dry_run_transfer<R, W>(
    ctx: &mut SenderCtx<'_, R, W>,
    idx: i32,
    iflags: u16,
    entry: &Flist,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let negotiated = ctx.sess.negotiated;
    write_ndx(ctx.writer, ctx.sess, ctx.ndx_out, negotiated, idx).await?;
    write_iflags(ctx.writer, ctx.sess, negotiated, iflags).await?;
    tracing::debug!(
        target: "rsync.wire.sender",
        idx,
        iflags = format_args!("0x{iflags:04x}"),
        path = %entry.path.display(),
        "dry-run ITEM_TRANSFER — echoed without blockset/tokens"
    );
    emit_file_skipped(ctx.progress_tx, entry, SkipReason::DryRun).await;
    Ok(())
}

/// Full per-file transfer — read blockset, echo ndx + iflags + header,
/// emit token stream + digest trailer.
async fn process_full_transfer<R, W>(
    ctx: &mut SenderCtx<'_, R, W>,
    idx: i32,
    iflags: u16,
    entry: &Flist,
    stats: &mut ZeroStats,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let negotiated = ctx.sess.negotiated;
    let blockset = read_blockset(ctx.reader, ctx.sess).await?;
    write_ndx(ctx.writer, ctx.sess, ctx.ndx_out, negotiated, idx).await?;
    // Echo the same iflags + attrs back to the receiver per
    // `sender.c::send_files` line 413 `write_ndx_and_attrs`.
    write_iflags(ctx.writer, ctx.sess, negotiated, iflags).await?;
    write_blockset_header(ctx.writer, ctx.sess, &blockset).await?;
    let bytes = read_local_file(ctx.source_root, entry).await?;
    let bytes_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let seed = ctx.sess.seed;
    // `negotiated` selects the per-file transfer digest: MD5 (proto
    // >= 30, default for stock rsync 3.x) vs the legacy MD4 + seed
    // shape (proto < 30). See `FileHasher::for_protocol` and upstream
    // rsync 3.2.7 `checksum.c::parse_csum_name` lines 109..143.
    let _digest =
        emit_token_stream(ctx.writer, ctx.sess, &blockset, &bytes, seed, negotiated).await?;
    emit_file_completed(ctx.progress_tx, entry, bytes_len).await;
    stats.note_file_completed(bytes_len);
    Ok(())
}

// =====================================================================
// iflags + per-file attrs codec — port of upstream rsync 3.2.7's
// `rsync.c::read_ndx_and_attrs` + `sender.c::write_ndx_and_attrs`
// (the post-ndx slice).
// =====================================================================

/// Bit set on `iflags` when the receiver follows the index with a
/// `fnamecmp_type` byte. Mirrors upstream rsync 3.2.7's
/// `ITEM_BASIS_TYPE_FOLLOWS = 1 << 11`.
const ITEM_BASIS_TYPE_FOLLOWS: u16 = 1 << 11;

/// Bit set on `iflags` when the receiver follows the index with a
/// vstring containing the basename of the file to compare against.
/// Mirrors upstream rsync 3.2.7's `ITEM_XNAME_FOLLOWS = 1 << 12`.
const ITEM_XNAME_FOLLOWS: u16 = 1 << 12;

/// Bit set on `iflags` when the receiver actually wants the sender to
/// transmit the file's contents (`read_sum_head` + token stream + MD4
/// trailer). Mirrors upstream rsync 3.2.7's `ITEM_TRANSFER = 1 << 15`
/// (`rsync.h` line 229). When this bit is **clear** the frame is
/// itemize-only (directory entry, metadata-only update, skipped file)
/// and the sender must echo `write_ndx + write_iflags` and `continue`
/// — no blockset, no tokens, no digest.
const ITEM_TRANSFER: u16 = 1 << 15;

/// Implicit iflags returned at protocol < 29 in lieu of an on-wire
/// short. Mirrors upstream rsync 3.2.7's `rsync.c::read_ndx_and_attrs`
/// line 384: `iflags = protocol_version >= 29 ? read_shortint(f_in)
/// : ITEM_TRANSFER | ITEM_MISSING_DATA;`. We synthesise the same
/// constant so callers can branch on `iflags & ITEM_TRANSFER` without
/// special-casing the legacy protocol path.
const ITEM_MISSING_DATA: u16 = 1 << 9;

/// Read the 2-byte `iflags` short from the receiver. Mirrors upstream
/// rsync 3.2.7's `rsync.c::read_ndx_and_attrs` line 384.
///
/// At protocol < 29 the receiver does not emit an on-wire short; the
/// upstream client synthesises `ITEM_TRANSFER | ITEM_MISSING_DATA` to
/// keep the downstream branching uniform. We do the same.
async fn read_iflags<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    negotiated: i32,
) -> Result<u16, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if negotiated < 29 {
        return Ok(ITEM_TRANSFER | ITEM_MISSING_DATA);
    }
    let mut buf = [0_u8; 2];
    reader.read_buf(sess, &mut buf).await?;
    Ok(u16::from_le_bytes(buf))
}

/// Write the 2-byte `iflags` short back to the receiver. Mirrors the
/// `write_shortint(f_out, iflags)` call inside upstream rsync 3.2.7's
/// `sender.c::write_ndx_and_attrs` (lines 178..195).
async fn write_iflags<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    negotiated: i32,
    iflags: u16,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    if negotiated < 29 {
        return Ok(());
    }
    let bytes = iflags.to_le_bytes();
    writer.write_buf(sess, &bytes).await
}

/// Drain the optional `fnamecmp_type` byte + `xname` vstring that
/// follow `iflags` when their respective bits are set. Slice 4 does
/// not act on the values — we only consume them so the wire stays in
/// frame for `read_sum_head`.
async fn consume_iflags_attrs<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    iflags: u16,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if (iflags & ITEM_BASIS_TYPE_FOLLOWS) != 0 {
        let _ = reader.read_byte(sess).await?;
    }
    if (iflags & ITEM_XNAME_FOLLOWS) != 0 {
        consume_vstring(reader, sess).await?;
    }
    Ok(())
}

/// Drain a `read_vstring` (1 length byte + N path bytes; or 2 length
/// bytes when high bit is set). Mirrors upstream rsync 3.2.7's
/// `io.c::read_vstring`. Slice 4 only needs to advance the cursor;
/// the actual bytes are unused because we don't drive a fuzzy basis.
async fn consume_vstring<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let first = reader.read_byte(sess).await?;
    let len = if first & 0x80 == 0 {
        usize::from(first)
    } else {
        let second = reader.read_byte(sess).await?;
        (usize::from(first & 0x7f) << 8_u32) | usize::from(second)
    };
    if len == 0 {
        return Ok(());
    }
    let mut buf = vec![0_u8; len];
    reader.read_buf(sess, &mut buf).await?;
    Ok(())
}

/// Locate the [`Flist`] entry the receiver referenced. Returns a
/// protocol error on out-of-range indices to match openrsync's
/// `send_dl_enqueue` line 304 ("file index out of bounds") guard.
fn lookup_flist_entry(flist: &[Flist], idx: i32) -> Result<&Flist, DomainError> {
    let Ok(idx_usize) = usize::try_from(idx) else {
        return Err(DomainError::RsyncProtocolError(format!(
            "sender: file index {idx} does not fit usize"
        )));
    };
    flist.get(idx_usize).ok_or_else(|| {
        DomainError::RsyncProtocolError(format!(
            "sender: file index {idx} out of range (flist len {})",
            flist.len()
        ))
    })
}

/// Read the local source file off disk in one shot. Slice 3
/// simplification — slice 4+ chunks via `mmap` / streaming.
async fn read_local_file(source_root: &Path, entry: &Flist) -> Result<Vec<u8>, DomainError> {
    let path = source_root.join(&entry.path);
    let bytes = fs::read(&path).await.map_err(|e| {
        DomainError::RsyncProtocolError(format!("sender: open({}): {e}", path.display()))
    })?;
    Ok(bytes)
}

/// Push a `FileCompleted` beacon onto the progress lane. Tolerates a
/// closed channel — the lane consumer may have dropped the receiver.
async fn emit_file_completed(progress_tx: &Sender<RsyncProgressEvent>, entry: &Flist, bytes: u64) {
    let rel_path = entry.path.to_string_lossy().into_owned();
    let _ = progress_tx
        .send(RsyncProgressEvent::FileCompleted {
            rel_path,
            bytes_transferred: bytes,
            bytes_skipped: 0,
        })
        .await;
}

/// Push a `FileSkipped` beacon onto the progress lane. Used by the
/// dry-run path so the lane consumer can report would-be transfers
/// without seeing a `FileCompleted` event. Tolerates a closed channel.
async fn emit_file_skipped(
    progress_tx: &Sender<RsyncProgressEvent>,
    entry: &Flist,
    reason: SkipReason,
) {
    let rel_path = entry.path.to_string_lossy().into_owned();
    let _ = progress_tx
        .send(RsyncProgressEvent::FileSkipped { rel_path, reason })
        .await;
}

/// Read the final receiver "goodbye" pair.
///
/// Direct port of upstream rsync 3.2.7's `main.c::read_final_goodbye`
/// (lines 875..906) — the client-sender path:
///
/// 1. Read `NDX_DONE`.
/// 2. At protocol >= 31 only: write `NDX_DONE` back AND read another
///    `NDX_DONE`. (See lines 887..898 of `main.c`.)
/// 3. At protocol < 31: a single `NDX_DONE` is the entire handshake.
///
/// The receiver's `read_final_goodbye` symmetrically writes 1
/// `NDX_DONE` at proto < 31 and 2 `NDX_DONE` frames at proto >= 31,
/// so the client-sender always sees the same number of `NDX_DONE`
/// frames as the protocol dictates.
///
/// We tolerate transport errors here because the channel may close
/// immediately after our own `NDX_DONE` write, before we get a chance
/// to read the ack — `drive_sender` consults the result via `let _`.
async fn read_goodbye<R, W>(
    reader: &mut MplexReader<R>,
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    state_in: &mut NdxState,
    state_out: &mut NdxState,
    cancel: &CancellationToken,
    negotiated: i32,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    if cancel.is_cancelled() {
        return Ok(());
    }
    let first = read_ndx(reader, sess, state_in, negotiated).await?;
    if first != NDX_DONE {
        return Err(DomainError::RsyncProtocolError(format!(
            "sender: bad goodbye ack {first}, want NDX_DONE"
        )));
    }
    if negotiated < 31 {
        return Ok(());
    }
    write_ndx(writer, sess, state_out, negotiated, NDX_DONE).await?;
    let second = read_ndx(reader, sess, state_in, negotiated).await?;
    if second != NDX_DONE {
        return Err(DomainError::RsyncProtocolError(format!(
            "sender: bad second goodbye ack {second}, want NDX_DONE"
        )));
    }
    Ok(())
}

/// Per-session running stats accumulator — feeds the final
/// [`RsyncStats`] returned by [`drive_sender`].
struct ZeroStats {
    files_total: u64,
    bytes_total: u64,
    files_done: u64,
    bytes_transferred: u64,
}

impl ZeroStats {
    fn new(flist: &[Flist]) -> Self {
        let files_total = u64::try_from(flist.len()).unwrap_or(u64::MAX);
        let bytes_total = flist
            .iter()
            .map(|e| u64::try_from(e.size).unwrap_or(0))
            .fold(0_u64, u64::saturating_add);
        Self {
            files_total,
            bytes_total,
            files_done: 0,
            bytes_transferred: 0,
        }
    }

    const fn note_file_completed(&mut self, bytes: u64) {
        self.files_done = self.files_done.saturating_add(1);
        self.bytes_transferred = self.bytes_transferred.saturating_add(bytes);
    }

    const fn finalize(self) -> RsyncStats {
        RsyncStats {
            files_total: self.files_total,
            files_done: self.files_done,
            bytes_total: self.bytes_total,
            bytes_transferred: self.bytes_transferred,
            bytes_skipped: 0,
            files_deleted: 0,
            files_failed: 0,
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    reason = "test code uses unwrap/expect for brevity per project convention; longer table-driven tests document the wire shape verbatim"
)]
mod tests {
    use super::drive_sender;
    /// Local alias for the proto-27 phase-end sentinel (`-1`) used by
    /// the synthetic generator tests below. The slice-4 ndx codec
    /// emits a single `0x00` byte at protocol 30+, but the tests pin
    /// `sess_sender.negotiated = 0` so the codec falls back to the
    /// proto-27 plain `read_int` / `write_int` shape.
    const PHASE_END_SENTINEL: i32 = -1;
    use crate::adapters::rsync::types::{RsyncProgressEvent, SkipReason};
    use crate::adapters::rsync::wire::flist::Flist;
    use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
    use crate::adapters::rsync::wire::session::WireSession;
    use std::path::PathBuf;
    use tokio::io::duplex;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    /// Drive `drive_sender` against a synthetic generator running on a
    /// `tokio::io::duplex` pair. The generator simulates a single-file
    /// whole-file request followed by the phase-end sentinels.
    #[tokio::test]
    async fn round_trip_one_small_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let payload: &[u8] = b"hello rsync";
        std::fs::write(dir.path().join("f.txt"), payload).expect("write");
        let flist = vec![Flist::regular(
            PathBuf::from("f.txt"),
            payload.len() as i64,
            0,
            0o644,
            0,
            0,
        )];

        // Two duplex pipes:
        //   server_to_client: server (test) -> client (sender)
        //   client_to_server: client (sender) -> server (test)
        let (server_w_left, client_r_right) = duplex(64 * 1024);
        let (client_w_left, server_r_right) = duplex(64 * 1024);

        // Sender owns: reader of server_to_client, writer of client_to_server.
        let (cl_r, _cl_w_unused) = tokio::io::split(client_r_right);
        let (_cl_r_unused, cl_w) = tokio::io::split(client_w_left);
        // Test driver owns: writer of server_to_client, reader of client_to_server.
        let (_sv_r_unused, sv_w) = tokio::io::split(server_w_left);
        let (sv_r, _sv_w_unused) = tokio::io::split(server_r_right);
        let mut sender_reader = MplexReader::new(cl_r);
        let mut sender_writer = MplexWriter::new(cl_w);
        let mut server_writer = MplexWriter::new(sv_w);
        let mut server_reader = MplexReader::new(sv_r);

        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();
        let mut sess_sender = WireSession::new();
        sess_sender.rver = 31;
        // Pin the negotiated version below 29 so the iflags codec stays
        // out of frame for the synthetic generator. At proto < 29 the
        // sender synthesises `ITEM_TRANSFER | ITEM_MISSING_DATA` per
        // upstream `read_ndx_and_attrs` line 384, so the transfer
        // branch fires and the blockset is consumed as expected.
        sess_sender.negotiated = 0;

        // Spawn the test-side generator.
        let payload_for_task = payload.to_vec();
        let server_task = tokio::spawn(async move {
            let mut sess_srv = WireSession::new();
            // Send file index 0. (Synthetic generator: pinned negotiated=0
            // so no iflags short follows.)
            server_writer
                .write_int(&mut sess_srv, 0)
                .await
                .expect("idx");
            // Send empty blockset (count=0, len=0, csum=16, rem=0).
            server_writer.write_int(&mut sess_srv, 0).await.expect("c");
            server_writer.write_int(&mut sess_srv, 0).await.expect("l");
            server_writer.write_int(&mut sess_srv, 16).await.expect("s");
            server_writer.write_int(&mut sess_srv, 0).await.expect("r");
            // Read sender's echo: idx + four-field blockset header.
            let echoed = server_reader.read_int(&mut sess_srv).await.expect("e_idx");
            assert_eq!(echoed, 0);
            for _ in 0..4 {
                let _ = server_reader.read_int(&mut sess_srv).await.expect("hdr");
            }
            // Read literal token + payload + EOF + 16-byte digest.
            let lit_len = server_reader.read_int(&mut sess_srv).await.expect("lit");
            assert_eq!(lit_len, payload_for_task.len() as i32);
            let mut got = vec![0_u8; payload_for_task.len()];
            server_reader
                .read_buf(&mut sess_srv, &mut got)
                .await
                .expect("payload");
            assert_eq!(got, payload_for_task);
            let eof = server_reader.read_int(&mut sess_srv).await.expect("eof");
            assert_eq!(eof, 0);
            let mut digest = [0_u8; 16];
            server_reader
                .read_buf(&mut sess_srv, &mut digest)
                .await
                .expect("d");
            // Phase 0 end: server writes NDX_DONE, sender echoes NDX_DONE.
            server_writer
                .write_int(&mut sess_srv, PHASE_END_SENTINEL)
                .await
                .expect("phase0");
            let d1 = server_reader.read_int(&mut sess_srv).await.expect("d1");
            assert_eq!(d1, PHASE_END_SENTINEL);
            // Phase 1 end (= max_phase at proto < 29): server writes
            // NDX_DONE, sender exits the loop without echoing per
            // upstream `send_files` line 252 (`++phase > max_phase`
            // check happens BEFORE the echo write).
            server_writer
                .write_int(&mut sess_srv, PHASE_END_SENTINEL)
                .await
                .expect("phase1");
            // After breaking the loop, sender writes the post-loop
            // NDX_DONE per upstream `sender.c::send_files` line 464.
            let d2 = server_reader.read_int(&mut sess_srv).await.expect("d2");
            assert_eq!(d2, PHASE_END_SENTINEL);
            // Final goodbye: at proto < 31 the client-sender reads a
            // single NDX_DONE in `read_final_goodbye` and does NOT
            // write back. Mirrors `main.c::read_final_goodbye` lines
            // 883..884.
            server_writer
                .write_int(&mut sess_srv, PHASE_END_SENTINEL)
                .await
                .expect("goodbye");
        });

        let stats = drive_sender(
            &mut sender_reader,
            &mut sender_writer,
            &mut sess_sender,
            &flist,
            dir.path(),
            &tx,
            &cancel,
            false,
        )
        .await
        .expect("sender");

        server_task.await.expect("server task");
        assert_eq!(stats.files_total, 1);
        assert_eq!(stats.files_done, 1);
        assert_eq!(stats.bytes_transferred, payload.len() as u64);

        // Ensure the FileCompleted beacon was emitted.
        let evt = rx.recv().await.expect("file completed");
        match evt {
            RsyncProgressEvent::FileCompleted {
                rel_path,
                bytes_transferred,
                ..
            } => {
                assert_eq!(rel_path, "f.txt");
                assert_eq!(bytes_transferred, payload.len() as u64);
            }
            other => panic!("unexpected event {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_negative_index_errors() {
        let flist: Vec<Flist> = vec![];
        let dir = tempfile::tempdir().expect("tempdir");

        let (server_w_left, client_r_right) = duplex(64);
        let (client_w_left, _server_r_right) = duplex(64);
        let (cl_r, _cl_w_unused) = tokio::io::split(client_r_right);
        let (_cl_r_unused, cl_w) = tokio::io::split(client_w_left);
        let (_sv_r_unused, sv_w) = tokio::io::split(server_w_left);
        let mut sender_reader = MplexReader::new(cl_r);
        let mut sender_writer = MplexWriter::new(cl_w);
        let mut server_writer = MplexWriter::new(sv_w);

        let (tx, _rx) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        let mut sess_sender = WireSession::new();
        sess_sender.rver = 31;

        let server_task = tokio::spawn(async move {
            let mut sess_srv = WireSession::new();
            server_writer
                .write_int(&mut sess_srv, -2)
                .await
                .expect("bad idx");
        });

        let err = drive_sender(
            &mut sender_reader,
            &mut sender_writer,
            &mut sess_sender,
            &flist,
            dir.path(),
            &tx,
            &cancel,
            false,
        )
        .await
        .expect_err("must err");
        assert!(format!("{err}").contains("invalid negative"));
        server_task.await.expect("server task");
    }

    /// Slice 6: when the receiver streams `count > 0` block signatures
    /// the sender now drives the block-match path
    /// ([`super::super::match_path::emit_block_match_tokens`]) instead
    /// of erroring out. This test feeds an `idx=0`+blockset that would
    /// produce a token-stream miss on every block (the local file is
    /// 1 byte but the receiver claims a 4-byte block) and verifies the
    /// sender routes through the matcher's tail-flush single-literal
    /// path without protocol-erroring.
    #[tokio::test]
    async fn block_match_path_drives_matcher_without_protocol_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("f.txt"), b"x").expect("write");
        let flist = vec![Flist::regular(PathBuf::from("f.txt"), 1, 0, 0o644, 0, 0)];

        let (server_w_left, client_r_right) = duplex(64 * 1024);
        let (client_w_left, server_r_right) = duplex(64 * 1024);
        let (cl_r, _cl_w_unused) = tokio::io::split(client_r_right);
        let (_cl_r_unused, cl_w) = tokio::io::split(client_w_left);
        let (_sv_r_unused, sv_w) = tokio::io::split(server_w_left);
        let (sv_r, _sv_w_unused) = tokio::io::split(server_r_right);
        let mut sender_reader = MplexReader::new(cl_r);
        let mut sender_writer = MplexWriter::new(cl_w);
        let mut server_writer = MplexWriter::new(sv_w);
        let mut server_reader = MplexReader::new(sv_r);

        let (tx, _rx) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        let mut sess_sender = WireSession::new();
        sess_sender.rver = 31;
        // proto < 29 — no iflags codec, sender synthesises ITEM_TRANSFER.
        sess_sender.negotiated = 0;

        let server_task = tokio::spawn(async move {
            let mut sess_srv = WireSession::new();
            server_writer
                .write_int(&mut sess_srv, 0)
                .await
                .expect("idx");
            // count=1 blockset, block_len=4 (> bytes.len()=1) → matcher
            // routes to single-literal-tail path.
            server_writer.write_int(&mut sess_srv, 1).await.expect("c");
            server_writer.write_int(&mut sess_srv, 4).await.expect("l");
            server_writer.write_int(&mut sess_srv, 16).await.expect("s");
            server_writer.write_int(&mut sess_srv, 0).await.expect("r");
            // One block: rolling + 16-byte strong (won't match — local
            // is too short to even fit the window).
            server_writer
                .write_int(&mut sess_srv, 0)
                .await
                .expect("rolling");
            server_writer
                .write_buf(&mut sess_srv, &[0_u8; 16])
                .await
                .expect("strong");
            // Drain sender's response: idx + 4-field blockset header +
            // single literal token + payload + EOF + 16-byte digest.
            let echoed = server_reader.read_int(&mut sess_srv).await.expect("e_idx");
            assert_eq!(echoed, 0);
            for _ in 0..4 {
                let _ = server_reader.read_int(&mut sess_srv).await.expect("hdr");
            }
            let lit_len = server_reader.read_int(&mut sess_srv).await.expect("lit");
            assert_eq!(lit_len, 1, "1 byte local file → 1-byte literal");
            let mut got = vec![0_u8; 1];
            server_reader
                .read_buf(&mut sess_srv, &mut got)
                .await
                .expect("payload");
            assert_eq!(got, b"x");
            let eof = server_reader.read_int(&mut sess_srv).await.expect("eof");
            assert_eq!(eof, 0);
            let mut digest = [0_u8; 16];
            server_reader
                .read_buf(&mut sess_srv, &mut digest)
                .await
                .expect("d");
            // Phase boundaries (proto < 29 → max_phase = 1).
            server_writer
                .write_int(&mut sess_srv, -1)
                .await
                .expect("phase0");
            let d1 = server_reader.read_int(&mut sess_srv).await.expect("d1");
            assert_eq!(d1, -1);
            server_writer
                .write_int(&mut sess_srv, -1)
                .await
                .expect("phase1");
            let d2 = server_reader.read_int(&mut sess_srv).await.expect("d2");
            assert_eq!(d2, -1);
            server_writer
                .write_int(&mut sess_srv, -1)
                .await
                .expect("goodbye");
        });

        let stats = drive_sender(
            &mut sender_reader,
            &mut sender_writer,
            &mut sess_sender,
            &flist,
            dir.path(),
            &tx,
            &cancel,
            false,
        )
        .await
        .expect("sender drives matcher path without error");
        assert_eq!(stats.files_done, 1);
        let _ = server_task.await;
    }

    /// Itemize-only frame (`!ITEM_TRANSFER`) followed by a transfer
    /// frame: the sender must NOT consume any blockset bytes for the
    /// itemize-only frame. Reproduces the slice-5 e2e bug where the
    /// directory entry frame consumed bytes meant for the next file.
    ///
    /// Wire shape (proto >= 29):
    /// - itemize-only: `ndx + iflags=0x0000` (no transfer bit)
    /// - transfer:     `ndx + iflags=0x8000 + blockset + tokens + md4`
    /// - phase end:    `-1` + `-1`
    /// - goodbye:      `-1`
    #[tokio::test]
    async fn itemize_only_frame_does_not_consume_blockset_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let payload: &[u8] = b"hello rsync";
        std::fs::write(dir.path().join("f.txt"), payload).expect("write");
        // flist[0] = directory placeholder (".")
        // flist[1] = the actual file ("f.txt")
        let flist = vec![
            Flist::regular(PathBuf::from("."), 0, 0, 0o755, 0, 0),
            Flist::regular(PathBuf::from("f.txt"), payload.len() as i64, 0, 0o644, 0, 0),
        ];

        let (server_w_left, client_r_right) = duplex(64 * 1024);
        let (client_w_left, server_r_right) = duplex(64 * 1024);
        let (cl_r, _cl_w_unused) = tokio::io::split(client_r_right);
        let (_cl_r_unused, cl_w) = tokio::io::split(client_w_left);
        let (_sv_r_unused, sv_w) = tokio::io::split(server_w_left);
        let (sv_r, _sv_w_unused) = tokio::io::split(server_r_right);
        let mut sender_reader = MplexReader::new(cl_r);
        let mut sender_writer = MplexWriter::new(cl_w);
        let mut server_writer = MplexWriter::new(sv_w);
        let mut server_reader = MplexReader::new(sv_r);

        let (tx, _rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let mut sess_sender = WireSession::new();
        sess_sender.rver = 31;
        sess_sender.negotiated = 31; // exercise iflags read+write codec.

        let payload_for_task = payload.to_vec();
        let server_task = tokio::spawn(async move {
            use crate::adapters::rsync::wire::ndx::{NdxState, read_ndx, write_ndx};
            let mut sess_srv = WireSession::new();
            sess_srv.negotiated = 31;
            // Per-direction NDX codec state — server writes via its
            // `out` cursor and reads sender's echoes via its `in`
            // cursor. Mirrors the per-direction `prev_*` setup in
            // upstream rsync 3.2.7 `io.c::read_ndx` / `write_ndx`.
            let mut ndx_out = NdxState::new();
            let mut ndx_in = NdxState::new();
            // Frame 1: itemize-only for the directory placeholder.
            write_ndx(&mut server_writer, &mut sess_srv, &mut ndx_out, 31, 0)
                .await
                .expect("idx0");
            server_writer
                .write_buf(&mut sess_srv, &0_u16.to_le_bytes()) // iflags = 0 (no transfer)
                .await
                .expect("if0");
            // Frame 2: full transfer for the file.
            write_ndx(&mut server_writer, &mut sess_srv, &mut ndx_out, 31, 1)
                .await
                .expect("idx1");
            server_writer
                .write_buf(&mut sess_srv, &0x8000_u16.to_le_bytes()) // ITEM_TRANSFER
                .await
                .expect("if1");
            // Empty blockset (count=0, len=0, csum=16, rem=0).
            server_writer.write_int(&mut sess_srv, 0).await.expect("c");
            server_writer.write_int(&mut sess_srv, 0).await.expect("l");
            server_writer.write_int(&mut sess_srv, 16).await.expect("s");
            server_writer.write_int(&mut sess_srv, 0).await.expect("r");

            // Read sender's itemize-only echo (frame 1).
            let echo0 = read_ndx(&mut server_reader, &mut sess_srv, &mut ndx_in, 31)
                .await
                .expect("e0");
            assert_eq!(echo0, 0);
            let mut iflags0 = [0_u8; 2];
            server_reader
                .read_buf(&mut sess_srv, &mut iflags0)
                .await
                .expect("eif0");
            assert_eq!(u16::from_le_bytes(iflags0), 0);

            // Read sender's full-transfer echo (frame 2).
            let echo1 = read_ndx(&mut server_reader, &mut sess_srv, &mut ndx_in, 31)
                .await
                .expect("e1");
            assert_eq!(echo1, 1);
            let mut iflags1 = [0_u8; 2];
            server_reader
                .read_buf(&mut sess_srv, &mut iflags1)
                .await
                .expect("eif1");
            assert_eq!(u16::from_le_bytes(iflags1), 0x8000);
            // Read 4-field blockset header.
            for _ in 0..4 {
                let _ = server_reader.read_int(&mut sess_srv).await.expect("hdr");
            }
            // Read literal token + payload + EOF + 16-byte digest.
            let lit_len = server_reader.read_int(&mut sess_srv).await.expect("lit");
            assert_eq!(lit_len, payload_for_task.len() as i32);
            let mut got = vec![0_u8; payload_for_task.len()];
            server_reader
                .read_buf(&mut sess_srv, &mut got)
                .await
                .expect("payload");
            assert_eq!(got, payload_for_task);
            let eof = server_reader.read_int(&mut sess_srv).await.expect("eof");
            assert_eq!(eof, 0);
            let mut digest = [0_u8; 16];
            server_reader
                .read_buf(&mut sess_srv, &mut digest)
                .await
                .expect("d");

            // Phase boundaries at proto 31 (max_phase = 2): server
            // writes 3 NDX_DONE (single 0x00 each), sender echoes the
            // first 2 of them per upstream `send_files` line 252,
            // then writes a final post-loop NDX_DONE per upstream
            // `sender.c::send_files` line 464. After that
            // `read_final_goodbye` (proto >= 31) exchanges another
            // pair: server writes NDX_DONE, sender writes NDX_DONE
            // back, server writes NDX_DONE — matching
            // `main.c::read_final_goodbye` lines 887..898.
            server_writer
                .write_byte(&mut sess_srv, 0)
                .await
                .expect("ph0");
            let e0 = server_reader.read_byte(&mut sess_srv).await.expect("d0");
            assert_eq!(e0, 0, "phase 0 echo");
            server_writer
                .write_byte(&mut sess_srv, 0)
                .await
                .expect("ph1");
            let e1 = server_reader.read_byte(&mut sess_srv).await.expect("d1");
            assert_eq!(e1, 0, "phase 1 echo");
            server_writer
                .write_byte(&mut sess_srv, 0)
                .await
                .expect("ph2");
            // Sender exits send_files loop here, then writes the
            // post-loop NDX_DONE (line 464 of upstream).
            let post = server_reader.read_byte(&mut sess_srv).await.expect("post");
            assert_eq!(post, 0, "post-loop NDX_DONE");
            server_writer
                .write_byte(&mut sess_srv, 0)
                .await
                .expect("gb1");
            let g1 = server_reader.read_byte(&mut sess_srv).await.expect("g1");
            assert_eq!(g1, 0, "first goodbye echo");
            server_writer
                .write_byte(&mut sess_srv, 0)
                .await
                .expect("gb2");
        });

        let stats = drive_sender(
            &mut sender_reader,
            &mut sender_writer,
            &mut sess_sender,
            &flist,
            dir.path(),
            &tx,
            &cancel,
            false,
        )
        .await
        .expect("sender");
        server_task.await.expect("server task");
        assert_eq!(stats.files_done, 1, "only the transfer frame counts");
        assert_eq!(stats.bytes_transferred, payload.len() as u64);
    }

    /// Wire shape uncovered in the live e2e: server emits the standard
    /// `null_sum` (block_count=0, block_len=0, **strong_len=0**,
    /// remainder=0) for files it already has. Validate that
    /// `read_blockset` accepts strong_len=0 only when block_count=0.
    #[tokio::test]
    async fn null_sum_with_strong_len_zero_is_accepted() {
        use super::super::blocks::read_blockset;
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut writer = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        // null_sum: all four fields zero. Mirrors upstream
        // io.c::write_sum_head with sum=NULL.
        writer.write_int(&mut sess_w, 0).await.expect("c");
        writer.write_int(&mut sess_w, 0).await.expect("l");
        writer.write_int(&mut sess_w, 0).await.expect("s");
        writer.write_int(&mut sess_w, 0).await.expect("r");
        let mut reader = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let bs = read_blockset(&mut reader, &mut sess_r).await.expect("read");
        assert_eq!(bs.block_count, 0);
        assert_eq!(bs.strong_len, 0);
        assert!(bs.is_whole_file());
    }

    /// Slice-13: dry-run `ITEM_TRANSFER` frame must NOT consume a
    /// blockset off the wire. Mirrors upstream rsync 3.2.7's
    /// `sender.c::send_files` lines 343..347 (`if (!do_xfers) { ...
    /// continue; }`) and `generator.c::recv_generator` line 1939 — the
    /// generator's `do_xfers = 0` path emits `ndx + iflags` only and
    /// the sender must echo `ndx + iflags` only, no blockset, no
    /// tokens, no digest. Pre-fix the sender hung on `read_blockset`
    /// after the iflags read.
    ///
    /// Wire shape (proto >= 29, dry-run):
    /// - generator: `ndx=0 + iflags=0x8000`  (ITEM_TRANSFER set)
    /// - sender:    `ndx=0 + iflags=0x8000`  (echo, no blockset)
    /// - phase end: `-1` (server) → `-1` echo (sender) → `-1` (server)
    ///   → post-loop `-1` (sender) → `-1` goodbye (server)
    #[tokio::test]
    async fn dry_run_transfer_frame_skips_blockset_and_tokens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let payload: &[u8] = b"dry-run payload\n";
        std::fs::write(dir.path().join("hello.txt"), payload).expect("write");
        let flist = vec![Flist::regular(
            PathBuf::from("hello.txt"),
            payload.len() as i64,
            0,
            0o644,
            0,
            0,
        )];

        let (server_w_left, client_r_right) = duplex(64 * 1024);
        let (client_w_left, server_r_right) = duplex(64 * 1024);
        let (cl_r, _cl_w_unused) = tokio::io::split(client_r_right);
        let (_cl_r_unused, cl_w) = tokio::io::split(client_w_left);
        let (_sv_r_unused, sv_w) = tokio::io::split(server_w_left);
        let (sv_r, _sv_w_unused) = tokio::io::split(server_r_right);
        let mut sender_reader = MplexReader::new(cl_r);
        let mut sender_writer = MplexWriter::new(cl_w);
        let mut server_writer = MplexWriter::new(sv_w);
        let mut server_reader = MplexReader::new(sv_r);

        let (tx, mut rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let mut sess_sender = WireSession::new();
        sess_sender.rver = 31;
        // proto < 29 keeps the synthetic generator simple: the sender
        // synthesises `ITEM_TRANSFER | ITEM_MISSING_DATA` per upstream
        // `read_ndx_and_attrs` line 384, so the dry-run echo path
        // exercises the same `process_dry_run_transfer` branch as a
        // proto-31 wire would.
        sess_sender.negotiated = 0;

        let server_task = tokio::spawn(async move {
            let mut sess_srv = WireSession::new();
            // Frame: file index 0. (proto < 29 → no iflags short.)
            server_writer
                .write_int(&mut sess_srv, 0)
                .await
                .expect("idx");
            // No blockset bytes — the dry-run sender must NOT read any.
            // Read the sender's echo: ndx only (proto < 29 → no iflags).
            let echoed = server_reader.read_int(&mut sess_srv).await.expect("echo");
            assert_eq!(echoed, 0);
            // Phase boundaries (proto < 29 → max_phase = 1).
            server_writer
                .write_int(&mut sess_srv, -1)
                .await
                .expect("phase0");
            let d1 = server_reader.read_int(&mut sess_srv).await.expect("d1");
            assert_eq!(d1, -1);
            server_writer
                .write_int(&mut sess_srv, -1)
                .await
                .expect("phase1");
            let d2 = server_reader.read_int(&mut sess_srv).await.expect("d2");
            assert_eq!(d2, -1);
            server_writer
                .write_int(&mut sess_srv, -1)
                .await
                .expect("goodbye");
        });

        let stats = drive_sender(
            &mut sender_reader,
            &mut sender_writer,
            &mut sess_sender,
            &flist,
            dir.path(),
            &tx,
            &cancel,
            true,
        )
        .await
        .expect("dry-run sender completes without hang");
        server_task.await.expect("server task");

        // No file was transferred — files_done stays 0.
        assert_eq!(stats.files_done, 0, "dry-run skips transfer");
        assert_eq!(stats.bytes_transferred, 0);

        // The lane carries one FileSkipped(DryRun), no FileCompleted.
        let evt = rx.recv().await.expect("file skipped");
        match evt {
            RsyncProgressEvent::FileSkipped { rel_path, reason } => {
                assert_eq!(rel_path, "hello.txt");
                assert_eq!(reason, SkipReason::DryRun);
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
}
