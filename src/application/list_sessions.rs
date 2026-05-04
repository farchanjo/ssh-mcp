//! Use case: list active SSH sessions, optionally filtered by agent id.
//!
//! Translates the v3 `ssh_list_sessions_impl` (from
//! `src/mcp/tools/connection.rs`) plus the `process_health_results` /
//! `health_probe` helpers (from `src/mcp/tools/legacy_helpers.rs`) into a
//! hexagonal use case orchestrating the session repository, the SSH client
//! port, and the clock port.
//!
//! # Orchestration shape
//!
//! 1. Resolve the session universe:
//!    - When [`ListSessionsRequest::filter_agent_id`] is `Some`, fetch the
//!      bound session ids via [`SessionRepository::list_by_agent`] and resolve
//!      each entity through [`SessionRepository::get`].
//!    - When `None`, dump every session via [`SessionRepository::list`].
//! 2. For each candidate, run a liveness probe via
//!    [`SshClientPort::health_check`].
//! 3. Healthy candidates have their probe stamps refreshed via
//!    [`SessionRepository::update_health`] and are pushed into the result
//!    vector.
//! 4. Dead candidates are best-effort disconnected, removed from the
//!    repository (and unbound from their agent index when applicable), and
//!    their ids accumulated into [`ListSessionsOutcome::removed_dead`].
//! 5. Healthy entries are sorted by `connected_at` descending (most recent
//!    first — matches the v3 ordering after the inverse sort hand-off).
//! 6. The total count is captured **before** truncation so callers can render
//!    a "showing N of M" hint; the result is then truncated to
//!    `req.max_items.unwrap_or(default).clamp(1, cap)`, where the default and
//!    cap come from [`ConfigPort::list_max_items_default`] and
//!    [`ConfigPort::list_max_items_cap`].
//!
//! # Concurrency contract
//!
//! Mirrors the canary [`crate::application::connect_session`]: every
//! `.await` is performed outside any repository / fake guard. The repository
//! port returns owned values so no shard-spanning borrows escape, and the SSH
//! client port consumes / returns owned types only.

use std::cmp::Reverse;
use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::ids::{AgentId, SessionId};
use crate::domain::session::SessionEntity;
use crate::ports::clock::ClockPort;
use crate::ports::config::ConfigPort;
use crate::ports::session_repo::SessionRepository;
use crate::ports::ssh_client::SshClientPort;

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone, Default)]
pub struct ListSessionsRequest {
    /// Optional agent id used to narrow the universe to sessions bound to a
    /// single agent.
    pub filter_agent_id: Option<AgentId>,
    /// Optional caller-supplied truncation cap. `None` defers to
    /// [`ConfigPort::list_max_items_default`]; values are clamped to
    /// `1..=ConfigPort::list_max_items_cap`.
    pub max_items: Option<usize>,
}

/// Outbound DTO surfacing every observable result the rmcp tool wrapper must
/// render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListSessionsOutcome {
    /// Healthy sessions ordered newest first, truncated to `max_items`.
    pub healthy: Vec<SessionEntity>,
    /// Ids of sessions removed during the probe because the SSH layer
    /// reported them as dead.
    pub removed_dead: Vec<SessionId>,
    /// Total healthy count **before** the `max_items` truncation. Allows the
    /// caller to render a "showing N of M" hint without losing the unbounded
    /// figure.
    pub total: usize,
}

/// List-sessions use case generic over every adapter dependency. The
/// composition root pins concrete adapter types per binary; tests inject
/// fakes via [`crate::adapters`].
#[derive(Debug)]
pub struct ListSessionsUseCase<S, R, C, Cfg>
where
    S: SshClientPort + Send + Sync,
    R: SessionRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    ssh: Arc<S>,
    sessions: Arc<R>,
    clock: Arc<C>,
    config: Arc<Cfg>,
}

