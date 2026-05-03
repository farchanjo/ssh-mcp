//! Use case: cooperatively cancel a running async command.
//!
//! Mirrors the v3 `ssh_cancel_command_impl` (see
//! `src/mcp/tools/execute.rs::ssh_cancel_command_impl` and the
//! `get_running_command` helper in `src/mcp/tools/legacy_helpers.rs`).
//!
//! # Orchestration shape
//!
//! 1. Look up the command snapshot through
//!    [`CommandRepository::get`]. A missing id is the only fatal outcome,
//!    surfaced as [`DomainError::CommandNotFound`] so the inbound adapter
//!    can render the v3 `COMMAND_NOT_FOUND` payload.
//! 2. If the snapshot is anything other than [`CommandStatus::Running`] the
//!    use case returns [`CancelCommandOutcome::NotRunning`] without touching
//!    the SSH port. v3 surfaced this branch as a "noop" success and v4
//!    keeps the same non-error semantics so callers can render the same
//!    `noop` markdown body without distinguishing between repository
//!    misses and benign no-ops.
//! 3. Issue [`SshClientPort::cancel`] to trigger the underlying
//!    cancellation token. The SSH adapter is responsible for
//!    propagating the cancellation to the running channel and for
//!    eventually flipping the [`CommandRepository`] entry to
//!    [`CommandStatus::Cancelled`] (mirrors the v3 `RunningCommand`
//!    cancellation task).
//! 4. Wait up to five seconds for the repository entry to leave the
//!    `Running` state. The poll cadence is fifty milliseconds, matching
//!    the responsiveness of the v3 `watch::Receiver` wakeup loop without
//!    pinning a tokio executor type into the port surface.
//! 5. Sleep an additional one hundred milliseconds after the status
//!    transition to give the SSH side a beat to drain its channel
//!    buffers — the v3 implementation documents this as required to
//!    avoid tripping OpenSSH's `MaxSessions` budget when callers issue
//!    a tight `execute + cancel` burst.
//! 6. Snapshot stdout / stderr through [`OutputStreamPort::snapshot_command`]
//!    so the caller receives the byte ranges captured up to the moment
//!    of the cancellation, then return [`CancelCommandOutcome::Cancelled`].
//!
//! # `max_output_bytes`
//!
//! When [`CancelCommandRequest::max_output_bytes`] is `Some(n)` the
//! returned snapshot is head-truncated so only the trailing `n` bytes of
//! each stream survive (matches the v3
//! `CancelCommandCancelledBuilder` rendering: the *most recent* output
//! is what the operator typically wants to see). The caller is expected
//! to have already clamped the value through the rmcp tool wrapper's
//! caller -> env -> default budget; the use case treats it as a raw
//! byte budget and never validates it against an environment cap.
//!
//! # Concurrency contract
//!
//! Every `.await` happens outside any repository / fake guard — the
//! use case clones small handles ([`Arc`]) into local bindings before
//! entering await points, exactly like the H10 canary.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::time::{Instant, sleep};

use crate::domain::command::CommandStatus;
use crate::domain::error::DomainError;
use crate::domain::ids::CommandId;
use crate::ports::command_repo::CommandRepository;
use crate::ports::output_stream::OutputStreamPort;
use crate::ports::ssh_client::SshClientPort;

/// Maximum amount of time the use case waits for the SSH cancellation to
/// flip the repository entry out of [`CommandStatus::Running`]. Mirrors the
/// v3 `timeout(Duration::from_secs(5), ...)` wrapping the
/// `wait_for_command_completion` helper.
const CANCEL_WAIT_BUDGET: Duration = Duration::from_secs(5);

