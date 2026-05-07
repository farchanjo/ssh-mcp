//! SFTP fallback rsync transport (ADR 0011 Tier 2).
//!
//! Drives a recursive sync entirely through SFTP `readdir` + `stat` +
//! `read` + `write` + `setstat` — no remote helper required, works on
//! any host that exposes a working SFTP subsystem (which is every host
//! that already passes `ssh_upload`). Slower than the Wire transport
//! because every block crosses the wire (no rolling-checksum delta
//! sync), but universal.
//!
//! ## Pipeline (v7.0.0-alpha.3 first SFTP slice)
//!
//! 1. [`walker::SftpWalker`] enumerates the source + destination trees
//!    via [`crate::ports::rsync_sftp_fs::RsyncSftpFsPort::readdir`].
//! 2. [`comparator::compare_trees`] derives an ordered
//!    [`comparator::SyncAction`] list (mkdir / transfer / setstat /
//!    symlink / delete / skip).
//! 3. [`executor::SftpExecutor`] applies the actions, emits
//!    [`crate::adapters::rsync::types::RsyncProgressEvent`] frames into
//!    an `mpsc::Sender`, honours bandwidth limits, dry-run, and cancel.
//!
//! The transport adapter ties the three together behind the
//! [`crate::ports::rsync_transport::RsyncTransportPort`] interface. It
//! buffers progress frames on a per-`RsyncId` lane so `recv_event`
//! drains them in order; closing a session cancels the pump task and
//! drops the lane.
//!
//! When the adapter is constructed without an [`RsyncSftpFsPort`]
//! (composition root not yet wired) every method returns the original
//! `RsyncProtocolError` "being implemented" stub so the public MCP
//! surface stays honest until the russh-sftp adapter is wired.

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fake;

pub mod bwlimit;
pub mod comparator;
pub mod executor;
pub mod probe;
pub mod walker;

use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use dashmap::DashMap;
use globset::GlobSet;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::adapters::rsync::sftp::bwlimit::TokenBucket;
use crate::adapters::rsync::sftp::comparator::{CompareOpts, Direction, SyncAction, compare_trees};
use crate::adapters::rsync::sftp::executor::ExecutorStats;
use crate::adapters::rsync::sftp::executor::SftpExecutor;
use crate::adapters::rsync::sftp::walker::{RsyncEntry, SftpWalker};
use crate::adapters::rsync::types::{
    FileKind, PreserveFlags, RsyncProgressEvent, RsyncTransportKind,
};
use crate::domain::error::DomainError;
use crate::domain::ids::SessionId;
use crate::domain::rsync::RsyncStats;
use crate::domain::rsync_ids::RsyncId;
use crate::ports::rsync_sftp_fs::{RemoteDirEntry, RemoteMetadata, RsyncSftpFsPort};
use crate::ports::rsync_transport::{RsyncStartOutcome, RsyncStartRequest, RsyncTransportPort};

const STUB_DETAIL: &str = "SFTP transport is being implemented; install rsync >= 3.2.0 on the remote and pass transport=Wire, or wait for the next slice";

const DEFAULT_LANE_CAPACITY: usize = 256;
const DEFAULT_FILE_LIST_LIMIT: u64 = 1_000_000;

/// Per-session sync configuration carried by the transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SftpRsyncOpts {
    /// `--delete` — remove destination entries missing from source.
    pub delete: bool,
    /// `--dry-run` — every destructive op short-circuits to
    /// `FileSkipped { reason: DryRun }`.
    pub dry_run: bool,
    /// `--bwlimit` — bytes per second; `None` disables the limit.
    pub bwlimit_bps: Option<u64>,
    /// `--exclude` glob patterns (gitignore-style).
    pub excludes: Vec<String>,
    /// `--include` glob patterns; non-empty includes override matching
    /// excludes.
    pub includes: Vec<String>,
    /// File-list cap (`SSH_RSYNC_FILE_LIST_LIMIT`).
    pub file_list_limit: u64,
    /// Sync direction.
    pub direction: Direction,
    /// Attribute-preservation mask.
    pub preserve: PreserveFlags,
    /// `--checksum` flag — force full transfer even when size+mtime
    /// match. The SFTP transport does not have a delta path, so the
    /// flag merely suppresses the `Skip` arm.
    pub force_transfer: bool,
}

