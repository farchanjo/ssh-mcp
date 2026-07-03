// SPDX-License-Identifier: ISC
//! Wire-compat rsync transport — openrsync-faithful port (ADR 0011 phase 8).
//!
//! Drives a remote `rsync --server` process over an existing russh
//! exec channel and speaks rsync wire protocol v32 (downgrades to v31). The handshake +
//! mplex framing layers are direct ports of OpenBSD's `openrsync`
//! (ISC license — see [`LICENSES/openrsync-ISC.txt`]).
//!
//! # Slice 3 status (v7.0.0-alpha.X)
//!
//! Cumulative landed scope — every entry is verified end-to-end
//! against `rsync 3.2.7 --server` on the project's Linux VM:
//!
//! - [`session::handshake`] — port of `client.c::rsync_client` (lines
//!   51..76) plus `extern.h::struct sess`.
//! - [`io::MplexReader`] / [`io::MplexWriter`] — ports of
//!   `io.c::io_read_*` / `io_write_*` / `io_read_flush`. Lock-free:
//!   each owns its `AsyncRead` / `AsyncWrite` half exclusively.
//! - [`flist::send_flist`] / [`flist::recv_flist`] — port of
//!   `flist.c::flist_send` (lines 264..428) and `flist.c::flist_recv`
//!   (lines 597..795).
//! - [`blocks::read_blockset`] / [`blocks::write_blockset_header`] —
//!   port of `blocks.c::blk_recv` (lines 349..442) and the payload
//!   tail of `blocks.c::blk_recv_ack` (lines 326..343).
//! - [`hash::hash_fast`] / [`hash::hash_slow_into`] / [`hash::FileHasher`] —
//!   port of `hash.c` (Tridgell rolling 32-bit + per-file MD4 with
//!   prepended seed). MD4 is delegated to the `md4` crate from the
//!   `RustCrypto` family.
//! - [`tokens::emit_whole_file_tokens`] — sender token stream emit for
//!   the whole-file path (`BLKSTAT_DATA` + `BLKSTAT_TOK` + `BLKSTAT_HASH`
//!   arms of `sender.c::send_up_fsm`).
//! - [`tokens::emit_token_stream`] — slice-6 dispatcher that routes
//!   on `blockset.is_whole_file()` to either the whole-file path or
//!   the block-match path.
//! - [`match_path::emit_block_match_tokens`] — slice-6 block-match
//!   path. Adler32 rolling-hash hashtable + MD4-with-seed strong
//!   verification + literal/match token emit. Direct port of
//!   `blocks.c::blk_match` driven by `blocks.c::blk_find` + the
//!   hashtable helpers `blkhash_alloc` / `blkhash_set`.
//! - [`sender::drive_sender`] — port of `sender.c::rsync_sender`. Drives
//!   the per-file request loop, echoes blockset headers back, emits
//!   token streams, terminates with the doubled `-1` sentinel.
//! - [`receiver::drive_receiver`] — port of `receiver.c::rsync_receiver`
//!   followed by `downloader.c::rsync_downloader`. Slice 7 wires the
//!   whole-file pull path through `drive_post_handshake_pull`. The
//!   receiver emits `null_sum` blocksets, drains literal-only token
//!   streams into per-file tempfiles, verifies the MD5 (proto >= 30)
//!   or MD4-with-seed (proto < 30) trailer, and atomically renames
//!   into the destination tree.
//!
//! # Subsequent slices
//!
//! - **Slice 8** — incremental pull (local block-signature emit + match
//!   token consume on the receiver side); `--delete`; attrs apply
//!   (mtime, mode, uid, gid); hardlinks; sparse; `--partial`;
//!   `--checksum` (full-file MD5 pre-check).
//!
//! # Lock-free contract (highest-priority invariant)
//!
//! Every file in this module tree:
//!
//! - Owns its `AsyncRead` / `AsyncWrite` half exclusively from a
//!   single `tokio::spawn`-ed task. Reader and writer halves of the
//!   russh channel never share state.
//! - Carries no `Mutex<T>` field on a hot path. The only `tokio::sync::
//!   Mutex` here wraps a per-session `mpsc::Receiver` slot exactly the
//!   way [`crate::adapters::rsync::sftp`] does — per-lane, per-session,
//!   never held across `.await` of another resource.
//! - Threads session state (`WireSession`) by value or `&mut`. Never
//!   `Arc<Mutex<WireSession>>`.
//! - openrsync's `lowbuffer` / output queue does NOT port to a Rust
//!   struct. If multi-task aggregation is ever required, the writer
//!   task receives bytes via `tokio::sync::mpsc::channel(N)`.

pub mod blocks;
pub mod flist;
pub mod hash;
pub mod io;
#[path = "match_.rs"]
pub mod match_path;
pub mod ndx;
pub mod receiver;
pub mod sender;
pub mod session;
pub mod tokens;

use std::fmt;
use std::io::Cursor;
use std::mem;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use russh::ChannelMsg;
use russh::client::{self, Msg};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::JoinHandle;
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::adapters::rsync::sftp::walker::build_globset;
use crate::adapters::rsync::types::{RsyncProgressEvent, RsyncTransportKind};
use crate::adapters::rsync::wire::flist::{
    Flist, FlistFilters, FlistSendOpts, gen_flist_local_with_filters, is_reg, send_flist,
};
use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
use crate::adapters::rsync::wire::receiver::{ReceiverApplyOpts, drive_receiver_with_opts};
use crate::adapters::rsync::wire::sender::drive_sender as drive_sender_state_machine;
use crate::adapters::rsync::wire::session::{RSYNC_PROTOCOL, RSYNC_PROTOCOL_MIN, WireSession};
use crate::adapters::sftp::russh_sftp_adapter::SshHandleRegistry;
use crate::adapters::ssh::internal::session::SshClientHandler;
use crate::domain::error::DomainError;
use crate::domain::rsync::RsyncStats;
use crate::domain::rsync_ids::RsyncId;
use crate::ports::rsync_transport::{
    RsyncDirection, RsyncStartOutcome, RsyncStartRequest, RsyncTransportPort,
};

