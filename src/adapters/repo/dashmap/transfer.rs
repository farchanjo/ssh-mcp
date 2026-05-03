//! [`crate::ports::transfer_repo::TransferRepository`] adapter backed by
//! [`dashmap::DashMap`].
//!
//! Two maps:
//!
//! - `by_id: DashMap<TransferId, TransferEntity>` — primary store of
//!   transfer aggregates keyed by id.
//! - `by_session: DashMap<SessionId, HashSet<TransferId>>` — secondary
//!   index that powers `list_filtered(Some(_))` / `count_by_session` in
//!   `O(1)` after the shard lookup.
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
//! - `update` requires the id to exist and reports
//!   [`DomainError::TransferNotFound`] otherwise. The session bucket is
//!   re-keyed if the entity's `session_id` changed between snapshots so
//!   the secondary index stays consistent.
//! - `remove` cleans the session bucket and drops the empty entry so
//!   stale buckets never linger.
//! - No `await` is performed while a `DashMap` shard guard is alive.

use std::collections::HashSet;
use std::sync::Arc;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use crate::domain::error::DomainError;
use crate::domain::ids::{SessionId, TransferId};
use crate::domain::transfer::TransferEntity;
use crate::ports::transfer_repo::TransferRepository;

/// In-process [`TransferRepository`] backed by [`DashMap`].
#[derive(Debug, Default, Clone)]
pub struct DashMapTransferRepo {
    by_id: Arc<DashMap<TransferId, TransferEntity>>,
    by_session: Arc<DashMap<SessionId, HashSet<TransferId>>>,
}

impl DashMapTransferRepo {
    /// Build an empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a transfer id to its owning session bucket. Idempotent.
    fn index_session(&self, session_id: &SessionId, transfer_id: &TransferId) {
        self.by_session
            .entry(session_id.clone())
            .or_default()
            .insert(transfer_id.clone());
    }

    /// Detach a transfer id from its session bucket and drop the bucket
    /// when it becomes empty.
    fn deindex_session(&self, session_id: &SessionId, transfer_id: &TransferId) {
        let drop_bucket = self
            .by_session
            .get_mut(session_id)
            .is_some_and(|mut bucket| {
                bucket.remove(transfer_id);
                bucket.is_empty()
            });
        if drop_bucket {
            // Re-check inside `remove_if` so a concurrent `index_session`
            // cannot lose the entry.
            self.by_session
                .remove_if(session_id, |_, bucket| bucket.is_empty());
        }
    }
}

impl TransferRepository for DashMapTransferRepo {
    async fn insert(&self, entity: TransferEntity) -> Result<(), DomainError> {
        let id = entity.id.clone();
        let session_id = entity.session_id.clone();
        // `entry` keeps the insert atomic per shard so a concurrent
        // `insert` for the same id cannot both succeed.
        match self.by_id.entry(id.clone()) {
            Entry::Vacant(slot) => {
                // `slot.insert` consumes the entry guard, returning a
                // RefMut. Drop it before touching the secondary index so
                // we never hold two shard guards at once.
                drop(slot.insert(entity));
                self.index_session(&session_id, &id);
                Ok(())
            }
            Entry::Occupied(_) => Err(DomainError::Internal(format!(
                "transfer {id} already exists; remove() before re-insert"
            ))),
        }
    }

    async fn update(&self, entity: TransferEntity) -> Result<(), DomainError> {
        let id = entity.id.clone();
        let new_session = entity.session_id.clone();
        // Capture the previous owning session before mutating so we can
        // rewire the secondary index if it changed. Cloning out before
        // dropping the shard guard keeps us off the no-await rule.
        let old_session = match self.by_id.get_mut(&id) {
            Some(mut slot) => {
                let prev = slot.session_id.clone();
                *slot = entity;
                prev
            }
            None => return Err(DomainError::TransferNotFound(id)),
        };
        if old_session != new_session {
            self.deindex_session(&old_session, &id);
            self.index_session(&new_session, &id);
        }
        Ok(())
    }

    async fn get(&self, id: &TransferId) -> Result<Option<TransferEntity>, DomainError> {
        // Clone out before dropping the shard guard; never hold a guard
        // across `.await` (no await follows here, but the rule keeps
        // future maintainers honest).
        Ok(self.by_id.get(id).map(|entry| entry.value().clone()))
    }

    async fn remove(&self, id: &TransferId) -> Result<Option<TransferEntity>, DomainError> {
        let removed = self.by_id.remove(id).map(|(_, entity)| entity);
        if let Some(entity) = &removed {
            self.deindex_session(&entity.session_id, id);
        }
        Ok(removed)
    }