/// Polling interval used while waiting for the status transition. Set to
/// fifty milliseconds so the use case responds in roughly the same time
/// budget as the v3 `watch::Receiver::changed()` wakeup without taking a
/// dependency on tokio's `watch` channel inside the port surface.
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Fixed pause inserted between the status transition and the snapshot
/// read. Mirrors the v3 `time::sleep(Duration::from_millis(100))` call
/// documented as the OpenSSH `MaxSessions` pacing buffer.
const POST_DRAIN_SLEEP: Duration = Duration::from_millis(100);

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelCommandRequest {
    /// Identifier of the command targeted by the cancellation.
    pub command_id: CommandId,
    /// Optional caller-supplied byte budget. When `Some(n)` each output
    /// stream is head-truncated to its trailing `n` bytes before being
    /// returned in [`CancelCommandOutcome::Cancelled`]. `None` returns
    /// the full snapshot.
    pub max_output_bytes: Option<usize>,
}

/// Outbound DTO surfacing every observable result the rmcp tool wrapper
/// must render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelCommandOutcome {
    /// The command was running when the use case picked it up and was
    /// cancelled successfully.
    Cancelled {
        /// Echo of the targeted command id so the inbound adapter can
        /// render the response without the original DTO.
        command_id: CommandId,
        /// Stdout snapshot at the moment of the cancellation (head-truncated
        /// when [`CancelCommandRequest::max_output_bytes`] is set).
        stdout: Bytes,
        /// Stderr snapshot at the moment of the cancellation (head-truncated
        /// when [`CancelCommandRequest::max_output_bytes`] is set).
        stderr: Bytes,
    },
    /// The command was found but had already left the `Running` state.
    /// The use case treats this as a benign no-op (matches v3
    /// `render_cancel_command_noop`).
    NotRunning {
        /// Echo of the targeted command id.
        command_id: CommandId,
        /// Lifecycle state observed by the use case.
        status: CommandStatus,
    },
}

/// Cancel-command use case generic over every adapter dependency. The
/// composition root pins concrete adapter types per binary; tests inject
/// fakes inside the module's test scope.
#[derive(Debug)]
pub struct CancelCommandUseCase<S, CR, OS>
where
    S: SshClientPort + Send + Sync,
    CR: CommandRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
{
    ssh: Arc<S>,
    commands: Arc<CR>,
    output: Arc<OS>,
}

impl<S, CR, OS> CancelCommandUseCase<S, CR, OS>
where
    S: SshClientPort + Send + Sync,
    CR: CommandRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(ssh: Arc<S>, commands: Arc<CR>, output: Arc<OS>) -> Self {
        Self {
            ssh,
            commands,
            output,
        }
    }

    /// Drive the cancel orchestration. See module-level docs for the
    /// step-by-step semantics.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::CommandNotFound`] when the repository has
    /// no entry for [`CancelCommandRequest::command_id`]. All other
    /// outcomes (including non-running snapshots and SSH-side cancel
    /// failures) are folded into [`CancelCommandOutcome`] variants so
    /// the caller never has to distinguish between transport faults and
    /// benign no-ops at the error layer.
    pub async fn execute(
        &self,
        req: CancelCommandRequest,
    ) -> Result<CancelCommandOutcome, DomainError> {
        let snapshot = self
            .commands
            .get(&req.command_id)
            .await?
            .ok_or_else(|| DomainError::CommandNotFound(req.command_id.clone()))?;
        if snapshot.status != CommandStatus::Running {
            return Ok(CancelCommandOutcome::NotRunning {
                command_id: req.command_id,
                status: snapshot.status,
            });
        }
        // Best-effort: a transport-level failure on the cancel call does
        // not abort the operation. The SSH adapter may still drive the
        // status transition through other paths (e.g. a process exit
        // racing the cancel) and the caller is interested in the final
        // snapshot regardless.
        let _ = self.ssh.cancel(&req.command_id).await;
        self.wait_for_status_transition(&req.command_id).await;
        sleep(POST_DRAIN_SLEEP).await;
        let snap = self.output.snapshot_command(&req.command_id).await?;
        let stdout = truncate_head(snap.stdout, req.max_output_bytes);
        let stderr = truncate_head(snap.stderr, req.max_output_bytes);
        Ok(CancelCommandOutcome::Cancelled {
            command_id: req.command_id,
            stdout,
            stderr,
        })
    }

    /// Poll the repository at [`STATUS_POLL_INTERVAL`] until either the
    /// command leaves [`CommandStatus::Running`] or [`CANCEL_WAIT_BUDGET`]
    /// elapses. Either way the snapshot read still happens — the cancel
    /// pipeline is best-effort by design.
    async fn wait_for_status_transition(&self, command_id: &CommandId) {
        let deadline = Instant::now() + CANCEL_WAIT_BUDGET;
        loop {
            if Self::has_left_running(&self.commands, command_id).await {
                return;
            }
            if Instant::now() >= deadline {
                return;
            }
            sleep(STATUS_POLL_INTERVAL).await;
        }
    }

    /// Single-shot poll on the repository's status field. Repository
    /// faults are swallowed (treated as "still running") so a transient
    /// storage hiccup cannot collapse the wait loop into a busy spin
    /// past the deadline.
    async fn has_left_running(commands: &Arc<CR>, command_id: &CommandId) -> bool {
        match commands.get(command_id).await {
            Ok(Some(entity)) => entity.status != CommandStatus::Running,
            // Removed mid-flight is treated as "no longer running" so
            // the use case stops polling immediately.
            Ok(None) => true,
            Err(_) => false,
        }
    }
}