/// Default lane capacity for the per-session progress mpsc.
const DEFAULT_LANE_CAPACITY: usize = 256;

/// Read-side prefetch capacity for the mplex reader's `BufReader`
/// wrapper. Perf-only: the russh channel reader otherwise takes bare
/// 1..4-byte `read_exact` calls per varint/header decode (see
/// [`io::MplexReader`]); batching the underlying reads into 64 KiB
/// chunks cuts channel-read syscall/poll overhead without changing a
/// single decoded byte — the decoder still consumes the exact same
/// byte stream, just from a buffered front.
const WIRE_READ_BUFFER_CAPACITY: usize = 65536;

/// Stub-transport detail for the un-registry-wired path. The transport
/// only matters when a russh channel registry is plugged in via
/// [`WireRsyncTransport::with_registry`]. Slice 3 has the live wire
/// transport but the stub path is still surfaced when the composition
/// root cannot wire a registry (e.g. unit tests / fixtures).
const STUB_DETAIL: &str = "Wire transport (rsync v32 wire-compat, downgrades to v31) is being implemented; pass transport=Sftp for the SFTP fallback or wait for the next slice";

/// Hard deadline on the per-session task. Slice 3 transfers a single
/// small file end-to-end; the cap stays generous so a hung server
/// never wedges the lane consumer.
const WIRE_SESSION_DEADLINE: Duration = Duration::from_mins(2);

/// One in-flight wire-rsync session — the receive half of the
/// progress lane plus the cancel + join handles for its pump task.
#[derive(Debug)]
struct LaneState {
    rx: AsyncMutex<Receiver<RsyncProgressEvent>>,
    cancel: CancellationToken,
    join: AsyncMutex<Option<JoinHandle<()>>>,
}

/// Wire-compat rsync transport.
///
/// See module-level docs for the lock-free invariants and slice-1
/// scope. The struct itself is small — every running session owns its
/// own `tokio::spawn`-ed task, mpsc lane, and cancel token, all
/// disposed of through [`Self::close`] or `drop`.
pub struct WireRsyncTransport {
    registry: Option<SshHandleRegistry>,
    lane_capacity: usize,
    lanes: Arc<DashMap<RsyncId, Arc<LaneState>>>,
}

impl fmt::Debug for WireRsyncTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WireRsyncTransport")
            .field("has_registry", &self.registry.is_some())
            .field("lane_capacity", &self.lane_capacity)
            .field("lanes", &self.lanes.len())
            .finish_non_exhaustive()
    }
}

impl WireRsyncTransport {
    /// Build a stub adapter — every `start_session` call returns the
    /// "being implemented" wire error. Used by the production wiring
    /// in [`crate::composition::prod`] before the SFTP fallback is
    /// also constructed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            registry: None,
            lane_capacity: DEFAULT_LANE_CAPACITY,
            lanes: Arc::new(DashMap::new()),
        }
    }

    /// Build a fully-wired adapter that drives `rsync --server` over
    /// the given russh handle registry.
    #[must_use]
    pub fn with_registry(registry: SshHandleRegistry) -> Self {
        Self {
            registry: Some(registry),
            lane_capacity: DEFAULT_LANE_CAPACITY,
            lanes: Arc::new(DashMap::new()),
        }
    }

    /// Override the per-session lane capacity. Defaults to
    /// [`DEFAULT_LANE_CAPACITY`].
    #[must_use]
    pub const fn with_lane_capacity(mut self, capacity: usize) -> Self {
        self.lane_capacity = if capacity == 0 { 1 } else { capacity };
        self
    }

    /// Mint a fresh per-session [`LaneState`] backed by a `tokio::spawn`
    /// driving [`run_wire_session`] to completion. Pulled out of
    /// [`Self::start_session`] so that fn stays under the project's
    /// 30-line cognitive-complexity threshold.
    fn spawn_session_task(
        &self,
        handle: Arc<client::Handle<SshClientHandler>>,
        request: RsyncStartRequest,
    ) -> Arc<LaneState> {
        let (tx, rx) = mpsc::channel(self.lane_capacity);
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let join = tokio::spawn(async move {
            run_wire_session(handle, request, tx, cancel_for_task).await;
        });
        Arc::new(LaneState {
            rx: AsyncMutex::new(rx),
            cancel,
            join: AsyncMutex::new(Some(join)),
        })
    }
}

impl Default for WireRsyncTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl RsyncTransportPort for WireRsyncTransport {
    async fn start_session(
        &self,
        request: RsyncStartRequest,
    ) -> Result<RsyncStartOutcome, DomainError> {
        let Some(registry) = self.registry.clone() else {
            return Err(DomainError::RsyncProtocolError(STUB_DETAIL.to_string()));
        };
        let handle = registry
            .get(&request.session_id)
            .ok_or_else(|| DomainError::SessionNotFound(request.session_id.clone()))?;
        let rsync_id = RsyncId::new(format!("rs-{}", uuid::Uuid::now_v7().simple()));
        let lane = self.spawn_session_task(handle, request);
        self.lanes.insert(rsync_id.clone(), lane);
        Ok(RsyncStartOutcome {
            rsync_id,
            wire_transport: true,
        })
    }

    async fn recv_event(
        &self,
        rsync_id: &RsyncId,
    ) -> Result<Option<RsyncProgressEvent>, DomainError> {
        let Some(lane) = self.lanes.get(rsync_id).map(|kv| Arc::clone(kv.value())) else {
            return Ok(None);
        };
        let mut rx = lane.rx.lock().await;
        Ok(rx.recv().await)
    }

    async fn close(&self, rsync_id: &RsyncId) -> Result<(), DomainError> {
        let Some((_, lane)) = self.lanes.remove(rsync_id) else {
            return Ok(());
        };
        lane.cancel.cancel();
        let handle = {
            let mut slot = lane.join.lock().await;
            slot.take()
        };
        if let Some(handle) = handle {
            handle.abort();
            let _ = handle.await;
        }
        Ok(())
    }
}

