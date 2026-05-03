//! [`crate::ports::session_repo::SessionRepository`] adapter backed by
//! [`dashmap::DashMap`].
//!
//! Two maps:
//!
//! - `by_id: DashMap<SessionId, SessionEntity>` — primary store of
//!   session aggregates keyed by id.
//! - `by_agent: DashMap<AgentId, HashSet<SessionId>>` — secondary index
//!   that powers `list_by_agent` / `remove_by_agent` in `O(1)` after the
//!   shard lookup.
//!
//! ## Ownership and concurrency
//!
//! Both maps are wrapped in `Arc` so cloning the adapter (e.g. handing
//! it to multiple use cases at the composition root) is a pointer bump.
//! All mutating methods are sync internally; the trait stays `async` so
//! a future remote-backed repository fits without breaking the API.
//!
//! ## Invariants
//!
//! - `insert` rejects re-binding an existing id with
//!   [`DomainError::Internal`]; callers must `remove` first.
//! - `register_agent` is idempotent — re-binding the same `(agent,
//!   session)` pair is a no-op, matching the v3
//!   `mcp::storage::session::DashMapSessionStorage` semantics.
//! - `unregister_agent` cleans the agent entry when the last session is
//!   removed so empty buckets never linger.
//! - The agent-secondary index is independent of the optional
//!   `SessionEntity::agent_id` field. Use cases pick one model
//!   (entity-embedded vs index-tracked) and stick with it.
//! - No `await` is performed while a `DashMap` shard guard is alive.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use crate::domain::error::DomainError;
use crate::domain::ids::{AgentId, SessionId};
use crate::domain::session::SessionEntity;
use crate::ports::session_repo::SessionRepository;

/// In-process [`SessionRepository`] backed by [`DashMap`].
#[derive(Debug, Default, Clone)]
pub struct DashMapSessionRepo {
    by_id: Arc<DashMap<SessionId, SessionEntity>>,
    by_agent: Arc<DashMap<AgentId, HashSet<SessionId>>>,
}