/// Head-truncate a byte buffer so only the trailing `cap` bytes survive.
/// Returns the original buffer unchanged when `cap` is `None` or larger
/// than the buffer's length.
fn truncate_head(bytes: Bytes, cap: Option<usize>) -> Bytes {
    match cap {
        Some(n) if n < bytes.len() => bytes.slice(bytes.len().saturating_sub(n)..),
        Some(_) | None => bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CANCEL_WAIT_BUDGET, CancelCommandOutcome, CancelCommandRequest, CancelCommandUseCase,
        POST_DRAIN_SLEEP, STATUS_POLL_INTERVAL,
    };
    use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
    use crate::domain::command::{CommandEntity, CommandStatus};
    use crate::domain::error::DomainError;
    use crate::domain::ids::{CommandId, SessionId, ShellId};
    use crate::ports::command_repo::CommandRepository;
    use crate::ports::output_stream::{OutputSnapshot, OutputStreamPort};
    use crate::ports::ssh_client::{CommandHandle, CommandOutcome, SshClientPort};
    use bytes::Bytes;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    // --- Test doubles ----------------------------------------------------
    //
    // The shared `FakeSshClient` adapter does not script `cancel` outcomes
    // and is concurrently extended by sibling H12 use cases; spinning a
    // bespoke double inside the use case's test module keeps this etapa
    // independent of those changes and lets the test scripts wire the
    // cancel side-effect (status transition + output growth) deterministically.

    /// Test [`SshClientPort`] adapter. Records every `cancel` call and
    /// optionally invokes a closure to emulate the SSH adapter's
    /// side-effect on the repository (status transition + output growth).
    type CancelCallback = Box<dyn Fn() + Send + Sync>;

    struct CancellingSsh {
        cancel_calls: Mutex<Vec<CommandId>>,
        cancel_outcome: Mutex<Result<(), DomainError>>,
        on_cancel: Mutex<Option<CancelCallback>>,
    }

    impl CancellingSsh {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                cancel_calls: Mutex::new(Vec::new()),
                cancel_outcome: Mutex::new(Ok(())),
                on_cancel: Mutex::new(None),
            })
        }

        fn set_cancel_outcome(&self, outcome: Result<(), DomainError>) {
            if let Ok(mut slot) = self.cancel_outcome.lock() {
                *slot = outcome;
            }
        }

        fn on_cancel<F>(&self, f: F)
        where
            F: Fn() + Send + Sync + 'static,
        {
            if let Ok(mut slot) = self.on_cancel.lock() {
                *slot = Some(Box::new(f));
            }
        }

        fn cancel_count(&self) -> usize {
            self.cancel_calls.lock().map(|g| g.len()).unwrap_or(0)
        }

        fn cancel_targets(&self) -> Vec<CommandId> {
            self.cancel_calls
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default()
        }
    }

    impl SshClientPort for CancellingSsh {
        async fn connect(
            &self,
            _session_id: SessionId,
            _address: crate::domain::identity::Address,
            _credentials: crate::domain::identity::Credentials,
            _connect_timeout: Duration,
        ) -> Result<crate::domain::session::SessionEntity, DomainError> {
            Err(DomainError::Internal(
                "CancellingSsh::connect unused".to_string(),
            ))
        }

        async fn disconnect(&self, _session_id: &SessionId) -> Result<(), DomainError> {
            Err(DomainError::Internal(
                "CancellingSsh::disconnect unused".to_string(),
            ))
        }

        async fn execute(
            &self,
            _request: crate::domain::command::CommandRequest,
        ) -> Result<CommandOutcome, DomainError> {
            Err(DomainError::Internal(
                "CancellingSsh::execute unused".to_string(),
            ))
        }

        async fn execute_async(
            &self,
            _command_id: CommandId,
            _request: crate::domain::command::CommandRequest,
        ) -> Result<CommandHandle, DomainError> {
            Err(DomainError::Internal(
                "CancellingSsh::execute_async unused".to_string(),
            ))
        }

        async fn cancel(&self, command_id: &CommandId) -> Result<(), DomainError> {
            if let Ok(mut log) = self.cancel_calls.lock() {
                log.push(command_id.clone());
            }
            if let Ok(slot) = self.on_cancel.lock() {
                if let Some(callback) = slot.as_ref() {
                    callback();
                }
            }
            self.cancel_outcome
                .lock()
                .map_or_else(|_| Ok(()), |g| g.clone())
        }

        async fn open_shell(
            &self,
            _session_id: &SessionId,
            _terminal: crate::domain::shell::ShellTerminal,
        ) -> Result<crate::domain::shell::ShellEntity, DomainError> {
            Err(DomainError::Internal(
                "CancellingSsh::open_shell unused".to_string(),
            ))
        }

        async fn write_shell(
            &self,
            _shell_id: &crate::domain::ids::ShellId,
            _bytes: bytes::Bytes,
        ) -> Result<usize, DomainError> {
            Err(DomainError::Internal(
                "CancellingSsh::write_shell unused".to_string(),
            ))
        }

        async fn close_shell(
            &self,
            _shell_id: &crate::domain::ids::ShellId,
        ) -> Result<(), DomainError> {
            Err(DomainError::Internal(
                "CancellingSsh::close_shell unused".to_string(),
            ))
        }

        async fn health_check(&self, _session_id: &SessionId) -> Result<(), DomainError> {
            Err(DomainError::Internal(
                "CancellingSsh::health_check unused".to_string(),
            ))
        }
    }

    /// Test [`OutputStreamPort`] adapter. Stores per-command snapshots in
    /// a shared map; tests script the snapshot returned at read time so
    /// the use case sees the same bytes the SSH adapter would have
    /// captured up to the cancellation point.
    struct ScriptedOutput {
        snapshots: Mutex<Vec<(CommandId, OutputSnapshot)>>,
        default: OutputSnapshot,
    }

    impl ScriptedOutput {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                snapshots: Mutex::new(Vec::new()),
                default: OutputSnapshot {
                    byte_cursor: 0,
                    last_seq: 0,
                    stdout: Bytes::new(),
                    stderr: Bytes::new(),
                },
            })
        }

        fn set(&self, command_id: &CommandId, snap: OutputSnapshot) {
            if let Ok(mut bag) = self.snapshots.lock() {
                bag.retain(|(id, _)| id != command_id);
                bag.push((command_id.clone(), snap));
            }
        }
    }

    impl OutputStreamPort for ScriptedOutput {
        async fn snapshot_command(&self, id: &CommandId) -> Result<OutputSnapshot, DomainError> {
            let stored = self
                .snapshots
                .lock()
                .map(|g| g.iter().find(|(cid, _)| cid == id).map(|(_, s)| s.clone()))
                .unwrap_or(None);
            Ok(stored.unwrap_or_else(|| self.default.clone()))
        }

        async fn snapshot_shell(&self, _id: &ShellId) -> Result<OutputSnapshot, DomainError> {
            Err(DomainError::Internal(
                "ScriptedOutput::snapshot_shell unused".to_string(),
            ))
        }
    }

    // --- Harness ---------------------------------------------------------

    type UseCaseUnderTest = CancelCommandUseCase<CancellingSsh, DashMapCommandRepo, ScriptedOutput>;

    struct Harness {
        uc: UseCaseUnderTest,
        ssh: Arc<CancellingSsh>,
        commands: Arc<DashMapCommandRepo>,
        output: Arc<ScriptedOutput>,
    }

    fn build_harness() -> Harness {
        let ssh = CancellingSsh::new();
        let commands = Arc::new(DashMapCommandRepo::new());
        let output = ScriptedOutput::new();
        let uc =
            CancelCommandUseCase::new(Arc::clone(&ssh), Arc::clone(&commands), Arc::clone(&output));
        Harness {
            uc,
            ssh,
            commands,
            output,
        }
    }

    fn cmd_id(s: &str) -> CommandId {
        CommandId::new(s.to_string())
    }

    fn sample_entity(id: &str, status: CommandStatus) -> CommandEntity {
        let base = CommandEntity::new(
            cmd_id(id),
            SessionId::new("s-1".to_string()),
            "echo hi".to_string(),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        );
        match status {
            CommandStatus::Running => base,
            CommandStatus::Completed => base.complete(0, false),
            CommandStatus::Cancelled => base.cancel(),
            CommandStatus::Failed => base.fail(),
        }
    }

    async fn seed(repo: &DashMapCommandRepo, id: &str, status: CommandStatus) -> CommandEntity {
        let entity = sample_entity(id, status);
        repo.insert(entity.clone()).await.expect("seed");
        entity
    }

    fn req(id: &str, max: Option<usize>) -> CancelCommandRequest {
        CancelCommandRequest {
            command_id: cmd_id(id),
            max_output_bytes: max,
        }
    }

    fn snap_with(stdout: &[u8], stderr: &[u8]) -> OutputSnapshot {
        OutputSnapshot {
            byte_cursor: u64::try_from(stdout.len()).unwrap_or(u64::MAX),
            last_seq: 1,
            stdout: Bytes::copy_from_slice(stdout),
            stderr: Bytes::copy_from_slice(stderr),
        }
    }

    // --- Scenario 1 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_command_id_returns_command_not_found_error() {
        let h = build_harness();
        let err =
            h.uc.execute(req("ghost", None))
                .await
                .expect_err("expected CommandNotFound");
        assert_eq!(err, DomainError::CommandNotFound(cmd_id("ghost")));
        assert_eq!(
            h.ssh.cancel_count(),
            0,
            "no SSH cancel must be issued for an unknown id"
        );
    }

    // --- Scenario 2 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_running_command_yields_cancelled_outcome_with_snapshot() {
        let h = build_harness();
        let _ = seed(&h.commands, "c-1", CommandStatus::Running).await;
        h.output.set(&cmd_id("c-1"), snap_with(b"out", b"err"));
        // Side-effect: the SSH adapter would flip the status to
        // Cancelled. Mirror that here so the wait loop completes
        // promptly.
        let commands_for_cb = Arc::clone(&h.commands);
        h.ssh.on_cancel(move || {
            let repo = Arc::clone(&commands_for_cb);
            tokio::spawn(async move {
                if let Ok(Some(entity)) = repo.get(&cmd_id("c-1")).await {
                    let _ = repo.update(entity.cancel()).await;
                }
            });
        });

        let outcome = h.uc.execute(req("c-1", None)).await.expect("cancel");
        match outcome {
            CancelCommandOutcome::Cancelled {
                command_id,
                stdout,
                stderr,
            } => {
                assert_eq!(command_id, cmd_id("c-1"));
                assert_eq!(stdout.as_ref(), b"out");
                assert_eq!(stderr.as_ref(), b"err");
            }
            CancelCommandOutcome::NotRunning { .. } => {
                panic!("expected Cancelled, got NotRunning")
            }
        }
        assert_eq!(h.ssh.cancel_count(), 1, "exactly one SSH cancel issued");
        assert_eq!(h.ssh.cancel_targets(), vec![cmd_id("c-1")]);
    }

    // --- Scenario 3 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_completed_command_returns_not_running_without_ssh_call() {
        let h = build_harness();
        let _ = seed(&h.commands, "c-done", CommandStatus::Completed).await;
        let outcome = h.uc.execute(req("c-done", None)).await.expect("execute");
        match outcome {
            CancelCommandOutcome::NotRunning { command_id, status } => {
                assert_eq!(command_id, cmd_id("c-done"));
                assert_eq!(status, CommandStatus::Completed);
            }
            CancelCommandOutcome::Cancelled { .. } => {
                panic!("expected NotRunning, got Cancelled")
            }
        }
        assert_eq!(
            h.ssh.cancel_count(),
            0,
            "no SSH cancel must be issued for a completed command"
        );
    }

    // --- Scenario 4 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_failed_command_returns_not_running_without_ssh_call() {
        let h = build_harness();
        let _ = seed(&h.commands, "c-fail", CommandStatus::Failed).await;
        let outcome = h.uc.execute(req("c-fail", None)).await.expect("execute");
        match outcome {
            CancelCommandOutcome::NotRunning { command_id, status } => {
                assert_eq!(command_id, cmd_id("c-fail"));
                assert_eq!(status, CommandStatus::Failed);
            }
            CancelCommandOutcome::Cancelled { .. } => {
                panic!("expected NotRunning, got Cancelled")
            }
        }
        assert_eq!(h.ssh.cancel_count(), 0);
    }

    // --- Scenario 5 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_already_cancelled_command_returns_not_running() {
        let h = build_harness();
        let _ = seed(&h.commands, "c-prev", CommandStatus::Cancelled).await;
        let outcome = h.uc.execute(req("c-prev", None)).await.expect("execute");
        match outcome {
            CancelCommandOutcome::NotRunning { command_id, status } => {
                assert_eq!(command_id, cmd_id("c-prev"));
                assert_eq!(status, CommandStatus::Cancelled);
            }
            CancelCommandOutcome::Cancelled { .. } => {
                panic!("expected NotRunning, got Cancelled")
            }
        }
        assert_eq!(h.ssh.cancel_count(), 0);
    }

    // --- Scenario 6 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn max_output_bytes_truncates_head_of_each_stream() {
        let h = build_harness();
        let _ = seed(&h.commands, "c-big", CommandStatus::Running).await;
        // Twenty-byte stdout, ten-byte stderr; cap at five bytes.
        h.output.set(
            &cmd_id("c-big"),
            snap_with(b"AAAAABBBBBCCCCCDDDDD", b"0123456789"),
        );
        let commands_for_cb = Arc::clone(&h.commands);
        h.ssh.on_cancel(move || {
            let repo = Arc::clone(&commands_for_cb);
            tokio::spawn(async move {
                if let Ok(Some(entity)) = repo.get(&cmd_id("c-big")).await {
                    let _ = repo.update(entity.cancel()).await;
                }
            });
        });
        let outcome = h.uc.execute(req("c-big", Some(5))).await.expect("cancel");
        match outcome {
            CancelCommandOutcome::Cancelled { stdout, stderr, .. } => {
                // Trailing five bytes survive (most recent output).
                assert_eq!(stdout.as_ref(), b"DDDDD");
                assert_eq!(stderr.as_ref(), b"56789");
            }
            CancelCommandOutcome::NotRunning { .. } => panic!("expected Cancelled"),
        }
    }

    // --- Scenario 7 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn max_output_bytes_none_returns_full_snapshot_unchanged() {
        let h = build_harness();
        let _ = seed(&h.commands, "c-full", CommandStatus::Running).await;
        h.output.set(
            &cmd_id("c-full"),
            snap_with(b"complete-stdout", b"complete-stderr"),
        );
        let commands_for_cb = Arc::clone(&h.commands);
        h.ssh.on_cancel(move || {
            let repo = Arc::clone(&commands_for_cb);
            tokio::spawn(async move {
                if let Ok(Some(entity)) = repo.get(&cmd_id("c-full")).await {
                    let _ = repo.update(entity.cancel()).await;
                }
            });
        });
        let outcome = h.uc.execute(req("c-full", None)).await.expect("cancel");
        match outcome {
            CancelCommandOutcome::Cancelled { stdout, stderr, .. } => {
                assert_eq!(stdout.as_ref(), b"complete-stdout");
                assert_eq!(stderr.as_ref(), b"complete-stderr");
            }
            CancelCommandOutcome::NotRunning { .. } => panic!("expected Cancelled"),
        }
    }

    // --- Scenario 8 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn ssh_cancel_failure_is_swallowed_and_snapshot_still_returned() {
        let h = build_harness();
        let _ = seed(&h.commands, "c-err", CommandStatus::Running).await;
        h.output.set(&cmd_id("c-err"), snap_with(b"partial", b""));
        h.ssh
            .set_cancel_outcome(Err(DomainError::Transport("dead".to_string())));
        // Even though the SSH cancel returned an error, the use case
        // still walks the wait loop until the budget elapses (status
        // never flips because no callback flips it).
        let outcome = h.uc.execute(req("c-err", None)).await.expect("cancel");
        match outcome {
            CancelCommandOutcome::Cancelled { stdout, stderr, .. } => {
                assert_eq!(stdout.as_ref(), b"partial");
                assert!(stderr.is_empty());
            }
            CancelCommandOutcome::NotRunning { .. } => {
                panic!("Cancelled variant must be returned regardless of SSH cancel outcome")
            }
        }
        assert_eq!(h.ssh.cancel_count(), 1, "SSH cancel was attempted once");
    }

    // --- Scenario 9 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn snapshot_reflects_state_at_cancel_time() {
        // The snapshot read happens AFTER the wait + post-drain sleep,
        // so the use case must surface whatever the OutputStreamPort
        // returns at that moment — not what the buffer contained when
        // the cancel was issued.
        let h = build_harness();
        let _ = seed(&h.commands, "c-grow", CommandStatus::Running).await;
        h.output.set(&cmd_id("c-grow"), snap_with(b"early", b""));
        let commands_for_cb = Arc::clone(&h.commands);
        let output_for_cb = Arc::clone(&h.output);
        h.ssh.on_cancel(move || {
            let repo = Arc::clone(&commands_for_cb);
            let out = Arc::clone(&output_for_cb);
            tokio::spawn(async move {
                if let Ok(Some(entity)) = repo.get(&cmd_id("c-grow")).await {
                    out.set(&cmd_id("c-grow"), snap_with(b"early-then-late", b"!"));
                    let _ = repo.update(entity.cancel()).await;
                }
            });
        });
        let outcome = h.uc.execute(req("c-grow", None)).await.expect("cancel");
        match outcome {
            CancelCommandOutcome::Cancelled { stdout, stderr, .. } => {
                assert_eq!(
                    stdout.as_ref(),
                    b"early-then-late",
                    "snapshot must reflect the buffer at read time, not at cancel time"
                );
                assert_eq!(stderr.as_ref(), b"!");
            }
            CancelCommandOutcome::NotRunning { .. } => panic!("expected Cancelled"),
        }
    }

    // --- Scenario 10 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn timing_constants_match_v3_pacing_invariants() {
        // Locks the v3-derived pacing constants so accidental drift
        // (e.g. dropping the post-drain sleep or shrinking the wait
        // budget) shows up as a unit-test failure rather than a subtle
        // OpenSSH MaxSessions regression in production.
        assert_eq!(CANCEL_WAIT_BUDGET, Duration::from_secs(5));
        assert_eq!(STATUS_POLL_INTERVAL, Duration::from_millis(50));
        assert_eq!(POST_DRAIN_SLEEP, Duration::from_millis(100));
    }
}