/// Drive a single wire-rsync session to completion.
///
/// Slice-9 driver — opens the rsync `--server` exec channel, runs the
/// handshake, walks the local source tree (for push) or accepts the
/// remote flist (for pull), and drives the sender / receiver state
/// machine. The slice-9 additions are:
///
/// - the rsync server cmdline now reflects the `delete` flag plus the
///   `-pt -o -g -l` short-flag suffix derived from
///   [`RsyncStartRequest::preserve`];
/// - the local walker preserves real perms / mtime / link targets when
///   the matching preserve flag is set;
/// - the receiver applies mode + mtime + symlinks after rename, and
///   post-walks the destination tree for `--delete`.
async fn run_wire_session(
    handle: Arc<client::Handle<SshClientHandler>>,
    request: RsyncStartRequest,
    tx: Sender<RsyncProgressEvent>,
    cancel: CancellationToken,
) {
    if cancel.is_cancelled() {
        return;
    }
    let outcome = time::timeout(
        WIRE_SESSION_DEADLINE,
        drive_session(&handle, &request, &tx, &cancel),
    )
    .await
    .unwrap_or_else(|_| {
        Err(DomainError::Timeout(format!(
            "wire-rsync session exceeded {WIRE_SESSION_DEADLINE:?} deadline"
        )))
    });
    surface_terminal_frame(&tx, outcome).await;
}

/// Top-level wire session driver. Branches on [`RsyncDirection`]:
///
/// - **Push** — `rsync --server` exec command, walk local source tree,
///   send flist, drive sender state machine.
/// - **Pull** — `rsync --server --sender` exec command, receive flist
///   from the remote sender, drive receiver state machine into the
///   local destination directory.
///
/// Both paths share the handshake + `io_error` envelope; only the
/// command-line shape, the flist source / sink, and the post-handshake
/// state machine differ.
async fn drive_session(
    handle: &Arc<client::Handle<SshClientHandler>>,
    request: &RsyncStartRequest,
    tx: &Sender<RsyncProgressEvent>,
    cancel: &CancellationToken,
) -> Result<RsyncStats, DomainError> {
    match request.direction {
        RsyncDirection::Push => drive_push_session(handle, request, tx, cancel).await,
        RsyncDirection::Pull => drive_pull_session(handle, request, tx, cancel).await,
    }
}

/// Push direction — local tree → remote tree. Drives the sender state
/// machine.
async fn drive_push_session(
    handle: &Arc<client::Handle<SshClientHandler>>,
    request: &RsyncStartRequest,
    tx: &Sender<RsyncProgressEvent>,
    cancel: &CancellationToken,
) -> Result<RsyncStats, DomainError> {
    let mut channel = open_rsync_channel(handle, strip_host_prefix(&request.dst), request).await?;
    let (mut sess, leftover) = run_handshake_via_msg(&mut channel, cancel).await?;
    let entries = walk_local_flist_for_push(request).await?;
    emit_session_started(tx, &entries).await;
    drive_post_handshake_push(
        channel,
        &mut sess,
        leftover,
        &request.src,
        &entries,
        request,
        tx,
        cancel,
    )
    .await
}

/// Bug-B fix — compile per-call exclude / include patterns into
/// globsets and walk the local source tree, dropping entries that match
/// the exclude set unless the include set rescues them. The wire
/// transport is the sender for push; filtering at flist generation is
/// equivalent to the upstream `--exclude` / `--include` cmdline emit
/// and avoids re-engineering the filter-frame writer (see
/// [`build_rsync_server_cmdline`] for the rationale).
async fn walk_local_flist_for_push(request: &RsyncStartRequest) -> Result<Vec<Flist>, DomainError> {
    let excludes = build_globset(&request.exclude)?;
    let includes = build_globset(&request.include)?;
    let filters = FlistFilters {
        excludes: &excludes,
        includes: &includes,
    };
    let filters_arg = if request.exclude.is_empty() && request.include.is_empty() {
        None
    } else {
        Some(&filters)
    };
    gen_flist_local_with_filters(Path::new(&request.src), request.preserve, filters_arg).await
}

/// Pull direction — remote tree → local tree. Drives the receiver
/// state machine.
async fn drive_pull_session(
    handle: &Arc<client::Handle<SshClientHandler>>,
    request: &RsyncStartRequest,
    tx: &Sender<RsyncProgressEvent>,
    cancel: &CancellationToken,
) -> Result<RsyncStats, DomainError> {
    let mut channel = open_rsync_channel(handle, strip_host_prefix(&request.src), request).await?;
    let (mut sess, leftover) = run_handshake_via_msg(&mut channel, cancel).await?;
    drive_post_handshake_pull(channel, &mut sess, leftover, request, tx, cancel).await
}

/// Push the `SessionStarted` beacon onto the lane. Computes
/// `files_planned` and `bytes_planned` from the freshly-walked flist.
async fn emit_session_started(tx: &Sender<RsyncProgressEvent>, entries: &[Flist]) {
    let files_planned = u64::try_from(entries.len()).unwrap_or(u64::MAX);
    let bytes_planned = entries
        .iter()
        .filter(|e| is_reg(e.mode))
        .map(|e| u64::try_from(e.size).unwrap_or(0))
        .fold(0_u64, u64::saturating_add);
    let _ = tx
        .send(RsyncProgressEvent::SessionStarted {
            transport: RsyncTransportKind::Wire,
            files_planned,
            bytes_planned,
        })
        .await;
}

