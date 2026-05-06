// SPDX-License-Identifier: ISC
//! Receiver state machine — port of openrsync's `receiver.c::rsync_receiver`,
//! `downloader.c::rsync_downloader`, and the `pre_file` plus
//! `rsync_uploader` block-signature emit path from `uploader.c`.
//!
//! Original copyright: Kristaps Dzonsons, Florian Obser; ISC license.
//! See [`LICENSES/openrsync-ISC.txt`] for the full notice.
//!
//! # Slice 7 scope (v7.0.0-alpha.X)
//!
//! Drives the **pull** direction once the handshake + filter list +
//! flist exchange has completed. The state machine is a simplified
//! Rust translation that:
//!
//! 1. Recovers the flist + post-flist `int32(0)` IO-error sentinel
//!    from the sender.
//! 2. For each regular file in the flist:
//!    - Always emits a `null_sum` blockset (`count=0`, `len=0`,
//!      `csum_len=0`, `rem=0`) — slice 7 is the whole-file path. Local
//!      block matching ships in slice 8.
//!    - Reads back the sender's blockset header echo per
//!      `downloader.c::blk_send_ack` (lines 337..339).
//!    - Drains the token stream into a local tempfile (`literal` only
//!      because we never request a block match), tracking a per-file
//!      digest (MD5 at proto >= 30, MD4-with-seed at proto < 30).
//!    - Verifies the 16-byte digest trailer.
//!    - Atomically renames the tempfile to the final destination path.
//! 3. Skips directory / symlink / dev / fifo / sock entries with a
//!    `mkdir_all` for directories. Slice 7 does not preserve mtime
//!    / mode / uid / gid (defer to slice 8).
//! 4. Emits per-file `FileCompleted` progress beacons.
//! 5. Closes the protocol with the standard receiver goodbye —
//!    `NDX_DONE` write + matching read + protocol-31 second pair —
//!    mirroring `receiver.c` lines 460..465 plus
//!    `main.c::read_final_goodbye` lines 875..906 (with the receiver
//!    side flipped relative to the sender).
//!
//! # Lock-free contract (CRITICAL)
//!
//! - The state machine is one async fn driven by a single tokio task.
//! - The [`MplexReader`] / [`MplexWriter`] halves are owned exclusively
//!   by that task — no shared cell, no `Mutex`.
//! - The [`WireSession`] is threaded as `&mut` and never wrapped in
//!   `Arc<Mutex<...>>`.
//! - The progress `mpsc::Sender` is the only cross-task hook; the
//!   receive side is the lane consumer in `super::WireRsyncTransport`.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::fs::{FileTimes, Metadata, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tokio::fs;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

use crate::adapters::rsync::types::{RsyncProgressEvent, RsyncTransportKind};
use crate::adapters::rsync::wire::flist::{
    Flist, FlistRecvOpts, is_dir, is_lnk, is_reg, recv_flist,
};
use crate::adapters::rsync::wire::hash::FileHasher;
use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
use crate::adapters::rsync::wire::ndx::{NDX_DONE, NdxState, read_ndx, write_ndx};
use crate::adapters::rsync::wire::session::WireSession;
use crate::domain::error::DomainError;
use crate::domain::rsync::RsyncStats;

/// Bit set on `iflags` shorts that would mean "transfer me" if the
/// receiver were itemizing. Mirrors upstream rsync 3.2.7's
/// `ITEM_TRANSFER = 1 << 15` (`rsync.h` line 229). Slice 7 always
/// requests a transfer for every regular file in the flist (no local
/// matching), so we always set this bit when emitting.
const ITEM_TRANSFER: u16 = 1 << 15;

/// Slice 10 — minimum length of a zero-run before the sparse-aware
/// writer collapses it into a filesystem hole via `seek`. 4 KiB matches
/// the typical 4 KiB block size on ext4/xfs/apfs — anything shorter is
/// not worth the extra syscall + would not free a block on rename
/// anyway. Mirrors upstream rsync 3.2.7's `sparse_end` write boundary
/// in `fileio.c::write_sparse` (lines 51..78).
const SPARSE_RUN_THRESHOLD: usize = 4096;

/// Slice 10 — deterministic tempfile suffix when `--partial` is set.
/// Mirrors upstream rsync 3.2.7's default partial-file naming convention
/// (`.rsync-partial-<basename>` style); we use the variant
/// `.<basename>.rsync-partial` because openrsync's tempfile helper builds
/// the suffix on the basename and we keep the dot-prefix to hide the
/// partial from naive directory listings.
const PARTIAL_TMP_SUFFIX: &str = "rsync-partial";

/// Bit set on `iflags` to mark "data is missing locally" — the basis
/// is gone or there is no local copy. Mirrors upstream rsync 3.2.7's
/// `ITEM_MISSING_DATA = 1 << 9`. Slice 7 always sends this because
/// the receiver never compares the local destination tree.
const ITEM_MISSING_DATA: u16 = 1 << 9;

/// Slice 9 / 10 — caller-supplied attribute apply mask plus the
/// `--delete` post-walk flag plus the slice-10 `--partial` /
/// `-S` (sparse) receiver-side knobs.
///
/// Threaded through [`drive_receiver_with_opts`] so the receiver can
/// stamp mode / mtime / link semantics on the destination tree without
/// pulling [`crate::adapters::rsync::types::PreserveFlags`] (which lives
/// in the value-object module and carries fields the receiver does not
/// need yet — owner / group / hardlinks / devices). Keeping a
/// purpose-built struct here means the receiver path stays focused on
/// the knobs that actually have a port.
///
/// Slice 10 additions are pure receiver-side filesystem behaviour — no
/// wire-format change. `--partial` and `-S` are post-walk decisions the
/// receiver makes against the local destination tree, never echoed back
/// to the sender. Both default to `false` so v9 callers stay
/// byte-identical.
#[expect(
    clippy::struct_excessive_bools,
    reason = "field-by-field 1:1 mirror of the rsync long flags the receiver applies (-p / -t / -l / --delete / --partial / -S); collapsing into a bitmask would lose the rust-doc parity with the upstream rsync flag matrix and obscure the six independent knobs."
)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ReceiverApplyOpts {
    /// Apply the on-wire mode bits to the destination file (rsync
    /// `-p`). When unset the destination keeps the umask-derived mode
    /// from the tempfile create.
    pub preserve_perms: bool,
    /// Apply the on-wire mtime to the destination file (rsync `-t`).
    /// When unset the destination keeps the host's "now".
    pub preserve_mtime: bool,
    /// Materialise `S_IFLNK` flist entries as symlinks in the
    /// destination tree (rsync `-l`). When unset the receiver skips
    /// symlink entries entirely (the rsync server respects the same
    /// boundary so we never see them on the wire).
    pub preserve_links: bool,
    /// Walk the destination tree after the per-file phase and remove
    /// any path that is not in the flist (rsync `--delete`).
    pub delete: bool,
    /// Slice 10 — keep partially-transferred tempfiles on a per-file
    /// failure (rsync `--partial`). When set, the tempfile path is
    /// stable across runs (`<dir>/.<basename>.rsync-partial`) so a
    /// subsequent invocation can resume; when unset, the tempfile uses a
    /// UUID suffix and is unlinked on per-file failure to avoid
    /// littering the destination tree.
    pub partial: bool,
    /// Slice 10 — preserve sparse holes when writing literal payload
    /// to the tempfile (rsync `-S`). When set, runs of zero bytes
    /// longer than [`SPARSE_RUN_THRESHOLD`] are turned into filesystem
    /// holes via `seek` (no `write`). When unset, every literal byte is
    /// written verbatim. Either way the per-file digest hashes the
    /// original literal stream — sparse is a filesystem-side
    /// optimisation, not a wire-format change.
    pub sparse: bool,
}

/// Bundle of every borrow the per-file request loop threads through
/// its helpers. Mirrors the `SenderCtx` pattern in
/// [`crate::adapters::rsync::wire::sender`] — collapses
/// `too_many_arguments` warnings without forcing one-shot parameters
/// to be packed into a single struct that's only used for a single
/// call.
struct ReceiverCtx<'a, R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    reader: &'a mut MplexReader<R>,
    writer: &'a mut MplexWriter<W>,
    sess: &'a mut WireSession,
    dst_root: &'a Path,
    progress_tx: &'a Sender<RsyncProgressEvent>,
    cancel: &'a CancellationToken,
    /// Codec state for `read_ndx` (proto-30+ index byte-reduction).
    ndx_in: &'a mut NdxState,
    /// Codec state for `write_ndx`.
    ndx_out: &'a mut NdxState,
    /// Apply mask threaded through from the caller.
    apply: ReceiverApplyOpts,
}

