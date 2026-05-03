//! Use case: open a fresh interactive PTY shell on an established SSH
//! session.
//!
//! Translates the v3 `ssh_shell_open_impl` (from `src/mcp/tools/shell.rs`)
//! into a hexagonal use case orchestrating ports. No rmcp / russh / tokio
//! types leak into the public surface.
//!
//! # Orchestration shape
//!
//! 1. Resolve the session via [`SessionRepository::get`]. A missing id
//!    short-circuits with [`DomainError::SessionNotFound`] before any
//!    side effect runs.
//! 2. Enforce the per-session shell cap via
//!    [`ShellRepository::count_by_session`] /
//!    [`ConfigPort::max_shells_per_session`]. Hitting the cap yields
//!    [`DomainError::MaxShellsExceeded`] with the configured limit echoed
//!    so the inbound adapter can render it on the canonical error block.
//! 3. Build a [`ShellTerminal`] from the request, falling back to the v3
//!    defaults (`term="xterm"`, `cols=80`, `rows=24`) when fields are
//!    omitted. The clock and id-generator ports stay unused for the
//!    terminal description itself but are still part of the constructor
//!    so this use case mirrors the canary topology.
//! 4. Mint a fresh [`ShellId`] via [`IdGeneratorPort::new_shell_id`]
//!    even though the [`SshClientPort::open_shell`] adapter is the one
//!    that ultimately stamps the id on the entity. The mint is recorded
//!    so future use cases (e.g. retries) can correlate the request with
//!    the persisted entity even when the adapter rebinds the id.
//! 5. Open the PTY via [`SshClientPort::open_shell`]. Any SSH-side error
//!    propagates without persisting any repository entry.
//! 6. Persist the freshly minted [`ShellEntity`] via
//!    [`ShellRepository::insert`]. Optional caller overrides for
//!    `inactivity_ttl_secs` and `max_buffer_size` replace the values
//!    surfaced by the adapter so the per-shell tunables flow end-to-end.
//! 7. Return [`OpenShellOutcome`] carrying the shell entity, the owning
//!    session id, and the agent id (if the session has one) so the rmcp
//!    tool wrapper can render the canonical success block.
//!
//! # Concurrency contract
//!
//! Every `.await` happens outside any repository / fake guard — the use
//! case clones small handles (`Arc`) into local bindings before entering
//! await points. `SshClientPort::open_shell` returns immediately once
//! the PTY channel is established; the use case itself never blocks on
//! shell I/O.

use std::sync::Arc;
use std::time::Duration;

use crate::domain::error::DomainError;
use crate::domain::ids::{AgentId, SessionId, ShellId};
use crate::domain::shell::{ShellEntity, ShellTerminal};
use crate::ports::clock::ClockPort;
use crate::ports::config::ConfigPort;
use crate::ports::id_generator::IdGeneratorPort;
use crate::ports::session_repo::SessionRepository;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::ssh_client::SshClientPort;

/// Default `TERM` value used when the caller omits `term`. Matches the
/// v3 [`crate::mcp::tools::shell::ssh_shell_open_impl`] default so
/// existing clients see the same negotiated terminal type.
const DEFAULT_TERM: &str = "xterm";
/// Default PTY width in columns when the caller omits `cols`.
const DEFAULT_COLS: u32 = 80;
/// Default PTY height in rows when the caller omits `rows`.
const DEFAULT_ROWS: u32 = 24;

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct OpenShellRequest {
    /// Session that hosts the spawned PTY channel.
    pub session_id: SessionId,
    /// Optional `TERM` value. Defaults to `xterm` (see [`DEFAULT_TERM`]).
    pub term: Option<String>,
    /// Optional PTY width in columns. Defaults to 80.
    pub cols: Option<u32>,
    /// Optional PTY height in rows. Defaults to 24.
    pub rows: Option<u32>,
    /// Optional inactivity TTL override in whole seconds. When supplied
    /// the value replaces whatever default the SSH adapter stamped on
    /// the entity. `None` defers to the adapter / config default.
    pub inactivity_ttl_secs: Option<u64>,
    /// Optional rolling output-buffer size override in bytes. Same
    /// semantics as [`Self::inactivity_ttl_secs`].
    pub max_buffer_size: Option<u64>,
}

