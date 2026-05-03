//! Use case: snapshot the current output of an async command and, optionally,
//! block until the command transitions out of [`CommandStatus::Running`].
//!
//! Translates the v3 `ssh_get_command_output_impl` (from
//! `src/mcp/tools/execute.rs`) into a hexagonal use case orchestrating ports.
//! No rmcp / russh / tokio types leak into the public DTO surface.
//!
//! # Orchestration shape
//!
//! 1. Resolve the [`CommandEntity`] via [`CommandRepository::get`]. A missing
//!    id short-circuits with [`DomainError::CommandNotFound`] so the inbound
//!    adapter can render the canonical
//!    `SSH_GET_COMMAND_OUTPUT: ERROR / REASON: [COMMAND_NOT_FOUND]` block.
//! 2. When [`GetCommandOutputRequest::wait`] is `true`, poll the repository
//!    until the command leaves [`CommandStatus::Running`] or the
//!    `wait_timeout` budget is exhausted. The `OutputStreamPort` does not
//!    expose an edge-triggered "wait until terminal" surface (and adding one
//!    would impact every adapter / port consumer), so a small cooperative
//!    polling loop is used here. The cadence is bounded so tests stay fast
//!    and a closed channel does not spin.
//! 3. Take a final [`OutputSnapshot`] via [`OutputStreamPort::snapshot_command`]
//!    and head-truncate the stdout/stderr buffers to `max_output_bytes`
//!    (preserving the *tail*, i.e. the most recent output — same semantics as
//!    v3 `clamp_output_bytes` + `render_output_block`).
//! 4. Re-read the [`CommandEntity`] (status may have changed during the
//!    wait) and return [`GetCommandOutputResult`].
//!
//! # Concurrency contract
//!
//! Every `.await` happens outside any repository / fake guard — repository
//! ports return owned values so no shard-spanning borrows escape. The
//! polling loop yields via [`tokio::time::sleep`] so the executor stays
//! responsive even when the command never completes.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::time::{Instant, sleep};

use crate::domain::command::{CommandEntity, CommandStatus};
use crate::domain::error::DomainError;
use crate::domain::ids::CommandId;
use crate::ports::command_repo::CommandRepository;
use crate::ports::output_stream::OutputStreamPort;

/// Cooperative polling cadence used by the wait loop. Small enough to keep
/// completion latency well under one human-perceptible tick (50 ms) while
/// avoiding a tight spin when the command never completes.
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct GetCommandOutputRequest {
    /// `COMMAND_ID` returned from `ssh_execute`.
    pub command_id: CommandId,
    /// `true` to block until the command leaves
    /// [`CommandStatus::Running`] or [`Self::wait_timeout`] elapses.
    pub wait: bool,
    /// Maximum time to block when [`Self::wait`] is `true`. `None` falls back
    /// to the v3 default of 30 s. Capped at 300 s by the inbound adapter
    /// before it constructs this DTO.
    pub wait_timeout: Option<Duration>,
    /// Maximum bytes returned in `stdout` / `stderr`. `None` defers to the
    /// inbound adapter, which is responsible for clamping against
    /// `SSH_MCP_OUTPUT_DEFAULT_BYTES` / `SSH_MCP_OUTPUT_MAX_BYTES_CAP`. When
    /// the snapshot exceeds the cap the *head* is dropped (the most recent
    /// tail bytes are preserved), matching v3 `render_output_block` semantics.
    pub max_output_bytes: Option<usize>,
}

/// Outbound DTO surfacing every observable result the rmcp tool wrapper
/// must render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetCommandOutputResult {
    /// Echoed back so the caller can correlate without recreating the request.
    pub command_id: CommandId,
    /// Final lifecycle state observed at snapshot time.
    pub status: CommandStatus,
    /// Stdout bytes, head-truncated to [`GetCommandOutputRequest::max_output_bytes`]
    /// when set.
    pub stdout: Bytes,
    /// Stderr bytes, head-truncated under the same rule.
    pub stderr: Bytes,
    /// Captured exit code (set when [`Self::status`] is
    /// [`CommandStatus::Completed`]).
    pub exit_code: Option<i32>,
    /// Optional error string set when the command failed before producing an
    /// exit status. The current `CommandEntity` does not carry this field, so
    /// it is `None` until the domain model grows it; preserved here so the
    /// inbound adapter rendering is forward-compatible without another DTO
    /// churn.
    pub error: Option<String>,
    /// `true` when the command exited because the configured timeout fired.
    /// Mirrors v3's `timed_out` boolean.
    pub timed_out: bool,
    /// Latest sequence number from the output stream snapshot. Lets clients
    /// correlate this snapshot with subsequent `resources/read` cursor pages.
    pub last_seq: u64,
}

