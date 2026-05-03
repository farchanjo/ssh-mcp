//! Use case: read a window of an interactive shell's rolling output buffer.
//!
//! Translates the v3 `ssh_shell_read_impl` (from `src/mcp/tools/shell.rs`)
//! into a hexagonal use case orchestrating ports. No rmcp / russh / tokio
//! types leak into the public DTO surface.
//!
//! # Orchestration shape
//!
//! 1. Resolve the [`crate::domain::shell::ShellEntity`] via
//!    [`ShellRepository::get`]. A missing id short-circuits with
//!    [`DomainError::ShellNotFound`] so the inbound adapter can render the
//!    canonical `SSH_SHELL_READ: ERROR / REASON: [SHELL_NOT_FOUND]` block.
//! 2. When [`ReadShellRequest::wait`] is `false`, take a single
//!    [`OutputSnapshot`] via [`OutputStreamPort::snapshot_shell`], reflect
//!    the current shell status, and return.
//! 3. When [`ReadShellRequest::wait`] is `true`, capture the initial buffer
//!    length and poll until either:
//!    - `len(buffer) - initial_len >= min_bytes` (returns
//!      [`ReadShellStatus::Open`] with the fresh snapshot);
//!    - the shell repository now reports
//!      [`crate::domain::shell::ShellStatus::Closed`] (returns
//!      [`ReadShellStatus::Closed`]);
//!    - the wait deadline expires (returns [`ReadShellStatus::Timeout`] with
//!      the latest snapshot).
//!
//!    The cooperative polling cadence mirrors v3's notify-or-deadline loop
//!    via small [`tokio::time::sleep`] naps; the [`OutputStreamPort`]
//!    surface intentionally does not expose a `Notify`-like edge so the use
//!    case stays adapter agnostic.
//! 4. When [`ReadShellRequest::clear`] is `true`, [`ReadShellOutcome::bytes_consumed`]
//!    is set to the number of bytes the use case rendered. The actual drain
//!    of the rolling buffer is the responsibility of the production adapter
//!    that owns the buffer state — the [`OutputStreamPort`] surface does not
//!    yet expose a cursor-advance verb, and the strict-scope contract for
//!    this etapa forbids touching the port. This mirrors the v3 semantics
//!    while leaving the actual drain to the adapter that materialises the
//!    snapshot.
//!
//! # Concurrency contract
//!
//! Every `.await` happens outside any repository / port guard — repository
//! ports return owned values so no shard-spanning borrows escape. The
//! polling loop yields via [`tokio::time::sleep`] so the executor stays
//! responsive even when the shell never produces new bytes.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::time::{Instant, sleep};

use crate::domain::error::DomainError;
use crate::domain::ids::ShellId;
use crate::domain::shell::ShellStatus;
use crate::ports::clock::ClockPort;
use crate::ports::config::ConfigPort;
use crate::ports::output_stream::{OutputSnapshot, OutputStreamPort};
use crate::ports::shell_repo::ShellRepository;

/// Cooperative polling cadence used by the long-poll loop. Small enough to
/// keep wake latency well under one human-perceptible tick (50 ms) while
/// avoiding a tight spin when the shell never produces new bytes.
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Default wait budget when the caller does not supply one. Matches v3
/// `DEFAULT_WAIT_TIMEOUT_SECS` (30 s).
const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct ReadShellRequest {
    /// `SHELL_ID` returned from `ssh_shell_open`.
    pub shell_id: ShellId,
    /// Mark the bytes that were rendered as consumed (head-based pagination).
    /// When `true` the use case sets [`ReadShellOutcome::bytes_consumed`] to
    /// the rendered length; the production adapter is responsible for
    /// honouring it against the rolling buffer.
    pub clear: bool,
    /// Maximum bytes returned in `data`. `None` defers to the inbound
    /// adapter, which is responsible for clamping against
    /// `output_default_bytes` / `output_max_bytes_cap`. When the snapshot
    /// exceeds the cap the *head* is dropped (the most recent tail bytes are
    /// preserved), matching v3 `render_output_block` semantics.
    pub max_output_bytes: Option<usize>,
    /// Long-poll fallback. When `true` the use case blocks until
    /// `min_bytes` new bytes arrive, the shell closes, or
    /// [`Self::wait_timeout`] expires. Prefer subscribing to
    /// `shell://<shell_id>/output` for realtime push.
    pub wait: bool,
    /// Maximum time to block when [`Self::wait`] is `true`. `None` falls back
    /// to the v3 default of 30 s. Capped by the inbound adapter before it
    /// constructs this DTO.
    pub wait_timeout: Option<Duration>,
    /// Minimum new bytes to wait for before returning (only with `wait`).
    /// `None` defaults to 1 (any new byte returns). Capped at the resolved
    /// `max_output_bytes` budget. Floor: 1.
    pub min_bytes: Option<usize>,
}