    async fn count_by_session(&self, session_id: &SessionId) -> Result<usize, DomainError> {
        Ok(self
            .by_session
            .get(session_id)
            .map_or(0, |bucket| bucket.len()))
    }

    async fn list_filtered(
        &self,
        session_id: Option<&SessionId>,
    ) -> Result<Vec<TransferEntity>, DomainError> {
        Ok(session_id.map_or_else(
            || {
                let mut out = Vec::with_capacity(self.by_id.len());
                for entry in self.by_id.iter() {
                    out.push(entry.value().clone());
                }
                out
            },
            |sid| {
                let ids: Vec<TransferId> = self
                    .by_session
                    .get(sid)
                    .map(|bucket| bucket.iter().cloned().collect())
                    .unwrap_or_default();
                let mut out = Vec::with_capacity(ids.len());
                for id in &ids {
                    if let Some(entry) = self.by_id.get(id) {
                        out.push(entry.value().clone());
                    }
                }
                out
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::DashMapTransferRepo;
    use crate::domain::error::DomainError;
    use crate::domain::ids::{SessionId, TransferId};
    use crate::domain::transfer::{TransferDirection, TransferEntity, TransferStatus};
    use crate::ports::transfer_repo::TransferRepository;

    fn entity(id: &str, session: &str, direction: TransferDirection) -> TransferEntity {
        TransferEntity::new(
            TransferId::new(id.to_string()),
            SessionId::new(session.to_string()),
            direction,
            "/tmp/local".to_string(),
            "/srv/remote".to_string(),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            1024,
        )
    }

    #[tokio::test]
    async fn insert_then_get_round_trip() {
        let repo = DashMapTransferRepo::new();
        let e = entity("t-1", "s-1", TransferDirection::Upload);
        repo.insert(e.clone()).await.expect("insert");
        let got = repo
            .get(&TransferId::new("t-1".to_string()))
            .await
            .expect("get");
        assert_eq!(got, Some(e));
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let repo = DashMapTransferRepo::new();
        let got = repo
            .get(&TransferId::new("absent".to_string()))
            .await
            .expect("get");
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn insert_duplicate_returns_internal_error() {
        let repo = DashMapTransferRepo::new();
        let e = entity("t-1", "s-1", TransferDirection::Upload);
        repo.insert(e.clone()).await.expect("first insert");
        let err = repo
            .insert(e)
            .await
            .expect_err("second insert must fail with Internal");
        match err {
            DomainError::Internal(msg) => assert!(msg.contains("t-1")),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn remove_returns_entity_and_clears_index() {
        let repo = DashMapTransferRepo::new();
        let e = entity("t-1", "s-1", TransferDirection::Upload);
        repo.insert(e.clone()).await.expect("insert");
        let removed = repo
            .remove(&TransferId::new("t-1".to_string()))
            .await
            .expect("remove");
        assert_eq!(removed, Some(e));
        let after = repo
            .get(&TransferId::new("t-1".to_string()))
            .await
            .expect("get");
        assert!(after.is_none());
        let count = repo
            .count_by_session(&SessionId::new("s-1".to_string()))
            .await
            .expect("count");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn remove_missing_returns_none() {
        let repo = DashMapTransferRepo::new();
        let removed = repo
            .remove(&TransferId::new("absent".to_string()))
            .await
            .expect("remove");
        assert!(removed.is_none());
    }

    #[tokio::test]
    async fn update_persists_progress() {
        let repo = DashMapTransferRepo::new();
        let e = entity("t-1", "s-1", TransferDirection::Upload);
        repo.insert(e.clone()).await.expect("insert");
        let progressed = e.with_progress(512);
        repo.update(progressed.clone()).await.expect("update");
        let got = repo
            .get(&TransferId::new("t-1".to_string()))
            .await
            .expect("get")
            .expect("present");
        assert_eq!(got.bytes_transferred, 512);
        assert_eq!(got.status, TransferStatus::Running);
    }

    #[tokio::test]
    async fn update_missing_returns_transfer_not_found() {
        let repo = DashMapTransferRepo::new();
        let e = entity("ghost", "s-1", TransferDirection::Upload);
        let id = e.id.clone();
        let err = repo.update(e).await.expect_err("expected TransferNotFound");
        assert_eq!(err, DomainError::TransferNotFound(id));
    }

    #[tokio::test]
    async fn update_re_keys_session_index_when_session_changes() {
        let repo = DashMapTransferRepo::new();
        let e = entity("t-1", "s-1", TransferDirection::Upload);
        repo.insert(e.clone()).await.expect("insert");
        let mut moved = e;
        moved.session_id = SessionId::new("s-2".to_string());
        repo.update(moved).await.expect("update with new session");
        let s1_count = repo
            .count_by_session(&SessionId::new("s-1".to_string()))
            .await
            .expect("count s1");
        let s2_count = repo
            .count_by_session(&SessionId::new("s-2".to_string()))
            .await
            .expect("count s2");
        assert_eq!(s1_count, 0);
        assert_eq!(s2_count, 1);
    }

    #[tokio::test]
    async fn count_by_session_grows_with_each_insert() {
        let repo = DashMapTransferRepo::new();
        let session = SessionId::new("s-1".to_string());
        for i in 0..5_usize {
            let id = format!("t-{i}");
            repo.insert(entity(&id, "s-1", TransferDirection::Upload))
                .await
                .expect("insert");
        }
        let count = repo.count_by_session(&session).await.expect("count");
        assert_eq!(count, 5);
    }

    #[tokio::test]
    async fn count_by_session_unknown_returns_zero() {
        let repo = DashMapTransferRepo::new();
        let count = repo
            .count_by_session(&SessionId::new("absent".to_string()))
            .await
            .expect("count");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn list_filtered_by_session_returns_only_matching() {
        let repo = DashMapTransferRepo::new();
        repo.insert(entity("t-a1", "s-A", TransferDirection::Upload))
            .await
            .expect("a1");
        repo.insert(entity("t-a2", "s-A", TransferDirection::Download))
            .await
            .expect("a2");
        repo.insert(entity("t-b1", "s-B", TransferDirection::Upload))
            .await
            .expect("b1");
        let only_a = repo
            .list_filtered(Some(&SessionId::new("s-A".to_string())))
            .await
            .expect("filtered");
        assert_eq!(only_a.len(), 2);
        assert!(only_a.iter().all(|t| t.session_id.as_str() == "s-A"));
    }

    #[tokio::test]
    async fn list_filtered_with_none_returns_all() {
        let repo = DashMapTransferRepo::new();
        repo.insert(entity("t-1", "s-A", TransferDirection::Upload))
            .await
            .expect("t-1");
        repo.insert(entity("t-2", "s-B", TransferDirection::Download))
            .await
            .expect("t-2");
        let all = repo.list_filtered(None).await.expect("list all");
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn list_filtered_unknown_session_returns_empty() {
        let repo = DashMapTransferRepo::new();
        repo.insert(entity("t-1", "s-A", TransferDirection::Upload))
            .await
            .expect("insert");
        let none = repo
            .list_filtered(Some(&SessionId::new("absent".to_string())))
            .await
            .expect("filtered");
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn multiple_sessions_isolated() {
        let repo = DashMapTransferRepo::new();
        for i in 0..3_usize {
            let id = format!("t-A-{i}");
            repo.insert(entity(&id, "s-A", TransferDirection::Upload))
                .await
                .expect("insert A");
        }
        for i in 0..2_usize {
            let id = format!("t-B-{i}");
            repo.insert(entity(&id, "s-B", TransferDirection::Download))
                .await
                .expect("insert B");
        }
        let count_a = repo
            .count_by_session(&SessionId::new("s-A".to_string()))
            .await
            .expect("count A");
        let count_b = repo
            .count_by_session(&SessionId::new("s-B".to_string()))
            .await
            .expect("count B");
        assert_eq!(count_a, 3);
        assert_eq!(count_b, 2);
    }

    #[tokio::test]
    async fn cross_instance_isolation_no_shared_state() {
        let repo_a = DashMapTransferRepo::new();
        let repo_b = DashMapTransferRepo::new();
        repo_a
            .insert(entity("t-1", "s-1", TransferDirection::Upload))
            .await
            .expect("insert a");
        let in_b = repo_b
            .get(&TransferId::new("t-1".to_string()))
            .await
            .expect("get b");
        assert!(in_b.is_none(), "fresh repo must not see other repo's data");
    }

    #[tokio::test]
    async fn clone_shares_state_via_arc() {
        // Cloning the adapter must share the underlying maps so the
        // composition root can hand the same repo to multiple use cases.
        let repo = DashMapTransferRepo::new();
        let twin = repo.clone();
        repo.insert(entity("t-1", "s-1", TransferDirection::Upload))
            .await
            .expect("insert via repo");
        let via_twin = twin
            .get(&TransferId::new("t-1".to_string()))
            .await
            .expect("get via twin")
            .expect("entity present via twin");
        assert_eq!(via_twin.id.as_str(), "t-1");
    }

    #[tokio::test]
    async fn repo_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DashMapTransferRepo>();
    }
}
