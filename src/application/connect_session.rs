//! Use case: connect to an SSH server (or reuse an existing session).
//!
//! Translates the v3 `ssh_connect_impl` (from `src/mcp/tools/connection.rs`)
//! into a hexagonal use case orchestrating ports. No rmcp / russh / tokio
//! types leak into the public surface.
//!
//! # Orchestration shape
//!
//! 1. If [`ConnectRequest::explicit_session_id`] is supplied, probe its
//!    health via [`SshClientPort::health_check`]. On success the existing
//!    entity is returned as [`ConnectOutcome::Reused`]; on failure the dead
//!    session is purged (disconnect + repository remove) and the request
//!    falls through to a fresh connect.
//! 2. Look up identity matches via [`SessionRepository::list`] filtered
//!    through [`SessionEntity::matches_identity`]. The session repository
//!    port intentionally does not expose `find_by_identity` (see H4 audit
//!    notes), so the use case loops in-memory; for the small N a session
//!    repo holds in practice this is well within budget.
//! 3. Apply [`ReusePolicy`]:
//!    - [`ReusePolicy::ForceNew`] skips identity matching entirely.
//!    - [`ReusePolicy::Auto`] returns the most-recent healthy match as
//!      [`ConnectOutcome::Reused`] when one exists.
//!    - [`ReusePolicy::Suggest`] returns every healthy match as
//!      [`ConnectOutcome::Suggested`] when at least one exists.
//! 4. Unhealthy matches are disconnected (best-effort) and counted in the
//!    `replaced` field of [`ConnectOutcome::Connected`].
//! 5. A new connection is opened via [`SshClientPort::connect`] with the
//!    caller-supplied or config-default timeout. The session entity is
//!    augmented with the optional name / agent id / persistent flag and
//!    persisted via [`SessionRepository::insert`]. When an agent id is
//!    present the secondary index is updated via
//!    [`SessionRepository::register_agent`].
//!
//! # Concurrency contract
//!
//! Every `.await` happens outside any repository / fake guard — the use
//! case clones small handles (`Arc`) into local bindings before entering
//! await points. The `connect`/`health_check` ports return owned values so
//! no shard-spanning borrows escape.

use std::sync::Arc;
use std::time::Duration;

use crate::domain::error::DomainError;
use crate::domain::identity::{Address, Credentials};
use crate::domain::ids::{AgentId, SessionId};
use crate::domain::policy::ReusePolicy;
use crate::domain::session::SessionEntity;
use crate::ports::clock::ClockPort;
use crate::ports::config::ConfigPort;
use crate::ports::id_generator::IdGeneratorPort;
use crate::ports::session_repo::SessionRepository;
use crate::ports::ssh_client::SshClientPort;

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct ConnectRequest {
    /// Optional caller-supplied session id used for the explicit-reuse fast
    /// path. When the id resolves to a healthy session the use case
    /// short-circuits and returns [`ConnectOutcome::Reused`].
    pub explicit_session_id: Option<SessionId>,
    /// Target SSH endpoint.
    pub address: Address,
    /// Login username surfaced into the [`SessionEntity`]. Must match the
    /// username embedded in [`Self::credentials`].
    pub username: String,
    /// Authentication material handed straight to the SSH client port.
    pub credentials: Credentials,
    /// Optional override for the connect timeout. Falls back to
    /// [`ConfigPort::connect_timeout`] when absent.
    pub timeout_secs: Option<u64>,
    /// Optional override for the maximum retry budget. Surfaced into the
    /// resolved configuration but not consumed by the use case directly —
    /// the SSH client adapter is responsible for honouring it. Recorded so
    /// that future use cases (e.g. a `reconnect_session`) can reuse the
    /// same DTO without losing the override.
    pub max_retries: Option<u32>,
    /// Optional override for the initial retry delay. Same notes as
    /// [`Self::max_retries`].
    pub retry_delay_ms: Option<u64>,
    /// Optional override for SSH zlib compression. `None` defers to
    /// [`ConfigPort::compression_enabled`].
    pub compress: Option<bool>,
    /// Optional human-friendly label persisted on the session entity.
    pub name: Option<String>,
    /// `true` when the session must survive the inactivity sweeper.
    pub persistent: bool,
    /// Optional agent grouping. Persisted on the entity and registered in
    /// the secondary index when present.
    pub agent_id: Option<AgentId>,
    /// Reuse strategy applied when identity matches exist.
    pub reuse: ReusePolicy,
}