/// Run the slice-9 post-handshake push pipeline:
///
/// - split the russh channel into reader + writer halves;
/// - prepend any leftover bytes from the handshake cursor in front of
///   the channel reader (otherwise the first bytes the server
///   piggybacks on the seed segment would be silently dropped);
/// - emit the empty filter terminator + flist + IO-error sentinel;
/// - drive the sender state machine.
#[expect(
    clippy::too_many_arguments,
    reason = "post-handshake push pipeline threads channel + session + leftover + src path + flist + per-call request + lane sender + cancel; collapsing into a struct just for one call is more obscuring than helpful and the threshold is bumped one slot above the project default."
)]
async fn drive_post_handshake_push(
    channel: russh::Channel<Msg>,
    sess: &mut WireSession,
    leftover: Vec<u8>,
    src: &str,
    entries: &[Flist],
    request: &RsyncStartRequest,
    tx: &Sender<RsyncProgressEvent>,
    cancel: &CancellationToken,
) -> Result<RsyncStats, DomainError> {
    let (mut read_half, write_half) = channel.split();
    let mut writer = MplexWriter::new(write_half.make_writer());
    let buffered = BufReader::with_capacity(WIRE_READ_BUFFER_CAPACITY, read_half.make_reader());
    let chained = AsyncReadExt::chain(Cursor::new(leftover), buffered);
    let mut reader = MplexReader::new(chained);
    // We are the client/sender — write raw, read framed. `mplex_writes`
    // stays false; `mplex_reads` was set during handshake.
    // At protocol >= 30 the client-sender mplex-frames its OUTPUT too
    // (per upstream rsync 3.2.7's `main.c::client_run` lines 1297..1300:
    // `if (protocol_version >= 30) io_start_multiplex_out(f_out);`). At
    // protocol < 30 the client writes raw and only the server frames.
    sess.mplex_writes = sess.negotiated >= MPLEX_FRAMING_MIN_PROTOCOL_RUNTIME;
    let send_opts = FlistSendOpts {
        preserve_uids: request.preserve.owner,
        preserve_gids: request.preserve.group,
        preserve_links: request.preserve.links,
    };
    send_filter_terminator_and_flist_ext(&mut writer, sess, entries, send_opts).await?;
    // At protocol < 30 the receiver reads a trailing io_error int32
    // after the flist (`recv_file_list` line 2728: `if
    // (protocol_version < 30) { ... read_int(f); ... }`). At protocol
    // >= 30 the io_error is encoded into the flist's end-of-list
    // sentinel (`write_shortint(XMIT_EXTENDED_FLAGS | XMIT_IO_ERROR_ENDLIST,
    // ...)`) when there is an error to report — and skipped entirely
    // for clean flists. We never have an io_error to report so we
    // simply omit the trailing int32 at proto >= 30.
    if sess.negotiated < 30 {
        write_io_error_sentinel(&mut writer, sess).await?;
    }
    drive_sender_state_machine(
        &mut reader,
        &mut writer,
        sess,
        entries,
        Path::new(src),
        tx,
        cancel,
        request.dry_run,
    )
    .await
}

/// Run the slice-7 pull post-handshake pipeline:
///
/// - split the russh channel into reader + writer halves (chaining the
///   handshake leftover bytes onto the read half so the first
///   piggy-backed payload is not dropped);
/// - send our (empty) filter rule list — at proto >= 30 the
///   `--sender` server reads a `read_filter_list(f_in)` here even when
///   `am_sender` flips back to client-side. Empty list = single int32(0)
///   terminator.
/// - read the flist + post-flist `int32(0)` IO-error sentinel from the
///   sender;
/// - emit `SessionStarted` with planned files / bytes;
/// - drive the receiver state machine to completion.
async fn drive_post_handshake_pull(
    channel: russh::Channel<Msg>,
    sess: &mut WireSession,
    leftover: Vec<u8>,
    request: &RsyncStartRequest,
    tx: &Sender<RsyncProgressEvent>,
    cancel: &CancellationToken,
) -> Result<RsyncStats, DomainError> {
    let (mut read_half, write_half) = channel.split();
    let mut writer = MplexWriter::new(write_half.make_writer());
    let buffered = BufReader::with_capacity(WIRE_READ_BUFFER_CAPACITY, read_half.make_reader());
    let chained = AsyncReadExt::chain(Cursor::new(leftover), buffered);
    let mut reader = MplexReader::new(chained);
    // At protocol >= 30 upstream rsync `compat.c::setup_protocol`
    // (line 776) unconditionally sets `need_messages_from_generator =
    // 1`, which makes `main.c::start_server` (line 1252) call
    // `io_start_multiplex_in(f_in)` on the server-sender side. Our
    // outbound is therefore mplex-framed from this point onward.
    //
    // Mirrors `main.c::client_run` lines 1297..1300 (the symmetric
    // sender-side flip). The server-sender always frames its output,
    // so `mplex_reads` stays true (set during the handshake).
    sess.mplex_writes = sess.negotiated >= MPLEX_FRAMING_MIN_PROTOCOL_RUNTIME;
    // Empty rule list — `exclude.c::send_filter_list` lines 1635..1660
    // pass `am_sender = false` here (we are the client-receiver). The
    // empty list collapses to a single int32(0) terminator.
    writer.write_int(sess, 0).await?;
    let apply = ReceiverApplyOpts {
        preserve_perms: request.preserve.perms,
        preserve_mtime: request.preserve.mtime,
        preserve_links: request.preserve.links,
        delete: request.delete,
        // Slice 10 — `--partial` is host-side filesystem behaviour
        // (deterministic tempfile name + skip-unlink-on-error). The
        // receiver-side knob is wired off the upcoming
        // `RsyncStartRequest::partial` field; until that DTO field
        // lands the wire transport defaults to `false` so v9 callers
        // remain byte-identical.
        partial: false,
        // Slice 10 — `-S` (sparse) detects long zero runs in the
        // sender's literal payload and turns them into filesystem holes
        // via `seek` instead of writing zeros. Threaded off
        // `request.preserve.sparse`, which already exists on the
        // PreserveFlags value object.
        sparse: request.preserve.sparse,
    };
    drive_receiver_with_opts(
        &mut reader,
        &mut writer,
        sess,
        Path::new(&request.dst),
        tx,
        cancel,
        apply,
    )
    .await
}