/// Get-command-output use case generic over every adapter dependency.
#[derive(Debug)]
pub struct GetCommandOutputUseCase<CR, OS>
where
    CR: CommandRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
{
    commands: Arc<CR>,
    streams: Arc<OS>,
}

impl<CR, OS> GetCommandOutputUseCase<CR, OS>
where
    CR: CommandRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(commands: Arc<CR>, streams: Arc<OS>) -> Self {
        Self { commands, streams }
    }

    /// Borrow the shared output-stream port handle.
    ///
    /// The inbound rmcp layer uses this to interleave snapshot reads
    /// with the use case future when emitting bounded
    /// `notifications/progress` notifications during a `wait=true`
    /// poll. The use case body itself does not need it (it already owns
    /// the same `Arc`); the accessor is purely an inbound-side helper.
    #[must_use]
    pub const fn streams(&self) -> &Arc<OS> {
        &self.streams
    }

    /// Drive the orchestration. See module-level docs for the step-by-step
    /// semantics.
    ///
    /// # Errors
    ///
    /// - [`DomainError::CommandNotFound`] when the id is unknown at probe
    ///   time. The use case returns before any side effect.
    /// - Any [`DomainError`] surfaced by the underlying ports.
    pub async fn execute(
        &self,
        req: GetCommandOutputRequest,
    ) -> Result<GetCommandOutputResult, DomainError> {
        let entity = self.fetch_or_not_found(&req.command_id).await?;

        let timed_out = if req.wait && entity.status == CommandStatus::Running {
            let budget = req.wait_timeout.unwrap_or(Duration::from_secs(30));
            self.wait_for_terminal(&req.command_id, budget).await?
        } else {
            false
        };

        let final_entity = self.fetch_or_not_found(&req.command_id).await?;
        let (stdout, stderr, last_seq) = self
            .resolve_output(&req.command_id, &final_entity.status, req.max_output_bytes)
            .await?;

        Ok(GetCommandOutputResult {
            command_id: req.command_id,
            status: final_entity.status,
            stdout,
            stderr,
            exit_code: final_entity.exit_code,
            error: None,
            timed_out: final_entity.timed_out || timed_out,
            last_seq,
        })
    }

    /// Snapshot the output stream for the given command and head-truncate
    /// each buffer to `max_output_bytes`. v4.7.1 (Bug #2) fix: when the
    /// adapter tears down its internal record before the snapshot is read
    /// (the russh adapter does this on cancel) and the entity has reached
    /// a terminal state, treat
    /// [`DomainError::CommandNotFound`] as a benign "snapshot evicted"
    /// and surface an empty stdout/stderr pair with `last_seq = 0` so the
    /// inbound layer can render the saved status without a hard error.
    async fn resolve_output(
        &self,
        command_id: &CommandId,
        status: &CommandStatus,
        max_output_bytes: Option<usize>,
    ) -> Result<(Bytes, Bytes, u64), DomainError> {
        let snapshot = match self.streams.snapshot_command(command_id).await {
            Ok(snap) => Some(snap),
            Err(DomainError::CommandNotFound(_)) if status.is_terminal() => None,
            Err(err) => return Err(err),
        };
        let (raw_stdout, raw_stderr, last_seq) = snapshot.map_or_else(
            || (Bytes::new(), Bytes::new(), 0_u64),
            |snap| (snap.stdout, snap.stderr, snap.last_seq),
        );
        Ok((
            head_truncate(raw_stdout, max_output_bytes),
            head_truncate(raw_stderr, max_output_bytes),
            last_seq,
        ))
    }

    /// Re-read the entity by id, mapping `Ok(None)` to
    /// [`DomainError::CommandNotFound`]. Used both at the entry point and
    /// after the wait loop so the returned status always reflects the most
    /// recent repository state.
    async fn fetch_or_not_found(
        &self,
        command_id: &CommandId,
    ) -> Result<CommandEntity, DomainError> {
        self.commands
            .get(command_id)
            .await?
            .ok_or_else(|| DomainError::CommandNotFound(command_id.clone()))
    }

    /// Cooperative polling loop. Returns `true` when the budget was
    /// exhausted before the command left [`CommandStatus::Running`], `false`
    /// when the command transitioned to a terminal state in time. The loop
    /// is bounded by `budget` *and* relies on `commands.get` returning owned
    /// data so no repository guard is ever held across a `.await`.
    async fn wait_for_terminal(
        &self,
        command_id: &CommandId,
        budget: Duration,
    ) -> Result<bool, DomainError> {
        let deadline = Instant::now() + budget;
        loop {
            let entity = self.fetch_or_not_found(command_id).await?;
            match entity.status {
                CommandStatus::Completed | CommandStatus::Cancelled | CommandStatus::Failed => {
                    return Ok(false);
                }
                CommandStatus::Running => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(true);
                    }
                    let remaining = deadline.saturating_duration_since(now);
                    let nap = remaining.min(WAIT_POLL_INTERVAL);
                    sleep(nap).await;
                }
            }
        }
    }
}