/// Outbound DTO surfacing every observable result the rmcp tool wrapper
/// must render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectOutcome {
    /// A fresh session was opened. `replaced` counts the unhealthy identity
    /// matches that were disconnected before the new connection. `retries`
    /// echoes the number of retry attempts the SSH client adapter
    /// consumed during the handshake.
    Connected {
        /// Newly persisted session entity.
        session: SessionEntity,
        /// Count of unhealthy matches purged before the fresh connect.
        replaced: usize,
        /// Retry attempts consumed by the SSH client adapter.
        retries: u32,
    },
    /// An existing session was reused (either via explicit id or via the
    /// [`ReusePolicy::Auto`] path).
    Reused {
        /// The reused session entity, with its `last_health_check` /
        /// `healthy` fields refreshed.
        session: SessionEntity,
    },
    /// One or more identity matches were found under
    /// [`ReusePolicy::Suggest`]; the caller decides whether to reuse one.
    Suggested {
        /// Healthy matches, newest first.
        matches: Vec<SessionEntity>,
    },
}

/// Connect-session use case generic over every adapter dependency. The
/// composition root pins concrete adapter types per binary; tests inject
/// fakes via [`crate::adapters`].
#[derive(Debug)]
pub struct ConnectSessionUseCase<S, R, C, I, Cfg>
where
    S: SshClientPort + Send + Sync,
    R: SessionRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    I: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    ssh: Arc<S>,
    sessions: Arc<R>,
    clock: Arc<C>,
    ids: Arc<I>,
    config: Arc<Cfg>,
}