impl<S, R, C, Cfg> ListSessionsUseCase<S, R, C, Cfg>
where
    S: SshClientPort + Send + Sync,
    R: SessionRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(ssh: Arc<S>, sessions: Arc<R>, clock: Arc<C>, config: Arc<Cfg>) -> Self {
        Self {
            ssh,
            sessions,
            clock,
            config,
        }
    }

    /// Drive the list-sessions orchestration. See module-level docs for the
    /// step-by-step semantics.
    ///
    /// # Errors
    ///
    /// Propagates any [`DomainError::Storage`] surfaced by the session
    /// repository. Health-probe failures and disconnect failures are absorbed
    /// (the dead session is purged regardless) so the caller always receives
    /// a well-formed [`ListSessionsOutcome`].
    pub async fn execute(
        &self,
        req: ListSessionsRequest,
    ) -> Result<ListSessionsOutcome, DomainError> {
        let candidates = self
            .collect_candidates(req.filter_agent_id.as_ref())
            .await?;
        let (mut healthy, removed_dead) = self.partition_by_health(candidates).await?;
        // Newest first matches the v3 ordering used by the rmcp builder.
        healthy.sort_by_key(|entry| Reverse(entry.connected_at));
        let total = healthy.len();
        let cap = self.resolve_max_items(req.max_items);
        healthy.truncate(cap);
        Ok(ListSessionsOutcome {
            healthy,
            removed_dead,
            total,
        })
    }

    /// Resolve the candidate universe before the health probe runs. When the
    /// caller supplies a filter, missing ids in the secondary index are
    /// silently dropped — the same behaviour as the v3 implementation, which
    /// uses `SESSION_STORAGE.get(&id)` and `filter_map`s the misses.
    async fn collect_candidates(
        &self,
        filter: Option<&AgentId>,
    ) -> Result<Vec<SessionEntity>, DomainError> {
        match filter {
            None => self.sessions.list().await,
            Some(agent_id) => self.collect_by_agent(agent_id).await,
        }
    }

    /// Resolve every entity bound to `agent_id`. Stale ids in the secondary
    /// index (sessions removed without `unregister_agent`) resolve to `None`
    /// and are dropped.
    async fn collect_by_agent(
        &self,
        agent_id: &AgentId,
    ) -> Result<Vec<SessionEntity>, DomainError> {
        let ids = self.sessions.list_by_agent(agent_id).await?;
        let mut entities = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(entity) = self.sessions.get(&id).await? {
                entities.push(entity);
            }
        }
        Ok(entities)
    }

    /// Probe every candidate and split the survivors from the dead. Each dead
    /// session is purged through [`Self::purge_dead_session`] so the
    /// repository stays in sync with the SSH layer.
    async fn partition_by_health(
        &self,
        candidates: Vec<SessionEntity>,
    ) -> Result<(Vec<SessionEntity>, Vec<SessionId>), DomainError> {
        let mut healthy: Vec<SessionEntity> = Vec::with_capacity(candidates.len());
        let mut removed_dead: Vec<SessionId> = Vec::new();
        for candidate in candidates {
            let probe = self.ssh.health_check(&candidate.id).await;
            if probe.is_err() {
                self.purge_dead_session(&candidate).await;
                removed_dead.push(candidate.id);
                continue;
            }
            let now = self.clock.utc_now();
            self.sessions
                .update_health(&candidate.id, now, true)
                .await?;
            healthy.push(candidate.with_health(now, true));
        }
        Ok((healthy, removed_dead))
    }

    /// Best-effort cleanup of a session known to be dead. Mirrors the
    /// canary's `purge_dead_session`: disconnect failures (the SSH side may
    /// already be gone) and repository churn around the agent secondary
    /// index are swallowed so the partition pass keeps making progress.
    async fn purge_dead_session(&self, candidate: &SessionEntity) {
        let _ = self.ssh.disconnect(&candidate.id).await;
        let removed = self.sessions.remove(&candidate.id).await;
        if let Ok(Some(removed_entity)) = removed
            && let Some(agent_id) = removed_entity.agent_id.as_ref()
        {
            let _ = self
                .sessions
                .unregister_agent(agent_id, &candidate.id)
                .await;
        }
    }

    /// Resolve the truncation cap from the request override or the config
    /// defaults. Mirrors the v3 `clamp_list_items` helper: missing input
    /// falls back to the configured default; supplied values are clamped to
    /// `1..=cap`.
    fn resolve_max_items(&self, requested: Option<usize>) -> usize {
        let default = self.config.list_max_items_default();
        let cap = self.config.list_max_items_cap();
        requested.unwrap_or(default).clamp(1, cap)
    }
}

#[cfg(test)]
mod tests {
    use super::{ListSessionsOutcome, ListSessionsRequest, ListSessionsUseCase};
    use crate::adapters::clock::fake::FakeClock;
    use crate::adapters::config::memory::MapConfig;
    use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
    use crate::adapters::ssh::fake::{FakeSshCall, FakeSshClient};
    use crate::domain::error::DomainError;
    use crate::domain::identity::Address;
    use crate::domain::ids::{AgentId, SessionId};
    use crate::domain::session::SessionEntity;
    use crate::ports::session_repo::SessionRepository;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use std::time::Duration;

