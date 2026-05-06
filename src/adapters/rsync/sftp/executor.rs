//! Apply [`super::comparator::SyncAction`]s through the
//! [`crate::ports::rsync_sftp_fs::RsyncSftpFsPort`].
//!
//! Drives chunked uploads (32 KiB chunks per ADR 0010), emits
//! per-action [`crate::adapters::rsync::types::RsyncProgressEvent`]s,
//! checks the cancellation token between every action and inside the
//! upload chunk loop, and respects the optional bandwidth-limit token
//! bucket on every byte that crosses the wire.
//!
//! Failures are isolated per action — a `FileFailed` event is emitted
//! and the executor moves on to the next action so a single
//! permission-denied entry does not abort the rest of the sync.
//!
//! `dry_run = true` short-circuits every destructive op into a
//! `FileSkipped { reason: DryRun }` event without touching the
//! destination.

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

use crate::adapters::rsync::sftp::bwlimit::TokenBucket;
use crate::adapters::rsync::sftp::comparator::SyncAction;
use crate::adapters::rsync::types::{RsyncProgressEvent, SkipReason};
use crate::domain::error::DomainError;
use crate::domain::ids::SessionId;
use crate::ports::rsync_sftp_fs::{RemoteMetadata, RsyncSftpFsPort};

/// Bytes per `read_chunk` / `write_chunk` round (ADR 0010 transfer
/// chunk size).
pub const CHUNK_BYTES: usize = 32 * 1024;

/// Cluster of metadata fields the executor passes to
/// [`SftpExecutor::run_setstat`]. Exists so the function signature stays
/// under the strict 7-argument cap.
#[derive(Debug, Clone, Copy)]
struct SetstatArgs {
    mode: u32,
    mtime: i64,
    uid: u32,
    gid: u32,
}

/// Outcome of [`SftpExecutor::copy_chunks`] driving the chunked
/// upload loop. `Completed` carries the byte count; `Aborted` means
/// the helper already emitted the failure / cancel beacon.
enum ChunkLoopOutcome {
    Completed { bytes: u64 },
    Aborted,
}

struct ChunkPaths {
    src_abs: String,
    dst_abs: String,
}

enum ChunkStep {
    Continue,
    Done(ChunkLoopOutcome),
}

enum ChunkPull {
    Some(Bytes),
    Done(ChunkLoopOutcome),
}

/// Mutable state threaded through [`SftpExecutor::copy_chunks`].
struct ChunkLoopState {
    offset: u64,
    last_progress: u64,
    chunk_len: u64,
}

impl ChunkLoopState {
    fn new(_size: u64) -> Self {
        Self {
            offset: 0,
            last_progress: 0,
            chunk_len: u64::try_from(CHUNK_BYTES).unwrap_or(u64::MAX),
        }
    }

    fn advance(&mut self, chunk: &Bytes, stats: &mut ExecutorStats) {
        let written = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        self.offset = self.offset.saturating_add(written);
        stats.bytes_transferred = stats.bytes_transferred.saturating_add(written);
    }
}

/// Bytes between mid-file `FileProgress` beacons.
pub const PROGRESS_BYTES_THRESHOLD: u64 = 64 * 1024;

/// Aggregate counters surfaced once the executor finishes draining the
/// action list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecutorStats {
    /// Files transferred end-to-end.
    pub files_done: u64,
    /// Files skipped without crossing the wire.
    pub files_skipped: u64,
    /// Files that failed mid-sync.
    pub files_failed: u64,
    /// Files / dirs deleted on the destination.
    pub files_deleted: u64,
    /// Bytes that crossed the wire.
    pub bytes_transferred: u64,
}

/// Drive an action list to completion.
#[derive(Debug)]
pub struct SftpExecutor<F> {
    fs: Arc<F>,
    bwlimit: Option<Arc<TokenBucket>>,
    dry_run: bool,
    progress_tx: Sender<RsyncProgressEvent>,
    cancel: CancellationToken,
    /// Where new files are written / read on the remote (the absolute
    /// destination root).
    dst_root: String,
    /// Where existing files are read from on the remote (the absolute
    /// source root). For Push the executor copies bytes from the local
    /// filesystem and ignores this; for Pull the executor reads from
    /// here and writes locally. The first slice ships Push-only and
    /// reads from `src_root` via the same SFTP port (treats it as
    /// "remote bytes" when no local FS is wired).
    src_root: String,
}