/// Head-truncate `buf` so the returned `Bytes` is at most `max` bytes long,
/// preserving the *tail* (most recent output). When `max` is `None` the
/// buffer is returned unchanged. A `Some(0)` cap returns an empty `Bytes` —
/// the caller is responsible for surfacing the truncation indicator (the
/// rmcp wrapper renders a `(empty)` block under that condition).
fn head_truncate(buf: Bytes, max: Option<usize>) -> Bytes {
    let Some(cap) = max else {
        return buf;
    };
    if buf.len() <= cap {
        return buf;
    }
    let drop_head = buf.len() - cap;
    buf.slice(drop_head..)
}

#[cfg(test)]
mod tests {
    use super::{
        GetCommandOutputRequest, GetCommandOutputResult, GetCommandOutputUseCase, head_truncate,
    };
    use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
    use crate::domain::command::{CommandEntity, CommandStatus};
    use crate::domain::error::DomainError;
    use crate::domain::ids::{CommandId, SessionId};
    use crate::ports::command_repo::CommandRepository;
    use crate::ports::output_stream::{OutputSnapshot, OutputStreamPort};
    use bytes::Bytes;
    use chrono::Utc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::time::sleep;

    /// In-memory [`OutputStreamPort`] fake. Tests pre-load a snapshot per
    /// command id; `snapshot_command` returns a clone. `snapshot_shell` is
    /// not exercised by this use case but is implemented so the fake
    /// satisfies the trait.
    #[derive(Debug, Default, Clone)]
    struct FakeOutputStream {
        cmd_snapshots: Arc<Mutex<std::collections::HashMap<CommandId, OutputSnapshot>>>,
    }

    impl FakeOutputStream {
        fn new() -> Self {
            Self::default()
        }

        fn put_command(&self, id: &CommandId, snap: OutputSnapshot) {
            let mut map = self.cmd_snapshots.lock().expect("snapshot map mutex");
            map.insert(id.clone(), snap);
        }
    }

    impl OutputStreamPort for FakeOutputStream {
        async fn snapshot_command(&self, id: &CommandId) -> Result<OutputSnapshot, DomainError> {
            let map = self.cmd_snapshots.lock().expect("snapshot map mutex");
            map.get(id)
                .cloned()
                .ok_or_else(|| DomainError::CommandNotFound(id.clone()))
        }

        async fn snapshot_shell(
            &self,
            id: &crate::domain::ids::ShellId,
        ) -> Result<OutputSnapshot, DomainError> {
            Err(DomainError::ShellNotFound(id.clone()))
        }
    }

    type UseCaseUnderTest = GetCommandOutputUseCase<DashMapCommandRepo, FakeOutputStream>;