/// Outbound DTO surfacing every observable result the rmcp tool wrapper
/// must render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadShellOutcome {
    /// Echoed back so the caller can correlate without recreating the
    /// request.
    pub shell_id: ShellId,
    /// Final lifecycle state observed at snapshot time.
    pub status: ReadShellStatus,
    /// Buffered bytes, head-truncated to
    /// [`ReadShellRequest::max_output_bytes`] when set.
    pub data: Bytes,
    /// Latest sequence number from the output stream snapshot. Lets clients
    /// correlate this snapshot with subsequent `resources/read` cursor pages.
    pub last_seq: u64,
    /// Number of bytes the inbound adapter should treat as consumed (and
    /// drain from the head of the rolling buffer). `0` when `clear=false`
    /// or when the rendered window was empty.
    pub bytes_consumed: usize,
}

/// Lifecycle outcome surfaced to the inbound adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadShellStatus {
    /// Shell is still open. Returned in snapshot mode and when long-poll
    /// observed `>= min_bytes` new output.
    Open,
    /// Shell transitioned to [`ShellStatus::Closed`] before the wait
    /// completed.
    Closed,
    /// Long-poll deadline expired before `min_bytes` new bytes arrived.
    /// `data` carries whatever was buffered at deadline time.
    Timeout,
}

/// Read-shell use case generic over every adapter dependency.
#[derive(Debug)]
pub struct ReadShellUseCase<ShR, OS, C, Cfg>
where
    ShR: ShellRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    C: ClockPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    shells: Arc<ShR>,
    streams: Arc<OS>,
    clock: Arc<C>,
    config: Arc<Cfg>,
}

