//! [`crate::ports::forward_repo::ForwardRepository`] adapter backed by
//! [`dashmap::DashMap`].
//!
//! Two maps:
//!
//! - `by_id: DashMap<ForwardId, ForwardEntity>` — primary store of
//!   forwarder aggregates keyed by id.
//! - `by_session: DashMap<SessionId, HashSet<ForwardId>>` — secondary
//!   index that powers `list_filtered(Some(&session))` in `O(1)` after
//!   the shard lookup. Without it the trait would have to scan every
//!   forwarder on every per-session listing.
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
//!   [`DomainError::Internal`]; callers must `remove` first. This
//!   mirrors the H4 [`crate::adapters::repo::dashmap::session`] canary.
//! - `update` requires the id to already exist and surfaces
//!   [`DomainError::ForwardNotFound`] otherwise.
//! - `update` may move a forwarder between sessions: the secondary
//!   index is rebuilt accordingly so the source bucket is shrunk and
//!   the target bucket is grown atomically per shard.
//! - `remove` cleans the session bucket entry and drops the bucket
//!   when it becomes empty so empty buckets never linger.
//! - `list_filtered(None)` returns every stored forwarder; ordering is
//!   unspecified — callers sort if they need stability.
//! - No `await` is performed while a `DashMap` shard guard is alive.
//!
//! ## Feature gate
//!
//! Compiled only when the `port_forward` Cargo feature is enabled,
//! mirroring the v3 module gate at `mcp::forward`.

#![cfg(feature = "port_forward")]

use std::collections::HashSet;
use std::sync::Arc;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use crate::domain::error::DomainError;
use crate::domain::forward::ForwardEntity;
use crate::domain::ids::{ForwardId, SessionId};
use crate::ports::forward_repo::ForwardRepository;

/// In-process [`ForwardRepository`] backed by [`DashMap`].
#[derive(Debug, Default, Clone)]
pub struct DashMapForwardRepo {
    by_id: Arc<DashMap<ForwardId, ForwardEntity>>,
    by_session: Arc<DashMap<SessionId, HashSet<ForwardId>>>,
}

impl DashMapForwardRepo {
    /// Build an empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `forward_id` into the bucket owned by `session_id`,
    /// creating the bucket on first use.
    fn index_attach(&self, session_id: &SessionId, forward_id: &ForwardId) {
        self.by_session
            .entry(session_id.clone())
            .or_default()
            .insert(forward_id.clone());
    }

    /// Remove `forward_id` from `session_id`'s bucket and drop the
    /// bucket if it becomes empty.
    fn index_detach(&self, session_id: &SessionId, forward_id: &ForwardId) {
        let drop_bucket = self
            .by_session
            .get_mut(session_id)
            .is_some_and(|mut bucket| {
                bucket.remove(forward_id);
                bucket.is_empty()
            });
        if drop_bucket {
            // Re-check inside `remove_if` so a concurrent attach
            // cannot lose the entry between the `get_mut` and the
            // remove.
            self.by_session
                .remove_if(session_id, |_, bucket| bucket.is_empty());
        }
    }
}