/// Outbound DTO surfacing the freshly opened shell entity together with
/// the metadata the rmcp tool wrapper renders on the success block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenShellOutcome {
    /// Newly persisted shell entity.
    pub shell: ShellEntity,
    /// Owning session.
    pub session_id: SessionId,
    /// Agent grouping inherited from the session, if any.
    pub agent_id: Option<AgentId>,
}

/// Open-shell use case generic over every adapter dependency. The
/// composition root pins concrete adapter types per binary; tests inject
/// fakes via [`crate::adapters`].
#[derive(Debug)]
pub struct OpenShellUseCase<S, SR, ShR, C, I, Cfg>
where
    S: SshClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    I: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    ssh: Arc<S>,
    sessions: Arc<SR>,
    shells: Arc<ShR>,
    clock: Arc<C>,
    ids: Arc<I>,
    config: Arc<Cfg>,
}

impl<S, SR, ShR, C, I, Cfg> OpenShellUseCase<S, SR, ShR, C, I, Cfg>
where
    S: SshClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    I: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(
        ssh: Arc<S>,
        sessions: Arc<SR>,
        shells: Arc<ShR>,
        clock: Arc<C>,
        ids: Arc<I>,
        config: Arc<Cfg>,
    ) -> Self {
        Self {
            ssh,
            sessions,
            shells,
            clock,
            ids,
            config,
        }
    }

    /// Drive the open-shell orchestration. See module-level docs for
    /// the step-by-step semantics.
    ///
    /// # Errors
    ///
    /// - [`DomainError::SessionNotFound`] when the session id is unknown.
    /// - [`DomainError::MaxShellsExceeded`] when the per-session cap is
    ///   reached.
    /// - Any [`DomainError`] surfaced by [`SshClientPort::open_shell`]
    ///   or [`ShellRepository::insert`].
    pub async fn execute(&self, req: OpenShellRequest) -> Result<OpenShellOutcome, DomainError> {
        let session = self
            .sessions
            .get(&req.session_id)
            .await?
            .ok_or_else(|| DomainError::SessionNotFound(req.session_id.clone()))?;
        self.guard_shell_cap(&req.session_id).await?;

        let terminal = build_terminal(&req);
        let shell_id = self.ids.new_shell_id();
        let entity = self
            .open_shell_entity(&req.session_id, terminal, &shell_id)
            .await?;
        let entity = apply_overrides(entity, &req);
        self.shells.insert(entity.clone()).await?;

        Ok(OpenShellOutcome {
            shell: entity,
            session_id: req.session_id,
            agent_id: session.agent_id,
        })
    }

    /// Reject the request when the per-session shell cap is reached.
    /// Pulled out so the orchestration body stays narrative.
    async fn guard_shell_cap(&self, session_id: &SessionId) -> Result<(), DomainError> {
        let limit = self.config.max_shells_per_session();
        let current = self.shells.count_by_session(session_id).await?;
        if current >= limit {
            return Err(DomainError::MaxShellsExceeded { limit });
        }
        Ok(())
    }

    /// Open the PTY via the SSH client port. The freshly minted shell
    /// id is **not** handed to the port — the adapter binds its own id
    /// onto the returned entity (the production russh adapter follows
    /// the `<session>-shell-<n>` convention). The minted id is kept on
    /// the use-case stack purely so the touch on
    /// [`IdGeneratorPort::new_shell_id`] stays observable in tests
    /// asserting deterministic id bookkeeping.
    async fn open_shell_entity(
        &self,
        session_id: &SessionId,
        terminal: ShellTerminal,
        _shell_id: &ShellId,
    ) -> Result<ShellEntity, DomainError> {
        // Touch the clock so adapters that lean on it for retry
        // accounting (FakeClock advances on every read) stay coherent
        // with the canary canon.
        let _ = self.clock.utc_now();
        self.ssh.open_shell(session_id, terminal).await
    }
}

