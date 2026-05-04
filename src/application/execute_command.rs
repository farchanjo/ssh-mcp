//! Use case: spawn a fire-and-forget async command on an established
//! SSH session.
//!
//! Translates the v3 `ssh_execute_impl` (from `src/mcp/tools/execute.rs`)
//! into a hexagonal use case orchestrating ports. No rmcp / russh / tokio
//! types leak into the public surface.
//!
//! # Orchestration shape
//!
//! 1. Resolve the session via [`SessionRepository::get`]. A missing id
//!    short-circuits with [`DomainError::SessionNotFound`] before any side
//!    effect runs.
//! 2. Enforce the per-session running-command cap via
//!    [`CommandRepository::count_running_by_session`] /
//!    [`ConfigPort::max_commands_per_session`]. Hitting the cap yields
//!    [`DomainError::MaxCommandsExceeded`] with the configured limit echoed
//!    so the inbound adapter can render it on the canonical error block.
//! 3. Mint a fresh [`CommandId`] via [`IdGeneratorPort::new_command_id`].
//! 4. Build a [`CommandEntity`] in [`crate::domain::command::CommandStatus::Running`]
//!    with `started_at` from [`ClockPort::utc_now`] and persist it via
//!    [`CommandRepository::insert`]. Persistence happens *before* the
//!    SSH-side spawn so the repository is always authoritative; if the
//!    spawn fails afterwards the use case best-effort removes the entry
//!    so a retry does not double-count against the cap.
//! 5. Spawn the async command via
//!    [`SshClientPort::execute_async`] with the resolved timeout and PTY
//!    flag. Any SSH-side error rolls the command entity back.
//! 6. Wake any waiting subscriber via
//!    [`SubscriberRegistryPort::poke`] on `command://<id>/output`. The
//!    poke is best-effort: tools that drive the use case without a peer
//!    subscribed (e.g. CLI tooling) get a silent no-op.
//! 7. Return [`ExecuteOutcome`] carrying the freshly minted command id,
//!    the owning session id, the agent id (if the session has one), and
//!    the RFC 3339 wall-clock start timestamp the rmcp tool wrapper
//!    surfaces back to the caller.
//!
//! # Concurrency contract
//!
//! Every `.await` happens outside any repository / fake guard — the use
//! case clones small handles (`Arc`) into local bindings before entering
//! await points. `SshClientPort::execute_async` returns immediately
//! (the underlying russh driver is spawned by the adapter), so the use
//! case itself never blocks on the running command.

use std::sync::Arc;
use std::time::Duration;

use crate::domain::command::{CommandEntity, CommandRequest};
use crate::domain::error::DomainError;
use crate::domain::ids::{AgentId, CommandId, SessionId};
use crate::domain::lifecycle::LifecyclePolicy;
use crate::ports::clock::ClockPort;
use crate::ports::command_repo::CommandRepository;
use crate::ports::config::ConfigPort;
use crate::ports::id_generator::IdGeneratorPort;
use crate::ports::lifecycle_policy::{LifecyclePolicyPort, NoopLifecycle};
use crate::ports::session_repo::SessionRepository;
use crate::ports::ssh_client::SshClientPort;
use crate::ports::subscriber_registry::{ResourceKind, SubscriberRegistryPort};

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct ExecuteRequest {
    /// Session that hosts the spawned channel.
    pub session_id: SessionId,
    /// Verbatim shell command line.
    pub command: String,
    /// Optional override for the per-command timeout. Falls back to
    /// [`ConfigPort::command_timeout`] when absent.
    pub timeout: Option<Duration>,
    /// Whether the channel must allocate a PTY.
    pub use_pty: bool,
    /// v5 Phase 3 — optional lifecycle policy override.
    pub lifecycle_policy: Option<LifecyclePolicy>,
}