/// Per-token wire codes drained from the sender.
enum Token {
    /// Positive — `count` literal bytes follow.
    Literal {
        /// Byte count to read off the wire.
        len: u32,
    },
    /// Negative — copy block `idx` from the local block map.
    /// Slice 7 errors on this branch because we never advertise a
    /// non-empty block set.
    Match {
        /// Zero-based block index — `-(rawtok + 1)` from the wire
        /// encoding.
        idx: u32,
    },
    /// Zero — end of file; the 16-byte digest trailer follows.
    Eof,
}

/// Drive the receiver state machine to completion.
///
/// Reads the flist + IO-error sentinel from the sender, then processes
/// each file in flist order. The destination directory `dst_root` is
/// created if missing.
///
/// # Errors
///
/// - [`DomainError::RsyncProtocolError`] on transport failure, EOF in
///   the wrong place, malformed token, digest mismatch, or non-empty
///   block-match request from the sender (defer to slice 8).
/// - [`DomainError::RsyncProtocolError`] when filesystem I/O on the
///   destination root fails (mkdir, tempfile create, rename).
pub async fn drive_receiver<R, W>(
    reader: &mut MplexReader<R>,
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    dst_root: &Path,
    progress_tx: &Sender<RsyncProgressEvent>,
    cancel: &CancellationToken,
) -> Result<RsyncStats, DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    drive_receiver_with_opts(
        reader,
        writer,
        sess,
        dst_root,
        progress_tx,
        cancel,
        ReceiverApplyOpts::default(),
    )
    .await
}

/// Slice 9 — receiver entry that consults a [`ReceiverApplyOpts`] mask.
///
/// Callers that want to preserve attrs / honour `--delete` use this
/// variant; the legacy [`drive_receiver`] keeps the slice-7/8 contract
/// (no apply, no delete).
///
/// # Errors
///
/// Same shape as [`drive_receiver`]; additionally surfaces
/// [`DomainError::RsyncProtocolError`] when the post-walk delete pass
/// fails (e.g. permission denied during `unlink`).
pub async fn drive_receiver_with_opts<R, W>(
    reader: &mut MplexReader<R>,
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    dst_root: &Path,
    progress_tx: &Sender<RsyncProgressEvent>,
    cancel: &CancellationToken,
    apply: ReceiverApplyOpts,
) -> Result<RsyncStats, DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let entries = receive_flist_and_prepare(reader, sess, dst_root, progress_tx).await?;
    let mut stats = ZeroStats::new(&entries);
    let mut ndx_in = NdxState::new();
    let mut ndx_out = NdxState::new();
    let mut ctx = ReceiverCtx {
        reader,
        writer,
        sess,
        dst_root,
        progress_tx,
        cancel,
        ndx_in: &mut ndx_in,
        ndx_out: &mut ndx_out,
        apply,
    };
    let negotiated = ctx.sess.negotiated;
    request_all_files(&mut ctx, &entries, &mut stats).await?;
    finalize_goodbye(&mut ctx, negotiated).await?;
    apply_post_phase(dst_root, &entries, apply, &mut stats).await?;
    Ok(stats.finalize())
}

/// Read the flist + post-flist IO-error sentinel, sort the entries into
/// post-receive order, and pre-create every directory on the
/// destination tree. Pulled out of [`drive_receiver_with_opts`] so the
/// entry stays under the project's 30-line cognitive ceiling.
async fn receive_flist_and_prepare<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    dst_root: &Path,
    progress_tx: &Sender<RsyncProgressEvent>,
) -> Result<Vec<Flist>, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    fs::create_dir_all(dst_root).await.map_err(|e| {
        DomainError::RsyncProtocolError(format!("receiver: mkdir_all {}: {e}", dst_root.display()))
    })?;
    let negotiated = sess.negotiated;
    let mut entries = recv_flist(reader, sess, FlistRecvOpts::default(), negotiated).await?;
    sort_entries_for_request_index(&mut entries);
    let _io_error = read_io_error_sentinel(reader, sess, negotiated).await?;
    emit_session_started(progress_tx, &entries).await;
    pre_create_directories(dst_root, &entries).await?;
    Ok(entries)
}

/// Run the post-protocol-close phase: `--delete` walk + post-phase
/// attrs apply (symlink materialisation + directory mode/mtime).
///
/// The post-flist walk fires after the protocol close so a partial
/// transfer never deletes destination paths the sender hasn't replaced
/// yet.
async fn apply_post_phase(
    dst_root: &Path,
    entries: &[Flist],
    apply: ReceiverApplyOpts,
    stats: &mut ZeroStats,
) -> Result<(), DomainError> {
    if apply.delete {
        let removed = delete_extras(dst_root, entries).await?;
        stats.note_deleted(removed);
    }
    apply_post_phase_attrs(dst_root, entries, apply, stats).await;
    Ok(())
}

/// Sort the entries into the same order both peers will use to address
/// files. Mirrors rsync 3.2.7's `flist.c::flist_sort_and_clean` —
/// the sender writes entries in directory-walk order and then both
/// sides sort their local copies by full path before issuing
/// per-file requests; the index used over the wire is the post-sort
/// position. Without this, our `request_one_or_skip` loop addresses
/// entries by their receive-order index, which doesn't match the
/// sender's sorted index, and the two sides talk past each other.
///
/// We use a byte-wise lexicographic compare on the path. Upstream
/// rsync's `f_name_cmp` walks dirname / basename / type fields with
/// directory-aware promotion (dirs sort after files of the same
/// parent); for the slice-8 fixture (root + flat regular files +
/// one nested directory + one nested regular file) the byte sort
/// matches `f_name_cmp` because the only level with a directory at
/// it has the directory's name shorter than the nested entry's
/// extended path. Slice 9 will swap this for the full directory-aware
/// comparator.
fn sort_entries_for_request_index(entries: &mut [Flist]) {
    entries.sort_by(|a, b| {
        let a_bytes = path_to_sort_bytes(&a.path);
        let b_bytes = path_to_sort_bytes(&b.path);
        a_bytes.cmp(&b_bytes)
    });
}

/// Convert a [`Path`] to bytes for the sort comparator. Mirrors the
/// POSIX byte projection used by [`crate::adapters::rsync::wire::flist`].
fn path_to_sort_bytes(path: &Path) -> Vec<u8> {
    let s = path.to_string_lossy();
    s.bytes()
        .map(|b| if b == b'\\' { b'/' } else { b })
        .collect()
}

/// Read the post-flist `int32(0)` IO-error sentinel from the sender.
/// Mirrors `receiver.c::rsync_receiver` lines 254..262.
///
/// At protocol >= 30 the sender encodes the IO error into the flist's
/// end-of-list sentinel and skips the trailing int32 — same boundary as
/// our send path. We only consume the int32 when running below
/// protocol 30.
async fn read_io_error_sentinel<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    negotiated: i32,
) -> Result<i32, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if negotiated >= 30 {
        return Ok(0);
    }
    let val = reader.read_int(sess).await?;
    if val != 0 {
        return Err(DomainError::RsyncProtocolError(format!(
            "receiver: sender reported io_error {val}"
        )));
    }
    Ok(0)
}

/// Emit the `SessionStarted` beacon onto the progress lane.
async fn emit_session_started(progress_tx: &Sender<RsyncProgressEvent>, entries: &[Flist]) {
    let files_planned = u64::try_from(entries.len()).unwrap_or(u64::MAX);
    let bytes_planned = entries
        .iter()
        .filter(|e| is_reg(e.mode))
        .map(|e| u64::try_from(e.size).unwrap_or(0))
        .fold(0_u64, u64::saturating_add);
    let _ = progress_tx
        .send(RsyncProgressEvent::SessionStarted {
            transport: RsyncTransportKind::Wire,
            files_planned,
            bytes_planned,
        })
        .await;
}

/// Create every directory entry from the flist under `dst_root`.
/// Mirrors the receiver's behaviour of preparing the destination tree
/// before any files arrive — without this, a tempfile create inside a
/// not-yet-created subdirectory would fail.
async fn pre_create_directories(dst_root: &Path, entries: &[Flist]) -> Result<(), DomainError> {
    for entry in entries {
        if !is_dir(entry.mode) {
            continue;
        }
        let target = dst_root.join(&entry.path);
        fs::create_dir_all(&target).await.map_err(|e| {
            DomainError::RsyncProtocolError(format!(
                "receiver: mkdir_all {}: {e}",
                target.display()
            ))
        })?;
    }
    Ok(())
}

/// Walk the flist in order; for every regular file emit a request +
/// drain the sender's reply. Directories were pre-created so we just
/// skip non-regular entries. Mirrors the per-iteration pump in
/// `receiver.c::rsync_receiver` lines 341..425.
async fn request_all_files<R, W>(
    ctx: &mut ReceiverCtx<'_, R, W>,
    entries: &[Flist],
    stats: &mut ZeroStats,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let negotiated = ctx.sess.negotiated;
    for (idx, entry) in entries.iter().enumerate() {
        request_one_or_skip(ctx, idx, entry, stats).await?;
    }
    // End-of-phase marker — mirrors `uploader.c::rsync_uploader` line
    // 970 (`io_write_int(sess, fdout, -1)` once the loop finishes).
    write_ndx(ctx.writer, ctx.sess, ctx.ndx_out, negotiated, NDX_DONE).await?;
    Ok(())
}