/// Send the flist using a caller-owned [`MplexWriter`].
///
/// **No filter list terminator is emitted.** Per upstream rsync 3.2.7's
/// `exclude.c::send_filter_list` lines 1644..1660, when `am_sender` is
/// true and `receiver_wants_list` is false (the latter requires
/// `--delete` or `--prune-empty-dirs`), the client passes
/// `f_out = -1` to `send_rules` and skips the trailing
/// `write_int(f_out, 0)`. We never send `--delete` or
/// `--prune-empty-dirs` in this slice, so the filter list is omitted
/// entirely. Slice-3 was sending a stray 4-byte zero filter
/// terminator that the server (correctly) interpreted as the start of
/// the next mplex header on protocol-30+ wires — which is what
/// produced the `unexpected tag -7` diagnostic against rsync 3.2.7.
///
/// `send_flist` branches on `sess.negotiated`: at protocol 28+ it
/// emits the 16-bit `XMIT_EXTENDED_FLAGS` flag short, at protocol 30+
/// it switches the length / size / mtime fields to varint30 /
/// varlong30 per upstream rsync 3.2.7. The slice-4 driver pins our
/// local protocol to 31 (see [`session::RSYNC_PROTOCOL`]) so the
/// wire negotiates to 31 against any rsync 3.x server.
async fn send_filter_terminator_and_flist_ext<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    entries: &[Flist],
    opts: FlistSendOpts,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    let negotiated = sess.negotiated;
    send_flist(writer, sess, entries, opts, negotiated).await?;
    tracing::info!(
        target: "rsync.wire",
        entries = entries.len(),
        negotiated,
        "slice9: flist sent"
    );
    Ok(())
}

/// Write the post-flist `int32(0)` IO-error sentinel. Mirrors
/// `sender.c::rsync_sender` lines 408..411 (`io_write_int(sess, fdout,
/// 0)`). The rsync server uses this as "end of flist, my IO error
/// counter is 0" — without it the generator never starts emitting
/// per-file requests.
async fn write_io_error_sentinel<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    writer.write_int(sess, 0).await
}

/// Build + send the `rsync --server` exec request.
///
/// Slice-9 cmdline shape — mirrors upstream rsync 3.2.7's
/// `options.c::server_options` flag emission order. The base short flags
/// always include `-e.LsfxC` (capability handshake bits the upstream
/// client always sends). Per-call short flags are appended in the
/// canonical rsync order: `-l` (links) → `-p` (perms) → `-t` (times) →
/// `-g` (group) → `-o` (owner). The `--delete` long flag is appended
/// last, before the recursion / source / destination arguments.
///
/// - `direction = Push`: `rsync --server <flags...> -r [--delete] . <remote>`
/// - `direction = Pull`: `rsync --server --sender <flags...> -r . <remote>/`
///
/// `<remote>` is the path the server walks: the destination directory
/// on push (server is receiver) or the source directory on pull (server
/// is sender). Pull appends a trailing `/` to mirror a stock
/// `rsync src/ dst/` invocation (sync-contents semantics).
async fn open_rsync_channel(
    handle: &Arc<client::Handle<SshClientHandler>>,
    remote_path: &str,
    request: &RsyncStartRequest,
) -> Result<russh::Channel<Msg>, DomainError> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| DomainError::Transport(format!("CHANNEL_FAILED: {e}")))?;
    let cmdline = build_rsync_server_cmdline(remote_path, request);
    tracing::info!(
        target: "rsync.wire",
        cmd = cmdline.as_str(),
        direction = ?request.direction,
        delete = request.delete,
        preserve_perms = request.preserve.perms,
        preserve_mtime = request.preserve.mtime,
        preserve_links = request.preserve.links,
        "opening rsync exec channel"
    );
    channel
        .exec(true, cmdline.as_bytes())
        .await
        .map_err(|e| DomainError::RsyncProtocolError(format!("rsync exec failed: {e}")))?;
    Ok(channel)
}

/// Strip a leading `host:` prefix from a `host:path` rsync-style spec.
/// Returns the remote-side path (i.e. the substring after the first `:`).
/// If no `:` is present, returns the input unchanged. Used by the wire
/// transport to extract the remote path before building the
/// `rsync --server` cmdline — the server runs on the remote and only
/// needs the local-to-it path.
fn strip_host_prefix(spec: &str) -> &str {
    spec.split_once(':').map_or(spec, |(_, p)| p)
}

/// Build the `rsync --server` cmdline. Pulled out of
/// [`open_rsync_channel`] so it can be unit-tested without a live ssh
/// channel.
///
/// Bug-A fix — when `request.dry_run` is set, the `n` short flag is
/// folded into the leading capability bundle (`-Ne.LsfxC` — `N` is
/// not a flag, but the upstream rsync 3.2.7 client emits `-n` adjacent
/// to the engine-version delimiter on the server cmdline; see
/// `options.c::server_options` line 2350 + verified strace output:
/// `rsync --server -vvvnlogDtpre.iLsfxCIvu`). Emitting `-n` as a
/// separate token between `--server` and the bundle works against
/// rsync 3.2.7 invoked locally but causes the v31 server to wait
/// indefinitely after the handshake when driven over an exec channel.
/// Bundling `-n` with the leading shortflags resolves the hang.
///
/// Bug-B fix — exclude / include patterns flow through to the wire
/// client's local flist walker (see [`gen_flist_local_with_filters`]),
/// not through extra cmdline flags. Reason: `rsync --server`'s filter
/// pipeline expects the rules over a delimited frame after handshake
/// (see `exclude.c::send_filter_list`), but our protocol-30+ filter
/// channel today writes a single empty terminator per the upstream
/// "client is sender, no `--delete`/`--prune-empty-dirs`" branch
/// (see [`drive_post_handshake_push`]). Filtering at flist generation
/// is equivalent for the non-delete case and avoids re-engineering the
/// filter-frame writer.
fn build_rsync_server_cmdline(remote_path: &str, request: &RsyncStartRequest) -> String {
    let short_flags = build_short_flags(request);
    let delete_flag = if request.delete { " --delete" } else { "" };
    match request.direction {
        RsyncDirection::Push => {
            format!("rsync --server{delete_flag} {short_flags} -r . {remote_path}")
        }
        RsyncDirection::Pull => {
            let with_slash = pull_path_with_trailing_slash(remote_path);
            format!("rsync --server --sender{delete_flag} {short_flags} -r . {with_slash}")
        }
    }
}