impl Default for SftpRsyncOpts {
    fn default() -> Self {
        Self {
            delete: false,
            dry_run: false,
            bwlimit_bps: None,
            excludes: Vec::new(),
            includes: Vec::new(),
            file_list_limit: DEFAULT_FILE_LIST_LIMIT,
            direction: Direction::Push,
            preserve: PreserveFlags::default(),
            force_transfer: false,
        }
    }
}

/// In-flight per-session state — the receiving end of the progress
/// lane, the cancel token, and the join handle for the pump task.
#[derive(Debug)]
struct LaneState {
    rx: AsyncMutex<Receiver<RsyncProgressEvent>>,
    cancel: CancellationToken,
    join: AsyncMutex<Option<JoinHandle<()>>>,
}

/// SFTP `RsyncTransportPort` adapter.
///
/// Generic over the `F: RsyncSftpFsPort` so the production wiring
/// injects a russh-sftp adapter while tests inject
/// [`fake::FakeRsyncSftpFs`]. When the adapter is built via
/// [`Self::without_fs`] every transport method falls back to the
/// honest "being implemented" stub.
pub struct SftpRsyncTransport<F = NoopFs>
where
    F: RsyncSftpFsPort + 'static,
{
    fs: Option<Arc<F>>,
    opts: SftpRsyncOpts,
    lane_capacity: usize,
    /// Lock-free `DashMap<RsyncId, Arc<LaneState>>` — keyed lookups in
    /// `recv_event` / `close` never race against the spawn task.
    lanes: Arc<DashMap<RsyncId, Arc<LaneState>>>,
}

impl<F> fmt::Debug for SftpRsyncTransport<F>
where
    F: RsyncSftpFsPort + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SftpRsyncTransport")
            .field("has_fs", &self.fs.is_some())
            .field("lanes", &self.lanes.len())
            .finish_non_exhaustive()
    }
}

impl SftpRsyncTransport<NoopFs> {
    /// Build a stub adapter — every method returns the
    /// "being implemented" wire error. Kept so the composition root
    /// can wire the transport before the russh-sftp adapter lands in
    /// the next slice without churning the public surface.
    #[must_use]
    pub fn without_fs() -> Self {
        Self {
            fs: None,
            opts: SftpRsyncOpts::default(),
            lane_capacity: DEFAULT_LANE_CAPACITY,
            lanes: Arc::new(DashMap::new()),
        }
    }

    /// Backwards-compatible alias: the previous slice exposed
    /// `SftpRsyncTransport::new()` returning the stub. Keep that shape
    /// so the existing composition root and tests still compile.
    #[must_use]
    pub fn new() -> Self {
        Self::without_fs()
    }
}

impl Default for SftpRsyncTransport<NoopFs> {
    fn default() -> Self {
        Self::new()
    }
}

impl<F> SftpRsyncTransport<F>
where
    F: RsyncSftpFsPort + 'static,
{
    /// Build a fully-wired adapter.
    #[must_use]
    pub fn with_fs(fs: Arc<F>, opts: SftpRsyncOpts, lane_capacity: usize) -> Self {
        Self {
            fs: Some(fs),
            opts,
            lane_capacity: lane_capacity.max(1),
            lanes: Arc::new(DashMap::new()),
        }
    }

    /// Mint a fresh per-session [`LaneState`] backed by a `tokio::spawn`
    /// driving [`run_session`] to completion. Pulled out of the
    /// [`RsyncTransportPort::start_session`] body so that fn stays
    /// under the project's 30-line cognitive ceiling.
    ///
    /// Slice 9 — merges per-call `delete` + `preserve` from the
    /// [`RsyncStartRequest`] over the adapter-level `opts` baseline so
    /// that existing callers that mutate the adapter's `opts` at
    /// composition time keep their wiring unchanged for every other
    /// knob (excludes / includes / bwlimit / dry-run / file-list cap /
    /// direction / force-transfer).
    fn spawn_session_task(&self, fs: Arc<F>, request: RsyncStartRequest) -> Arc<LaneState> {
        let (tx, rx) = mpsc::channel(self.lane_capacity);
        let cancel = CancellationToken::new();
        let opts = merge_request_opts(&self.opts, &request);
        let cancel_for_task = cancel.clone();
        let RsyncStartRequest {
            session_id,
            src,
            dst,
            direction: _,
            delete: _,
            preserve: _,
            dry_run: _,
            exclude: _,
            include: _,
        } = request;
        let join = tokio::spawn(async move {
            run_session(fs, session_id, src, dst, opts, tx, cancel_for_task).await;
        });
        Arc::new(LaneState {
            rx: AsyncMutex::new(rx),
            cancel,
            join: AsyncMutex::new(Some(join)),
        })
    }
}