/// Pump one iteration of the receiver loop: cancellation guard, skip
/// non-regular entries, then request the file's payload.
async fn request_one_or_skip<R, W>(
    ctx: &mut ReceiverCtx<'_, R, W>,
    idx: usize,
    entry: &Flist,
    stats: &mut ZeroStats,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    if ctx.cancel.is_cancelled() {
        return Err(DomainError::RsyncProtocolError(
            "receiver: cancelled mid-phase".to_string(),
        ));
    }
    if !is_reg(entry.mode) {
        return Ok(());
    }
    let i32_idx = i32::try_from(idx).map_err(|_e| {
        DomainError::RsyncProtocolError(format!("receiver: idx {idx} overflows i32"))
    })?;
    request_single_file(ctx, i32_idx, entry).await?;
    stats.note_file_completed(u64::try_from(entry.size).unwrap_or(0));
    Ok(())
}

/// Send the per-file request (idx + iflags + `null_sum` signatures),
/// then drain the sender's echo + token stream.
async fn request_single_file<R, W>(
    ctx: &mut ReceiverCtx<'_, R, W>,
    idx: i32,
    entry: &Flist,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let negotiated = ctx.sess.negotiated;
    write_ndx(ctx.writer, ctx.sess, ctx.ndx_out, negotiated, idx).await?;
    write_iflags(
        ctx.writer,
        ctx.sess,
        negotiated,
        ITEM_TRANSFER | ITEM_MISSING_DATA,
    )
    .await?;
    write_null_sum_blockset(ctx.writer, ctx.sess).await?;
    // Sender echoes idx + iflags + blockset header back per
    // `sender.c::send_files` lines 411..413 + `blocks.c::blk_recv_ack`.
    let echoed_idx = read_ndx(ctx.reader, ctx.sess, ctx.ndx_in, negotiated).await?;
    if echoed_idx != idx {
        return Err(DomainError::RsyncProtocolError(format!(
            "receiver: sender echoed idx {echoed_idx}, expected {idx}"
        )));
    }
    let _echoed_iflags = read_iflags(ctx.reader, ctx.sess, negotiated).await?;
    skip_blockset_header(ctx.reader, ctx.sess).await?;
    drain_file_payload(ctx, entry).await
}

/// Drain the token stream + digest trailer, write into a tempfile,
/// then rename atomically over the final destination path.
///
/// Slice 10: when [`ReceiverApplyOpts::partial`] is set, the tempfile
/// uses a deterministic name (`<dir>/.<basename>.rsync-partial`) and is
/// **not** unlinked on a per-file failure path — the partial bytes are
/// preserved so a subsequent run can reuse them. When the flag is unset
/// the legacy UUID-suffixed tempfile is used and is unlinked on failure
/// to avoid littering the destination tree.
async fn drain_file_payload<R, W>(
    ctx: &mut ReceiverCtx<'_, R, W>,
    entry: &Flist,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let final_path = ctx.dst_root.join(&entry.path);
    let tmp_path = tempfile_for(&final_path, ctx.apply.partial)?;
    ensure_parent_dir(&final_path).await?;
    let result = receive_into_tempfile(ctx.reader, ctx.sess, &tmp_path, entry, ctx.apply).await;
    let bytes_written = match result {
        Ok(b) => b,
        Err(err) => {
            cleanup_failed_tempfile(&tmp_path, ctx.apply.partial).await;
            return Err(err);
        }
    };
    rename_into_place(&tmp_path, &final_path).await?;
    apply_file_attrs(&final_path, entry, ctx.apply);
    emit_file_completed(ctx.progress_tx, entry, bytes_written).await;
    Ok(())
}

/// Clean up the tempfile when [`receive_into_tempfile`] errored out.
///
/// - `partial = true` (slice 10 `--partial`) — keep the file on disk so
///   the next invocation can resume from the partial bytes.
/// - `partial = false` — unlink the tempfile to avoid littering the
///   destination tree with `.<basename>.<uuid>.tmp` orphans.
///
/// Errors during the cleanup unlink are logged + swallowed so a single
/// permission denial does not mask the original transfer failure.
async fn cleanup_failed_tempfile(tmp_path: &Path, partial: bool) {
    if partial {
        tracing::info!(
            target: "rsync.wire",
            path = %tmp_path.display(),
            "receiver: --partial — preserving partial tempfile after failure"
        );
        return;
    }
    if let Err(err) = fs::remove_file(tmp_path).await {
        tracing::warn!(
            target: "rsync.wire",
            path = %tmp_path.display(),
            error = %err,
            "receiver: cleanup of failed tempfile failed (continuing)"
        );
    }
}

/// Stamp `mode` and `mtime` on a regular file when the matching
/// preserve flags are set. Errors are logged + swallowed so a single
/// attr stamp never aborts the whole transfer (mirrors openrsync's
/// downloader.c behaviour where the per-file `chmod` / `utimes` calls
/// log on failure but never propagate — the file's bytes have already
/// landed).
fn apply_file_attrs(final_path: &Path, entry: &Flist, apply: ReceiverApplyOpts) {
    if apply.preserve_perms {
        apply_mode_bits(final_path, entry.mode);
    }
    if apply.preserve_mtime {
        apply_mtime(final_path, entry.mtime);
    }
}

#[cfg(unix)]
fn apply_mode_bits(path: &Path, mode: u32) {
    use std::fs::{Permissions, set_permissions};
    let perms = Permissions::from_mode(mode & 0o7777);
    if let Err(err) = set_permissions(path, perms) {
        tracing::warn!(
            target: "rsync.wire",
            path = %path.display(),
            mode = format!("{:#o}", mode & 0o7777),
            error = %err,
            "receiver: chmod failed (continuing)"
        );
    }
}

#[cfg(not(unix))]
fn apply_mode_bits(_path: &Path, _mode: u32) {
    // No-op on non-Unix hosts. The wire mode field is a POSIX
    // construct — mirroring it onto e.g. NTFS would be a synthetic
    // mapping the rsync wire never specifies.
}

fn apply_mtime(path: &Path, mtime_secs: i64) {
    let Ok(secs_u64) = u64::try_from(mtime_secs) else {
        return;
    };
    let target = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs_u64))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let Ok(file) = OpenOptions::new().write(true).open(path) else {
        // Best-effort: a directory cannot be opened with write access
        // on every Unix; the dir-walk path uses the same helper but
        // hits this branch silently. The post-phase dir walk will
        // surface a chmod attempt instead.
        return;
    };
    let times = FileTimes::new().set_modified(target).set_accessed(target);
    if let Err(err) = file.set_times(times) {
        tracing::warn!(
            target: "rsync.wire",
            path = %path.display(),
            mtime = mtime_secs,
            error = %err,
            "receiver: utimensat failed (continuing)"
        );
    }
}

/// Walk the flist after the per-file phase and apply directory + symlink
/// attrs that could not be applied during the file phase:
///
/// - directory mode + mtime (must run after the directory's children
///   are written — applying mtime to a directory and *then* writing a
///   file inside bumps the directory mtime back to "now").
/// - symlink materialisation (`std::os::unix::fs::symlink` for
///   `S_IFLNK` entries when `apply.preserve_links` is set).
///
/// Stats: each materialised symlink bumps the per-session counters via
/// the `note_file_completed` arm so the post-session render still sees
/// a non-zero `files_done` for symlink-only transfers.
async fn apply_post_phase_attrs(
    dst_root: &Path,
    entries: &[Flist],
    apply: ReceiverApplyOpts,
    stats: &mut ZeroStats,
) {
    if !apply.preserve_perms && !apply.preserve_mtime && !apply.preserve_links {
        return;
    }
    for entry in entries {
        let target = dst_root.join(&entry.path);
        if is_lnk(entry.mode) {
            apply_symlink_entry(&target, entry, apply, stats).await;
        } else if is_dir(entry.mode) {
            apply_directory_attrs(&target, entry, apply);
        }
    }
}

/// Materialise a symlink under `target`. Removes any pre-existing
/// non-symlink occupant first (rsync `-l` always overwrites). Logs +
/// swallows individual failures.
async fn apply_symlink_entry(
    target: &Path,
    entry: &Flist,
    apply: ReceiverApplyOpts,
    stats: &mut ZeroStats,
) {
    if !apply.preserve_links {
        return;
    }
    let Some(link) = entry.link.as_ref() else {
        tracing::warn!(
            target: "rsync.wire",
            path = %target.display(),
            "receiver: symlink entry without link target — skipping"
        );
        return;
    };
    remove_existing_path(target).await;
    if let Err(err) = create_symlink(link, target).await {
        tracing::warn!(
            target: "rsync.wire",
            path = %target.display(),
            link = %link.display(),
            error = %err,
            "receiver: symlink create failed (continuing)"
        );
        return;
    }
    stats.note_file_completed(0);
}