/// Assemble the short-flag bundle. Mirrors upstream rsync 3.2.7's
/// `options.c::server_options` cmdline shape (verified by strace:
/// `rsync --server -vvvnlogDtpre.iLsfxCIvu . dst/`).
///
/// Bundle layout (in order):
///
/// 1. Leading `-`.
/// 2. Bug-A fix — `n` (dry-run) prefix when `request.dry_run`. Bundling
///    `n` here instead of as a separate `-n` token mirrors the upstream
///    cmdline shape AND avoids a deadlock observed when the v31 server
///    is invoked with an isolated `-n` token over an exec channel
///    (the server hangs after the handshake).
/// 3. Capability bundle `e.LsfxC` — leading `e.` engine-version
///    delimiter, `L` = preserve-symlinks-handshake, `s` = "secluded
///    args", `f` = "filter rule", `x` = "x-attrs OFF", `C` = "cks-cap".
///    Always emitted — rsync uses these to negotiate transport
///    capabilities, semantically distinct from per-call preserve flags.
/// 4. Per-call preserve flags in canonical rsync order: `l` (links) →
///    `p` (perms) → `t` (times) → `g` (group) → `o` (owner).
fn build_short_flags(request: &RsyncStartRequest) -> String {
    let mut flags = String::from("-");
    if request.dry_run {
        flags.push('n');
    }
    flags.push_str("e.LsfxC");
    let preserve = &request.preserve;
    if preserve.links {
        flags.push('l');
    }
    if preserve.perms {
        flags.push('p');
    }
    if preserve.mtime {
        flags.push('t');
    }
    if preserve.group {
        flags.push('g');
    }
    if preserve.owner {
        flags.push('o');
    }
    flags
}

/// Append a trailing `/` to `remote_path` when it is missing — pull
/// direction always passes the source dir's *contents* to the
/// server-sender (mirrors a stock `rsync src/ dst/` invocation).
fn pull_path_with_trailing_slash(remote_path: &str) -> String {
    if remote_path.ends_with('/') {
        remote_path.to_string()
    } else {
        format!("{remote_path}/")
    }
}

/// Drive the handshake at the [`ChannelMsg`] level.
///
/// Returns the parsed [`WireSession`] plus the **post-handshake
/// leftover bytes** still sitting in the accumulator — server side
/// often piggy-backs the first inner-protocol bytes (e.g. the start
/// of a generator request) on the same TCP segment as the seed.
/// Losing them would manifest as an "early eof" on the first
/// post-handshake read.
///
/// Behaviour depends on the **negotiated** protocol:
///
/// - rver < 30: 8 bytes total (rver=4 + seed=4). No `compat_flags`
///   byte exists in pre-mplex rsync (e.g. when our lver = 27 forces
///   the negotiation down).
/// - rver >= 30: rver (4) + `compat_flags` varint (1..=5) + seed (4).
async fn run_handshake_via_msg(
    channel: &mut russh::Channel<Msg>,
    cancel: &CancellationToken,
) -> Result<(WireSession, Vec<u8>), DomainError> {
    write_lver_through_channel(channel).await?;
    let mut accum = read_at_least(channel, cancel, 4).await?;
    let mut sess = WireSession::new();
    sess.rver = parse_le_i32_at(&accum, 0)?;
    if sess.rver < RSYNC_PROTOCOL_MIN {
        return Err(DomainError::RsyncVersionTooOld(format!(
            "remote protocol {}",
            sess.rver
        )));
    }
    sess.negotiated = sess.rver.min(sess.lver);
    let mut cursor_pos = 4_usize;
    if sess.negotiated >= MPLEX_FRAMING_MIN_PROTOCOL_RUNTIME {
        cursor_pos = drain_compat_flags_from_accum(channel, cancel, &mut accum, cursor_pos).await?;
    }
    accum = read_until_total(channel, cancel, accum, cursor_pos.saturating_add(4)).await?;
    sess.seed = parse_le_i32_at(&accum, cursor_pos)?;
    cursor_pos = cursor_pos.saturating_add(4);
    // openrsync `client.c` line 76: `sess.mplex_reads = 1` is set
    // unconditionally after handshake — even at protocol 27 the
    // server emits mplex-framed inner-protocol output (the framer is
    // transparent for `MSG_DATA` payloads and routes other tags
    // through the log channel). The client (us, as sender) does NOT
    // set `mplex_writes` — `server.c` line 92 shows that's the
    // server-side responsibility.
    sess.mplex_reads = true;
    tracing::info!(
        target: "rsync.wire",
        rver = sess.rver,
        negotiated = sess.negotiated,
        seed = format!("{:#x}", u32::from_le_bytes(sess.seed.to_le_bytes())),
        "handshake: complete"
    );
    let leftover = accum.get(cursor_pos..).unwrap_or(&[]).to_vec();
    Ok((sess, leftover))
}

/// Same protocol floor for mplex framing as `MPLEX_FRAMING_MIN_PROTOCOL`
/// in `session.rs`. Re-declared here so the handshake's branching does
/// not have to import the private constant; both values are pinned at
/// 30 (rsync's own boundary for the `MSG_DATA` framing layer).
const MPLEX_FRAMING_MIN_PROTOCOL_RUNTIME: i32 = 30;

/// Decode a 4-byte little-endian i32 at offset `pos` of `buf`.
fn parse_le_i32_at(buf: &[u8], pos: usize) -> Result<i32, DomainError> {
    let slice = buf.get(pos..pos.saturating_add(4)).ok_or_else(|| {
        DomainError::RsyncProtocolError(format!(
            "handshake: short read while parsing i32 at offset {pos}"
        ))
    })?;
    let arr: [u8; 4] = slice.try_into().map_err(|err| {
        DomainError::RsyncProtocolError(format!("handshake: i32 slice convert failed: {err}"))
    })?;
    Ok(i32::from_le_bytes(arr))
}