impl<S, R, C, I, Cfg> ConnectSessionUseCase<S, R, C, I, Cfg>
where
    S: SshClientPort + Send + Sync,
    R: SessionRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    I: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(
        ssh: Arc<S>,
        sessions: Arc<R>,
        clock: Arc<C>,
        ids: Arc<I>,
        config: Arc<Cfg>,
    ) -> Self {
        Self {
            ssh,
            sessions,
            clock,
            ids,
            config,
        }
    }

    /// Drive the connect orchestration. See module-level docs for the
    /// step-by-step semantics.
    ///
    /// # Errors
    ///
    /// Propagates any [`DomainError`] returned by the underlying ports
    /// (chiefly `ConnectFailed`, `Auth`, `Storage`).
    pub async fn execute(&self, req: ConnectRequest) -> Result<ConnectOutcome, DomainError> {
        if let Some(sid) = req.explicit_session_id.clone() {
            if let Some(reused) = self.try_explicit_reuse(&sid).await? {
                return Ok(ConnectOutcome::Reused { session: reused });
            }
        }
        let probed = self.probe_for_policy(&req).await?;
        if let Some(short_circuit) = decide_reuse(&probed, req.reuse) {
            return Ok(short_circuit);
        }
        self.create_new_connection(req, probed.replaced).await
    }

    /// Identity probing dispatcher. Skipped entirely for
    /// [`ReusePolicy::ForceNew`] so `Connected { replaced: 0, .. }` matches
    /// the v3 semantics.
    async fn probe_for_policy(&self, req: &ConnectRequest) -> Result<IdentityMatches, DomainError> {
        match req.reuse {
            ReusePolicy::ForceNew => Ok(IdentityMatches::default()),
            ReusePolicy::Auto | ReusePolicy::Suggest => {
                self.probe_identity_matches(&req.address, &req.username)
                    .await
            }
        }
    }

    /// Probe an explicit session id. Returns:
    ///
    /// - `Ok(Some(entity))` when the session exists and the health probe
    ///   succeeded; the entity reflects the refreshed health stamps.
    /// - `Ok(None)` when the session is unknown OR the probe failed (in
    ///   which case the session is removed + disconnected before falling
    ///   through).
    async fn try_explicit_reuse(
        &self,
        sid: &SessionId,
    ) -> Result<Option<SessionEntity>, DomainError> {
        let Some(existing) = self.sessions.get(sid).await? else {
            return Ok(None);
        };
        let probe = self.ssh.health_check(sid).await;
        if probe.is_err() {
            self.purge_dead_session(sid).await;
            return Ok(None);
        }
        let now = self.clock.utc_now();
        self.sessions.update_health(sid, now, true).await?;
        Ok(Some(existing.with_health(now, true)))
    }

    /// Best-effort cleanup of a session known to be dead. Disconnect
    /// failures (the session may already be gone on the SSH side) and
    /// repository churn are swallowed; the use case must always be free to
    /// fall through to a fresh connect.
    async fn purge_dead_session(&self, sid: &SessionId) {
        let _ = self.ssh.disconnect(sid).await;
        if let Ok(Some(removed)) = self.sessions.remove(sid).await {
            if let Some(agent_id) = removed.agent_id.as_ref() {
                let _ = self.sessions.unregister_agent(agent_id, sid).await;
            }
        }
    }

    /// Walk every stored session, run a health probe per identity match,
    /// disconnect the unhealthy ones, and return the survivors sorted
    /// newest-first.
    async fn probe_identity_matches(
        &self,
        address: &Address,
        username: &str,
    ) -> Result<IdentityMatches, DomainError> {
        let candidates = self.collect_identity_candidates(address, username).await?;
        let mut healthy: Vec<SessionEntity> = Vec::with_capacity(candidates.len());
        let mut replaced = 0_usize;
        for candidate in candidates {
            let probe = self.ssh.health_check(&candidate.id).await;
            if probe.is_err() {
                self.purge_dead_session(&candidate.id).await;
                replaced = replaced.saturating_add(1);
                continue;
            }
            let now = self.clock.utc_now();
            self.sessions
                .update_health(&candidate.id, now, true)
                .await?;
            healthy.push(candidate.with_health(now, true));
        }
        // Newest first matches the v3 `evaluate_identity_matches` ordering
        // so `Auto` returns the most recently connected session.
        healthy.sort_by_key(|entry| std::cmp::Reverse(entry.connected_at));
        Ok(IdentityMatches { healthy, replaced })
    }

    /// Filter every stored session through [`SessionEntity::matches_identity`].
    async fn collect_identity_candidates(
        &self,
        address: &Address,
        username: &str,
    ) -> Result<Vec<SessionEntity>, DomainError> {
        let all = self.sessions.list().await?;
        Ok(all
            .into_iter()
            .filter(|s| s.matches_identity(address, username))
            .collect())
    }

    /// Open a fresh SSH connection, persist the resulting entity, and
    /// register it under the optional agent id.
    async fn create_new_connection(
        &self,
        req: ConnectRequest,
        replaced: usize,
    ) -> Result<ConnectOutcome, DomainError> {
        let connect_timeout = req
            .timeout_secs
            .map_or_else(|| self.config.connect_timeout(), Duration::from_secs);
        let session_id = self.ids.new_session_id();
        let entity = self
            .ssh
            .connect(
                session_id.clone(),
                req.address.clone(),
                req.credentials.clone(),
                connect_timeout,
            )
            .await?;
        let enriched = enrich_entity(entity, &req);
        let retries = enriched.retry_attempts;
        self.persist_session(&enriched).await?;
        Ok(ConnectOutcome::Connected {
            session: enriched,
            replaced,
            retries,
        })
    }

    /// Persist the freshly minted entity and update the agent secondary
    /// index when an agent id is present. Failure to insert rolls the
    /// SSH-side connection back so the SSH adapter and the repository stay
    /// in sync.
    async fn persist_session(&self, entity: &SessionEntity) -> Result<(), DomainError> {
        let insert_outcome = self.sessions.insert(entity.clone()).await;
        if let Err(err) = insert_outcome {
            let _ = self.ssh.disconnect(&entity.id).await;
            return Err(err);
        }
        if let Some(agent_id) = entity.agent_id.as_ref() {
            self.sessions.register_agent(agent_id, &entity.id).await?;
        }
        Ok(())
    }
}

/// Outcome of probing every identity match.
#[derive(Debug, Default)]
struct IdentityMatches {
    /// Healthy matches, newest first.
    healthy: Vec<SessionEntity>,
    /// Number of unhealthy matches that were disconnected.
    replaced: usize,
}

/// Decorate the entity returned by the SSH client port with the request's
/// optional metadata fields (`name`, `agent_id`). The SSH adapter doesn't
/// know about these — they are use-case-level concerns.
fn enrich_entity(mut entity: SessionEntity, req: &ConnectRequest) -> SessionEntity {
    entity.name.clone_from(&req.name);
    entity.agent_id.clone_from(&req.agent_id);
    if let Some(compress) = req.compress {
        entity.compression_enabled = compress;
    }
    entity
}