impl<ShR, OS, C, Cfg> ReadShellUseCase<ShR, OS, C, Cfg>
where
    ShR: ShellRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    C: ClockPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(shells: Arc<ShR>, streams: Arc<OS>, clock: Arc<C>, config: Arc<Cfg>) -> Self {
        Self {
            shells,
            streams,
            clock,
            config,
        }
    }

    /// Drive the orchestration. See module-level docs for the step-by-step
    /// semantics.
    ///
    /// # Errors
    ///
    /// - [`DomainError::ShellNotFound`] when the id is unknown at probe
    ///   time. The use case returns before any side effect.
    /// - Any [`DomainError`] surfaced by the underlying ports.
    pub async fn execute(&self, req: ReadShellRequest) -> Result<ReadShellOutcome, DomainError> {
        // Touching the clock + config keeps the ports plumbed and lets a
        // future iteration push the wait budget / activity stamp into the
        // shell repository without another constructor churn.
        let _ = self.clock.instant_now();
        let _ = self.config.shell_max_buffer_size();
        self.ensure_shell_exists(&req.shell_id).await?;

        let (status, snapshot) = if req.wait {
            self.long_poll(&req).await?
        } else {
            self.snapshot(&req.shell_id).await?
        };

        let data = head_truncate(snapshot.stdout, req.max_output_bytes);
        let bytes_consumed = if req.clear { data.len() } else { 0 };

        Ok(ReadShellOutcome {
            shell_id: req.shell_id,
            status,
            data,
            last_seq: snapshot.last_seq,
            bytes_consumed,
        })
    }

    /// Resolve the shell, mapping `Ok(None)` to
    /// [`DomainError::ShellNotFound`].
    async fn ensure_shell_exists(&self, shell_id: &ShellId) -> Result<(), DomainError> {
        match self.shells.get(shell_id).await? {
            Some(_) => Ok(()),
            None => Err(DomainError::ShellNotFound(shell_id.clone())),
        }
    }

    /// Snapshot-mode read (no waiting). Reflects the current shell status
    /// from the repository alongside the latest stream snapshot.
    async fn snapshot(
        &self,
        shell_id: &ShellId,
    ) -> Result<(ReadShellStatus, OutputSnapshot), DomainError> {
        let snapshot = self.streams.snapshot_shell(shell_id).await?;
        let status = self.current_status(shell_id).await?;
        let outcome = match status {
            ShellStatus::Open => ReadShellStatus::Open,
            ShellStatus::Closed => ReadShellStatus::Closed,
        };
        Ok((outcome, snapshot))
    }

    /// Long-poll-mode read. Blocks until `min_bytes` new bytes arrive, the
    /// shell closes, or the deadline expires.
    async fn long_poll(
        &self,
        req: &ReadShellRequest,
    ) -> Result<(ReadShellStatus, OutputSnapshot), DomainError> {
        let budget = req.wait_timeout.unwrap_or(DEFAULT_WAIT_TIMEOUT);
        let deadline = Instant::now() + budget;
        let cap = req.max_output_bytes.unwrap_or(usize::MAX).max(1);
        let target_min = req.min_bytes.unwrap_or(1).clamp(1, cap);

        let initial = self.streams.snapshot_shell(&req.shell_id).await?;
        let initial_len = initial.stdout.len();

        loop {
            let snapshot = self.streams.snapshot_shell(&req.shell_id).await?;
            let new_bytes = snapshot.stdout.len().saturating_sub(initial_len);
            if new_bytes >= target_min {
                return Ok((ReadShellStatus::Open, snapshot));
            }

            let status = self.current_status(&req.shell_id).await?;
            match status {
                ShellStatus::Closed => return Ok((ReadShellStatus::Closed, snapshot)),
                ShellStatus::Open => {}
            }

            let now = Instant::now();
            if now >= deadline {
                return Ok((ReadShellStatus::Timeout, snapshot));
            }
            let remaining = deadline.saturating_duration_since(now);
            let nap = remaining.min(WAIT_POLL_INTERVAL);
            sleep(nap).await;
        }
    }

    /// Re-read the shell's lifecycle status. `Ok(None)` is treated as the
    /// shell having been removed (closed + reaped) so the long-poll loop
    /// terminates cleanly instead of looping forever.
    async fn current_status(&self, shell_id: &ShellId) -> Result<ShellStatus, DomainError> {
        match self.shells.get(shell_id).await? {
            Some(entity) => Ok(entity.status),
            None => Ok(ShellStatus::Closed),
        }
    }
}