/// Merge per-call `RsyncStartRequest` flags over the adapter's
/// baseline [`SftpRsyncOpts`]. Per-call `dry_run` is OR'd with the
/// baseline so existing wirings that pre-set the flag keep working
/// while per-call requests can opt in dynamically. Non-empty
/// per-call exclude / include lists override the baseline.
fn merge_request_opts(base: &SftpRsyncOpts, request: &RsyncStartRequest) -> SftpRsyncOpts {
    let mut opts = base.clone();
    opts.delete = request.delete;
    opts.preserve = request.preserve;
    opts.dry_run = opts.dry_run || request.dry_run;
    if !request.exclude.is_empty() {
        opts.excludes.clone_from(&request.exclude);
    }
    if !request.include.is_empty() {
        opts.includes.clone_from(&request.include);
    }
    opts
}

impl<F> RsyncTransportPort for SftpRsyncTransport<F>
where
    F: RsyncSftpFsPort + 'static,
{
    async fn start_session(
        &self,
        request: RsyncStartRequest,
    ) -> Result<RsyncStartOutcome, DomainError> {
        let Some(fs) = self.fs.clone() else {
            return Err(DomainError::RsyncProtocolError(STUB_DETAIL.to_string()));
        };
        let rsync_id = RsyncId::new(format!("rs-{}", uuid::Uuid::now_v7().simple()));
        let lane = self.spawn_session_task(fs, request);
        self.lanes.insert(rsync_id.clone(), lane);
        Ok(RsyncStartOutcome {
            rsync_id,
            wire_transport: false,
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

async fn run_session<F>(
    fs: Arc<F>,
    session_id: SessionId,
    src_root: String,
    dst_root: String,
    opts: SftpRsyncOpts,
    tx: Sender<RsyncProgressEvent>,
    cancel: CancellationToken,
) where
    F: RsyncSftpFsPort + 'static,
{
    let Some(walked) = walk_both_trees(&fs, &session_id, &src_root, &dst_root, &opts, &tx).await
    else {
        return;
    };
    let plan = SyncPlan::new(&walked.src);
    announce_session_started(&tx, plan).await;
    let actions = compare_trees(
        &walked.src,
        &walked.dst,
        opts.direction,
        CompareOpts {
            delete: opts.delete,
            force_transfer: opts.force_transfer,
            preserve: opts.preserve,
        },
    );
    let wiring = ExecutorWiring {
        fs,
        src_root,
        dst_root,
        cancel,
        plan,
    };
    drive_executor(wiring, &session_id, &opts, &tx, &actions).await;
}

async fn announce_session_started(tx: &Sender<RsyncProgressEvent>, plan: SyncPlan) {
    let _ = tx
        .send(RsyncProgressEvent::SessionStarted {
            transport: RsyncTransportKind::Sftp,
            files_planned: plan.files_planned,
            bytes_planned: plan.bytes_planned,
        })
        .await;
}

struct WalkedTrees {
    src: Vec<RsyncEntry>,
    dst: Vec<RsyncEntry>,
}

async fn walk_both_trees<F>(
    fs: &Arc<F>,
    session_id: &SessionId,
    src_root: &str,
    dst_root: &str,
    opts: &SftpRsyncOpts,
    tx: &Sender<RsyncProgressEvent>,
) -> Option<WalkedTrees>
where
    F: RsyncSftpFsPort + 'static,
{
    let (excludes, includes) = build_filter_sets(opts, tx).await?;
    let walker = SftpWalker::new(Arc::clone(fs), excludes, includes, opts.file_list_limit);
    let src = match walker.walk(session_id, src_root).await {
        Ok(v) => v,
        Err(err) => {
            send_session_failed(tx, &err).await;
            return None;
        }
    };
    // Best-effort root-mkdir before walking the destination — when the
    // caller passes a fresh `dst` path the SFTP server has no row to
    // descend into, so the comparator would emit Mkdir actions for
    // every nested child but never for the root itself. The mkdir
    // here closes that gap; "path exists" failures are swallowed
    // because they are the happy path on a re-run.
    if !opts.dry_run {
        let _ = fs.mkdir(session_id, dst_root, 0o755).await;
    }
    let dst = walker.walk(session_id, dst_root).await.unwrap_or_default();
    Some(WalkedTrees { src, dst })
}

#[derive(Debug, Clone, Copy)]
struct SyncPlan {
    files_planned: u64,
    bytes_planned: u64,
}

impl SyncPlan {
    fn new(src_entries: &[RsyncEntry]) -> Self {
        let bytes_planned: u64 = src_entries
            .iter()
            .filter(|e| matches!(e.kind, FileKind::File))
            .map(|e| e.size)
            .sum();
        Self {
            files_planned: u64::try_from(src_entries.len()).unwrap_or(u64::MAX),
            bytes_planned,
        }
    }
}

async fn build_filter_sets(
    opts: &SftpRsyncOpts,
    tx: &Sender<RsyncProgressEvent>,
) -> Option<(GlobSet, GlobSet)> {
    let excludes = match walker::build_globset(&opts.excludes) {
        Ok(set) => set,
        Err(err) => {
            let _ = tx
                .send(RsyncProgressEvent::SessionFailed {
                    code: "INVALID_GLOB".to_string(),
                    detail: err.to_string(),
                })
                .await;
            return None;
        }
    };
    let includes = match walker::build_globset(&opts.includes) {
        Ok(set) => set,
        Err(err) => {
            let _ = tx
                .send(RsyncProgressEvent::SessionFailed {
                    code: "INVALID_GLOB".to_string(),
                    detail: err.to_string(),
                })
                .await;
            return None;
        }
    };
    Some((excludes, includes))
}

struct ExecutorWiring<F> {
    fs: Arc<F>,
    src_root: String,
    dst_root: String,
    cancel: CancellationToken,
    plan: SyncPlan,
}

async fn drive_executor<F>(
    wiring: ExecutorWiring<F>,
    session_id: &SessionId,
    opts: &SftpRsyncOpts,
    tx: &Sender<RsyncProgressEvent>,
    actions: &[SyncAction],
) where
    F: RsyncSftpFsPort + 'static,
{
    let plan = wiring.plan;
    let bwlimit = opts.bwlimit_bps.map(|bps| Arc::new(TokenBucket::new(bps)));
    let executor = SftpExecutor::new(
        wiring.fs,
        bwlimit,
        opts.dry_run,
        tx.clone(),
        wiring.cancel,
        wiring.src_root,
        wiring.dst_root,
    );
    let stats = match executor.execute(session_id, actions).await {
        Ok(s) => s,
        Err(err) => {
            send_session_failed(tx, &err).await;
            return;
        }
    };
    let _ = tx
        .send(RsyncProgressEvent::SyncCompleted {
            stats: build_final_stats(plan, stats),
        })
        .await;
}

const fn build_final_stats(plan: SyncPlan, stats: ExecutorStats) -> RsyncStats {
    RsyncStats {
        files_total: plan.files_planned,
        files_done: stats.files_done.saturating_add(stats.files_skipped),
        bytes_total: plan.bytes_planned,
        bytes_transferred: stats.bytes_transferred,
        bytes_skipped: plan.bytes_planned.saturating_sub(stats.bytes_transferred),
        files_deleted: stats.files_deleted,
        files_failed: stats.files_failed,
    }
}

async fn send_session_failed(tx: &Sender<RsyncProgressEvent>, err: &DomainError) {
    let _ = tx
        .send(RsyncProgressEvent::SessionFailed {
            code: error_code(err),
            detail: err.to_string(),
        })
        .await;
}

/// Translate a [`DomainError`] into the wire error code stamped on
/// `SessionFailed` / `FileFailed` events. Used by both the transport
/// adapter and the executor (via re-export) so the lookup table lives
/// in one place.
pub(super) fn error_code(err: &DomainError) -> String {
    error_code_primary(err)
        .or_else(|| error_code_fallback(err))
        .unwrap_or("INTERNAL")
        .to_string()
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "the helper is intentionally a partial lookup — every \
              non-listed variant falls through to the fallback table; \
              an exhaustive match here would duplicate the taxonomy in \
              two places without changing behaviour."
)]
const fn error_code_primary(err: &DomainError) -> Option<&'static str> {
    Some(match err {
        DomainError::Sftp(_) => "SFTP_ERROR",
        DomainError::SftpFeatureMissing(_) => "SFTP_FEATURE_MISSING",
        DomainError::RsyncFileListTooLarge { .. } => "RSYNC_FILE_LIST_TOO_LARGE",
        DomainError::Timeout(_) => "TIMEOUT",
        DomainError::Transport(_) => "TRANSPORT_ERROR",
        DomainError::RsyncProtocolError(_) => "RSYNC_PROTOCOL_ERROR",
        DomainError::RsyncPartialTransfer(_) => "RSYNC_PARTIAL_TRANSFER",
        DomainError::RsyncNotFound(_) => "RSYNC_NOT_FOUND",
        DomainError::RsyncVersionTooOld(_) => "RSYNC_VERSION_TOO_OLD",
        DomainError::Auth(_) => "AUTH",
        DomainError::ConnectFailed(_) => "CONNECT_FAILED",
        DomainError::PortInUse(_) => "PORT_IN_USE",
        DomainError::InvalidArgument(_) => "INVALID_ARGUMENT",
        _ => return None,
    })
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "fallback table — every non-listed variant collapses to the \
              caller's `INTERNAL` default; exhaustive enumeration would \
              duplicate the taxonomy with no behavioural change."
)]
const fn error_code_fallback(err: &DomainError) -> Option<&'static str> {
    match err {
        DomainError::SessionNotFound(_)
        | DomainError::CommandNotFound(_)
        | DomainError::ShellNotFound(_)
        | DomainError::TransferNotFound(_)
        | DomainError::ForwardNotFound(_)
        | DomainError::SerialNotFound(_)
        | DomainError::SubNotFound(_) => Some("NOT_FOUND"),
        _ => None,
    }
}