    fn build_use_case() -> (
        UseCaseUnderTest,
        Arc<DashMapCommandRepo>,
        Arc<FakeOutputStream>,
    ) {
        let commands = Arc::new(DashMapCommandRepo::new());
        let streams = Arc::new(FakeOutputStream::new());
        let uc = GetCommandOutputUseCase::new(Arc::clone(&commands), Arc::clone(&streams));
        (uc, commands, streams)
    }

    fn seed_command(repo: &DashMapCommandRepo, id: &str, status: CommandStatus) -> CommandEntity {
        let mut entity = CommandEntity::new(
            CommandId::new(id.to_string()),
            SessionId::new("sess-1".to_string()),
            "echo hi".to_string(),
            Utc::now(),
        );
        entity.status = status;
        if status == CommandStatus::Completed {
            entity.exit_code = Some(0);
        }
        let to_insert = entity.clone();
        let repo_clone = repo.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                repo_clone.insert(to_insert).await.expect("seed insert");
            });
        });
        entity
    }

    fn snapshot_with_streams(stdout: &[u8], stderr: &[u8], last_seq: u64) -> OutputSnapshot {
        OutputSnapshot {
            byte_cursor: 0,
            last_seq,
            stdout: Bytes::copy_from_slice(stdout),
            stderr: Bytes::copy_from_slice(stderr),
        }
    }

    fn base_request(id: &CommandId) -> GetCommandOutputRequest {
        GetCommandOutputRequest {
            command_id: id.clone(),
            wait: false,
            wait_timeout: None,
            max_output_bytes: None,
        }
    }

    fn unwrap_result(r: Result<GetCommandOutputResult, DomainError>) -> GetCommandOutputResult {
        match r {
            Ok(v) => v,
            Err(e) => panic!("unexpected DomainError: {e:?}"),
        }
    }

    // --- Scenario 1: not found ----------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_command_id_returns_command_not_found() {
        let (uc, _commands, _streams) = build_use_case();
        let req = base_request(&CommandId::new("absent".to_string()));
        let err = uc.execute(req).await.expect_err("must fail");
        match err {
            DomainError::CommandNotFound(id) => assert_eq!(id.as_str(), "absent"),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    // --- Scenario 2: no wait, snapshot returned ----------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn no_wait_returns_current_snapshot_with_running_status() {
        let (uc, commands, streams) = build_use_case();
        let entity = seed_command(&commands, "c-1", CommandStatus::Running);
        streams.put_command(
            &entity.id,
            snapshot_with_streams(b"out-so-far", b"err-so-far", 7),
        );
        let res = unwrap_result(uc.execute(base_request(&entity.id)).await);
        assert_eq!(res.command_id, entity.id);
        assert_eq!(res.status, CommandStatus::Running);
        assert_eq!(res.stdout.as_ref(), b"out-so-far");
        assert_eq!(res.stderr.as_ref(), b"err-so-far");
        assert!(!res.timed_out);
        assert_eq!(res.exit_code, None);
        assert_eq!(res.last_seq, 7);
    }

    // --- Scenario 3: wait with already-terminal status returns immediately --

    #[tokio::test(flavor = "multi_thread")]
    async fn wait_short_circuits_when_command_already_completed() {
        let (uc, commands, streams) = build_use_case();
        let entity = seed_command(&commands, "c-done", CommandStatus::Completed);
        streams.put_command(&entity.id, snapshot_with_streams(b"final", b"", 12));
        let mut req = base_request(&entity.id);
        req.wait = true;
        // Even with a generous budget, the loop must not enter sleep.
        req.wait_timeout = Some(Duration::from_secs(60));
        let started = std::time::Instant::now();
        let res = unwrap_result(uc.execute(req).await);
        let elapsed = started.elapsed();
        assert_eq!(res.status, CommandStatus::Completed);
        assert_eq!(res.exit_code, Some(0));
        assert!(!res.timed_out);
        assert!(
            elapsed < Duration::from_millis(500),
            "must short-circuit (got {elapsed:?})"
        );
    }

    // --- Scenario 4: wait observes external completion ---------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn wait_returns_after_command_transitions_to_completed() {
        let (uc, commands, streams) = build_use_case();
        let entity = seed_command(&commands, "c-trans", CommandStatus::Running);
        streams.put_command(&entity.id, snapshot_with_streams(b"final", b"", 5));

        let commands_bg = Arc::clone(&commands);
        let id_bg = entity.id.clone();
        let task = tokio::spawn(async move {
            sleep(Duration::from_millis(120)).await;
            // Flip to completed.
            let mut updated = commands_bg
                .get(&id_bg)
                .await
                .expect("get bg")
                .expect("entity present");
            updated.status = CommandStatus::Completed;
            updated.exit_code = Some(7);
            commands_bg.update(updated).await.expect("update bg");
        });

        let mut req = base_request(&entity.id);
        req.wait = true;
        req.wait_timeout = Some(Duration::from_secs(2));
        let res = unwrap_result(uc.execute(req).await);
        task.await.expect("bg task join");
        assert_eq!(res.status, CommandStatus::Completed);
        assert_eq!(res.exit_code, Some(7));
        assert!(!res.timed_out);
    }

    // --- Scenario 5: wait budget exhausted ---------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn wait_returns_partial_snapshot_when_timeout_fires() {
        let (uc, commands, streams) = build_use_case();
        let entity = seed_command(&commands, "c-stuck", CommandStatus::Running);
        streams.put_command(&entity.id, snapshot_with_streams(b"partial", b"", 3));
        let mut req = base_request(&entity.id);
        req.wait = true;
        req.wait_timeout = Some(Duration::from_millis(120));
        let started = std::time::Instant::now();
        let res = unwrap_result(uc.execute(req).await);
        let elapsed = started.elapsed();
        assert_eq!(res.status, CommandStatus::Running);
        assert!(
            res.timed_out,
            "wait budget exhausted must surface timed_out"
        );
        assert_eq!(res.stdout.as_ref(), b"partial");
        assert!(
            elapsed >= Duration::from_millis(100),
            "must have waited at least the budget (got {elapsed:?})"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "must not exceed the budget by orders of magnitude (got {elapsed:?})"
        );
    }

    // --- Scenario 6: zero budget exits immediately -------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn wait_with_zero_budget_returns_immediately_as_timed_out() {
        let (uc, commands, streams) = build_use_case();
        let entity = seed_command(&commands, "c-zero", CommandStatus::Running);
        streams.put_command(&entity.id, snapshot_with_streams(b"x", b"", 1));
        let mut req = base_request(&entity.id);
        req.wait = true;
        req.wait_timeout = Some(Duration::ZERO);
        let started = std::time::Instant::now();
        let res = unwrap_result(uc.execute(req).await);
        assert_eq!(res.status, CommandStatus::Running);
        assert!(res.timed_out);
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "zero-budget wait must not sleep"
        );
    }

    // --- Scenario 7: head truncation -------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn max_output_bytes_truncates_head_preserving_tail() {
        let (uc, commands, streams) = build_use_case();
        let entity = seed_command(&commands, "c-big", CommandStatus::Completed);
        // 32 bytes; cap to 8 so the returned slice is the last 8.
        streams.put_command(
            &entity.id,
            snapshot_with_streams(b"AAAAAAAAAAAAAAAAAAAAAAAATAILSEEN", b"E", 9),
        );
        let mut req = base_request(&entity.id);
        req.max_output_bytes = Some(8);
        let res = unwrap_result(uc.execute(req).await);
        assert_eq!(res.stdout.len(), 8);
        assert_eq!(res.stdout.as_ref(), b"TAILSEEN");
        assert_eq!(res.stderr.as_ref(), b"E");
    }

    // --- Scenario 8: last_seq propagates -------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn last_seq_is_taken_from_output_snapshot() {
        let (uc, commands, streams) = build_use_case();
        let entity = seed_command(&commands, "c-seq", CommandStatus::Running);
        streams.put_command(&entity.id, snapshot_with_streams(b"data", b"", 4_242));
        let res = unwrap_result(uc.execute(base_request(&entity.id)).await);
        assert_eq!(res.last_seq, 4_242);
    }

    // --- Scenario 9: cancelled status surfaces correctly ----------------

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_command_returns_cancelled_status_and_no_exit_code() {
        let (uc, commands, streams) = build_use_case();
        let entity = seed_command(&commands, "c-cancel", CommandStatus::Cancelled);
        streams.put_command(&entity.id, snapshot_with_streams(b"interrupted", b"", 2));
        let mut req = base_request(&entity.id);
        req.wait = true;
        req.wait_timeout = Some(Duration::from_secs(5));
        let res = unwrap_result(uc.execute(req).await);
        assert_eq!(res.status, CommandStatus::Cancelled);
        assert_eq!(res.exit_code, None);
        assert!(!res.timed_out);
    }

    // --- Scenario 10: head_truncate edge cases ------------------------------

    #[test]
    fn head_truncate_keeps_buffer_when_no_cap() {
        let buf = Bytes::from_static(b"hello world");
        let out = head_truncate(buf.clone(), None);
        assert_eq!(out, buf);
    }

    #[test]
    fn head_truncate_keeps_buffer_when_under_cap() {
        let buf = Bytes::from_static(b"hi");
        let out = head_truncate(buf.clone(), Some(8));
        assert_eq!(out, buf);
    }

    #[test]
    fn head_truncate_drops_head_when_over_cap() {
        let buf = Bytes::from_static(b"abcdefghij");
        let out = head_truncate(buf, Some(4));
        assert_eq!(out.as_ref(), b"ghij");
    }

    #[test]
    fn head_truncate_zero_cap_returns_empty() {
        let buf = Bytes::from_static(b"abcdef");
        let out = head_truncate(buf, Some(0));
        assert!(out.is_empty());
    }

    // --- Scenario 11: terminal-state + evicted snapshot (Bug #2 v4.7.1) ---
    //
    // The russh adapter tears down its internal `CommandRecord` the
    // moment `cancel` is invoked — `OutputStreamPort::snapshot_command`
    // therefore returns `CommandNotFound` even though the repo entity
    // is still alive in `Cancelled`. The use case must NOT translate
    // that into a hard `CommandNotFound` error: it must surface the
    // saved status with empty stdout/stderr so the caller can render
    // the canonical post-cancel block.

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_with_evicted_snapshot_returns_status_with_empty_output() {
        let (uc, commands, _streams) = build_use_case();
        // Seed Cancelled entity, but do NOT put a snapshot — fake
        // returns CommandNotFound, mirroring the russh adapter's
        // post-cancel record teardown.
        let entity = seed_command(&commands, "c-cancel-evicted", CommandStatus::Cancelled);
        let res = unwrap_result(uc.execute(base_request(&entity.id)).await);
        assert_eq!(res.status, CommandStatus::Cancelled);
        assert!(res.stdout.is_empty());
        assert!(res.stderr.is_empty());
        assert_eq!(res.last_seq, 0);
        assert_eq!(res.exit_code, None);
        assert!(!res.timed_out);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn completed_with_evicted_snapshot_returns_status_with_empty_output() {
        let (uc, commands, _streams) = build_use_case();
        let entity = seed_command(&commands, "c-done-evicted", CommandStatus::Completed);
        let res = unwrap_result(uc.execute(base_request(&entity.id)).await);
        assert_eq!(res.status, CommandStatus::Completed);
        assert!(res.stdout.is_empty());
        assert!(res.stderr.is_empty());
        assert_eq!(res.exit_code, Some(0));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_with_evicted_snapshot_returns_status_with_empty_output() {
        let (uc, commands, _streams) = build_use_case();
        let entity = seed_command(&commands, "c-fail-evicted", CommandStatus::Failed);
        let res = unwrap_result(uc.execute(base_request(&entity.id)).await);
        assert_eq!(res.status, CommandStatus::Failed);
        assert!(res.stdout.is_empty());
        assert!(res.stderr.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn running_with_evicted_snapshot_still_propagates_command_not_found() {
        // Sanity check: the lenient fallback only applies to terminal
        // states. A `Running` entity whose snapshot is missing is a
        // genuine inconsistency and must surface as CommandNotFound so
        // the caller can decide whether to retry or surface the error.
        let (uc, commands, _streams) = build_use_case();
        let entity = seed_command(&commands, "c-run-evicted", CommandStatus::Running);
        let mut req = base_request(&entity.id);
        req.wait = false;
        let err = uc.execute(req).await.expect_err("must propagate");
        match err {
            DomainError::CommandNotFound(id) => assert_eq!(id, entity.id),
            other => panic!("expected CommandNotFound, got {other:?}"),
        }
    }
}