/// Remove a pre-existing path so the symlink create never collides
/// with a stale dir / file entry. Errors are swallowed — the
/// [`create_symlink`] call surfaces whatever failure mode actually
/// blocks the rsync `-l` semantics.
async fn remove_existing_path(target: &Path) {
    let Ok(meta) = fs::symlink_metadata(target).await else {
        return;
    };
    let _ = if meta.is_dir() {
        fs::remove_dir_all(target).await
    } else {
        fs::remove_file(target).await
    };
}

#[cfg(unix)]
async fn create_symlink(link: &Path, target: &Path) -> io::Result<()> {
    use std::os::unix::fs::symlink;

    use tokio::task::spawn_blocking;
    let link = link.to_path_buf();
    let target = target.to_path_buf();
    spawn_blocking(move || symlink(&link, &target))
        .await
        .map_err(|join_err| io::Error::other(format!("symlink join error: {join_err}")))?
}

#[cfg(not(unix))]
async fn create_symlink(_link: &Path, _target: &Path) -> io::Result<()> {
    Err(io::Error::other("symlink unsupported on this platform"))
}

/// Apply mode + mtime to a destination directory after every child has
/// been written.
fn apply_directory_attrs(target: &Path, entry: &Flist, apply: ReceiverApplyOpts) {
    if apply.preserve_perms {
        apply_mode_bits(target, entry.mode);
    }
    if apply.preserve_mtime {
        apply_dir_mtime(target, entry.mtime);
    }
}

/// `apply_mtime` opens the path with `write` access — directories on
/// many Unix hosts can't be opened that way. Use `set_times` on a
/// read-only directory handle instead.
fn apply_dir_mtime(path: &Path, mtime_secs: i64) {
    let Ok(secs_u64) = u64::try_from(mtime_secs) else {
        return;
    };
    let target = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs_u64))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let Ok(file) = OpenOptions::new().read(true).open(path) else {
        tracing::warn!(
            target: "rsync.wire",
            path = %path.display(),
            "receiver: dir-mtime open failed (continuing)"
        );
        return;
    };
    let times = FileTimes::new().set_modified(target).set_accessed(target);
    if let Err(err) = file.set_times(times) {
        tracing::warn!(
            target: "rsync.wire",
            path = %path.display(),
            mtime = mtime_secs,
            error = %err,
            "receiver: dir-utimensat failed (continuing)"
        );
    }
}

/// Slice 9 — `--delete` post-walk. Walks the destination tree
/// recursively and removes every path that is not in the flist set.
/// Directories are deleted last (post-order traversal) so the unlink
/// calls succeed even when the directory contained other tracked
/// entries.
async fn delete_extras(dst_root: &Path, entries: &[Flist]) -> Result<u64, DomainError> {
    let keep = build_keep_set(dst_root, entries);
    let mut files: Vec<PathBuf> = Vec::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    collect_paths(dst_root, &mut files, &mut dirs).await?;
    let removed_files = remove_files_outside_set(files, &keep).await;
    let removed_dirs = remove_dirs_outside_set(dirs, &keep, dst_root).await;
    Ok(removed_files.saturating_add(removed_dirs))
}

/// Materialise the absolute "keep" path set from the flist. The root
/// `.` entry collapses to `dst_root` itself so the post-walk never
/// tries to remove the destination root.
fn build_keep_set(dst_root: &Path, entries: &[Flist]) -> HashSet<PathBuf> {
    entries
        .iter()
        .map(|e| {
            if e.path.as_os_str() == "." {
                dst_root.to_path_buf()
            } else {
                dst_root.join(&e.path)
            }
        })
        .collect()
}

/// Remove every file under `dst_root` that is not in `keep`. Failures
/// log + continue so a single permission denial does not abort the
/// whole `--delete` pass.
async fn remove_files_outside_set(files: Vec<PathBuf>, keep: &HashSet<PathBuf>) -> u64 {
    let mut removed = 0_u64;
    for path in files {
        if keep.contains(&path) {
            continue;
        }
        match fs::remove_file(&path).await {
            Ok(()) => removed = removed.saturating_add(1),
            Err(err) => tracing::warn!(
                target: "rsync.wire",
                path = %path.display(),
                error = %err,
                "receiver: --delete unlink failed (continuing)"
            ),
        }
    }
    removed
}

/// Remove every directory under `dst_root` that is not in `keep`,
/// deepest-first so `remove_dir` does not race against children.
async fn remove_dirs_outside_set(
    mut dirs: Vec<PathBuf>,
    keep: &HashSet<PathBuf>,
    dst_root: &Path,
) -> u64 {
    dirs.sort_by_key(|p| Reverse(p.components().count()));
    let mut removed = 0_u64;
    for path in dirs {
        if keep.contains(&path) || path == dst_root {
            continue;
        }
        match fs::remove_dir(&path).await {
            Ok(()) => removed = removed.saturating_add(1),
            Err(err) => tracing::warn!(
                target: "rsync.wire",
                path = %path.display(),
                error = %err,
                "receiver: --delete rmdir failed (continuing)"
            ),
        }
    }
    removed
}

/// Walk `root` recursively, splitting paths into `files` and `dirs`.
/// Used by [`delete_extras`] for the post-order delete walk.
async fn collect_paths(
    root: &Path,
    files: &mut Vec<PathBuf>,
    dirs: &mut Vec<PathBuf>,
) -> Result<(), DomainError> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        dirs.push(dir.clone());
        drain_directory_into(&dir, &mut stack, files).await?;
    }
    Ok(())
}

/// Read one directory into `stack` (subdirs) and `files` (everything
/// else, including symlinks-to-dirs). Errors are logged + swallowed
/// so a single readdir failure does not abort the whole walk.
async fn drain_directory_into(
    dir: &Path,
    stack: &mut Vec<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> Result<(), DomainError> {
    let Some(mut readdir) = open_readdir_logging(dir).await else {
        return Ok(());
    };
    while let Some(entry) = readdir.next_entry().await.map_err(|e| {
        DomainError::RsyncProtocolError(format!("receiver: --delete readdir-next: {e}"))
    })? {
        let path = entry.path();
        if let Some(meta) = fetch_lstat_logging(&path).await {
            if meta.is_dir() && !meta.file_type().is_symlink() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    Ok(())
}

/// `read_dir` wrapper that logs + returns `None` instead of bubbling
/// the error up. Used by the `--delete` post-walk where a single
/// failed directory should not abort the whole scan.
async fn open_readdir_logging(dir: &Path) -> Option<fs::ReadDir> {
    match fs::read_dir(dir).await {
        Ok(rd) => Some(rd),
        Err(err) => {
            tracing::warn!(
                target: "rsync.wire",
                path = %dir.display(),
                error = %err,
                "receiver: --delete readdir failed (continuing)"
            );
            None
        }
    }
}

/// `symlink_metadata` wrapper that logs + returns `None` instead of
/// bubbling. Same swallow contract as [`open_readdir_logging`].
async fn fetch_lstat_logging(path: &Path) -> Option<Metadata> {
    match fs::symlink_metadata(path).await {
        Ok(m) => Some(m),
        Err(err) => {
            tracing::warn!(
                target: "rsync.wire",
                path = %path.display(),
                error = %err,
                "receiver: --delete lstat failed (continuing)"
            );
            None
        }
    }
}

/// Ensure the destination's parent directory exists before opening a
/// tempfile in it. The flist walker pre-creates every directory entry,
/// but `parent()` may still be missing for top-level files placed
/// directly in `dst_root`.
async fn ensure_parent_dir(final_path: &Path) -> Result<(), DomainError> {
    let Some(parent) = final_path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent).await.map_err(|e| {
        DomainError::RsyncProtocolError(format!(
            "receiver: mkdir_all parent of {}: {e}",
            final_path.display()
        ))
    })
}

/// Open a tempfile + drain the sender's token stream into it. Flushes
/// + closes the file so the rename can take over.
///
/// Slice 10: when [`ReceiverApplyOpts::sparse`] is set, the writer
/// looks for runs of zero bytes longer than [`SPARSE_RUN_THRESHOLD`]
/// and turns them into filesystem holes via `seek` instead of writing
/// zeros. Trailing holes are preserved by truncating the file to the
/// flist-declared size after the token stream EOF. When the flag is
/// unset every literal byte is written verbatim.
async fn receive_into_tempfile<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    tmp_path: &Path,
    entry: &Flist,
    apply: ReceiverApplyOpts,
) -> Result<u64, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let mut tmp_file = fs::File::create(tmp_path).await.map_err(|e| {
        DomainError::RsyncProtocolError(format!(
            "receiver: create tempfile {}: {e}",
            tmp_path.display()
        ))
    })?;
    let bytes_written = consume_token_stream(reader, sess, &mut tmp_file, apply.sparse).await?;
    if apply.sparse {
        finalize_sparse_length(&tmp_file, tmp_path, entry).await?;
    }
    tmp_file.flush().await.map_err(|e| {
        DomainError::RsyncProtocolError(format!("receiver: flush {}: {e}", tmp_path.display()))
    })?;
    drop(tmp_file);
    Ok(bytes_written)
}