/// Marker type satisfying the generic bound on the stub transport.
///
/// The stub constructor [`SftpRsyncTransport::without_fs`] picks `NoopFs`
/// so the public surface keeps returning the honest "being implemented"
/// wire error before the production russh-sftp adapter lands. Trait
/// methods are unreachable in practice — `start_session` short-circuits
/// to the stub error before any other method is dispatched.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopFs;

impl RsyncSftpFsPort for NoopFs {
    async fn readdir(
        &self,
        _session_id: &SessionId,
        _path: &str,
    ) -> Result<Vec<RemoteDirEntry>, DomainError> {
        Err(DomainError::RsyncProtocolError(STUB_DETAIL.to_string()))
    }

    async fn lstat(
        &self,
        _session_id: &SessionId,
        _path: &str,
    ) -> Result<RemoteMetadata, DomainError> {
        Err(DomainError::RsyncProtocolError(STUB_DETAIL.to_string()))
    }

    async fn read_link(&self, _session_id: &SessionId, _path: &str) -> Result<String, DomainError> {
        Err(DomainError::RsyncProtocolError(STUB_DETAIL.to_string()))
    }

    async fn mkdir(
        &self,
        _session_id: &SessionId,
        _path: &str,
        _mode: u32,
    ) -> Result<(), DomainError> {
        Err(DomainError::RsyncProtocolError(STUB_DETAIL.to_string()))
    }