/// Head-truncate `buf` so the returned `Bytes` is at most `max` bytes long,
/// preserving the *tail* (most recent output). When `max` is `None` the
/// buffer is returned unchanged. A `Some(0)` cap returns an empty `Bytes` —
/// the caller is responsible for surfacing the truncation indicator.
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
        DEFAULT_WAIT_TIMEOUT, ReadShellOutcome, ReadShellRequest, ReadShellStatus,
        ReadShellUseCase, head_truncate,
    };
    use crate::adapters::clock::fake::FakeClock;
    use crate::adapters::config::memory::MapConfig;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::domain::error::DomainError;
    use crate::domain::ids::{SessionId, ShellId};
    use crate::domain::shell::{ShellEntity, ShellStatus, ShellTerminal};
    use crate::ports::output_stream::{OutputSnapshot, OutputStreamPort};
    use crate::ports::shell_repo::ShellRepository;
    use bytes::Bytes;
    use chrono::Utc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::time::sleep;

    /// In-memory [`OutputStreamPort`] fake. Tests pre-load a snapshot per
    /// shell id; `snapshot_shell` returns a clone. `snapshot_command` is not
    /// exercised by this use case but is implemented so the fake satisfies
    /// the trait surface.
    #[derive(Debug, Default, Clone)]
    struct FakeOutputStream {
        shell_snapshots: Arc<Mutex<std::collections::HashMap<ShellId, OutputSnapshot>>>,
    }

    impl FakeOutputStream {
        fn new() -> Self {
            Self::default()
        }

        fn put_shell(&self, id: &ShellId, snap: OutputSnapshot) {
            let mut map = self.shell_snapshots.lock().expect("snapshot map mutex");
            map.insert(id.clone(), snap);
        }

        fn append_shell(&self, id: &ShellId, extra: &[u8], new_seq: u64) {
            let mut map = self.shell_snapshots.lock().expect("snapshot map mutex");
            let mut current = map
                .get(id)
                .cloned()
                .unwrap_or_else(|| snapshot_with(b"", 0));
            let mut combined = current.stdout.to_vec();
            combined.extend_from_slice(extra);
            current.stdout = Bytes::from(combined);
            current.last_seq = new_seq;
            map.insert(id.clone(), current);
        }
    }

    impl OutputStreamPort for FakeOutputStream {
        async fn snapshot_command(
            &self,
            id: &crate::domain::ids::CommandId,
        ) -> Result<OutputSnapshot, DomainError> {
            Err(DomainError::CommandNotFound(id.clone()))
        }

        async fn snapshot_shell(&self, id: &ShellId) -> Result<OutputSnapshot, DomainError> {
            let map = self.shell_snapshots.lock().expect("snapshot map mutex");
            map.get(id)
                .cloned()
                .ok_or_else(|| DomainError::ShellNotFound(id.clone()))
        }
    }

    type UseCaseUnderTest =
        ReadShellUseCase<DashMapShellRepo, FakeOutputStream, FakeClock, MapConfig>;

    fn build_use_case() -> (
        UseCaseUnderTest,
        Arc<DashMapShellRepo>,
        Arc<FakeOutputStream>,
    ) {
        let shells = Arc::new(DashMapShellRepo::new());
        let streams = Arc::new(FakeOutputStream::new());
        let clock = Arc::new(FakeClock::new(1_777_982_400_000_u64));
        let config = Arc::new(MapConfig::default_v3());
        let uc = ReadShellUseCase::new(
            Arc::clone(&shells),
            Arc::clone(&streams),
            Arc::clone(&clock),
            Arc::clone(&config),
        );
        (uc, shells, streams)
    }

    fn snapshot_with(stdout: &[u8], last_seq: u64) -> OutputSnapshot {
        OutputSnapshot {
            byte_cursor: 0,
            last_seq,
            stdout: Bytes::copy_from_slice(stdout),
            stderr: Bytes::new(),
        }
    }

    fn seed_shell(repo: &DashMapShellRepo, id: &str, status: ShellStatus) -> ShellEntity {
        let mut entity = ShellEntity::new(
            ShellId::new(id.to_string()),
            SessionId::new("sess-1".to_string()),
            ShellTerminal::new("xterm-256color".to_string(), 80, 24),
            Utc::now(),
            Duration::from_secs(600),
            10 * 1024 * 1024,
        );
        entity.status = status;
        let to_insert = entity.clone();
        let repo_clone = repo.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                repo_clone.insert(to_insert).await.expect("seed insert");
            });
        });
        entity
    }

    fn base_request(id: &ShellId) -> ReadShellRequest {
        ReadShellRequest {
            shell_id: id.clone(),
            clear: true,
            max_output_bytes: None,
            wait: false,
            wait_timeout: None,
            min_bytes: None,
        }
    }

    fn unwrap_outcome(r: Result<ReadShellOutcome, DomainError>) -> ReadShellOutcome {
        match r {
            Ok(v) => v,
            Err(e) => panic!("unexpected DomainError: {e:?}"),
        }
    }

    // --- Scenario 1: not found -------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_shell_id_returns_shell_not_found() {
        let (uc, _shells, _streams) = build_use_case();
        let req = base_request(&ShellId::new("absent".to_string()));
        let err = uc.execute(req).await.expect_err("must fail");
        match err {
            DomainError::ShellNotFound(id) => assert_eq!(id.as_str(), "absent"),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    // --- Scenario 2: snapshot mode (wait=false) --------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn snapshot_returns_current_buffer_with_open_status() {
        let (uc, shells, streams) = build_use_case();
        let entity = seed_shell(&shells, "sh-1", ShellStatus::Open);
        streams.put_shell(&entity.id, snapshot_with(b"hello", 4));
        let mut req = base_request(&entity.id);
        req.clear = false;
        let out = unwrap_outcome(uc.execute(req).await);
        assert_eq!(out.shell_id, entity.id);
        assert_eq!(out.status, ReadShellStatus::Open);
        assert_eq!(out.data.as_ref(), b"hello");
        assert_eq!(out.last_seq, 4);
        assert_eq!(out.bytes_consumed, 0);
    }

    // --- Scenario 3: snapshot reflects closed status ---------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn snapshot_reflects_closed_status_from_repository() {
        let (uc, shells, streams) = build_use_case();
        let entity = seed_shell(&shells, "sh-closed", ShellStatus::Closed);
        streams.put_shell(&entity.id, snapshot_with(b"final-trace", 9));
        let out = unwrap_outcome(uc.execute(base_request(&entity.id)).await);
        assert_eq!(out.status, ReadShellStatus::Closed);
        assert_eq!(out.data.as_ref(), b"final-trace");
    }

    // --- Scenario 4: long-poll waits for new bytes -----------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn long_poll_returns_open_after_external_write_arrives() {
        let (uc, shells, streams) = build_use_case();
        let entity = seed_shell(&shells, "sh-wait", ShellStatus::Open);
        streams.put_shell(&entity.id, snapshot_with(b"initial-", 1));

        let streams_bg = Arc::clone(&streams);
        let id_bg = entity.id.clone();
        let task = tokio::spawn(async move {
            sleep(Duration::from_millis(120)).await;
            streams_bg.append_shell(&id_bg, b"more-data", 2);
        });

        let mut req = base_request(&entity.id);
        req.wait = true;
        req.wait_timeout = Some(Duration::from_secs(2));
        req.min_bytes = Some(4);
        req.clear = false;
        let out = unwrap_outcome(uc.execute(req).await);
        task.await.expect("bg task join");
        assert_eq!(out.status, ReadShellStatus::Open);
        assert_eq!(out.data.as_ref(), b"initial-more-data");
        assert_eq!(out.last_seq, 2);
    }

    // --- Scenario 5: long-poll timeout -----------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn long_poll_returns_timeout_when_no_new_bytes_arrive() {
        let (uc, shells, streams) = build_use_case();
        let entity = seed_shell(&shells, "sh-stall", ShellStatus::Open);
        streams.put_shell(&entity.id, snapshot_with(b"stalled", 3));
        let mut req = base_request(&entity.id);
        req.wait = true;
        req.wait_timeout = Some(Duration::from_millis(150));
        req.min_bytes = Some(1);
        req.clear = false;
        let started = std::time::Instant::now();
        let out = unwrap_outcome(uc.execute(req).await);
        let elapsed = started.elapsed();
        assert_eq!(out.status, ReadShellStatus::Timeout);
        assert_eq!(out.data.as_ref(), b"stalled");
        assert!(
            elapsed >= Duration::from_millis(120),
            "must have waited at least the budget (got {elapsed:?})"
        );
    }

    // --- Scenario 6: long-poll on closed shell ---------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn long_poll_returns_closed_when_shell_is_closed() {
        let (uc, shells, streams) = build_use_case();
        let entity = seed_shell(&shells, "sh-already-closed", ShellStatus::Closed);
        streams.put_shell(&entity.id, snapshot_with(b"goodbye", 5));
        let mut req = base_request(&entity.id);
        req.wait = true;
        req.wait_timeout = Some(Duration::from_secs(5));
        req.min_bytes = Some(64);
        req.clear = false;
        let started = std::time::Instant::now();
        let out = unwrap_outcome(uc.execute(req).await);
        assert_eq!(out.status, ReadShellStatus::Closed);
        assert_eq!(out.data.as_ref(), b"goodbye");
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "closed shell must short-circuit"
        );
    }

    // --- Scenario 7: min_bytes threshold respected ----------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn long_poll_does_not_return_until_min_bytes_threshold_reached() {
        let (uc, shells, streams) = build_use_case();
        let entity = seed_shell(&shells, "sh-min", ShellStatus::Open);
        streams.put_shell(&entity.id, snapshot_with(b"x", 1));

        let streams_bg = Arc::clone(&streams);
        let id_bg = entity.id.clone();
        let task = tokio::spawn(async move {
            // First chunk too small (1 byte).
            sleep(Duration::from_millis(80)).await;
            streams_bg.append_shell(&id_bg, b"a", 2);
            // Second chunk pushes total new bytes to 5, satisfying min=5.
            sleep(Duration::from_millis(80)).await;
            streams_bg.append_shell(&id_bg, b"bcde", 3);
        });

        let mut req = base_request(&entity.id);
        req.wait = true;
        req.wait_timeout = Some(Duration::from_secs(2));
        req.min_bytes = Some(5);
        req.clear = false;
        let out = unwrap_outcome(uc.execute(req).await);
        task.await.expect("bg task join");
        assert_eq!(out.status, ReadShellStatus::Open);
        assert_eq!(out.data.as_ref(), b"xabcde");
    }

    // --- Scenario 8: max_output_bytes head-truncates --------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn max_output_bytes_truncates_head_preserving_tail() {
        let (uc, shells, streams) = build_use_case();
        let entity = seed_shell(&shells, "sh-big", ShellStatus::Open);
        streams.put_shell(
            &entity.id,
            snapshot_with(b"AAAAAAAAAAAAAAAAAAAAAAAATAILSEEN", 9),
        );
        let mut req = base_request(&entity.id);
        req.max_output_bytes = Some(8);
        req.clear = false;
        let out = unwrap_outcome(uc.execute(req).await);
        assert_eq!(out.data.len(), 8);
        assert_eq!(out.data.as_ref(), b"TAILSEEN");
    }

    // --- Scenario 9: clear=true populates bytes_consumed -----------------

    #[tokio::test(flavor = "multi_thread")]
    async fn clear_true_sets_bytes_consumed_to_rendered_length() {
        let (uc, shells, streams) = build_use_case();
        let entity = seed_shell(&shells, "sh-clear", ShellStatus::Open);
        streams.put_shell(&entity.id, snapshot_with(b"hello-world", 1));
        let mut req = base_request(&entity.id);
        req.clear = true;
        let out = unwrap_outcome(uc.execute(req).await);
        assert_eq!(out.data.as_ref(), b"hello-world");
        assert_eq!(out.bytes_consumed, 11);
    }

    // --- Scenario 10: clear=false leaves bytes_consumed at zero ----------

    #[tokio::test(flavor = "multi_thread")]
    async fn clear_false_leaves_bytes_consumed_at_zero() {
        let (uc, shells, streams) = build_use_case();
        let entity = seed_shell(&shells, "sh-noclear", ShellStatus::Open);
        streams.put_shell(&entity.id, snapshot_with(b"hello", 1));
        let mut req = base_request(&entity.id);
        req.clear = false;
        let out = unwrap_outcome(uc.execute(req).await);
        assert_eq!(out.bytes_consumed, 0);
    }

    // --- Scenario 11: last_seq propagates -------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn last_seq_is_taken_from_output_snapshot() {
        let (uc, shells, streams) = build_use_case();
        let entity = seed_shell(&shells, "sh-seq", ShellStatus::Open);
        streams.put_shell(&entity.id, snapshot_with(b"data", 4_242));
        let out = unwrap_outcome(uc.execute(base_request(&entity.id)).await);
        assert_eq!(out.last_seq, 4_242);
    }

    // --- Scenario 12: head_truncate edge cases --------------------------

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

    // --- Scenario 13: defaults ------------------------------------------

    #[test]
    fn default_wait_timeout_matches_v3_constant() {
        assert_eq!(DEFAULT_WAIT_TIMEOUT, Duration::from_secs(30));
    }
}