impl<F> SftpExecutor<F>
where
    F: RsyncSftpFsPort + 'static,
{
    /// Wire a fresh executor.
    #[must_use]
    pub const fn new(
        fs: Arc<F>,
        bwlimit: Option<Arc<TokenBucket>>,
        dry_run: bool,
        progress_tx: Sender<RsyncProgressEvent>,
        cancel: CancellationToken,
        src_root: String,
        dst_root: String,
    ) -> Self {
        Self {
            fs,
            bwlimit,
            dry_run,
            progress_tx,
            cancel,
            dst_root,
            src_root,
        }
    }

    /// Drive the supplied action list to completion.
    ///
    /// # Errors
    ///
    /// Per-action errors are surfaced as `FileFailed` events and the
    /// executor moves on. The method itself only returns
    /// [`DomainError`] on cancellation or fatal infrastructure faults
    /// (lane closed before the `SyncCompleted` event fires).
    pub async fn execute(
        &self,
        session_id: &SessionId,
        actions: &[SyncAction],
    ) -> Result<ExecutorStats, DomainError> {
        let mut stats = ExecutorStats::default();
        for action in actions {
            if self.cancel.is_cancelled() {
                break;
            }
            self.run_action(session_id, action, &mut stats).await;
        }
        Ok(stats)
    }

    async fn run_action(
        &self,
        session_id: &SessionId,
        action: &SyncAction,
        stats: &mut ExecutorStats,
    ) {
        match action {
            SyncAction::Skip { rel_path } => self.handle_skip(rel_path, stats).await,
            SyncAction::Mkdir { rel_path, mode } => {
                self.run_mkdir(session_id, rel_path, *mode, stats).await;
            }
            SyncAction::Transfer { rel_path, size } => {
                self.run_transfer(session_id, rel_path, *size, stats).await;
            }
            SyncAction::Symlink { rel_path, target } => {
                self.run_symlink(session_id, rel_path, target, stats).await;
            }
            SyncAction::Setstat { .. } => {
                self.dispatch_setstat(session_id, action, stats).await;
            }
            SyncAction::Delete { rel_path, is_dir } => {
                self.run_delete(session_id, rel_path, *is_dir, stats).await;
            }
        }
    }

    async fn handle_skip(&self, rel_path: &str, stats: &mut ExecutorStats) {
        stats.files_skipped = stats.files_skipped.saturating_add(1);
        self.send(RsyncProgressEvent::FileSkipped {
            rel_path: rel_path.to_string(),
            reason: SkipReason::SizeMatch,
        })
        .await;
    }

    async fn dispatch_setstat(
        &self,
        session_id: &SessionId,
        action: &SyncAction,
        stats: &mut ExecutorStats,
    ) {
        if let SyncAction::Setstat {
            rel_path,
            mode,
            mtime,
            uid,
            gid,
        } = action
        {
            let args = SetstatArgs {
                mode: *mode,
                mtime: *mtime,
                uid: *uid,
                gid: *gid,
            };
            self.run_setstat(session_id, rel_path, args, stats).await;
        }
    }

    async fn run_mkdir(
        &self,
        session_id: &SessionId,
        rel_path: &str,
        mode: u32,
        stats: &mut ExecutorStats,
    ) {
        if self.dry_run {
            self.send_skip_dry_run(rel_path, stats).await;
            return;
        }
        let abs = join_root(&self.dst_root, rel_path);
        if let Err(err) = self.fs.mkdir(session_id, &abs, mode).await {
            self.report_failure(rel_path, &err, stats).await;
        }
    }

    async fn run_transfer(
        &self,
        session_id: &SessionId,
        rel_path: &str,
        size: u64,
        stats: &mut ExecutorStats,
    ) {
        if self.dry_run {
            self.send_skip_dry_run(rel_path, stats).await;
            return;
        }
        self.send(RsyncProgressEvent::FileStarted {
            rel_path: rel_path.to_string(),
            bytes_total: size,
        })
        .await;
        let outcome = self.copy_chunks(session_id, rel_path, size, stats).await;
        match outcome {
            ChunkLoopOutcome::Completed { bytes } => {
                stats.files_done = stats.files_done.saturating_add(1);
                self.send(RsyncProgressEvent::FileCompleted {
                    rel_path: rel_path.to_string(),
                    bytes_transferred: bytes,
                    bytes_skipped: 0,
                })
                .await;
            }
            ChunkLoopOutcome::Aborted => {}
        }
    }

    async fn copy_chunks(
        &self,
        session_id: &SessionId,
        rel_path: &str,
        size: u64,
        counters: &mut ExecutorStats,
    ) -> ChunkLoopOutcome {
        let paths = ChunkPaths {
            src_abs: join_root(&self.src_root, rel_path),
            dst_abs: join_root(&self.dst_root, rel_path),
        };
        let mut chunk_state = ChunkLoopState::new(size);
        loop {
            if let Some(outcome) = self
                .guard_loop(rel_path, &chunk_state, size, counters)
                .await
            {
                return outcome;
            }
            match self
                .step_chunk(
                    session_id,
                    &paths,
                    &mut chunk_state,
                    size,
                    rel_path,
                    counters,
                )
                .await
            {
                ChunkStep::Continue => {}
                ChunkStep::Done(outcome) => return outcome,
            }
        }
    }

    async fn guard_loop(
        &self,
        rel_path: &str,
        chunk_state: &ChunkLoopState,
        size: u64,
        counters: &mut ExecutorStats,
    ) -> Option<ChunkLoopOutcome> {
        if self.cancel.is_cancelled() {
            self.report_cancel(rel_path, counters).await;
            return Some(ChunkLoopOutcome::Aborted);
        }
        let remaining = size.saturating_sub(chunk_state.offset);
        if remaining == 0 {
            return Some(ChunkLoopOutcome::Completed {
                bytes: chunk_state.offset,
            });
        }
        None
    }

    async fn step_chunk(
        &self,
        session_id: &SessionId,
        paths: &ChunkPaths,
        chunk_state: &mut ChunkLoopState,
        size: u64,
        rel_path: &str,
        counters: &mut ExecutorStats,
    ) -> ChunkStep {
        let chunk = match self
            .pull_chunk(session_id, paths, chunk_state, size, rel_path, counters)
            .await
        {
            ChunkPull::Some(chunk) => chunk,
            ChunkPull::Done(outcome) => return ChunkStep::Done(outcome),
        };
        if !self
            .write_one_chunk(
                session_id,
                &paths.dst_abs,
                chunk_state.offset,
                chunk.clone(),
                rel_path,
                counters,
            )
            .await
        {
            return ChunkStep::Done(ChunkLoopOutcome::Aborted);
        }
        chunk_state.advance(&chunk, counters);
        self.maybe_emit_progress(rel_path, chunk_state, size).await;
        ChunkStep::Continue
    }

    async fn pull_chunk(
        &self,
        session_id: &SessionId,
        paths: &ChunkPaths,
        chunk_state: &ChunkLoopState,
        size: u64,
        rel_path: &str,
        counters: &mut ExecutorStats,
    ) -> ChunkPull {
        let want = size
            .saturating_sub(chunk_state.offset)
            .min(chunk_state.chunk_len);
        match self
            .read_chunk_at(
                session_id,
                &paths.src_abs,
                chunk_state.offset,
                want,
                rel_path,
                counters,
            )
            .await
        {
            Some(c) if c.is_empty() => ChunkPull::Done(ChunkLoopOutcome::Completed {
                bytes: chunk_state.offset,
            }),
            Some(c) => ChunkPull::Some(c),
            None => ChunkPull::Done(ChunkLoopOutcome::Aborted),
        }
    }

    async fn write_one_chunk(
        &self,
        session_id: &SessionId,
        dst_abs: &str,
        offset: u64,
        chunk: Bytes,
        rel_path: &str,
        stats: &mut ExecutorStats,
    ) -> bool {
        if let Some(bw) = &self.bwlimit {
            bw.take(u64::try_from(chunk.len()).unwrap_or(u64::MAX))
                .await;
        }
        match self
            .fs
            .write_chunk(session_id, dst_abs, offset, chunk)
            .await
        {
            Ok(()) => true,
            Err(err) => {
                self.report_failure(rel_path, &err, stats).await;
                false
            }
        }
    }

    async fn maybe_emit_progress(
        &self,
        rel_path: &str,
        state: &mut ChunkLoopState,
        bytes_total: u64,
    ) {
        if state.offset.saturating_sub(state.last_progress) >= PROGRESS_BYTES_THRESHOLD {
            self.send(RsyncProgressEvent::FileProgress {
                rel_path: rel_path.to_string(),
                bytes_done: state.offset,
                bytes_total,
            })
            .await;
            state.last_progress = state.offset;
        }
    }

    async fn read_chunk_at(
        &self,
        session_id: &SessionId,
        abs_path: &str,
        offset: u64,
        want: u64,
        rel_path: &str,
        stats: &mut ExecutorStats,
    ) -> Option<Bytes> {
        match self
            .fs
            .read_chunk(
                session_id,
                abs_path,
                offset,
                usize::try_from(want).unwrap_or(usize::MAX),
            )
            .await
        {
            Ok(chunk) => Some(chunk),
            Err(err) => {
                self.report_failure(rel_path, &err, stats).await;
                None
            }
        }
    }

    async fn report_cancel(&self, rel_path: &str, stats: &mut ExecutorStats) {
        stats.files_failed = stats.files_failed.saturating_add(1);
        self.send(RsyncProgressEvent::FileFailed {
            rel_path: rel_path.to_string(),
            code: "CANCELLED".to_string(),
            detail: "transfer cancelled mid-flight".to_string(),
        })
        .await;
    }

    async fn run_symlink(
        &self,
        session_id: &SessionId,
        rel_path: &str,
        target: &str,
        stats: &mut ExecutorStats,
    ) {
        if self.dry_run {
            self.send_skip_dry_run(rel_path, stats).await;
            return;
        }
        let abs = join_root(&self.dst_root, rel_path);
        if let Err(err) = self.fs.symlink(session_id, target, &abs).await {
            self.send(RsyncProgressEvent::FileFailed {
                rel_path: rel_path.to_string(),
                code: "SFTP_FEATURE_MISSING".to_string(),
                detail: format!("symlink rejected by remote: {err}"),
            })
            .await;
            stats.files_failed = stats.files_failed.saturating_add(1);
        }
    }

    async fn run_setstat(
        &self,
        session_id: &SessionId,
        rel_path: &str,
        args: SetstatArgs,
        stats: &mut ExecutorStats,
    ) {
        if self.dry_run {
            return;
        }
        let abs = join_root(&self.dst_root, rel_path);
        let meta = RemoteMetadata {
            size: 0,
            mode: args.mode,
            mtime: args.mtime,
            uid: args.uid,
            gid: args.gid,
            is_dir: false,
            is_symlink: false,
        };
        if let Err(err) = self.fs.set_metadata(session_id, &abs, meta).await {
            // Setstat failures are non-fatal — surface as FileFailed
            // tagged with SFTP_FEATURE_MISSING so the LLM host knows the
            // bytes landed but the metadata did not.
            self.send(RsyncProgressEvent::FileFailed {
                rel_path: rel_path.to_string(),
                code: "SFTP_FEATURE_MISSING".to_string(),
                detail: format!("setstat rejected by remote: {err}"),
            })
            .await;
            stats.files_failed = stats.files_failed.saturating_add(1);
        }
    }

    async fn run_delete(
        &self,
        session_id: &SessionId,
        rel_path: &str,
        is_dir: bool,
        stats: &mut ExecutorStats,
    ) {
        if self.dry_run {
            self.send_skip_dry_run(rel_path, stats).await;
            return;
        }
        let abs = join_root(&self.dst_root, rel_path);
        let outcome = if is_dir {
            self.fs.rmdir(session_id, &abs).await
        } else {
            self.fs.remove_file(session_id, &abs).await
        };
        match outcome {
            Ok(()) => {
                stats.files_deleted = stats.files_deleted.saturating_add(1);
            }
            Err(err) => self.report_failure(rel_path, &err, stats).await,
        }
    }

    async fn send_skip_dry_run(&self, rel_path: &str, stats: &mut ExecutorStats) {
        stats.files_skipped = stats.files_skipped.saturating_add(1);
        self.send(RsyncProgressEvent::FileSkipped {
            rel_path: rel_path.to_string(),
            reason: SkipReason::DryRun,
        })
        .await;
    }

    async fn report_failure(&self, rel_path: &str, err: &DomainError, stats: &mut ExecutorStats) {
        stats.files_failed = stats.files_failed.saturating_add(1);
        self.send(RsyncProgressEvent::FileFailed {
            rel_path: rel_path.to_string(),
            code: error_code(err),
            detail: err.to_string(),
        })
        .await;
    }

    async fn send(&self, event: RsyncProgressEvent) {
        // Fire-and-forget — closed lanes are a normal cancel signal.
        let _ = self.progress_tx.send(event).await;
    }
}