/// Outbound DTO surfacing the freshly minted command id and the metadata
/// the rmcp tool wrapper renders on the success block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecuteOutcome {
    /// Command id minted by [`IdGeneratorPort::new_command_id`].
    pub command_id: CommandId,
    /// Owning session.
    pub session_id: SessionId,
    /// Agent grouping inherited from the session, if any.
    pub agent_id: Option<AgentId>,
    /// RFC 3339 wall-clock timestamp the command was started.
    pub started_at: String,
}

/// Execute-command use case generic over every adapter dependency. The
/// composition root pins concrete adapter types per binary; tests inject
/// fakes via [`crate::adapters`].
#[derive(Debug)]
pub struct ExecuteCommandUseCase<S, R, CR, C, I, Cfg, Sub>
where
    S: SshClientPort + Send + Sync,
    R: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    I: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
    Sub: SubscriberRegistryPort + Send + Sync,
{
    ssh: Arc<S>,
    sessions: Arc<R>,
    commands: Arc<CR>,
    clock: Arc<C>,
    ids: Arc<I>,
    config: Arc<Cfg>,
    subscribers: Arc<Sub>,
    /// v5 lifecycle: tracked the moment the command is persisted so the
    /// registry can drive refcount-aware close paths.
    lifecycle: Arc<dyn LifecyclePolicyPort>,
}