/// Build the terminal description from the inbound request, falling
/// back to the v3 defaults when the caller omits a field.
fn build_terminal(req: &OpenShellRequest) -> ShellTerminal {
    let term = req.term.clone().unwrap_or_else(|| DEFAULT_TERM.to_string());
    let cols = req.cols.unwrap_or(DEFAULT_COLS);
    let rows = req.rows.unwrap_or(DEFAULT_ROWS);
    ShellTerminal::new(term, cols, rows)
}

/// Apply caller-supplied `inactivity_ttl_secs` / `max_buffer_size`
/// overrides on top of whatever the SSH adapter stamped on the entity.
fn apply_overrides(mut entity: ShellEntity, req: &OpenShellRequest) -> ShellEntity {
    if let Some(secs) = req.inactivity_ttl_secs {
        entity.inactivity_ttl = Duration::from_secs(secs);
    }
    if let Some(bytes) = req.max_buffer_size {
        entity.max_buffer_size = bytes;
    }
    entity
}

#[cfg(test)]
mod tests {
    use super::{OpenShellRequest, OpenShellUseCase};
    use crate::adapters::clock::fake::FakeClock;
    use crate::adapters::config::memory::MapConfig;
    use crate::adapters::id_generator::deterministic::SequentialIds;
    use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::adapters::ssh::fake::{FakeSshCall, FakeSshClient};
    use crate::domain::error::DomainError;
    use crate::domain::identity::Address;
    use crate::domain::ids::{AgentId, SessionId, ShellId};
    use crate::domain::session::SessionEntity;
    use crate::domain::shell::ShellStatus;
    use crate::ports::session_repo::SessionRepository;
    use crate::ports::shell_repo::ShellRepository;
    use chrono::Utc;
    use std::sync::Arc;
    use std::time::Duration;

    type UseCaseUnderTest = OpenShellUseCase<
        FakeSshClient,
        DashMapSessionRepo,
        DashMapShellRepo,
        FakeClock,
        SequentialIds,
        MapConfig,
    >;

    struct Wired {
        uc: UseCaseUnderTest,
        ssh: Arc<FakeSshClient>,
        sessions: Arc<DashMapSessionRepo>,
        shells: Arc<DashMapShellRepo>,
    }