    async fn rmdir(&self, _session_id: &SessionId, _path: &str) -> Result<(), DomainError> {
        Err(DomainError::RsyncProtocolError(STUB_DETAIL.to_string()))
    }

    async fn remove_file(&self, _session_id: &SessionId, _path: &str) -> Result<(), DomainError> {
        Err(DomainError::RsyncProtocolError(STUB_DETAIL.to_string()))
    }

    async fn symlink(
        &self,
        _session_id: &SessionId,
        _target: &str,
        _link_path: &str,
    ) -> Result<(), DomainError> {
        Err(DomainError::RsyncProtocolError(STUB_DETAIL.to_string()))
    }

    async fn set_metadata(
        &self,
        _session_id: &SessionId,
        _path: &str,
        _meta: RemoteMetadata,
    ) -> Result<(), DomainError> {
        Err(DomainError::RsyncProtocolError(STUB_DETAIL.to_string()))
    }

    async fn read_chunk(
        &self,
        _session_id: &SessionId,
        _path: &str,
        _offset: u64,
        _len: usize,
    ) -> Result<Bytes, DomainError> {
        Err(DomainError::RsyncProtocolError(STUB_DETAIL.to_string()))
    }

    async fn write_chunk(
        &self,
        _session_id: &SessionId,
        _path: &str,
        _offset: u64,
        _data: Bytes,
    ) -> Result<(), DomainError> {
        Err(DomainError::RsyncProtocolError(STUB_DETAIL.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_LANE_CAPACITY, NoopFs, STUB_DETAIL, SftpRsyncOpts, SftpRsyncTransport};
    use crate::adapters::rsync::sftp::comparator::Direction;
    use crate::adapters::rsync::sftp::fake::FakeRsyncSftpFs;
    use crate::adapters::rsync::types::{PreserveFlags, RsyncProgressEvent, RsyncTransportKind};
    use crate::domain::error::DomainError;
    use crate::domain::ids::SessionId;
    use crate::domain::rsync_ids::RsyncId;
    use crate::ports::rsync_transport::{RsyncDirection, RsyncStartRequest, RsyncTransportPort};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::timeout;

    fn start_request() -> RsyncStartRequest {
        RsyncStartRequest {
            session_id: SessionId::new("s-1".to_string()),
            src: "/src".to_string(),
            dst: "/dst".to_string(),
            direction: RsyncDirection::Push,
            ..RsyncStartRequest::default()
        }
    }

    fn opts() -> SftpRsyncOpts {
        SftpRsyncOpts {
            delete: false,
            dry_run: false,
            bwlimit_bps: None,
            excludes: Vec::new(),
            includes: Vec::new(),
            file_list_limit: 1_000,
            direction: Direction::Push,
            preserve: PreserveFlags::none(),
            force_transfer: false,
        }
    }

    #[tokio::test]
    async fn stub_constructor_returns_being_implemented_error() {
        let t: SftpRsyncTransport<NoopFs> = SftpRsyncTransport::<NoopFs>::without_fs();
        let err = t.start_session(start_request()).await.expect_err("err");
        match err {
            DomainError::RsyncProtocolError(msg) => {
                assert_eq!(msg, STUB_DETAIL);
                assert!(msg.contains("SFTP transport"));
                assert!(msg.contains("being implemented"));
            }
            other => panic!("expected RsyncProtocolError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn close_is_idempotent_on_unknown_id() {
        let t: SftpRsyncTransport<NoopFs> = SftpRsyncTransport::<NoopFs>::without_fs();
        t.close(&RsyncId::new("nope".to_string()))
            .await
            .expect("idempotent");
    }

    #[tokio::test]
    async fn live_pipeline_emits_session_started_then_completed() {
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/src", 0o755);
        fs.put_dir("/dst", 0o755);
        fs.put_file("/src/a.txt", b"hello", 0o644, 100);
        let t = SftpRsyncTransport::<FakeRsyncSftpFs>::with_fs(
            Arc::new(fs.clone()),
            opts(),
            DEFAULT_LANE_CAPACITY,
        );
        let outcome = t.start_session(start_request()).await.expect("start");
        let mut events = Vec::new();
        for _ in 0..32 {
            match timeout(Duration::from_secs(2), t.recv_event(&outcome.rsync_id)).await {
                Ok(Ok(Some(event))) => {
                    let stop = matches!(
                        event,
                        RsyncProgressEvent::SyncCompleted { .. }
                            | RsyncProgressEvent::SessionFailed { .. }
                    );
                    events.push(event);
                    if stop {
                        break;
                    }
                }
                _ => break,
            }
        }
        // Must see SessionStarted as the first event and SyncCompleted last.
        assert!(matches!(
            events.first(),
            Some(RsyncProgressEvent::SessionStarted {
                transport: RsyncTransportKind::Sftp,
                ..
            })
        ));
        assert!(matches!(
            events.last(),
            Some(RsyncProgressEvent::SyncCompleted { .. })
        ));
        // The destination must carry the seeded file.
        assert_eq!(
            fs.get_file("/dst/a.txt"),
            Some(bytes::Bytes::from_static(b"hello"))
        );
        t.close(&outcome.rsync_id).await.expect("close");
    }

    #[tokio::test]
    async fn dry_run_pipeline_does_not_touch_destination() {
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/src", 0o755);
        fs.put_dir("/dst", 0o755);
        fs.put_file("/src/a.txt", b"hello", 0o644, 100);
        let mut o = opts();
        o.dry_run = true;
        let t = SftpRsyncTransport::<FakeRsyncSftpFs>::with_fs(
            Arc::new(fs.clone()),
            o,
            DEFAULT_LANE_CAPACITY,
        );
        let outcome = t.start_session(start_request()).await.expect("start");
        // Drain events until SyncCompleted.
        for _ in 0..32 {
            match timeout(Duration::from_secs(2), t.recv_event(&outcome.rsync_id)).await {
                Ok(Ok(Some(RsyncProgressEvent::SyncCompleted { .. }))) => break,
                Ok(Ok(Some(_))) => {}
                _ => break,
            }
        }
        assert!(fs.get_file("/dst/a.txt").is_none());
        t.close(&outcome.rsync_id).await.expect("close");
    }
}