/// Truncate the tempfile to the flist-declared size when the file ends
/// inside a sparse hole. Without this, a file that ends with a long
/// zero-run would be shorter than the sender intended (the writer
/// `seek`-ed past the trailing zeros without writing them).
///
/// Mirrors upstream rsync 3.2.7's `fileio.c::write_sparse` finalisation
/// — when the last write was a sparse seek, `set_len` extends the file
/// out to the expected size so the reader sees the correct trailing
/// bytes (which the filesystem materialises as zeros on read).
async fn finalize_sparse_length(
    tmp_file: &fs::File,
    tmp_path: &Path,
    entry: &Flist,
) -> Result<(), DomainError> {
    let Ok(expected) = u64::try_from(entry.size) else {
        return Ok(());
    };
    tmp_file.set_len(expected).await.map_err(|e| {
        DomainError::RsyncProtocolError(format!(
            "receiver: sparse set_len {} -> {expected}: {e}",
            tmp_path.display()
        ))
    })
}

/// Atomically promote `tmp_path` to `final_path`. The unix rename is
/// atomic-on-same-volume, so a partial download never bubbles up to a
/// reader.
async fn rename_into_place(tmp_path: &Path, final_path: &Path) -> Result<(), DomainError> {
    fs::rename(tmp_path, final_path).await.map_err(|e| {
        DomainError::RsyncProtocolError(format!(
            "receiver: rename {} -> {}: {e}",
            tmp_path.display(),
            final_path.display()
        ))
    })
}

/// Push a `FileCompleted` beacon onto the progress lane.
async fn emit_file_completed(
    progress_tx: &Sender<RsyncProgressEvent>,
    entry: &Flist,
    bytes_written: u64,
) {
    let rel = entry.path.to_string_lossy().into_owned();
    let _ = progress_tx
        .send(RsyncProgressEvent::FileCompleted {
            rel_path: rel,
            bytes_transferred: bytes_written,
            bytes_skipped: 0,
        })
        .await;
}

/// Consume the sender's token stream into `out`. Returns the total
/// bytes written.
///
/// Mirrors `downloader.c::rsync_downloader` lines 449..549 — the inner
/// `again` loop reading rawtok values + a final 16-byte trailer
/// matched against the running per-file MD4/MD5 context.
///
/// Slice 10 — when `sparse` is set, the literal-byte drain detects
/// long zero runs and turns them into filesystem holes via `seek`
/// instead of writing zeros. The per-file digest is folded over the
/// original literal bytes either way (sparse is a filesystem-side
/// optimisation, not a wire-format change).
async fn consume_token_stream<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    out: &mut fs::File,
    sparse: bool,
) -> Result<u64, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let mut hasher = FileHasher::for_protocol(sess.seed, sess.negotiated);
    let mut total = 0_u64;
    loop {
        match next_token(reader, sess).await? {
            Token::Literal { len } => {
                let (n, chunk) = drain_literal(reader, sess, len, out, sparse).await?;
                hasher.update(&chunk);
                total = total.saturating_add(n);
            }
            Token::Match { idx } => return Err(reject_unexpected_match(idx)),
            Token::Eof => {
                verify_trailer(reader, sess, hasher).await?;
                return Ok(total);
            }
        }
    }
}

/// Build the protocol error for a match token in slice 7's pull
/// path. We never advertise non-empty block sets, so any match token
/// the sender emits is a wire violation.
fn reject_unexpected_match(idx: u32) -> DomainError {
    DomainError::RsyncProtocolError(format!(
        "receiver: unexpected match token idx={idx}: slice 7 only \
         advertises null_sum blocksets"
    ))
}

/// Read the 16-byte digest trailer off the wire and compare it against
/// the running per-file hasher. Mirrors `downloader.c::rsync_downloader`
/// lines 540..552.
async fn verify_trailer<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    hasher: FileHasher,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let mut trailer = [0_u8; 16];
    reader.read_buf(sess, &mut trailer).await?;
    let computed = hasher.finish();
    if trailer != computed {
        return Err(DomainError::RsyncProtocolError(
            "receiver: per-file digest trailer mismatch".to_string(),
        ));
    }
    Ok(())
}

/// Decode the next rawtok off the wire.
async fn next_token<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
) -> Result<Token, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let raw = reader.read_int(sess).await?;
    if raw == 0 {
        return Ok(Token::Eof);
    }
    if raw > 0 {
        let len = u32::try_from(raw).map_err(|_e| {
            DomainError::RsyncProtocolError(format!("receiver: bad literal token {raw}"))
        })?;
        return Ok(Token::Literal { len });
    }
    // raw < 0 — block-match token. idx = -raw - 1.
    let idx_signed = raw
        .checked_neg()
        .and_then(|n| n.checked_sub(1))
        .ok_or_else(|| {
            DomainError::RsyncProtocolError(format!("receiver: match token underflow {raw}"))
        })?;
    let idx = u32::try_from(idx_signed).map_err(|_e| {
        DomainError::RsyncProtocolError(format!("receiver: match token idx {idx_signed} oob"))
    })?;
    Ok(Token::Match { idx })
}

/// Drain `len` literal bytes off the wire into `out`. Returns
/// `(bytes_written, chunk_for_digest)` so the caller can fold the bytes
/// into the running per-file digest.
///
/// Slice 10 — when `sparse` is set, runs of zero bytes longer than
/// [`SPARSE_RUN_THRESHOLD`] are turned into filesystem holes via `seek`
/// instead of writing zeros. The non-zero head + tail are still written
/// verbatim. The returned `chunk_for_digest` always carries the
/// original wire bytes (zeros included) — sparse is a write-path
/// optimisation only.
async fn drain_literal<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    len: u32,
    out: &mut fs::File,
    sparse: bool,
) -> Result<(u64, Vec<u8>), DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let len_usize = usize::try_from(len).unwrap_or(0);
    let mut buf = vec![0_u8; len_usize];
    if !buf.is_empty() {
        reader.read_buf(sess, &mut buf).await?;
        if sparse {
            write_literal_sparse(out, &buf).await?;
        } else {
            out.write_all(&buf).await.map_err(|e| {
                DomainError::RsyncProtocolError(format!("receiver: tempfile write_all: {e}"))
            })?;
        }
    }
    Ok((u64::from(len), buf))
}

/// Write `buf` to `out` while collapsing zero-runs of at least
/// [`SPARSE_RUN_THRESHOLD`] bytes into filesystem holes via `seek`.
/// Mirrors upstream rsync 3.2.7's `fileio.c::write_sparse` (lines
/// 51..78) write loop — same inner state machine, same threshold
/// semantics, async-aware via tokio.
async fn write_literal_sparse(out: &mut fs::File, buf: &[u8]) -> Result<(), DomainError> {
    let mut idx = 0_usize;
    while idx < buf.len() {
        let zero_start = idx;
        while idx < buf.len() && buf[idx] == 0 {
            idx = idx.saturating_add(1);
        }
        let zero_len = idx.saturating_sub(zero_start);
        if zero_len >= SPARSE_RUN_THRESHOLD {
            seek_past_zeros(out, zero_len).await?;
        } else if zero_len > 0 {
            // Sub-threshold zero run — write verbatim.
            out.write_all(&buf[zero_start..idx]).await.map_err(|e| {
                DomainError::RsyncProtocolError(format!("receiver: sparse write zero-run: {e}"))
            })?;
        }
        let nonzero_start = idx;
        while idx < buf.len() && buf[idx] != 0 {
            idx = idx.saturating_add(1);
        }
        if idx > nonzero_start {
            out.write_all(&buf[nonzero_start..idx]).await.map_err(|e| {
                DomainError::RsyncProtocolError(format!("receiver: sparse write nonzero-run: {e}"))
            })?;
        }
    }
    Ok(())
}

/// Advance the file position by `len` bytes via `seek` so the
/// underlying filesystem leaves a hole. The filesystem allocates blocks
/// on first write past the seek; subsequent reads of the unwritten
/// region return zeros. Used by [`write_literal_sparse`] for
/// runs longer than [`SPARSE_RUN_THRESHOLD`].
async fn seek_past_zeros(out: &mut fs::File, len: usize) -> Result<(), DomainError> {
    use std::io::SeekFrom;

    use tokio::io::AsyncSeekExt;
    let len_i64 = i64::try_from(len).map_err(|_e| {
        DomainError::RsyncProtocolError(format!("receiver: sparse zero-run {len} overflows i64"))
    })?;
    out.seek(SeekFrom::Current(len_i64))
        .await
        .map_err(|e| DomainError::RsyncProtocolError(format!("receiver: sparse seek: {e}")))?;
    Ok(())
}