/// Apply the reuse policy to already-probed identity matches. Returns
/// `Some(outcome)` to short-circuit the caller, `None` to fall through to a
/// fresh connect.
fn decide_reuse(probed: &IdentityMatches, policy: ReusePolicy) -> Option<ConnectOutcome> {
    if probed.healthy.is_empty() {
        return None;
    }
    match policy {
        ReusePolicy::Auto => probed
            .healthy
            .first()
            .cloned()
            .map(|session| ConnectOutcome::Reused { session }),
        ReusePolicy::Suggest => Some(ConnectOutcome::Suggested {
            matches: probed.healthy.clone(),
        }),
        ReusePolicy::ForceNew => {
            // Unreachable in practice: ForceNew skipped the lookup so
            // `probed.healthy` is always empty here. Returning `None`
            // keeps the caller's fall-through behaviour without a panic.
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectOutcome, ConnectRequest, ConnectSessionUseCase};
    use crate::adapters::clock::fake::FakeClock;
    use crate::adapters::config::memory::MapConfig;
    use crate::adapters::id_generator::deterministic::SequentialIds;
    use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
    use crate::adapters::ssh::fake::{FakeSshCall, FakeSshClient};
    use crate::domain::error::DomainError;
    use crate::domain::identity::{Address, Credentials};
    use crate::domain::ids::{AgentId, SessionId};
    use crate::domain::policy::ReusePolicy;
    use crate::domain::session::SessionEntity;
    use crate::ports::session_repo::SessionRepository;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use std::time::Duration;

    type UseCaseUnderTest = ConnectSessionUseCase<
        FakeSshClient,
        DashMapSessionRepo,
        FakeClock,
        SequentialIds,
        MapConfig,
    >;

    fn build_use_case() -> (
        UseCaseUnderTest,
        Arc<FakeSshClient>,
        Arc<DashMapSessionRepo>,
        Arc<FakeClock>,
    ) {
        let ssh = Arc::new(FakeSshClient::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        // 2026-05-02 12:00:00 UTC ≈ 1_777_982_400_000 ms since epoch.
        let clock = Arc::new(FakeClock::new(1_777_982_400_000_u64));
        let ids = Arc::new(SequentialIds::default());
        let config = Arc::new(MapConfig::default_v3());
        let uc = ConnectSessionUseCase::new(
            Arc::clone(&ssh),
            Arc::clone(&sessions),
            Arc::clone(&clock),
            Arc::clone(&ids),
            Arc::clone(&config),
        );
        (uc, ssh, sessions, clock)
    }

    fn sample_address() -> Address {
        Address::new("h.example.com".to_string(), 22).expect("address")
    }

    fn sample_credentials() -> Credentials {
        Credentials::Password {
            username: "alice".to_string(),
            password: "x".to_string(),
        }
    }

    fn base_request() -> ConnectRequest {
        ConnectRequest {
            explicit_session_id: None,
            address: sample_address(),
            username: "alice".to_string(),
            credentials: sample_credentials(),
            timeout_secs: None,
            max_retries: None,
            retry_delay_ms: None,
            compress: None,
            name: None,
            persistent: false,
            agent_id: None,
            reuse: ReusePolicy::Suggest,
        }
    }

    fn seed_existing(repo: &DashMapSessionRepo, id: &str, ts_year: i32) -> SessionEntity {
        let entity = SessionEntity {
            id: SessionId::new(id.to_string()),
            name: Some("seeded".to_string()),
            agent_id: None,
            address: sample_address(),
            username: "alice".to_string(),
            connected_at: Utc.with_ymd_and_hms(ts_year, 1, 1, 0, 0, 0).unwrap(),
            default_timeout: Duration::from_secs(30),
            retry_attempts: 0,
            compression_enabled: true,
            last_health_check: None,
            healthy: None,
        };
        let blocking = entity.clone();
        let repo_clone = repo.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                repo_clone.insert(blocking).await.expect("seed insert");
            });
        });
        entity
    }

    // --- Scenario 1 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn fresh_connect_no_existing_no_explicit_yields_connected_replaced_zero() {
        let (uc, ssh, repo, _clock) = build_use_case();
        let outcome = uc.execute(base_request()).await.expect("connect");
        match outcome {
            ConnectOutcome::Connected {
                session,
                replaced,
                retries,
            } => {
                assert_eq!(session.id.as_str(), "sess-0");
                assert_eq!(replaced, 0);
                assert_eq!(retries, 0);
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        // Repo persisted exactly one session.
        let stored = repo.list().await.expect("list");
        assert_eq!(stored.len(), 1);
        // SSH adapter saw exactly one connect, no health probes (empty repo).
        let calls = ssh.calls();
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0], FakeSshCall::Connect { .. }));
    }

    // --- Scenario 2 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn explicit_session_id_when_healthy_returns_reused() {
        let (uc, ssh, _repo, _clock) = build_use_case();
        let seeded = seed_existing(&_repo, "explicit-1", 2025);
        ssh.queue_health_ok();
        let mut req = base_request();
        req.explicit_session_id = Some(seeded.id.clone());
        let outcome = uc.execute(req).await.expect("execute");
        match outcome {
            ConnectOutcome::Reused { session } => {
                assert_eq!(session.id.as_str(), "explicit-1");
                assert_eq!(session.healthy, Some(true));
                assert!(session.last_health_check.is_some());
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        // Health probe but no fresh connect.
        let calls = ssh.calls();
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0], FakeSshCall::HealthCheck { .. }));
    }

    // --- Scenario 3 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn explicit_session_id_when_dead_falls_through_to_fresh_connect() {
        let (uc, ssh, repo, _clock) = build_use_case();
        let seeded = seed_existing(&repo, "explicit-dead", 2025);
        // Explicit health probe fails.
        ssh.queue_health_fail(DomainError::Transport("dead".to_string()));
        // After purge the identity-match scan also runs; probed list will be
        // empty because the seeded session was removed. The fresh connect
        // queue defaults to OK.
        let mut req = base_request();
        req.explicit_session_id = Some(seeded.id.clone());
        let outcome = uc.execute(req).await.expect("execute");
        match outcome {
            ConnectOutcome::Connected {
                session,
                replaced,
                retries: _,
            } => {
                assert_ne!(session.id.as_str(), "explicit-dead");
                assert_eq!(replaced, 0);
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        // Sequence: HealthCheck (failed) -> Disconnect -> Connect.
        let calls = ssh.calls();
        let kinds: Vec<&str> = calls
            .iter()
            .map(|c| match c {
                FakeSshCall::Connect { .. } => "connect",
                FakeSshCall::Disconnect { .. } => "disconnect",
                FakeSshCall::Execute { .. } => "execute",
                FakeSshCall::ExecuteAsync { .. } => "execute_async",
                FakeSshCall::HealthCheck { .. } => "health",
            })
            .collect();
        assert_eq!(kinds, vec!["health", "disconnect", "connect"]);
        // Old session removed.
        assert!(
            repo.get(&seeded.id).await.expect("get").is_none(),
            "dead session must have been purged"
        );
    }

    // --- Scenario 4 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn identity_match_with_auto_and_healthy_yields_reused() {
        let (uc, ssh, repo, _clock) = build_use_case();
        let seeded = seed_existing(&repo, "match-1", 2025);
        ssh.queue_health_ok();
        let mut req = base_request();
        req.reuse = ReusePolicy::Auto;
        let outcome = uc.execute(req).await.expect("execute");
        match outcome {
            ConnectOutcome::Reused { session } => {
                assert_eq!(session.id, seeded.id);
                assert_eq!(session.healthy, Some(true));
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        // No fresh connect was opened.
        assert!(
            !ssh.calls()
                .iter()
                .any(|c| matches!(c, FakeSshCall::Connect { .. })),
            "Auto reuse must not open a new SSH connection"
        );
    }

    // --- Scenario 5 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn identity_match_with_suggest_yields_suggested_with_matches() {
        let (uc, ssh, repo, _clock) = build_use_case();
        let _ = seed_existing(&repo, "match-1", 2025);
        let _ = seed_existing(&repo, "match-2", 2024);
        ssh.queue_health_ok();
        ssh.queue_health_ok();
        let outcome = uc.execute(base_request()).await.expect("execute");
        match outcome {
            ConnectOutcome::Suggested { matches } => {
                assert_eq!(matches.len(), 2);
                // Newest first.
                assert_eq!(matches[0].id.as_str(), "match-1");
                assert_eq!(matches[1].id.as_str(), "match-2");
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    // --- Scenario 6 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn identity_match_with_force_new_skips_lookup_and_connects() {
        let (uc, ssh, repo, _clock) = build_use_case();
        let seeded = seed_existing(&repo, "untouched", 2025);
        let mut req = base_request();
        req.reuse = ReusePolicy::ForceNew;
        let outcome = uc.execute(req).await.expect("execute");
        match outcome {
            ConnectOutcome::Connected {
                session,
                replaced,
                retries: _,
            } => {
                assert_ne!(session.id, seeded.id);
                assert_eq!(replaced, 0);
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        // No health probe was issued at all.
        assert!(
            !ssh.calls()
                .iter()
                .any(|c| matches!(c, FakeSshCall::HealthCheck { .. })),
            "ForceNew must skip identity probing"
        );
        // Existing session left untouched.
        let still_there = repo.get(&seeded.id).await.expect("get");
        assert_eq!(still_there.map(|e| e.id), Some(seeded.id));
    }

    // --- Scenario 7 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn identity_match_unhealthy_is_purged_and_counted_as_replaced() {
        let (uc, ssh, repo, _clock) = build_use_case();
        let seeded = seed_existing(&repo, "dying-match", 2025);
        // Identity probe fails -> session disconnected and purged.
        ssh.queue_health_fail(DomainError::Transport("kaput".to_string()));
        let outcome = uc.execute(base_request()).await.expect("execute");
        match outcome {
            ConnectOutcome::Connected {
                session,
                replaced,
                retries: _,
            } => {
                assert_eq!(replaced, 1);
                assert_ne!(session.id, seeded.id);
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        assert!(
            repo.get(&seeded.id).await.expect("get").is_none(),
            "unhealthy match must be removed"
        );
    }

    // --- Scenario 8 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn multiple_identity_matches_with_auto_reuses_most_recent_healthy() {
        let (uc, ssh, repo, _clock) = build_use_case();
        let _ = seed_existing(&repo, "old", 2024);
        let _ = seed_existing(&repo, "newer", 2025);
        // Both healthy.
        ssh.queue_health_ok();
        ssh.queue_health_ok();
        let mut req = base_request();
        req.reuse = ReusePolicy::Auto;
        let outcome = uc.execute(req).await.expect("execute");
        match outcome {
            ConnectOutcome::Reused { session } => {
                assert_eq!(
                    session.id.as_str(),
                    "newer",
                    "Auto must pick the most recent healthy match"
                );
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    // --- Scenario 9 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn connect_failure_propagates_as_connect_failed_domain_error() {
        let (uc, ssh, repo, _clock) = build_use_case();
        ssh.queue_connect_error(DomainError::ConnectFailed("refused".to_string()));
        let err = uc
            .execute(base_request())
            .await
            .expect_err("connect must fail");
        assert_eq!(err, DomainError::ConnectFailed("refused".to_string()));
        // Nothing persisted.
        assert!(repo.list().await.expect("list").is_empty());
    }

    // --- Scenario 10 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn persistent_flag_propagates_to_the_session_entity() {
        let (uc, _ssh, repo, _clock) = build_use_case();
        let mut req = base_request();
        req.persistent = true;
        req.name = Some("persistent-backend".to_string());
        let outcome = uc.execute(req).await.expect("connect");
        match outcome {
            ConnectOutcome::Connected { session, .. } => {
                assert_eq!(session.name.as_deref(), Some("persistent-backend"));
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        let stored = repo.list().await.expect("list");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].name.as_deref(), Some("persistent-backend"));
    }

    // --- Scenario 11 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_id_propagates_to_entity_and_secondary_index() {
        let (uc, _ssh, repo, _clock) = build_use_case();
        let agent = AgentId::new("claude-instance-42".to_string());
        let mut req = base_request();
        req.agent_id = Some(agent.clone());
        let outcome = uc.execute(req).await.expect("connect");
        match outcome {
            ConnectOutcome::Connected { session, .. } => {
                assert_eq!(session.agent_id.as_ref(), Some(&agent));
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        let bound = repo.list_by_agent(&agent).await.expect("list_by_agent");
        assert_eq!(bound.len(), 1, "agent secondary index must contain one id");
    }
}