impl ForwardRepository for DashMapForwardRepo {
    async fn insert(&self, entity: ForwardEntity) -> Result<(), DomainError> {
        let id = entity.id.clone();
        let session_id = entity.session_id.clone();
        // `entry` keeps the insert atomic per shard so a concurrent
        // `insert` for the same id cannot both succeed.
        match self.by_id.entry(id.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(entity);
            }
            Entry::Occupied(_) => {
                return Err(DomainError::Internal(format!(
                    "forward {id} already exists; remove() before re-insert"
                )));
            }
        }
        self.index_attach(&session_id, &id);
        Ok(())
    }

    async fn update(&self, entity: ForwardEntity) -> Result<(), DomainError> {
        let id = entity.id.clone();
        let new_session = entity.session_id.clone();
        // Capture the previous session id (if any) without holding a
        // shard guard across the index mutations below.
        let previous_session = self
            .by_id
            .get(&id)
            .map(|guard| guard.value().session_id.clone());
        let Some(previous_session) = previous_session else {
            return Err(DomainError::ForwardNotFound(id));
        };
        // Replace the stored entity. Hold the guard only for the swap.
        match self.by_id.get_mut(&id) {
            Some(mut guard) => {
                *guard.value_mut() = entity;
            }
            None => {
                // Lost a race with `remove`; surface as not-found.
                return Err(DomainError::ForwardNotFound(id));
            }
        }
        if previous_session != new_session {
            self.index_detach(&previous_session, &id);
            self.index_attach(&new_session, &id);
        }
        Ok(())
    }

    async fn get(&self, id: &ForwardId) -> Result<Option<ForwardEntity>, DomainError> {
        // Clone out before dropping the shard guard; never hold a guard
        // across `.await` (no await follows here, but the rule keeps
        // future maintainers honest).
        Ok(self.by_id.get(id).map(|entry| entry.value().clone()))
    }

    async fn remove(&self, id: &ForwardId) -> Result<Option<ForwardEntity>, DomainError> {
        let removed = self.by_id.remove(id).map(|(_, entity)| entity);
        if let Some(ref entity) = removed {
            self.index_detach(&entity.session_id, id);
        }
        Ok(removed)
    }

    async fn list_filtered(
        &self,
        session_id: Option<&SessionId>,
    ) -> Result<Vec<ForwardEntity>, DomainError> {
        match session_id {
            Some(session) => {
                // Snapshot the bucket to a `Vec<ForwardId>` so we drop
                // the shard guard before issuing per-id lookups —
                // otherwise a `get` on the same shard could deadlock.
                let ids: Vec<ForwardId> = self
                    .by_session
                    .get(session)
                    .map(|bucket| bucket.iter().cloned().collect())
                    .unwrap_or_default();
                let mut out = Vec::with_capacity(ids.len());
                for fid in &ids {
                    if let Some(entry) = self.by_id.get(fid) {
                        out.push(entry.value().clone());
                    }
                }
                Ok(out)
            }
            None => {
                let mut out = Vec::with_capacity(self.by_id.len());
                for entry in self.by_id.iter() {
                    out.push(entry.value().clone());
                }
                Ok(out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::DashMapForwardRepo;
    use crate::domain::error::DomainError;
    use crate::domain::forward::{ForwardEntity, ForwardStatus};
    use crate::domain::ids::{ForwardId, SessionId};
    use crate::ports::forward_repo::ForwardRepository;

    fn entity(id: &str, session: &str, local_port: u16) -> ForwardEntity {
        ForwardEntity::new(
            ForwardId::new(id.to_string()),
            SessionId::new(session.to_string()),
            local_port,
            "internal".to_string(),
            80,
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        )
    }

    #[tokio::test]
    async fn insert_then_get_round_trip() {
        let repo = DashMapForwardRepo::new();
        let e = entity("f-1", "s-1", 8080);
        repo.insert(e.clone()).await.expect("insert");
        let got = repo
            .get(&ForwardId::new("f-1".to_string()))
            .await
            .expect("get");
        assert_eq!(got, Some(e));
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let repo = DashMapForwardRepo::new();
        let got = repo
            .get(&ForwardId::new("absent".to_string()))
            .await
            .expect("get");
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn insert_duplicate_returns_internal_error() {
        let repo = DashMapForwardRepo::new();
        let e = entity("f-1", "s-1", 8080);
        repo.insert(e.clone()).await.expect("first insert");
        let err = repo
            .insert(e)
            .await
            .expect_err("second insert must fail with Internal");
        match err {
            DomainError::Internal(msg) => assert!(msg.contains("f-1")),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn remove_returns_entity_then_get_returns_none() {
        let repo = DashMapForwardRepo::new();
        let e = entity("f-1", "s-1", 8080);
        repo.insert(e.clone()).await.expect("insert");
        let removed = repo
            .remove(&ForwardId::new("f-1".to_string()))
            .await
            .expect("remove");
        assert_eq!(removed, Some(e));
        let after = repo
            .get(&ForwardId::new("f-1".to_string()))
            .await
            .expect("get");
        assert!(after.is_none());
        // The session bucket must be cleaned up too.
        let by_session = repo
            .list_filtered(Some(&SessionId::new("s-1".to_string())))
            .await
            .expect("list_filtered");
        assert!(by_session.is_empty());
    }

    #[tokio::test]
    async fn remove_missing_returns_none() {
        let repo = DashMapForwardRepo::new();
        let removed = repo
            .remove(&ForwardId::new("absent".to_string()))
            .await
            .expect("remove");
        assert!(removed.is_none());
    }

    #[tokio::test]
    async fn list_filtered_none_returns_all_entities() {
        let repo = DashMapForwardRepo::new();
        repo.insert(entity("f-1", "s-1", 8080))
            .await
            .expect("insert 1");
        repo.insert(entity("f-2", "s-2", 8081))
            .await
            .expect("insert 2");
        repo.insert(entity("f-3", "s-1", 8082))
            .await
            .expect("insert 3");
        let mut all = repo.list_filtered(None).await.expect("list");
        all.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id.as_str(), "f-1");
        assert_eq!(all[1].id.as_str(), "f-2");
        assert_eq!(all[2].id.as_str(), "f-3");
    }

    #[tokio::test]
    async fn list_filtered_by_session_returns_only_owned_forwarders() {
        let repo = DashMapForwardRepo::new();
        repo.insert(entity("f-1", "s-1", 8080))
            .await
            .expect("insert f-1");
        repo.insert(entity("f-2", "s-2", 8081))
            .await
            .expect("insert f-2");
        repo.insert(entity("f-3", "s-1", 8082))
            .await
            .expect("insert f-3");
        let mut owned = repo
            .list_filtered(Some(&SessionId::new("s-1".to_string())))
            .await
            .expect("list_filtered s-1");
        owned.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        assert_eq!(owned.len(), 2);
        assert_eq!(owned[0].id.as_str(), "f-1");
        assert_eq!(owned[1].id.as_str(), "f-3");
    }

    #[tokio::test]
    async fn list_filtered_on_unknown_session_returns_empty_vec() {
        let repo = DashMapForwardRepo::new();
        repo.insert(entity("f-1", "s-1", 8080))
            .await
            .expect("insert");
        let owned = repo
            .list_filtered(Some(&SessionId::new("absent".to_string())))
            .await
            .expect("list_filtered absent");
        assert!(owned.is_empty());
    }

    #[tokio::test]
    async fn list_filtered_on_empty_repo_returns_empty_vec() {
        let repo = DashMapForwardRepo::new();
        let all = repo.list_filtered(None).await.expect("list");
        assert!(all.is_empty());
    }

    #[tokio::test]
    async fn update_mutates_stored_entity_and_keeps_session_bucket() {
        let repo = DashMapForwardRepo::new();
        repo.insert(entity("f-1", "s-1", 8080))
            .await
            .expect("insert");
        let stopped = entity("f-1", "s-1", 8080).stop();
        repo.update(stopped.clone()).await.expect("update");
        let got = repo
            .get(&ForwardId::new("f-1".to_string()))
            .await
            .expect("get")
            .expect("entity present");
        assert_eq!(got.status, ForwardStatus::Stopped);
        assert_eq!(got, stopped);
        // Session index is unchanged for same-session updates.
        let owned = repo
            .list_filtered(Some(&SessionId::new("s-1".to_string())))
            .await
            .expect("list");
        assert_eq!(owned.len(), 1);
    }

    #[tokio::test]
    async fn update_on_missing_id_returns_forward_not_found() {
        let repo = DashMapForwardRepo::new();
        let id = ForwardId::new("absent".to_string());
        let err = repo
            .update(entity("absent", "s-1", 8080))
            .await
            .expect_err("expected ForwardNotFound");
        assert_eq!(err, DomainError::ForwardNotFound(id));
    }

    #[tokio::test]
    async fn update_can_reassign_session_and_rebuild_index() {
        let repo = DashMapForwardRepo::new();
        repo.insert(entity("f-1", "s-1", 8080))
            .await
            .expect("insert");
        let moved = entity("f-1", "s-2", 8080);
        repo.update(moved).await.expect("update");
        // Old session bucket is empty (and dropped), new one carries the id.
        let in_old = repo
            .list_filtered(Some(&SessionId::new("s-1".to_string())))
            .await
            .expect("list s-1");
        assert!(in_old.is_empty());
        let in_new = repo
            .list_filtered(Some(&SessionId::new("s-2".to_string())))
            .await
            .expect("list s-2");
        assert_eq!(in_new.len(), 1);
        assert_eq!(in_new[0].id.as_str(), "f-1");
    }

    #[tokio::test]
    async fn session_index_segregates_distinct_sessions() {
        let repo = DashMapForwardRepo::new();
        repo.insert(entity("f-1", "s-1", 8080))
            .await
            .expect("insert f-1");
        repo.insert(entity("f-2", "s-2", 8081))
            .await
            .expect("insert f-2");
        // Removing one session's forwarder leaves the other intact.
        let removed = repo
            .remove(&ForwardId::new("f-1".to_string()))
            .await
            .expect("remove");
        assert!(removed.is_some());
        let remaining = repo
            .list_filtered(Some(&SessionId::new("s-2".to_string())))
            .await
            .expect("list s-2");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id.as_str(), "f-2");
    }

    #[tokio::test]
    async fn cross_instance_isolation_no_shared_state() {
        let repo_a = DashMapForwardRepo::new();
        let repo_b = DashMapForwardRepo::new();
        repo_a
            .insert(entity("f-1", "s-1", 8080))
            .await
            .expect("insert a");
        let in_b = repo_b
            .get(&ForwardId::new("f-1".to_string()))
            .await
            .expect("get b");
        assert!(in_b.is_none(), "fresh repo must not see other repo's data");
        let in_a = repo_a
            .get(&ForwardId::new("f-1".to_string()))
            .await
            .expect("get a")
            .expect("present in a");
        assert_eq!(in_a.id.as_str(), "f-1");
    }

    #[tokio::test]
    async fn clone_shares_state_via_arc() {
        // Cloning the adapter must share the underlying maps so the
        // composition root can hand the same repo to multiple use cases.
        let repo = DashMapForwardRepo::new();
        let twin = repo.clone();
        repo.insert(entity("f-1", "s-1", 8080))
            .await
            .expect("insert via repo");
        let via_twin = twin
            .get(&ForwardId::new("f-1".to_string()))
            .await
            .expect("get via twin")
            .expect("entity present via twin");
        assert_eq!(via_twin.id.as_str(), "f-1");
    }

    #[tokio::test]
    async fn repo_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DashMapForwardRepo>();
    }
}