/// Build the tempfile path for a final destination.
///
/// - `partial = false` (default) — sibling file with
///   `.{name}.{rand}.tmp` prefix. Mirrors openrsync `mktemplate` /
///   `mkstempat` (downloader.c lines 416..425) but uses tokio's async
///   rename to commit.
/// - `partial = true` (slice 10 `--partial`) — deterministic sibling
///   file `.{name}.rsync-partial`, so a subsequent invocation can find
///   and reuse the same path. The deterministic name is the contract
///   that lets `--partial` actually preserve resumable state across
///   runs.
fn tempfile_for(final_path: &Path, partial: bool) -> Result<PathBuf, DomainError> {
    let parent = final_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let name = final_path
        .file_name()
        .ok_or_else(|| {
            DomainError::RsyncProtocolError(format!(
                "receiver: cannot derive filename from {}",
                final_path.display()
            ))
        })?
        .to_string_lossy()
        .into_owned();
    if partial {
        return Ok(parent.join(format!(".{name}.{PARTIAL_TMP_SUFFIX}")));
    }
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    Ok(parent.join(format!(".{name}.{suffix}.tmp")))
}

/// Emit the `null_sum` blockset (count=0, len=0, csum=0, rem=0) — the
/// receiver-side wire shape that signals "I have nothing locally".
/// Mirrors `generator.c::generate_and_send_sums` lines 1948..1958.
async fn write_null_sum_blockset<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    writer.write_int(sess, 0).await?; // count
    writer.write_int(sess, 0).await?; // length
    writer.write_int(sess, 0).await?; // csum_len
    writer.write_int(sess, 0).await?; // remainder
    Ok(())
}

/// Skip the four-field blockset header echo from the sender. The
/// values are semantically identical to what we just wrote
/// (`null_sum`) so the receiver simply consumes them.
async fn skip_blockset_header<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let _count = reader.read_int(sess).await?;
    let _len = reader.read_int(sess).await?;
    let _csum_len = reader.read_int(sess).await?;
    let _rem = reader.read_int(sess).await?;
    Ok(())
}

/// Mirror of [`crate::adapters::rsync::wire::sender::write_iflags`]:
/// at protocol >= 29 emit a 2-byte little-endian short, at < 29 emit
/// nothing (the upstream client synthesises a default).
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
    writer.write_buf(sess, &iflags.to_le_bytes()).await
}

/// Mirror of [`crate::adapters::rsync::wire::sender::read_iflags`].
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

/// Symmetric receiver-side goodbye handshake. Mirrors `receiver.c`
/// lines 460..465 — write `NDX_DONE`, then read the sender's
/// `NDX_DONE` ack. At protocol >= 31 the handshake ships an extra
/// pair (`main.c::read_final_goodbye` lines 887..898).
async fn finalize_goodbye<R, W>(
    ctx: &mut ReceiverCtx<'_, R, W>,
    negotiated: i32,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    // Already wrote the trailing NDX_DONE inside `request_all_files`.
    // The sender now responds with NDX_DONE.
    let first = read_ndx(ctx.reader, ctx.sess, ctx.ndx_in, negotiated).await?;
    if first != NDX_DONE {
        return Err(DomainError::RsyncProtocolError(format!(
            "receiver: bad goodbye ack {first}, want NDX_DONE"
        )));
    }
    if negotiated < 31 {
        return Ok(());
    }
    // Protocol >= 31 — exchange another NDX_DONE pair.
    write_ndx(ctx.writer, ctx.sess, ctx.ndx_out, negotiated, NDX_DONE).await?;
    let second = read_ndx(ctx.reader, ctx.sess, ctx.ndx_in, negotiated).await?;
    if second != NDX_DONE {
        return Err(DomainError::RsyncProtocolError(format!(
            "receiver: bad second goodbye ack {second}, want NDX_DONE"
        )));
    }
    Ok(())
}

/// Per-session running stats accumulator.
struct ZeroStats {
    files_total: u64,
    bytes_total: u64,
    files_done: u64,
    bytes_transferred: u64,
    files_deleted: u64,
}

impl ZeroStats {
    fn new(flist: &[Flist]) -> Self {
        let files_total = u64::try_from(flist.len()).unwrap_or(u64::MAX);
        let bytes_total = flist
            .iter()
            .filter(|e| is_reg(e.mode))
            .map(|e| u64::try_from(e.size).unwrap_or(0))
            .fold(0_u64, u64::saturating_add);
        Self {
            files_total,
            bytes_total,
            files_done: 0,
            bytes_transferred: 0,
            files_deleted: 0,
        }
    }

    const fn note_file_completed(&mut self, bytes: u64) {
        self.files_done = self.files_done.saturating_add(1);
        self.bytes_transferred = self.bytes_transferred.saturating_add(bytes);
    }

    const fn note_deleted(&mut self, count: u64) {
        self.files_deleted = self.files_deleted.saturating_add(count);
    }

    const fn finalize(self) -> RsyncStats {
        RsyncStats {
            files_total: self.files_total,
            files_done: self.files_done,
            bytes_total: self.bytes_total,
            bytes_transferred: self.bytes_transferred,
            bytes_skipped: 0,
            files_deleted: self.files_deleted,
            files_failed: 0,
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code uses unwrap/expect for brevity per project convention"
)]
mod tests {
    use super::{
        ITEM_MISSING_DATA, ITEM_TRANSFER, ReceiverApplyOpts, apply_file_attrs, build_keep_set,
        consume_token_stream, delete_extras, next_token, sort_entries_for_request_index,
        tempfile_for, write_null_sum_blockset,
    };
    use crate::adapters::rsync::wire::flist::Flist;
    use crate::adapters::rsync::wire::hash::FileHasher;
    use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
    use crate::adapters::rsync::wire::session::WireSession;
    use std::path::{Path, PathBuf};
    use tokio::fs;
    use tokio::io::duplex;

    #[test]
    fn iflags_layout_matches_upstream_constants() {
        // upstream rsync.h line 229: ITEM_TRANSFER = 1<<15
        assert_eq!(ITEM_TRANSFER, 0x8000);
        // upstream rsync.h line 222: ITEM_MISSING_DATA = 1<<9
        assert_eq!(ITEM_MISSING_DATA, 0x0200);
    }

    /// Slice-8 — confirm that the post-flist sort produces the same
    /// per-entry order both peers use for the file-request index.
    /// rsync 3.2.7 sorts the flist after receive (`flist.c::flist_sort_and_clean`)
    /// and addresses files by post-sort index. Without this our request
    /// loop would talk past the sender's idea of which file we want.
    #[test]
    fn sort_entries_matches_real_rsync_indices_for_pull_fixture() {
        // Mirror the captured wire order from `rsync_wire_pull_pipeline_against_real_vm`:
        // sender emits in directory-walk order: top, b.txt, a.txt, nested, nested/c.txt.
        let mut entries = vec![
            Flist::directory(PathBuf::from("."), 0, 0o755, 0, 0),
            Flist::regular(PathBuf::from("b.txt"), 27, 0, 0o644, 0, 0),
            Flist::regular(PathBuf::from("a.txt"), 11, 0, 0o644, 0, 0),
            Flist::directory(PathBuf::from("nested"), 0, 0o755, 0, 0),
            Flist::regular(PathBuf::from("nested/c.txt"), 18, 0, 0o644, 0, 0),
        ];
        sort_entries_for_request_index(&mut entries);
        let paths: Vec<&Path> = entries.iter().map(|e| e.path.as_path()).collect();
        assert_eq!(
            paths,
            vec![
                Path::new("."),
                Path::new("a.txt"),
                Path::new("b.txt"),
                Path::new("nested"),
                Path::new("nested/c.txt"),
            ]
        );
    }