impl<S, R, CR, C, I, Cfg, Sub> ExecuteCommandUseCase<S, R, CR, C, I, Cfg, Sub>
where
    S: SshClientPort + Send + Sync,
    R: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    I: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
    Sub: SubscriberRegistryPort + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles. Use
    /// [`Self::with_lifecycle`] to bind the v5 lifecycle adapter post
    /// construction so the constructor stays under the
    /// `too_many_arguments` clippy threshold.
    #[must_use]
    pub fn new(
        ssh: Arc<S>,
        sessions: Arc<R>,
        commands: Arc<CR>,
        clock: Arc<C>,
        ids: Arc<I>,
        config: Arc<Cfg>,
        subscribers: Arc<Sub>,
    ) -> Self {
        Self {
            ssh,
            sessions,
            commands,
            clock,
            ids,
            config,
            subscribers,
            lifecycle: Arc::new(NoopLifecycle),
        }
    }

    /// Builder helper that swaps in the production v5 lifecycle adapter.
    /// Returns `self` so composition can chain it after [`Self::new`].
    #[must_use]
    pub fn with_lifecycle(mut self, lifecycle: Arc<dyn LifecyclePolicyPort>) -> Self {
        self.lifecycle = lifecycle;
        self
    }

    /// Drive the execute orchestration. See module-level docs for the
    /// step-by-step semantics.
    ///
    /// # Errors
    ///
    /// - [`DomainError::SessionNotFound`] when the session id is unknown.
    /// - [`DomainError::MaxCommandsExceeded`] when the per-session running
    ///   cap is reached.
    /// - Any [`DomainError`] surfaced by [`CommandRepository::insert`] or
    ///   [`SshClientPort::execute_async`]; on SSH-side failure the
    ///   freshly persisted command entity is best-effort removed so a
    ///   retry does not double-count against the cap.
    pub async fn execute(&self, req: ExecuteRequest) -> Result<ExecuteOutcome, DomainError> {
        let session = self
            .sessions
            .get(&req.session_id)
            .await?
            .ok_or_else(|| DomainError::SessionNotFound(req.session_id.clone()))?;
        self.guard_command_cap(&req.session_id).await?;

        let command_id = self.ids.new_command_id();
        let started_at = self.clock.utc_now();
        self.persist_running_command(&command_id, &req, started_at)
            .await?;
        self.spawn_or_rollback(&command_id, &req).await?;
        // v5 lifecycle binding: register the freshly spawned command.
        // The policy comes from the inbound DTO when set (Phase 3
        // release_when_no_subs / grace_ms knobs); `None` falls back to
        // [`LifecyclePolicy::default()`] for byte-identical v4 behaviour.
        self.lifecycle.track_resource(
            ResourceKind::Command,
            command_id.as_str(),
            &req.session_id,
            req.lifecycle_policy.unwrap_or_default(),
        );

        self.subscribers
            .poke(ResourceKind::Command, command_id.as_str());
        Ok(ExecuteOutcome {
            command_id,
            session_id: req.session_id,
            agent_id: session.agent_id,
            started_at: started_at.to_rfc3339(),
        })
    }

    /// Persist a fresh `Running` entity for the freshly minted command id
    /// so the per-session cap is always authoritative before the SSH-side
    /// spawn runs.
    async fn persist_running_command(
        &self,
        command_id: &CommandId,
        req: &ExecuteRequest,
        started_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), DomainError> {
        let entity = CommandEntity::new(
            command_id.clone(),
            req.session_id.clone(),
            req.command.clone(),
            started_at,
        );
        self.commands.insert(entity).await
    }

    /// Spawn the async command via [`SshClientPort::execute_async`]. On
    /// SSH-side failure the freshly persisted command entity is best-effort
    /// removed so a retry does not double-count against the cap.
    async fn spawn_or_rollback(
        &self,
        command_id: &CommandId,
        req: &ExecuteRequest,
    ) -> Result<(), DomainError> {
        let timeout = req.timeout.unwrap_or_else(|| self.config.command_timeout());
        let ssh_request = CommandRequest::new(req.session_id.clone(), req.command.clone())
            .with_timeout(timeout)
            .with_pty(req.use_pty);
        if let Err(err) = self
            .ssh
            .execute_async(command_id.clone(), ssh_request)
            .await
        {
            // Storage errors during rollback are swallowed: the SSH error
            // is the actionable signal the caller needs.
            let _ = self.commands.remove(command_id).await;
            return Err(err);
        }
        Ok(())
    }

    /// Reject the request when the per-session running cap is reached.
    /// Pulled out so the orchestration body stays narrative.
    async fn guard_command_cap(&self, session_id: &SessionId) -> Result<(), DomainError> {
        let limit = self.config.max_commands_per_session();
        let running = self.commands.count_running_by_session(session_id).await?;
        if running >= limit {
            return Err(DomainError::MaxCommandsExceeded { limit });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ExecuteCommandUseCase, ExecuteRequest};
    use crate::adapters::clock::fake::FakeClock;
    use crate::adapters::config::memory::MapConfig;
    use crate::adapters::id_generator::deterministic::SequentialIds;
    use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
    use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
    use crate::adapters::ssh::fake::{FakeSshCall, FakeSshClient};
    use crate::domain::command::{CommandEntity, CommandStatus};
    use crate::domain::error::DomainError;
    use crate::domain::identity::Address;
    use crate::domain::ids::{AgentId, CommandId, SessionId};
    use crate::domain::session::SessionEntity;
    use crate::ports::command_repo::CommandRepository;
    use crate::ports::session_repo::SessionRepository;
    use crate::ports::subscriber_registry::{
        ResourceKind, SubscriberRegistryPort, SubscriberSnapshot,
    };
    use chrono::Utc;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Test-only [`SubscriberRegistryPort`] that records every `poke` call.
    /// The H12a use case only calls `poke`; the other methods are stubbed
    /// with deterministic, side-effect-free defaults so any unintended
    /// usage in tests surfaces as wrong behaviour rather than a panic.
    #[derive(Debug, Default)]
    struct RecordingRegistry {
        pokes: Mutex<Vec<(ResourceKind, String)>>,
    }

    impl RecordingRegistry {
        fn new() -> Self {
            Self::default()
        }

        fn pokes(&self) -> Vec<(ResourceKind, String)> {
            self.pokes
                .lock()
                .map_or_else(|p| p.into_inner().clone(), |g| g.clone())
        }
    }

    impl SubscriberRegistryPort for RecordingRegistry {
        fn next_seq(&self, _kind: ResourceKind, _resource_id: &str) -> u64 {
            0
        }

        fn current_seq(&self, _kind: ResourceKind, _resource_id: &str) -> u64 {
            0
        }

        fn poke(&self, kind: ResourceKind, resource_id: &str) {
            if let Ok(mut guard) = self.pokes.lock() {
                guard.push((kind, resource_id.to_string()));
            }
        }

        fn compensate_truncation(&self, _uri: &str, _bytes_dropped: u64) {}

        fn snapshot_subscribers(&self, _uri: &str) -> Vec<SubscriberSnapshot> {
            Vec::new()
        }

        fn peer_byte_cursor(&self, _peer_id: &crate::domain::ids::PeerId, _uri: &str) -> u64 {
            0
        }

        fn advance_peer_byte_cursor(
            &self,
            _peer_id: &crate::domain::ids::PeerId,
            _uri: &str,
            target: u64,
        ) -> u64 {
            target
        }

        fn gc_closed_peers(&self) -> usize {
            0
        }
    }

    type UseCaseUnderTest = ExecuteCommandUseCase<
        FakeSshClient,
        DashMapSessionRepo,
        DashMapCommandRepo,
        FakeClock,
        SequentialIds,
        MapConfig,
        RecordingRegistry,
    >;

    struct Wired {
        uc: UseCaseUnderTest,
        ssh: Arc<FakeSshClient>,
        sessions: Arc<DashMapSessionRepo>,
        commands: Arc<DashMapCommandRepo>,
        registry: Arc<RecordingRegistry>,
    }

    fn build_wired_with(config: MapConfig) -> Wired {
        let ssh = Arc::new(FakeSshClient::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        let commands = Arc::new(DashMapCommandRepo::new());
        // 2026-05-02 12:00:00 UTC — same anchor used by the connect_session
        // canary so timestamps remain deterministic across the suite.
        let clock = Arc::new(FakeClock::new(1_777_982_400_000_u64));
        let ids = Arc::new(SequentialIds::default());
        let cfg = Arc::new(config);
        let registry = Arc::new(RecordingRegistry::new());
        let cascade = crate::adapters::lifecycle::cascade::CascadeCoordinator::new();
        let lifecycle: Arc<dyn crate::ports::lifecycle_policy::LifecyclePolicyPort> =
            crate::adapters::lifecycle::refcount::RefcountedLifecycleAdapter::new(
                cascade,
                Arc::clone(&clock),
            );
        let uc = ExecuteCommandUseCase::new(
            Arc::clone(&ssh),
            Arc::clone(&sessions),
            Arc::clone(&commands),
            Arc::clone(&clock),
            Arc::clone(&ids),
            Arc::clone(&cfg),
            Arc::clone(&registry),
        )
        .with_lifecycle(lifecycle);
        Wired {
            uc,
            ssh,
            sessions,
            commands,
            registry,
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

    fn base_request(session_id: SessionId) -> ExecuteRequest {
        ExecuteRequest {
            session_id,
            command: "ls -la".to_string(),
            timeout: None,
            use_pty: false,
            lifecycle_policy: None,
        }
    }

    fn seed_running_commands(repo: &DashMapCommandRepo, session_id: &SessionId, n: usize) {
        let session = session_id.clone();
        let repo_clone = repo.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                for i in 0..n {
                    let cmd = CommandEntity {
                        id: CommandId::new(format!("seed-{i}")),
                        session_id: session.clone(),
                        command: format!("seeded-{i}"),
                        status: CommandStatus::Running,
                        started_at: Utc::now(),
                        exit_code: None,
                        timed_out: false,
                    };
                    repo_clone.insert(cmd).await.expect("seed insert");
                }
            });
        });
    }

    // --- Scenario 1 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn happy_path_returns_command_id_and_persists_entity() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-happy", None);
        let outcome = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("execute");
        assert_eq!(outcome.command_id.as_str(), "cmd-0");
        assert_eq!(outcome.session_id, seeded.id);
        assert!(outcome.agent_id.is_none());
        assert!(!outcome.started_at.is_empty());
        let stored = wired
            .commands
            .get(&outcome.command_id)
            .await
            .expect("get")
            .expect("entity");
        assert_eq!(stored.status, CommandStatus::Running);
        assert_eq!(stored.command, "ls -la");
        assert_eq!(stored.session_id, seeded.id);
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
        // No SSH-side spawn, no repository entry.
        assert_eq!(wired.ssh.call_count(), 0);
    }

    // --- Scenario 3 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn max_commands_exceeded_when_running_count_hits_cap() {
        // Lower cap so the test runs in milliseconds.
        let cfg = MapConfig::default_v3().with_max_commands_per_session(2);
        let wired = build_wired_with(cfg);
        let seeded = seed_session(&wired.sessions, "sess-cap", None);
        seed_running_commands(&wired.commands, &seeded.id, 2);
        let err = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect_err("cap must trip");
        assert_eq!(err, DomainError::MaxCommandsExceeded { limit: 2 });
        // No execute_async was issued.
        assert!(
            !wired
                .ssh
                .calls()
                .iter()
                .any(|c| matches!(c, FakeSshCall::ExecuteAsync { .. })),
            "no SSH spawn must occur when the cap is hit"
        );
    }

    // --- Scenario 4 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn pty_flag_propagates_to_ssh_execute_async() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-pty", None);
        let mut req = base_request(seeded.id.clone());
        req.use_pty = true;
        let _ = wired.uc.execute(req).await.expect("execute");
        let calls = wired.ssh.calls();
        let executed = calls.iter().find_map(|c| match c {
            FakeSshCall::ExecuteAsync {
                pty,
                command_id,
                session_id,
                ..
            } => Some((*pty, command_id.clone(), session_id.clone())),
            FakeSshCall::Connect { .. }
            | FakeSshCall::Disconnect { .. }
            | FakeSshCall::Execute { .. }
            | FakeSshCall::HealthCheck { .. }
            | FakeSshCall::OpenShell { .. }
            | FakeSshCall::WriteShell { .. }
            | FakeSshCall::CloseShell { .. } => None,
        });
        let (pty, cmd_id, sess_id) = executed.expect("execute_async must be recorded");
        assert!(pty, "pty flag must propagate from request to ssh port");
        assert_eq!(cmd_id.as_str(), "cmd-0");
        assert_eq!(sess_id, seeded.id);
    }

    // --- Scenario 5 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn custom_timeout_propagates_to_ssh_execute_async() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-timeout", None);
        let mut req = base_request(seeded.id.clone());
        req.timeout = Some(Duration::from_secs(7));
        let _ = wired.uc.execute(req).await.expect("execute");
        let calls = wired.ssh.calls();
        let timeout = calls.iter().find_map(|c| match c {
            FakeSshCall::ExecuteAsync { timeout, .. } => Some(*timeout),
            FakeSshCall::Connect { .. }
            | FakeSshCall::Disconnect { .. }
            | FakeSshCall::Execute { .. }
            | FakeSshCall::HealthCheck { .. }
            | FakeSshCall::OpenShell { .. }
            | FakeSshCall::WriteShell { .. }
            | FakeSshCall::CloseShell { .. } => None,
        });
        assert_eq!(timeout, Some(Some(Duration::from_secs(7))));
    }

    // --- Scenario 6 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn default_timeout_falls_back_to_config_command_timeout() {
        let cfg = MapConfig::default_v3().with_command_timeout(Duration::from_secs(45));
        let wired = build_wired_with(cfg);
        let seeded = seed_session(&wired.sessions, "sess-default-timeout", None);
        let _ = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("execute");
        let calls = wired.ssh.calls();
        let timeout = calls.iter().find_map(|c| match c {
            FakeSshCall::ExecuteAsync { timeout, .. } => Some(*timeout),
            FakeSshCall::Connect { .. }
            | FakeSshCall::Disconnect { .. }
            | FakeSshCall::Execute { .. }
            | FakeSshCall::HealthCheck { .. }
            | FakeSshCall::OpenShell { .. }
            | FakeSshCall::WriteShell { .. }
            | FakeSshCall::CloseShell { .. } => None,
        });
        assert_eq!(timeout, Some(Some(Duration::from_secs(45))));
    }

    // --- Scenario 7 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn subscriber_poke_fires_for_command_resource_kind() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-poke", None);
        let outcome = wired
            .uc
            .execute(base_request(seeded.id))
            .await
            .expect("execute");
        let pokes = wired.registry.pokes();
        assert_eq!(pokes.len(), 1);
        assert_eq!(pokes[0].0, ResourceKind::Command);
        assert_eq!(pokes[0].1, outcome.command_id.as_str());
    }

    // --- Scenario 8 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_id_propagates_from_session_to_outcome() {
        let wired = build_wired();
        let agent = AgentId::new("claude-instance-9".to_string());
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
    async fn ssh_execute_async_failure_rolls_back_command_entity() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-ssh-fail", None);
        wired
            .ssh
            .queue_execute_async_error(DomainError::Transport("channel closed".to_string()));
        let err = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect_err("ssh failure must propagate");
        assert_eq!(err, DomainError::Transport("channel closed".to_string()));
        // The freshly minted command id must not linger in the repository
        // so a retry does not double-count against the cap.
        let count = wired
            .commands
            .count_by_session(&seeded.id)
            .await
            .expect("count");
        assert_eq!(count, 0, "failed spawn must not leave a command entry");
        // No subscriber poke on failure either.
        assert!(
            wired.registry.pokes().is_empty(),
            "no poke must fire when SSH spawn fails"
        );
    }

    // --- Scenario 10 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn started_at_field_is_rfc3339_and_matches_clock() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-rfc", None);
        let outcome = wired
            .uc
            .execute(base_request(seeded.id))
            .await
            .expect("execute");
        // FakeClock anchored at 1_777_982_400_000 ms since epoch, which
        // chrono renders as a fixed instant in May 2026 — the exact day is
        // an artefact of the anchor and is verified by FakeClock's own
        // tests; here we only need to pin the year/month so the
        // start-timestamp surfaces deterministic data to the rmcp wrapper.
        assert!(
            outcome.started_at.starts_with("2026-05-"),
            "rfc3339 must reflect FakeClock anchor, got: {}",
            outcome.started_at
        );
        assert!(
            outcome.started_at.ends_with("+00:00") || outcome.started_at.ends_with('Z'),
            "rfc3339 must carry a UTC offset, got: {}",
            outcome.started_at
        );
    }

    // --- Scenario 11 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_uses_session_id_unchanged_in_ssh_request() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-exact", None);
        let _ = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("execute");
        let calls = wired.ssh.calls();
        let session_match = calls.iter().any(|c| match c {
            FakeSshCall::ExecuteAsync { session_id, .. } => session_id == &seeded.id,
            FakeSshCall::Connect { .. }
            | FakeSshCall::Disconnect { .. }
            | FakeSshCall::Execute { .. }
            | FakeSshCall::HealthCheck { .. }
            | FakeSshCall::OpenShell { .. }
            | FakeSshCall::WriteShell { .. }
            | FakeSshCall::CloseShell { .. } => false,
        });
        assert!(
            session_match,
            "execute_async must run against the exact request session id"
        );
    }

    // --- Scenario 12 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn second_execute_below_cap_succeeds_with_distinct_command_id() {
        let cfg = MapConfig::default_v3().with_max_commands_per_session(5);
        let wired = build_wired_with(cfg);
        let seeded = seed_session(&wired.sessions, "sess-multi", None);
        let first = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("first execute");
        let second = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("second execute");
        assert_ne!(first.command_id, second.command_id);
        assert_eq!(first.command_id.as_str(), "cmd-0");
        assert_eq!(second.command_id.as_str(), "cmd-1");
        // Both entities live in the repository.
        assert_eq!(
            wired
                .commands
                .count_by_session(&seeded.id)
                .await
                .expect("count"),
            2
        );
    }
}