impl DashMapSessionRepo {
    /// Build an empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SessionRepository for DashMapSessionRepo {
    async fn insert(&self, entity: SessionEntity) -> Result<(), DomainError> {
        let id = entity.id.clone();
        // `entry` keeps the insert atomic per shard so a concurrent
        // `insert` for the same id cannot both succeed.
        match self.by_id.entry(id.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(entity);
                Ok(())
            }
            Entry::Occupied(_) => Err(DomainError::Internal(format!(
                "session {id} already exists; remove() before re-insert"
            ))),
        }
    }

    async fn get(&self, id: &SessionId) -> Result<Option<SessionEntity>, DomainError> {
        // Clone out before dropping the shard guard; never hold a guard
        // across `.await` (no await follows here, but the rule keeps
        // future maintainers honest).
        Ok(self.by_id.get(id).map(|entry| entry.value().clone()))
    }

    async fn remove(&self, id: &SessionId) -> Result<Option<SessionEntity>, DomainError> {
        Ok(self.by_id.remove(id).map(|(_, entity)| entity))
    }

    async fn list(&self) -> Result<Vec<SessionEntity>, DomainError> {
        let mut out = Vec::with_capacity(self.by_id.len());
        for entry in self.by_id.iter() {
            out.push(entry.value().clone());
        }
        Ok(out)
    }

    async fn update_health(
        &self,
        id: &SessionId,
        at: DateTime<Utc>,
        healthy: bool,
    ) -> Result<(), DomainError> {
        self.by_id.get_mut(id).map_or_else(
            || Err(DomainError::SessionNotFound(id.clone())),
            |mut entry| {
                entry.set_last_health_check(at);
                entry.set_healthy(healthy);
                Ok(())
            },
        )
    }

    async fn register_agent(
        &self,
        agent_id: &AgentId,
        session_id: &SessionId,
    ) -> Result<(), DomainError> {
        self.by_agent
            .entry(agent_id.clone())
            .or_default()
            .insert(session_id.clone());
        Ok(())
    }

    async fn unregister_agent(
        &self,
        agent_id: &AgentId,
        session_id: &SessionId,
    ) -> Result<(), DomainError> {
        let drop_bucket = self.by_agent.get_mut(agent_id).is_some_and(|mut bucket| {
            bucket.remove(session_id);
            bucket.is_empty()
        });
        if drop_bucket {
            // Re-check inside `remove_if` so a concurrent
            // `register_agent` cannot lose the entry.
            self.by_agent
                .remove_if(agent_id, |_, bucket| bucket.is_empty());
        }
        Ok(())
    }

    async fn list_by_agent(&self, agent_id: &AgentId) -> Result<Vec<SessionId>, DomainError> {
        Ok(self
            .by_agent
            .get(agent_id)
            .map(|bucket| bucket.iter().cloned().collect())
            .unwrap_or_default())
    }

    async fn remove_by_agent(&self, agent_id: &AgentId) -> Result<Vec<SessionId>, DomainError> {
        Ok(self
            .by_agent
            .remove(agent_id)
            .map(|(_, bucket)| bucket.into_iter().collect())
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::{TimeZone, Utc};

    use super::DashMapSessionRepo;
    use crate::domain::error::DomainError;
    use crate::domain::identity::Address;
    use crate::domain::ids::{AgentId, SessionId};
    use crate::domain::session::SessionEntity;
    use crate::ports::session_repo::SessionRepository;

    fn entity(id: &str, username: &str, host: &str, port: u16) -> SessionEntity {
        SessionEntity {
            id: SessionId::new(id.to_string()),
            name: None,
            agent_id: None,
            address: Address::new(host.to_string(), port).expect("address"),
            username: username.to_string(),
            connected_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            default_timeout: Duration::from_secs(30),
            retry_attempts: 0,
            compression_enabled: true,
            last_health_check: None,
            healthy: None,
        }
    }

    #[tokio::test]
    async fn insert_then_get_round_trip() {
        let repo = DashMapSessionRepo::new();
        let e = entity("s-1", "alice", "h", 22);
        repo.insert(e.clone()).await.expect("insert");
        let got = repo
            .get(&SessionId::new("s-1".to_string()))
            .await
            .expect("get");
        assert_eq!(got, Some(e));
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let repo = DashMapSessionRepo::new();
        let got = repo
            .get(&SessionId::new("absent".to_string()))
            .await
            .expect("get");
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn insert_duplicate_returns_internal_error() {
        let repo = DashMapSessionRepo::new();
        let e = entity("s-1", "alice", "h", 22);
        repo.insert(e.clone()).await.expect("first insert");
        let err = repo
            .insert(e)
            .await
            .expect_err("second insert must fail with Internal");
        match err {
            DomainError::Internal(msg) => assert!(msg.contains("s-1")),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn remove_returns_entity_then_get_returns_none() {
        let repo = DashMapSessionRepo::new();
        let e = entity("s-1", "alice", "h", 22);
        repo.insert(e.clone()).await.expect("insert");
        let removed = repo
            .remove(&SessionId::new("s-1".to_string()))
            .await
            .expect("remove");
        assert_eq!(removed, Some(e));
        let after = repo
            .get(&SessionId::new("s-1".to_string()))
            .await
            .expect("get");
        assert!(after.is_none());
    }

    #[tokio::test]
    async fn remove_missing_returns_none() {
        let repo = DashMapSessionRepo::new();
        let removed = repo
            .remove(&SessionId::new("absent".to_string()))
            .await
            .expect("remove");
        assert!(removed.is_none());
    }

    #[tokio::test]
    async fn list_returns_all_entities() {
        let repo = DashMapSessionRepo::new();
        repo.insert(entity("s-1", "u1", "h", 22))
            .await
            .expect("insert 1");
        repo.insert(entity("s-2", "u2", "h", 22))
            .await
            .expect("insert 2");
        repo.insert(entity("s-3", "u3", "h", 22))
            .await
            .expect("insert 3");
        let mut all = repo.list().await.expect("list");
        all.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id.as_str(), "s-1");
        assert_eq!(all[1].id.as_str(), "s-2");
        assert_eq!(all[2].id.as_str(), "s-3");
    }

    #[tokio::test]
    async fn list_on_empty_repo_returns_empty_vec() {
        let repo = DashMapSessionRepo::new();
        let all = repo.list().await.expect("list");
        assert!(all.is_empty());
    }

    #[tokio::test]
    async fn update_health_mutates_stored_entity() {
        let repo = DashMapSessionRepo::new();
        repo.insert(entity("s-1", "alice", "h", 22))
            .await
            .expect("insert");
        let at = Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();
        repo.update_health(&SessionId::new("s-1".to_string()), at, true)
            .await
            .expect("update_health");
        let got = repo
            .get(&SessionId::new("s-1".to_string()))
            .await
            .expect("get")
            .expect("entity present");
        assert_eq!(got.last_health_check, Some(at));
        assert_eq!(got.healthy, Some(true));
    }

    #[tokio::test]
    async fn update_health_on_missing_id_returns_session_not_found() {
        let repo = DashMapSessionRepo::new();
        let id = SessionId::new("absent".to_string());
        let err = repo
            .update_health(&id, Utc::now(), true)
            .await
            .expect_err("expected SessionNotFound");
        assert_eq!(err, DomainError::SessionNotFound(id));
    }

    #[tokio::test]
    async fn register_and_list_by_agent_returns_bound_ids() {
        let repo = DashMapSessionRepo::new();
        let agent = AgentId::new("agent-A".to_string());
        let s1 = SessionId::new("s-1".to_string());
        let s2 = SessionId::new("s-2".to_string());
        repo.register_agent(&agent, &s1).await.expect("register s1");
        repo.register_agent(&agent, &s2).await.expect("register s2");
        let mut ids = repo.list_by_agent(&agent).await.expect("list_by_agent");
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(ids, vec![s1, s2]);
    }

    #[tokio::test]
    async fn register_agent_is_idempotent() {
        let repo = DashMapSessionRepo::new();
        let agent = AgentId::new("agent-A".to_string());
        let s1 = SessionId::new("s-1".to_string());
        repo.register_agent(&agent, &s1).await.expect("first");
        repo.register_agent(&agent, &s1).await.expect("second");
        let ids = repo.list_by_agent(&agent).await.expect("list_by_agent");
        assert_eq!(ids, vec![s1]);
    }

    #[tokio::test]
    async fn unregister_agent_removes_session_and_drops_empty_bucket() {
        let repo = DashMapSessionRepo::new();
        let agent = AgentId::new("agent-A".to_string());
        let s1 = SessionId::new("s-1".to_string());
        repo.register_agent(&agent, &s1).await.expect("register");
        repo.unregister_agent(&agent, &s1)
            .await
            .expect("unregister");
        let ids = repo.list_by_agent(&agent).await.expect("list_by_agent");
        assert!(ids.is_empty());
    }

    #[tokio::test]
    async fn unregister_agent_on_missing_pair_is_noop() {
        let repo = DashMapSessionRepo::new();
        let agent = AgentId::new("agent-A".to_string());
        let s1 = SessionId::new("s-1".to_string());
        repo.unregister_agent(&agent, &s1)
            .await
            .expect("unregister noop");
        assert!(repo.list_by_agent(&agent).await.expect("list").is_empty());
    }

    #[tokio::test]
    async fn remove_by_agent_returns_all_bound_ids_and_clears_bucket() {
        let repo = DashMapSessionRepo::new();
        let agent = AgentId::new("agent-A".to_string());
        let s1 = SessionId::new("s-1".to_string());
        let s2 = SessionId::new("s-2".to_string());
        repo.register_agent(&agent, &s1).await.expect("r1");
        repo.register_agent(&agent, &s2).await.expect("r2");
        let mut removed = repo.remove_by_agent(&agent).await.expect("remove_by_agent");
        removed.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(removed, vec![s1, s2]);
        assert!(repo.list_by_agent(&agent).await.expect("list").is_empty());
    }

    #[tokio::test]
    async fn remove_by_agent_on_unknown_returns_empty_vec() {
        let repo = DashMapSessionRepo::new();
        let agent = AgentId::new("absent".to_string());
        let removed = repo.remove_by_agent(&agent).await.expect("remove_by_agent");
        assert!(removed.is_empty());
    }

    #[tokio::test]
    async fn agent_index_segregates_distinct_agents() {
        let repo = DashMapSessionRepo::new();
        let a1 = AgentId::new("agent-1".to_string());
        let a2 = AgentId::new("agent-2".to_string());
        let s1 = SessionId::new("s-1".to_string());
        let s2 = SessionId::new("s-2".to_string());
        repo.register_agent(&a1, &s1).await.expect("a1+s1");
        repo.register_agent(&a2, &s2).await.expect("a2+s2");
        // Removing one agent leaves the other intact.
        let removed = repo.remove_by_agent(&a1).await.expect("remove a1");
        assert_eq!(removed, vec![s1]);
        let ids2 = repo.list_by_agent(&a2).await.expect("list a2");
        assert_eq!(ids2, vec![s2]);
    }

    #[tokio::test]
    async fn cross_instance_isolation_no_shared_state() {
        let repo_a = DashMapSessionRepo::new();
        let repo_b = DashMapSessionRepo::new();
        repo_a
            .insert(entity("s-1", "alice", "h", 22))
            .await
            .expect("insert a");
        let in_b = repo_b
            .get(&SessionId::new("s-1".to_string()))
            .await
            .expect("get b");
        assert!(in_b.is_none(), "fresh repo must not see other repo's data");
        let in_a = repo_a
            .get(&SessionId::new("s-1".to_string()))
            .await
            .expect("get a")
            .expect("present in a");
        assert_eq!(in_a.id.as_str(), "s-1");
    }

    #[tokio::test]
    async fn clone_shares_state_via_arc() {
        // Cloning the adapter must share the underlying maps so the
        // composition root can hand the same repo to multiple use cases.
        let repo = DashMapSessionRepo::new();
        let twin = repo.clone();
        repo.insert(entity("s-1", "alice", "h", 22))
            .await
            .expect("insert via repo");
        let via_twin = twin
            .get(&SessionId::new("s-1".to_string()))
            .await
            .expect("get via twin")
            .expect("entity present via twin");
        assert_eq!(via_twin.id.as_str(), "s-1");
    }

    #[tokio::test]
    async fn repo_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DashMapSessionRepo>();
    }
}