    #[test]
    fn tempfile_for_uses_sibling_directory() {
        let final_path = Path::new("/tmp/sub/file.txt");
        let tmp = tempfile_for(final_path, false).expect("tempfile_for");
        assert_eq!(tmp.parent().unwrap(), Path::new("/tmp/sub"));
        let name = tmp.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".file.txt."), "unexpected name: {name}");
        assert!(name.ends_with(".tmp"), "unexpected name: {name}");
    }

    #[test]
    fn tempfile_for_relative_path_lands_in_cwd() {
        let final_path = Path::new("file.txt");
        let tmp = tempfile_for(final_path, false).expect("tempfile_for");
        assert_eq!(tmp.parent().unwrap(), Path::new("."));
    }

    /// Slice 10 — `--partial` produces a deterministic sibling path
    /// `<dir>/.<basename>.rsync-partial` so a subsequent run can find
    /// and reuse the partial bytes.
    #[test]
    fn tempfile_for_partial_uses_deterministic_suffix() {
        let final_path = Path::new("/tmp/sub/file.txt");
        let tmp = tempfile_for(final_path, true).expect("tempfile_for");
        assert_eq!(tmp.parent().unwrap(), Path::new("/tmp/sub"));
        let name = tmp.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name, ".file.txt.rsync-partial");
    }

    /// Slice 10 — calling `tempfile_for` twice with `partial=true` must
    /// yield the same path; the deterministic name is the contract that
    /// lets `--partial` resume across runs.
    #[test]
    fn tempfile_for_partial_is_stable_across_calls() {
        let final_path = Path::new("/tmp/sub/file.txt");
        let a = tempfile_for(final_path, true).expect("tempfile_for");
        let b = tempfile_for(final_path, true).expect("tempfile_for");
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn write_null_sum_blockset_emits_four_zero_u32() {
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        write_null_sum_blockset(&mut w, &mut sess_w)
            .await
            .expect("null_sum");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        for _ in 0..4 {
            let v = r.read_int(&mut sess_r).await.expect("read");
            assert_eq!(v, 0);
        }
    }

    #[tokio::test]
    async fn next_token_decodes_literal_match_eof() {
        // Build a synthetic stream: literal(5), match(idx=2), eof.
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        w.write_int(&mut sess_w, 5).await.expect("lit");
        // match token: idx=2 → wire = -(2+1) = -3
        w.write_int(&mut sess_w, -3).await.expect("match");
        w.write_int(&mut sess_w, 0).await.expect("eof");

        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let lit = next_token(&mut r, &mut sess_r).await.expect("lit");
        match lit {
            super::Token::Literal { len } => assert_eq!(len, 5),
            _ => panic!("expected literal"),
        }
        let m = next_token(&mut r, &mut sess_r).await.expect("match");
        match m {
            super::Token::Match { idx } => assert_eq!(idx, 2),
            _ => panic!("expected match"),
        }
        let e = next_token(&mut r, &mut sess_r).await.expect("eof");
        match e {
            super::Token::Eof => {}
            _ => panic!("expected eof"),
        }
    }

    #[tokio::test]
    async fn consume_token_stream_round_trips_literal_only_payload() {
        let payload: &[u8] = b"hello-from-the-sender";

        // Pre-compute the digest the sender would emit (proto 31 → MD5).
        let mut hasher = FileHasher::for_protocol(0, 31);
        hasher.update(payload);
        let trailer = hasher.finish();

        let (left, right) = duplex(8 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);

        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        // literal(payload.len()) + payload bytes.
        w.write_int(&mut sess_w, payload.len() as i32)
            .await
            .expect("lit");
        w.write_buf(&mut sess_w, payload).await.expect("payload");
        // eof + trailer.
        w.write_int(&mut sess_w, 0).await.expect("eof");
        w.write_buf(&mut sess_w, &trailer).await.expect("trailer");
        drop(w);

        let dir = tempfile::tempdir().expect("tempdir");
        let out_path = dir.path().join("out.bin");
        let mut out = fs::File::create(&out_path).await.expect("create");

        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        sess_r.negotiated = 31;
        sess_r.seed = 0;
        let bytes_written = consume_token_stream(&mut r, &mut sess_r, &mut out, false)
            .await
            .expect("consume");
        out.sync_all().await.expect("sync");
        drop(out);
        assert_eq!(bytes_written, payload.len() as u64);
        let got = fs::read(&out_path).await.expect("read back");
        assert_eq!(got, payload);
    }

    #[tokio::test]
    async fn consume_token_stream_rejects_match_token() {
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        // match token: idx=0 → wire = -1.
        w.write_int(&mut sess_w, -1).await.expect("match");
        drop(w);

        let dir = tempfile::tempdir().expect("tempdir");
        let out_path = dir.path().join("out.bin");
        let mut out = fs::File::create(&out_path).await.expect("create");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        sess_r.negotiated = 31;
        let err = consume_token_stream(&mut r, &mut sess_r, &mut out, false)
            .await
            .expect_err("must reject match");
        assert!(format!("{err}").contains("match token"));
    }

    #[tokio::test]
    async fn consume_token_stream_rejects_digest_mismatch() {
        // emit eof + 16 bytes of garbage trailer (won't match MD5 of empty).
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        w.write_int(&mut sess_w, 0).await.expect("eof");
        w.write_buf(&mut sess_w, &[0xff_u8; 16])
            .await
            .expect("bad trailer");
        drop(w);

        let dir = tempfile::tempdir().expect("tempdir");
        let out_path = dir.path().join("out.bin");
        let mut out = fs::File::create(&out_path).await.expect("create");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        sess_r.negotiated = 31;
        sess_r.seed = 0;
        let err = consume_token_stream(&mut r, &mut sess_r, &mut out, false)
            .await
            .expect_err("must reject mismatch");
        assert!(format!("{err}").contains("digest trailer mismatch"));
    }

    /// Slice 9 — confirm `apply_file_attrs` stamps the requested mode
    /// onto a regular file when `preserve_perms` is set, and leaves the
    /// file untouched when the flag is off.
    #[cfg(unix)]
    #[tokio::test]
    async fn apply_file_attrs_chmods_when_preserve_perms_set() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.txt");
        fs::write(&path, b"hello").await.expect("write");
        let entry = Flist::regular(PathBuf::from("a.txt"), 5, 1_700_000_000, 0o600, 0, 0);
        let apply = ReceiverApplyOpts {
            preserve_perms: true,
            preserve_mtime: false,
            preserve_links: false,
            delete: false,
            partial: false,
            sparse: false,
        };
        apply_file_attrs(&path, &entry, apply);
        let meta = fs::metadata(&path).await.expect("stat");
        // Upper file-type bits should be cleared by the masking inside
        // `apply_mode_bits`; we compare only the perm bits.
        assert_eq!(meta.permissions().mode() & 0o7777, 0o600);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn apply_file_attrs_skips_chmod_when_preserve_perms_unset() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("b.txt");
        fs::write(&path, b"hi").await.expect("write");
        // Set a known starting mode so we can detect a no-op.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("set perms");
        let entry = Flist::regular(PathBuf::from("b.txt"), 2, 1_700_000_000, 0o600, 0, 0);
        let apply = ReceiverApplyOpts::default();
        apply_file_attrs(&path, &entry, apply);
        let meta = fs::metadata(&path).await.expect("stat");
        assert_eq!(meta.permissions().mode() & 0o7777, 0o644);
    }

    #[test]
    fn build_keep_set_collapses_root_dot_to_dst_root() {
        let dst_root = Path::new("/tmp/dst");
        let entries = vec![
            Flist::directory(PathBuf::from("."), 0, 0o755, 0, 0),
            Flist::regular(PathBuf::from("a.txt"), 5, 0, 0o644, 0, 0),
            Flist::directory(PathBuf::from("nested"), 0, 0o755, 0, 0),
            Flist::regular(PathBuf::from("nested/c.txt"), 7, 0, 0o644, 0, 0),
        ];
        let keep = build_keep_set(dst_root, &entries);
        assert!(keep.contains(Path::new("/tmp/dst")));
        assert!(keep.contains(Path::new("/tmp/dst/a.txt")));
        assert!(keep.contains(Path::new("/tmp/dst/nested")));
        assert!(keep.contains(Path::new("/tmp/dst/nested/c.txt")));
        assert_eq!(keep.len(), 4);
    }

    /// Slice 9 — `delete_extras` removes files outside the keep set,
    /// leaves files inside it untouched, and returns the right count.
    #[tokio::test]
    async fn delete_extras_removes_files_not_in_flist_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dst_root = dir.path();
        // Plant: keeper.txt + extra.txt + nested/keeper.txt + nested/extra.txt.
        fs::write(dst_root.join("keeper.txt"), b"k")
            .await
            .expect("k");
        fs::write(dst_root.join("extra.txt"), b"x")
            .await
            .expect("x");
        fs::create_dir(dst_root.join("nested"))
            .await
            .expect("mkdir");
        fs::write(dst_root.join("nested/keeper.txt"), b"k2")
            .await
            .expect("k2");
        fs::write(dst_root.join("nested/extra.txt"), b"x2")
            .await
            .expect("x2");
        let entries = vec![
            Flist::directory(PathBuf::from("."), 0, 0o755, 0, 0),
            Flist::regular(PathBuf::from("keeper.txt"), 1, 0, 0o644, 0, 0),
            Flist::directory(PathBuf::from("nested"), 0, 0o755, 0, 0),
            Flist::regular(PathBuf::from("nested/keeper.txt"), 2, 0, 0o644, 0, 0),
        ];
        let removed = delete_extras(dst_root, &entries).await.expect("delete");
        assert_eq!(removed, 2);
        assert!(dst_root.join("keeper.txt").exists());
        assert!(!dst_root.join("extra.txt").exists());
        assert!(dst_root.join("nested/keeper.txt").exists());
        assert!(!dst_root.join("nested/extra.txt").exists());
    }

    /// Slice 9 — `delete_extras` removes empty directories that are not
    /// in the flist set, after dropping their contents in the file
    /// removal pass.
    #[tokio::test]
    async fn delete_extras_removes_empty_dirs_outside_flist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dst_root = dir.path();
        fs::write(dst_root.join("keeper.txt"), b"k")
            .await
            .expect("k");
        fs::create_dir(dst_root.join("orphan_dir"))
            .await
            .expect("mkdir orphan");
        fs::write(dst_root.join("orphan_dir/inside.txt"), b"i")
            .await
            .expect("i");
        let entries = vec![
            Flist::directory(PathBuf::from("."), 0, 0o755, 0, 0),
            Flist::regular(PathBuf::from("keeper.txt"), 1, 0, 0o644, 0, 0),
        ];
        let removed = delete_extras(dst_root, &entries).await.expect("delete");
        // 1 file (orphan_dir/inside.txt) + 1 dir (orphan_dir) removed.
        assert_eq!(removed, 2);
        assert!(dst_root.join("keeper.txt").exists());
        assert!(!dst_root.join("orphan_dir").exists());
    }

    /// Slice 9 — symlink decode + create round-trip. Confirms that an
    /// `S_IFLNK` flist entry materialises as a real symlink under the
    /// destination root when `apply.preserve_links` is set.
    #[cfg(unix)]
    #[tokio::test]
    async fn apply_post_phase_attrs_creates_symlinks() {
        use super::ZeroStats;
        use super::apply_post_phase_attrs;

        let dir = tempfile::tempdir().expect("tempdir");
        let dst_root = dir.path();
        let entries = vec![
            Flist::directory(PathBuf::from("."), 0, 0o755, 0, 0),
            Flist::symlink(
                PathBuf::from("link.txt"),
                PathBuf::from("../target.txt"),
                0,
                0o777,
                0,
                0,
            ),
        ];
        let apply = ReceiverApplyOpts {
            preserve_perms: false,
            preserve_mtime: false,
            preserve_links: true,
            delete: false,
            partial: false,
            sparse: false,
        };
        let mut stats = ZeroStats::new(&entries);
        apply_post_phase_attrs(dst_root, &entries, apply, &mut stats).await;
        let meta = fs::symlink_metadata(dst_root.join("link.txt"))
            .await
            .expect("lstat");
        assert!(meta.file_type().is_symlink());
        let target = fs::read_link(dst_root.join("link.txt"))
            .await
            .expect("readlink");
        assert_eq!(target, PathBuf::from("../target.txt"));
    }

    /// Slice 10 — `write_literal_sparse` writes non-zero runs verbatim.
    /// A buffer of pure non-zeros must round-trip byte-for-byte.
    #[tokio::test]
    async fn write_literal_sparse_round_trips_nonzero_payload() {
        use tokio::io::AsyncWriteExt;

        use super::write_literal_sparse;

        let dir = tempfile::tempdir().expect("tempdir");
        let out_path = dir.path().join("out.bin");
        let mut out = fs::File::create(&out_path).await.expect("create");
        let payload = vec![0xab_u8; 1024];
        write_literal_sparse(&mut out, &payload)
            .await
            .expect("write");
        out.flush().await.expect("flush");
        drop(out);

        let got = fs::read(&out_path).await.expect("read");
        assert_eq!(got, payload);
    }

    /// Slice 10 — `write_literal_sparse` materialises a long zero-run as
    /// a filesystem hole (`seek` past the run instead of writing zeros).
    /// We can't directly assert "filesystem hole" portably, but the
    /// post-truncate file length should be zero (no writes happened) and
    /// reading the file back yields zeros for the seek-past region.
    #[tokio::test]
    async fn write_literal_sparse_seeks_past_long_zero_run() {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};

        use super::{SPARSE_RUN_THRESHOLD, write_literal_sparse};

        let dir = tempfile::tempdir().expect("tempdir");
        let out_path = dir.path().join("hole.bin");
        let mut out = fs::File::create(&out_path).await.expect("create");
        // Threshold-sized zero run + one trailing non-zero byte.
        let mut payload = vec![0_u8; SPARSE_RUN_THRESHOLD];
        payload.push(0x42);
        write_literal_sparse(&mut out, &payload)
            .await
            .expect("write");
        out.flush().await.expect("flush");

        // After the call, the file cursor must be past the zero run
        // (seek-advanced) plus the trailing byte that we wrote
        // verbatim.
        let pos = out.stream_position().await.expect("stream_position");
        let expected_pos = u64::try_from(SPARSE_RUN_THRESHOLD)
            .expect("threshold fits in u64")
            .saturating_add(1);
        assert_eq!(pos, expected_pos);
        // Truncate to expected length so the read-back includes the
        // trailing zeros. (`seek` does not extend the file on its own —
        // the writer's caller is responsible for `set_len`).
        out.set_len(expected_pos).await.expect("set_len");
        drop(out);

        let got = fs::read(&out_path).await.expect("read");
        assert_eq!(got.len(), payload.len());
        // Read-back of the seek-past region must yield zeros (filesystem
        // materialises holes as zeros on read).
        assert!(got[..SPARSE_RUN_THRESHOLD].iter().all(|&b| b == 0));
        assert_eq!(got[SPARSE_RUN_THRESHOLD], 0x42);
    }

    /// Slice 10 — sub-threshold zero runs are written verbatim, not
    /// seeked past. The output file must contain the literal zeros so a
    /// non-sparse fs (e.g. fat32 mount) sees the expected bytes.
    #[tokio::test]
    async fn write_literal_sparse_writes_short_zero_run_verbatim() {
        use tokio::io::AsyncWriteExt;

        use super::write_literal_sparse;

        let dir = tempfile::tempdir().expect("tempdir");
        let out_path = dir.path().join("short.bin");
        let mut out = fs::File::create(&out_path).await.expect("create");
        // Short zero run (well under SPARSE_RUN_THRESHOLD).
        let payload = vec![0_u8; 64];
        write_literal_sparse(&mut out, &payload)
            .await
            .expect("write");
        out.flush().await.expect("flush");
        drop(out);

        let got = fs::read(&out_path).await.expect("read");
        // All 64 bytes were written verbatim.
        assert_eq!(got, payload);
    }

    /// Slice 10 — `cleanup_failed_tempfile` preserves the tempfile when
    /// `partial=true` and unlinks it when `partial=false`. The full
    /// `--partial` contract is "deterministic name + skip-unlink-on-error";
    /// this test covers the unlink half.
    #[tokio::test]
    async fn cleanup_failed_tempfile_unlinks_when_not_partial() {
        use super::cleanup_failed_tempfile;

        let dir = tempfile::tempdir().expect("tempdir");
        let tmp_path = dir.path().join(".out.uuid.tmp");
        fs::write(&tmp_path, b"partial-bytes").await.expect("write");
        cleanup_failed_tempfile(&tmp_path, false).await;
        assert!(!tmp_path.exists());
    }

    #[tokio::test]
    async fn cleanup_failed_tempfile_preserves_when_partial() {
        use super::cleanup_failed_tempfile;

        let dir = tempfile::tempdir().expect("tempdir");
        let tmp_path = dir.path().join(".out.rsync-partial");
        fs::write(&tmp_path, b"partial-bytes").await.expect("write");
        cleanup_failed_tempfile(&tmp_path, true).await;
        assert!(tmp_path.exists());
        // And the partial bytes are still readable.
        let got = fs::read(&tmp_path).await.expect("read");
        assert_eq!(got, b"partial-bytes");
    }

    /// Slice 10 — sparse mode round-trips a zero-only payload through
    /// `consume_token_stream` and yields a file whose declared size is
    /// `entry.size`. Without `set_len` after a trailing seek the file
    /// would be zero bytes (no writes happened); the
    /// [`finalize_sparse_length`] helper extends it.
    #[tokio::test]
    async fn finalize_sparse_length_sets_expected_size() {
        use super::{SPARSE_RUN_THRESHOLD, finalize_sparse_length};

        let dir = tempfile::tempdir().expect("tempdir");
        let out_path = dir.path().join("hole.bin");
        let mut out = fs::File::create(&out_path).await.expect("create");
        let entry = Flist::regular(
            PathBuf::from("hole.bin"),
            i64::try_from(SPARSE_RUN_THRESHOLD).expect("fits"),
            0,
            0o644,
            0,
            0,
        );
        finalize_sparse_length(&mut out, &out_path, &entry)
            .await
            .expect("set_len");
        drop(out);
        let meta = fs::metadata(&out_path).await.expect("stat");
        assert_eq!(
            meta.len(),
            u64::try_from(SPARSE_RUN_THRESHOLD).expect("fits")
        );
    }
}