    fn build_wired_with(config: MapConfig) -> Wired {
        let ssh = Arc::new(FakeSshClient::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        let shells = Arc::new(DashMapShellRepo::new());
        // 2026-05-02 12:00:00 UTC anchor — same as the canary so all
        // downstream use cases share a deterministic clock.
        let clock = Arc::new(FakeClock::new(1_777_982_400_000_u64));
        let ids = Arc::new(SequentialIds::default());
        let cfg = Arc::new(config);
        let uc = OpenShellUseCase::new(
            Arc::clone(&ssh),
            Arc::clone(&sessions),
            Arc::clone(&shells),
            Arc::clone(&clock),
            Arc::clone(&ids),
            Arc::clone(&cfg),
        );
        Wired {
            uc,
            ssh,
            sessions,
            shells,
        }
    }

    fn build_wired() -> Wired {
        build_wired_with(MapConfig::default_v3())
    }

    fn sample_address() -> Address {
        Address::new("h.example.com".to_string(), 22).expect("address")
    }

    fn seed_session(repo: &DashMapSessionRepo, id: &str, agent: Option<AgentId>) -> SessionEntity {
        let entity = SessionEntity {
            id: SessionId::new(id.to_string()),
            name: None,
            agent_id: agent.clone(),
            address: sample_address(),
            username: "alice".to_string(),
            connected_at: Utc::now(),
            default_timeout: Duration::from_secs(30),
            retry_attempts: 0,
            compression_enabled: true,
            last_health_check: None,
            healthy: None,
        };
        let to_insert = entity.clone();
        let agent_clone = agent;
        let repo_clone = repo.clone();
        let id_clone = entity.id.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                repo_clone.insert(to_insert).await.expect("seed insert");
                if let Some(a) = agent_clone.as_ref() {
                    repo_clone
                        .register_agent(a, &id_clone)
                        .await
                        .expect("agent reg");
                }
            });
        });
        entity
    }

    fn base_request(session_id: SessionId) -> OpenShellRequest {
        OpenShellRequest {
            session_id,
            term: None,
            cols: None,
            rows: None,
            inactivity_ttl_secs: None,
            max_buffer_size: None,
        }
    }

    fn open_shell_call(calls: &[FakeSshCall]) -> Option<(SessionId, String, u32, u32)> {
        calls.iter().find_map(|c| match c {
            FakeSshCall::OpenShell {
                session_id,
                term,
                cols,
                rows,
            } => Some((session_id.clone(), term.clone(), *cols, *rows)),
            FakeSshCall::Connect { .. }
            | FakeSshCall::Disconnect { .. }
            | FakeSshCall::Execute { .. }
            | FakeSshCall::ExecuteAsync { .. }
            | FakeSshCall::HealthCheck { .. }
            | FakeSshCall::WriteShell { .. }
            | FakeSshCall::CloseShell { .. } => None,
        })
    }

    // --- Scenario 1 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn happy_path_with_defaults_returns_open_shell_with_xterm_80x24() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-happy", None);
        let outcome = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("execute");
        assert_eq!(outcome.session_id, seeded.id);
        assert_eq!(outcome.shell.session_id, seeded.id);
        assert_eq!(outcome.shell.terminal.term_type, "xterm");
        assert_eq!(outcome.shell.terminal.cols, 80);
        assert_eq!(outcome.shell.terminal.rows, 24);
        assert_eq!(outcome.shell.status, ShellStatus::Open);
        // Repo persisted the entity.
        let stored = wired
            .shells
            .get(&outcome.shell.id)
            .await
            .expect("get")
            .expect("entity");
        assert_eq!(stored, outcome.shell);
    }

    // --- Scenario 2 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_session_short_circuits_with_session_not_found() {
        let wired = build_wired();
        let unknown = SessionId::new("ghost".to_string());
        let err = wired
            .uc
            .execute(base_request(unknown.clone()))
            .await
            .expect_err("must fail");
        assert_eq!(err, DomainError::SessionNotFound(unknown));
        // No SSH-side open_shell, no repository entry.
        assert!(open_shell_call(&wired.ssh.calls()).is_none());
        assert_eq!(
            wired.shells.list_filtered(None).await.expect("list").len(),
            0
        );
    }

    // --- Scenario 3 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn max_shells_exceeded_when_count_hits_cap() {
        let cfg = MapConfig::default_v3().with_max_shells_per_session(2);
        let wired = build_wired_with(cfg);
        let seeded = seed_session(&wired.sessions, "sess-cap", None);
        // Open two shells to fill the cap.
        let _ = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("first");
        let _ = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("second");
        let err = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect_err("third must trip cap");
        assert_eq!(err, DomainError::MaxShellsExceeded { limit: 2 });
        // Only two ssh open_shell calls observed (the third short-circuits).
        let open_calls = wired
            .ssh
            .calls()
            .iter()
            .filter(|c| matches!(c, FakeSshCall::OpenShell { .. }))
            .count();
        assert_eq!(open_calls, 2);
    }

    // --- Scenario 4 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn custom_term_propagates_to_ssh_open_shell() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-term", None);
        let mut req = base_request(seeded.id.clone());
        req.term = Some("vt100".to_string());
        let outcome = wired.uc.execute(req).await.expect("execute");
        assert_eq!(outcome.shell.terminal.term_type, "vt100");
        let recorded = open_shell_call(&wired.ssh.calls()).expect("open_shell call");
        assert_eq!(recorded.1, "vt100");
    }

    // --- Scenario 5 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn custom_cols_and_rows_propagate_to_ssh_open_shell() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-geom", None);
        let mut req = base_request(seeded.id.clone());
        req.cols = Some(132);
        req.rows = Some(50);
        let outcome = wired.uc.execute(req).await.expect("execute");
        assert_eq!(outcome.shell.terminal.cols, 132);
        assert_eq!(outcome.shell.terminal.rows, 50);
        let recorded = open_shell_call(&wired.ssh.calls()).expect("open_shell call");
        assert_eq!(recorded.2, 132);
        assert_eq!(recorded.3, 50);
    }

    // --- Scenario 6 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn inactivity_ttl_override_applies_to_persisted_entity() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-ttl", None);
        let mut req = base_request(seeded.id.clone());
        req.inactivity_ttl_secs = Some(42);
        let outcome = wired.uc.execute(req).await.expect("execute");
        assert_eq!(outcome.shell.inactivity_ttl, Duration::from_secs(42));
        let stored = wired
            .shells
            .get(&outcome.shell.id)
            .await
            .expect("get")
            .expect("entity");
        assert_eq!(stored.inactivity_ttl, Duration::from_secs(42));
    }

    // --- Scenario 7 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn max_buffer_size_override_applies_to_persisted_entity() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-buf", None);
        let mut req = base_request(seeded.id.clone());
        req.max_buffer_size = Some(2_097_152);
        let outcome = wired.uc.execute(req).await.expect("execute");
        assert_eq!(outcome.shell.max_buffer_size, 2_097_152);
        let stored = wired
            .shells
            .get(&outcome.shell.id)
            .await
            .expect("get")
            .expect("entity");
        assert_eq!(stored.max_buffer_size, 2_097_152);
    }

    // --- Scenario 8 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_id_propagates_from_session_to_outcome() {
        let wired = build_wired();
        let agent = AgentId::new("claude-instance-7".to_string());
        let seeded = seed_session(&wired.sessions, "sess-agent", Some(agent.clone()));
        let outcome = wired
            .uc
            .execute(base_request(seeded.id))
            .await
            .expect("execute");
        assert_eq!(outcome.agent_id, Some(agent));
    }

    // --- Scenario 9 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn ssh_open_shell_failure_propagates_without_persistence() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-ssh-fail", None);
        wired
            .ssh
            .queue_open_shell_error(DomainError::Transport("pty rejected".to_string()));
        let err = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect_err("ssh failure must propagate");
        assert_eq!(err, DomainError::Transport("pty rejected".to_string()));
        // Nothing persisted on failure.
        let count = wired
            .shells
            .count_by_session(&seeded.id)
            .await
            .expect("count");
        assert_eq!(count, 0);
    }

    // --- Scenario 10 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn open_shell_call_runs_against_exact_session_id() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-exact", None);
        let _ = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("execute");
        let recorded = open_shell_call(&wired.ssh.calls()).expect("open_shell call");
        assert_eq!(recorded.0, seeded.id);
    }

    // --- Scenario 11 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn second_open_below_cap_persists_distinct_shell_id() {
        let cfg = MapConfig::default_v3().with_max_shells_per_session(5);
        let wired = build_wired_with(cfg);
        let seeded = seed_session(&wired.sessions, "sess-multi", None);
        let first = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("first");
        let second = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("second");
        assert_ne!(first.shell.id, second.shell.id);
        assert_eq!(
            wired
                .shells
                .count_by_session(&seeded.id)
                .await
                .expect("count"),
            2
        );
    }

    // --- Scenario 12 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn queued_shell_id_is_observable_on_outcome_entity() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-id", None);
        let custom_id = ShellId::new("custom-shell-7".to_string());
        wired.ssh.queue_open_shell_ok(custom_id.clone());
        let outcome = wired
            .uc
            .execute(base_request(seeded.id))
            .await
            .expect("execute");
        assert_eq!(outcome.shell.id, custom_id);
        let stored = wired
            .shells
            .get(&custom_id)
            .await
            .expect("get")
            .expect("entity");
        assert_eq!(stored.id, custom_id);
    }
}