/// Drain the `compat_flags` varint off the accumulator, pulling more
/// bytes off the channel as needed. Returns the new cursor position.
/// Mirrors `compat_flags_extra_bytes` (in the unchanged helper below).
async fn drain_compat_flags_from_accum(
    channel: &mut russh::Channel<Msg>,
    cancel: &CancellationToken,
    accum: &mut Vec<u8>,
    pos: usize,
) -> Result<usize, DomainError> {
    let next_pos = pos.saturating_add(1);
    *accum = read_until_total(channel, cancel, mem::take(accum), next_pos).await?;
    let first = *accum.get(pos).ok_or_else(|| {
        DomainError::RsyncProtocolError("handshake: failed to fetch compat_flags byte".to_string())
    })?;
    if first & 0x80 == 0 {
        return Ok(next_pos);
    }
    let extra = compat_flags_extra_bytes(first);
    let target = next_pos.saturating_add(extra);
    *accum = read_until_total(channel, cancel, mem::take(accum), target).await?;
    Ok(target)
}

/// Pull bytes off the channel until `accum.len() >= target`. Returns
/// the (possibly grown) accumulator.
async fn read_until_total(
    channel: &mut russh::Channel<Msg>,
    cancel: &CancellationToken,
    mut accum: Vec<u8>,
    target: usize,
) -> Result<Vec<u8>, DomainError> {
    while accum.len() < target {
        if cancel.is_cancelled() {
            return Err(DomainError::RsyncProtocolError(
                "cancelled during handshake".to_string(),
            ));
        }
        match channel.wait().await {
            Some(ChannelMsg::Data { data }) => accum.extend_from_slice(&data),
            Some(ChannelMsg::ExtendedData { data, .. }) => log_stderr(&data, "handshake"),
            Some(ChannelMsg::Eof | ChannelMsg::Close) | None => {
                return Err(DomainError::RsyncProtocolError(
                    "channel closed during handshake".to_string(),
                ));
            }
            Some(_) => {}
        }
    }
    Ok(accum)
}

/// Convenience for an initial read of at least `target` bytes.
async fn read_at_least(
    channel: &mut russh::Channel<Msg>,
    cancel: &CancellationToken,
    target: usize,
) -> Result<Vec<u8>, DomainError> {
    read_until_total(channel, cancel, Vec::with_capacity(target), target).await
}

/// Write our `lver` straight onto the russh channel writer. We bypass
/// the [`MplexWriter`] for this single field because the wire is still
/// in pre-mplex mode at this point and the writer half is owned by a
/// different code path.
async fn write_lver_through_channel(channel: &russh::Channel<Msg>) -> Result<(), DomainError> {
    let mut raw_writer = channel.make_writer();
    let lver_bytes = i32::to_le_bytes(RSYNC_PROTOCOL);
    raw_writer
        .write_all(&lver_bytes)
        .await
        .map_err(|e| DomainError::RsyncProtocolError(format!("lver write failed: {e}")))?;
    raw_writer
        .flush()
        .await
        .map_err(|e| DomainError::RsyncProtocolError(format!("lver flush failed: {e}")))?;
    drop(raw_writer);
    Ok(())
}

/// Map a `compat_flags` varint prefix byte to the count of extra
/// bytes that follow on the wire. Mirrors the same table used inside
/// the cursor-backed handshake (see `session::compat_flags_extra_bytes`).
const fn compat_flags_extra_bytes(prefix: u8) -> usize {
    if prefix & 0xc0 == 0x80 {
        1
    } else if prefix & 0xe0 == 0xc0 {
        2
    } else if prefix & 0xf0 == 0xe0 {
        3
    } else {
        4
    }
}

/// Emit a `tracing::warn!` for an `ExtendedData` (stderr) frame from
/// the remote rsync process. Used by both the handshake and drain
/// paths to keep their match arms terse.
fn log_stderr(data: &[u8], context: &str) {
    tracing::warn!(
        target: "rsync.wire",
        stderr = %String::from_utf8_lossy(data),
        context = context,
        "stderr while reading from rsync channel"
    );
}

/// Translate the slice-3 driver outcome into the canonical lane
/// envelope: either `SyncCompleted` with the final stats (success), or
/// `SessionFailed` followed by a synthetic `SyncCompleted` carrying
/// zero stats (error path — keeps consumers byte-identical against the
/// SFTP transport).
async fn surface_terminal_frame(
    tx: &Sender<RsyncProgressEvent>,
    outcome: Result<RsyncStats, DomainError>,
) {
    match outcome {
        Ok(stats) => {
            let _ = tx.send(RsyncProgressEvent::SyncCompleted { stats }).await;
        }
        Err(err) => {
            let _ = tx
                .send(RsyncProgressEvent::SessionFailed {
                    code: error_code(&err),
                    detail: err.to_string(),
                })
                .await;
            let _ = tx
                .send(RsyncProgressEvent::SyncCompleted {
                    stats: empty_stats(),
                })
                .await;
        }
    }
}

const fn empty_stats() -> RsyncStats {
    RsyncStats {
        files_total: 0,
        files_done: 0,
        bytes_total: 0,
        bytes_transferred: 0,
        bytes_skipped: 0,
        files_deleted: 0,
        files_failed: 0,
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "the helper is intentionally a partial lookup keyed on \
              the rsync-relevant DomainError variants; every other \
              variant collapses to INTERNAL because the wire path \
              never produces them. Mirrors the same pattern used by \
              the SFTP transport's error_code helper."
)]
fn error_code(err: &DomainError) -> String {
    let code = match err {
        DomainError::RsyncProtocolError(_) => "RSYNC_PROTOCOL_ERROR",
        DomainError::RsyncPartialTransfer(_) => "RSYNC_PARTIAL_TRANSFER",
        DomainError::RsyncNotFound(_) => "RSYNC_NOT_FOUND",
        DomainError::RsyncVersionTooOld(_) => "RSYNC_VERSION_TOO_OLD",
        DomainError::Transport(_) => "TRANSPORT_ERROR",
        DomainError::Timeout(_) => "TIMEOUT",
        DomainError::SessionNotFound(_) => "SESSION_NOT_FOUND",
        _ => "INTERNAL",
    };
    code.to_string()
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code uses unwrap/expect for brevity per project convention"
)]
mod tests {
    use super::{STUB_DETAIL, WireRsyncTransport, build_rsync_server_cmdline};
    use crate::adapters::rsync::types::PreserveFlags;
    use crate::domain::error::DomainError;
    use crate::domain::ids::SessionId;
    use crate::domain::rsync_ids::RsyncId;
    use crate::ports::rsync_transport::{RsyncDirection, RsyncStartRequest, RsyncTransportPort};