    type UseCaseUnderTest =
        ListSessionsUseCase<FakeSshClient, DashMapSessionRepo, FakeClock, MapConfig>;

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
        let config = Arc::new(MapConfig::default_v3());
        let uc = ListSessionsUseCase::new(
            Arc::clone(&ssh),
            Arc::clone(&sessions),
            Arc::clone(&clock),
            Arc::clone(&config),
        );
        (uc, ssh, sessions, clock)
    }

    fn build_use_case_with_config(config: MapConfig) -> (UseCaseUnderTest, Arc<FakeSshClient>) {
        let ssh = Arc::new(FakeSshClient::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        let clock = Arc::new(FakeClock::new(1_777_982_400_000_u64));
        let config = Arc::new(config);
        let uc = ListSessionsUseCase::new(
            Arc::clone(&ssh),
            Arc::clone(&sessions),
            Arc::clone(&clock),
            Arc::clone(&config),
        );
        (uc, ssh)
    }

    fn sample_address() -> Address {
        Address::new("h.example.com".to_string(), 22).expect("address")
    }

    fn entity(id: &str, year: i32, agent: Option<&str>) -> SessionEntity {
        SessionEntity {
            id: SessionId::new(id.to_string()),
            name: Some("seeded".to_string()),
            agent_id: agent.map(|a| AgentId::new(a.to_string())),
            address: sample_address(),
            username: "alice".to_string(),
            connected_at: Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).unwrap(),
            default_timeout: Duration::from_secs(30),
            retry_attempts: 0,
            compression_enabled: true,
            last_health_check: None,
            healthy: None,
        }
    }

    async fn seed(repo: &DashMapSessionRepo, entity: SessionEntity) {
        let agent = entity.agent_id.clone();
        let id = entity.id.clone();
        repo.insert(entity).await.expect("seed insert");
        if let Some(agent_id) = agent.as_ref() {
            repo.register_agent(agent_id, &id)
                .await
                .expect("register_agent");
        }
    }

    fn empty_request() -> ListSessionsRequest {
        ListSessionsRequest::default()
    }

    // --- Scenario 1 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_repo_returns_empty_outcome() {
        let (uc, ssh, _repo, _clock) = build_use_case();
        let outcome = uc.execute(empty_request()).await.expect("execute");
        assert_eq!(
            outcome,
            ListSessionsOutcome {
                healthy: vec![],
                removed_dead: vec![],
                total: 0,
            }
        );
        // No SSH calls were issued because there were no candidates.
        assert!(ssh.calls().is_empty());
    }

    // --- Scenario 2 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn all_healthy_sessions_are_returned_newest_first() {
        let (uc, ssh, repo, _clock) = build_use_case();
        seed(&repo, entity("s-old", 2024, None)).await;
        seed(&repo, entity("s-newer", 2025, None)).await;
        seed(&repo, entity("s-newest", 2026, None)).await;
        ssh.queue_health_ok();
        ssh.queue_health_ok();
        ssh.queue_health_ok();
        let outcome = uc.execute(empty_request()).await.expect("execute");
        assert_eq!(outcome.removed_dead, Vec::<SessionId>::new());
        assert_eq!(outcome.total, 3);
        assert_eq!(outcome.healthy.len(), 3);
        let ids: Vec<&str> = outcome.healthy.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["s-newest", "s-newer", "s-old"]);
        for entry in &outcome.healthy {
            assert_eq!(entry.healthy, Some(true));
            assert!(entry.last_health_check.is_some());
        }
    }

    // --- Scenario 3 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn all_dead_sessions_are_purged_and_returned_in_removed_dead() {
        let (uc, ssh, repo, _clock) = build_use_case();
        seed(&repo, entity("dead-1", 2024, None)).await;
        seed(&repo, entity("dead-2", 2025, None)).await;
        ssh.queue_health_fail(DomainError::Transport("kaput-1".to_string()));
        ssh.queue_health_fail(DomainError::Transport("kaput-2".to_string()));
        let outcome = uc.execute(empty_request()).await.expect("execute");
        assert!(outcome.healthy.is_empty());
        assert_eq!(outcome.total, 0);
        let mut removed: Vec<String> = outcome
            .removed_dead
            .iter()
            .map(|s| s.as_str().to_string())
            .collect();
        removed.sort();
        assert_eq!(removed, vec!["dead-1".to_string(), "dead-2".to_string()]);
        // Repo cleared.
        let stored = repo.list().await.expect("list");
        assert!(
            stored.is_empty(),
            "dead sessions must be removed from the repository"
        );
    }

    // --- Scenario 4 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn mixed_healthy_and_dead_partitions_correctly() {
        let (uc, ssh, repo, _clock) = build_use_case();
        seed(&repo, entity("alive-1", 2025, None)).await;
        seed(&repo, entity("dead", 2024, None)).await;
        seed(&repo, entity("alive-2", 2026, None)).await;
        // Order of probes follows the order in which `list()` returns the
        // entities — DashMap iteration is unordered, so script every probe
        // outcome by id rather than by position.
        let stored = repo.list().await.expect("list");
        for entry in &stored {
            if entry.id.as_str() == "dead" {
                ssh.queue_health_fail(DomainError::Transport("dead".to_string()));
            } else {
                ssh.queue_health_ok();
            }
        }
        let outcome = uc.execute(empty_request()).await.expect("execute");
        assert_eq!(outcome.healthy.len(), 2);
        assert_eq!(outcome.removed_dead.len(), 1);
        assert_eq!(outcome.removed_dead[0].as_str(), "dead");
        assert_eq!(outcome.total, 2);
        let healthy_ids: Vec<&str> = outcome.healthy.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(healthy_ids, vec!["alive-2", "alive-1"]);
    }

    // --- Scenario 5 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_filter_narrows_the_candidate_universe() {
        let (uc, ssh, repo, _clock) = build_use_case();
        let agent_a = AgentId::new("agent-A".to_string());
        let agent_b = AgentId::new("agent-B".to_string());
        seed(&repo, entity("a-1", 2024, Some(agent_a.as_str()))).await;
        seed(&repo, entity("a-2", 2025, Some(agent_a.as_str()))).await;
        seed(&repo, entity("b-1", 2026, Some(agent_b.as_str()))).await;
        seed(&repo, entity("orphan", 2023, None)).await;
        // Only the two `agent-A` sessions are probed.
        ssh.queue_health_ok();
        ssh.queue_health_ok();
        let req = ListSessionsRequest {
            filter_agent_id: Some(agent_a.clone()),
            max_items: None,
        };
        let outcome = uc.execute(req).await.expect("execute");
        let ids: Vec<&str> = outcome.healthy.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["a-2", "a-1"]);
        assert_eq!(outcome.total, 2);
        assert!(outcome.removed_dead.is_empty());
        // Sanity: only two health probes were issued.
        let probe_count = ssh
            .calls()
            .iter()
            .filter(|c| matches!(c, FakeSshCall::HealthCheck { .. }))
            .count();
        assert_eq!(probe_count, 2);
    }

    // --- Scenario 6 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_filter_with_unknown_agent_returns_empty_outcome() {
        let (uc, ssh, repo, _clock) = build_use_case();
        seed(&repo, entity("orphan", 2024, None)).await;
        let req = ListSessionsRequest {
            filter_agent_id: Some(AgentId::new("unknown".to_string())),
            max_items: None,
        };
        let outcome = uc.execute(req).await.expect("execute");
        assert_eq!(
            outcome,
            ListSessionsOutcome {
                healthy: vec![],
                removed_dead: vec![],
                total: 0,
            }
        );
        // No probes issued because the secondary index returned no ids.
        assert!(ssh.calls().is_empty());
    }

    // --- Scenario 7 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn max_items_truncates_after_total_is_captured() {
        let (uc, ssh, repo, _clock) = build_use_case();
        seed(&repo, entity("s-1", 2024, None)).await;
        seed(&repo, entity("s-2", 2025, None)).await;
        seed(&repo, entity("s-3", 2026, None)).await;
        ssh.queue_health_ok();
        ssh.queue_health_ok();
        ssh.queue_health_ok();
        let req = ListSessionsRequest {
            filter_agent_id: None,
            max_items: Some(2),
        };
        let outcome = uc.execute(req).await.expect("execute");
        assert_eq!(
            outcome.total, 3,
            "total must reflect the pre-truncation count"
        );
        assert_eq!(outcome.healthy.len(), 2);
        let ids: Vec<&str> = outcome.healthy.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["s-3", "s-2"],
            "newest entries kept after truncation"
        );
    }

    // --- Scenario 8 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn max_items_zero_is_clamped_to_one() {
        let (uc, ssh, repo, _clock) = build_use_case();
        seed(&repo, entity("s-1", 2024, None)).await;
        seed(&repo, entity("s-2", 2026, None)).await;
        ssh.queue_health_ok();
        ssh.queue_health_ok();
        let req = ListSessionsRequest {
            filter_agent_id: None,
            max_items: Some(0),
        };
        let outcome = uc.execute(req).await.expect("execute");
        assert_eq!(outcome.healthy.len(), 1);
        assert_eq!(outcome.healthy[0].id.as_str(), "s-2");
        assert_eq!(outcome.total, 2);
    }

    // --- Scenario 9 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn max_items_above_cap_is_clamped_to_cap() {
        // Pin a tiny cap (3) so we can verify the clamp without seeding 10k
        // sessions.
        let custom = MapConfig::default_v3()
            .with_list_max_items_default(2)
            .with_list_max_items_cap(3);
        let (uc, ssh) = build_use_case_with_config(custom);
        // Borrow the repo through the use case's internal handle by seeding
        // via a fresh DashMap repo built into a second use case sharing the
        // same Arc handles is overkill — instead, mint sessions directly via
        // the use case's exposed adapters. Since `build_use_case_with_config`
        // returns owned Arcs only for ssh, rebuild end-to-end here.
        let _ = (uc, ssh);
        // Rebuild manually so we keep a handle on the repo too.
        let ssh2 = Arc::new(FakeSshClient::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        let clock = Arc::new(FakeClock::new(1_777_982_400_000_u64));
        let config = Arc::new(
            MapConfig::default_v3()
                .with_list_max_items_default(2)
                .with_list_max_items_cap(3),
        );
        let uc = ListSessionsUseCase::new(
            Arc::clone(&ssh2),
            Arc::clone(&sessions),
            Arc::clone(&clock),
            Arc::clone(&config),
        );
        for year in 2020..2026 {
            seed(&sessions, entity(&format!("s-{year}"), year, None)).await;
            ssh2.queue_health_ok();
        }
        let req = ListSessionsRequest {
            filter_agent_id: None,
            max_items: Some(100),
        };
        let outcome = uc.execute(req).await.expect("execute");
        assert_eq!(outcome.total, 6);
        assert_eq!(
            outcome.healthy.len(),
            3,
            "cap of 3 must override the request value"
        );
    }

    // --- Scenario 10 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_max_items_uses_config_default() {
        let custom = MapConfig::default_v3()
            .with_list_max_items_default(2)
            .with_list_max_items_cap(50);
        let (uc, ssh) = build_use_case_with_config(custom);
        // Same overhead as scenario 9: rebuild end-to-end so we own the repo.
        let _ = (uc, ssh);
        let ssh2 = Arc::new(FakeSshClient::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        let clock = Arc::new(FakeClock::new(1_777_982_400_000_u64));
        let config = Arc::new(
            MapConfig::default_v3()
                .with_list_max_items_default(2)
                .with_list_max_items_cap(50),
        );
        let uc = ListSessionsUseCase::new(
            Arc::clone(&ssh2),
            Arc::clone(&sessions),
            Arc::clone(&clock),
            Arc::clone(&config),
        );
        for year in 2023..2027 {
            seed(&sessions, entity(&format!("s-{year}"), year, None)).await;
            ssh2.queue_health_ok();
        }
        let req = empty_request();
        let outcome = uc.execute(req).await.expect("execute");
        assert_eq!(outcome.total, 4);
        assert_eq!(
            outcome.healthy.len(),
            2,
            "missing max_items must fall back to the config default"
        );
        let ids: Vec<&str> = outcome.healthy.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["s-2026", "s-2025"]);
    }

    // --- Scenario 11 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn dead_session_with_agent_id_is_unbound_from_secondary_index() {
        let (uc, ssh, repo, _clock) = build_use_case();
        let agent = AgentId::new("agent-A".to_string());
        seed(&repo, entity("dying", 2024, Some(agent.as_str()))).await;
        ssh.queue_health_fail(DomainError::Transport("dead".to_string()));
        let outcome = uc.execute(empty_request()).await.expect("execute");
        assert_eq!(outcome.removed_dead.len(), 1);
        assert!(outcome.healthy.is_empty());
        // The agent secondary index must no longer reference the dead id.
        let bound = repo.list_by_agent(&agent).await.expect("list_by_agent");
        assert!(
            bound.is_empty(),
            "dead session must be unbound from its agent secondary index"
        );
    }

    // --- Scenario 12 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn updated_health_persists_through_the_repository() {
        let (uc, ssh, repo, _clock) = build_use_case();
        seed(&repo, entity("alive", 2025, None)).await;
        ssh.queue_health_ok();
        let outcome = uc.execute(empty_request()).await.expect("execute");
        assert_eq!(outcome.healthy.len(), 1);
        // Re-read directly from the repository to ensure the health stamps
        // were persisted, not only echoed in the outcome.
        let stored = repo
            .get(&SessionId::new("alive".to_string()))
            .await
            .expect("get")
            .expect("session must still exist");
        assert_eq!(stored.healthy, Some(true));
        assert!(stored.last_health_check.is_some());
    }
}