fn error_code(err: &DomainError) -> String {
    super::error_code(err)
}

fn join_root(root: &str, rel: &str) -> String {
    if rel.is_empty() {
        root.to_string()
    } else if root.ends_with('/') {
        format!("{root}{rel}")
    } else {
        format!("{root}/{rel}")
    }
}

/// Drain the bytes a [`Bytes`] hides behind the `Buf` trait. Kept here
/// so the executor body stays free of `Buf` boilerplate.
#[allow(dead_code, reason = "reserved for the next slice's local-fs pull path")]
fn drain_bytes(b: &Bytes) -> Vec<u8> {
    b.to_vec()
}

#[cfg(test)]
mod tests {
    use super::{SftpExecutor, SyncAction};
    use crate::adapters::rsync::sftp::fake::FakeRsyncSftpFs;
    use crate::adapters::rsync::types::{RsyncProgressEvent, SkipReason};
    use crate::domain::ids::SessionId;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    fn s() -> SessionId {
        SessionId::new("sess-x".to_string())
    }

    async fn drain_events(rx: &mut mpsc::Receiver<RsyncProgressEvent>) -> Vec<RsyncProgressEvent> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    #[tokio::test]
    async fn skip_action_emits_file_skipped_event() {
        let fs = Arc::new(FakeRsyncSftpFs::new());
        let (tx, mut rx) = mpsc::channel(8);
        let exec = SftpExecutor::new(
            Arc::clone(&fs),
            None,
            false,
            tx,
            CancellationToken::new(),
            "/src".to_string(),
            "/dst".to_string(),
        );
        let stats = exec
            .execute(
                &s(),
                &[SyncAction::Skip {
                    rel_path: "a.txt".to_string(),
                }],
            )
            .await
            .expect("execute ok");
        assert_eq!(stats.files_skipped, 1);
        let events = drain_events(&mut rx).await;
        assert!(matches!(
            events[0],
            RsyncProgressEvent::FileSkipped {
                reason: SkipReason::SizeMatch,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn dry_run_skips_every_destructive_action() {
        let fs = Arc::new(FakeRsyncSftpFs::new());
        fs.put_dir("/src", 0o755);
        fs.put_dir("/dst", 0o755);
        fs.put_file("/src/a.txt", b"hello", 0o644, 0);
        let (tx, _rx) = mpsc::channel(8);
        let exec = SftpExecutor::new(
            Arc::clone(&fs),
            None,
            true,
            tx,
            CancellationToken::new(),
            "/src".to_string(),
            "/dst".to_string(),
        );
        let stats = exec
            .execute(
                &s(),
                &[SyncAction::Transfer {
                    rel_path: "a.txt".to_string(),
                    size: 5,
                }],
            )
            .await
            .expect("ok");
        assert_eq!(stats.files_done, 0);
        assert_eq!(stats.files_skipped, 1);
        // Destination must remain untouched.
        assert!(fs.get_file("/dst/a.txt").is_none());
    }

    #[tokio::test]
    async fn transfer_copies_bytes_from_src_to_dst() {
        let fs = Arc::new(FakeRsyncSftpFs::new());
        fs.put_dir("/src", 0o755);
        fs.put_dir("/dst", 0o755);
        fs.put_file("/src/a.txt", b"hello world", 0o644, 0);
        let (tx, mut rx) = mpsc::channel(16);
        let exec = SftpExecutor::new(
            Arc::clone(&fs),
            None,
            false,
            tx,
            CancellationToken::new(),
            "/src".to_string(),
            "/dst".to_string(),
        );
        let stats = exec
            .execute(
                &s(),
                &[SyncAction::Transfer {
                    rel_path: "a.txt".to_string(),
                    size: 11,
                }],
            )
            .await
            .expect("ok");
        assert_eq!(stats.files_done, 1);
        assert_eq!(stats.bytes_transferred, 11);
        assert_eq!(
            fs.get_file("/dst/a.txt"),
            Some(bytes::Bytes::from_static(b"hello world"))
        );
        let events = drain_events(&mut rx).await;
        let last = events.last().expect("events");
        assert!(matches!(last, RsyncProgressEvent::FileCompleted { .. }));
    }

    #[tokio::test]
    async fn cancel_short_circuits_action_loop() {
        let fs = Arc::new(FakeRsyncSftpFs::new());
        fs.put_dir("/src", 0o755);
        fs.put_dir("/dst", 0o755);
        fs.put_file("/src/a.txt", b"hello", 0o644, 0);
        let (tx, _rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let exec = SftpExecutor::new(
            Arc::clone(&fs),
            None,
            false,
            tx,
            cancel,
            "/src".to_string(),
            "/dst".to_string(),
        );
        let stats = exec
            .execute(
                &s(),
                &[SyncAction::Transfer {
                    rel_path: "a.txt".to_string(),
                    size: 5,
                }],
            )
            .await
            .expect("ok");
        assert_eq!(stats.files_done, 0);
        assert!(fs.get_file("/dst/a.txt").is_none());
    }

    #[tokio::test]
    async fn delete_removes_file_on_destination() {
        let fs = Arc::new(FakeRsyncSftpFs::new());
        fs.put_dir("/dst", 0o755);
        fs.put_file("/dst/old.txt", b"old", 0o644, 0);
        let (tx, _rx) = mpsc::channel(8);
        let exec = SftpExecutor::new(
            Arc::clone(&fs),
            None,
            false,
            tx,
            CancellationToken::new(),
            "/src".to_string(),
            "/dst".to_string(),
        );
        let stats = exec
            .execute(
                &s(),
                &[SyncAction::Delete {
                    rel_path: "old.txt".to_string(),
                    is_dir: false,
                }],
            )
            .await
            .expect("ok");
        assert_eq!(stats.files_deleted, 1);
        assert!(fs.get_file("/dst/old.txt").is_none());
    }

    #[tokio::test]
    async fn symlink_failure_emits_feature_missing_file_failed() {
        let fs = Arc::new(FakeRsyncSftpFs::new());
        fs.put_dir("/dst", 0o755);
        fs.fail_symlink();
        let (tx, mut rx) = mpsc::channel(8);
        let exec = SftpExecutor::new(
            Arc::clone(&fs),
            None,
            false,
            tx,
            CancellationToken::new(),
            "/src".to_string(),
            "/dst".to_string(),
        );
        let stats = exec
            .execute(
                &s(),
                &[SyncAction::Symlink {
                    rel_path: "lnk".to_string(),
                    target: "../t".to_string(),
                }],
            )
            .await
            .expect("ok");
        assert_eq!(stats.files_failed, 1);
        let events = drain_events(&mut rx).await;
        let last = events.last().expect("events");
        match last {
            RsyncProgressEvent::FileFailed { code, .. } => {
                assert_eq!(code, "SFTP_FEATURE_MISSING");
            }
            other => panic!("expected FileFailed, got {other:?}"),
        }
    }
}