    fn start_request() -> RsyncStartRequest {
        RsyncStartRequest {
            session_id: SessionId::new("s-1".to_string()),
            src: "/tmp/src".to_string(),
            dst: "/tmp/dst".to_string(),
            direction: super::RsyncDirection::Push,
            ..RsyncStartRequest::default()
        }
    }

    #[tokio::test]
    async fn start_session_returns_being_implemented_error_when_unwired() {
        let t = WireRsyncTransport::new();
        let err = t.start_session(start_request()).await.expect_err("err");
        match err {
            DomainError::RsyncProtocolError(msg) => {
                assert!(msg.contains("Wire transport"));
                assert!(msg.contains("being implemented"));
                assert_eq!(msg, STUB_DETAIL);
            }
            other => panic!("expected RsyncProtocolError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recv_event_returns_none_for_unknown_id() {
        let t = WireRsyncTransport::new();
        let id = RsyncId::new("rs-x".to_string());
        let result = t.recv_event(&id).await.expect("recv_event");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn close_is_idempotent_ok() {
        let t = WireRsyncTransport::new();
        let id = RsyncId::new("rs-x".to_string());
        t.close(&id).await.expect("idempotent close");
    }

    #[test]
    fn cmdline_push_no_preserve_no_delete_matches_baseline() {
        let mut req = start_request();
        req.preserve = PreserveFlags::none();
        req.delete = false;
        let cmd = build_rsync_server_cmdline("/tmp/dst", &req);
        assert_eq!(cmd, "rsync --server -e.LsfxC -r . /tmp/dst");
    }

    #[test]
    fn cmdline_push_with_perms_and_times_appends_pt_flags() {
        let mut req = start_request();
        req.preserve = PreserveFlags {
            perms: true,
            mtime: true,
            owner: false,
            group: false,
            links: false,
            hardlinks: false,
            sparse: false,
            devices: false,
        };
        req.delete = false;
        let cmd = build_rsync_server_cmdline("/tmp/dst", &req);
        assert_eq!(cmd, "rsync --server -e.LsfxCpt -r . /tmp/dst");
    }

    #[test]
    fn cmdline_push_with_full_archive_minus_devices_emits_lptgo() {
        let mut req = start_request();
        req.preserve = PreserveFlags::archive();
        req.delete = false;
        let cmd = build_rsync_server_cmdline("/tmp/dst", &req);
        assert_eq!(cmd, "rsync --server -e.LsfxClptgo -r . /tmp/dst");
    }

    #[test]
    fn cmdline_push_with_delete_inserts_long_flag() {
        let mut req = start_request();
        req.preserve = PreserveFlags::none();
        req.delete = true;
        let cmd = build_rsync_server_cmdline("/tmp/dst", &req);
        assert_eq!(cmd, "rsync --server --delete -e.LsfxC -r . /tmp/dst");
    }

    #[test]
    fn cmdline_pull_appends_trailing_slash_when_missing() {
        let mut req = start_request();
        req.direction = RsyncDirection::Pull;
        req.preserve = PreserveFlags::none();
        let cmd = build_rsync_server_cmdline("/tmp/src", &req);
        assert_eq!(cmd, "rsync --server --sender -e.LsfxC -r . /tmp/src/");
    }

    #[test]
    fn cmdline_pull_with_delete_and_perms_emits_combined_flags() {
        let mut req = start_request();
        req.direction = RsyncDirection::Pull;
        req.preserve = PreserveFlags {
            perms: true,
            mtime: true,
            owner: false,
            group: false,
            links: true,
            hardlinks: false,
            sparse: false,
            devices: false,
        };
        req.delete = true;
        let cmd = build_rsync_server_cmdline("/tmp/src/", &req);
        assert_eq!(
            cmd,
            "rsync --server --sender --delete -e.LsfxClpt -r . /tmp/src/"
        );
    }

    /// Bug-A regression — Wire transport must forward `dry_run=true` by
    /// bundling `n` into the short-flag header so the remote
    /// `rsync --server` skips destructive ops. Mirrors upstream rsync
    /// 3.2.7's cmdline shape (`-vvvnlogDtpre.iLsfxCIvu`); emitting `-n`
    /// as a separate token deadlocks the v31 server over an exec channel.
    #[test]
    fn cmdline_push_with_dry_run_bundles_n_into_short_flags() {
        let mut req = start_request();
        req.preserve = PreserveFlags::none();
        req.delete = false;
        req.dry_run = true;
        let cmd = build_rsync_server_cmdline("/tmp/dst", &req);
        assert_eq!(cmd, "rsync --server -ne.LsfxC -r . /tmp/dst");
    }

    /// Bug-A regression on pull — the receiver-side server also honours
    /// the bundled `n` flag.
    #[test]
    fn cmdline_pull_with_dry_run_bundles_n_into_short_flags() {
        let mut req = start_request();
        req.direction = RsyncDirection::Pull;
        req.preserve = PreserveFlags::none();
        req.dry_run = true;
        let cmd = build_rsync_server_cmdline("/tmp/src", &req);
        assert_eq!(cmd, "rsync --server --sender -ne.LsfxC -r . /tmp/src/");
    }

    /// Bug-A regression — `dry_run` and `delete` co-exist; `n` lands in
    /// the bundle, `--delete` lands as a long flag.
    #[test]
    fn cmdline_push_with_dry_run_and_delete_emits_both_flags() {
        let mut req = start_request();
        req.preserve = PreserveFlags::none();
        req.delete = true;
        req.dry_run = true;
        let cmd = build_rsync_server_cmdline("/tmp/dst", &req);
        assert_eq!(cmd, "rsync --server --delete -ne.LsfxC -r . /tmp/dst");
    }
}
