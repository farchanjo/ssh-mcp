//! [`crate::ports::rsync_repo::RsyncRepository`] adapter backed by
//! [`dashmap::DashMap`].
//!
//! Mirrors the layout of
//! [`crate::adapters::repo::dashmap::transfer::DashMapTransferRepo`] —
//! a primary `by_id` map plus a `by_session` secondary index that
//! powers [`Self::list_filtered`] and [`Self::count_by_session`] in
//! `O(1)` after the shard lookup.
//!
//! Stored values are wrapped in [`Arc`] so producer threads (the
//! transport reader loop) and consumer threads (`ssh_rsync_stats`)
//! observe the same atomic counters.

use std::collections::HashSet;
use std::sync::Arc;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use crate::domain::error::DomainError;
use crate::domain::ids::SessionId;
use crate::domain::rsync::RsyncSession;
use crate::domain::rsync_ids::RsyncId;
use crate::ports::rsync_repo::RsyncRepository;

/// In-process [`RsyncRepository`] backed by [`DashMap`].
#[derive(Debug, Default, Clone)]
pub struct DashMapRsyncRepo {
    by_id: Arc<DashMap<RsyncId, Arc<RsyncSession>>>,
    by_session: Arc<DashMap<SessionId, HashSet<RsyncId>>>,
}

impl DashMapRsyncRepo {
    /// Build an empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind an rsync id to its owning session bucket. Idempotent.
    fn index_session(&self, session_id: &SessionId, rsync_id: &RsyncId) {
        self.by_session
            .entry(session_id.clone())
            .or_default()
            .insert(rsync_id.clone());
    }

    /// Detach an rsync id from its session bucket and drop the bucket
    /// when it becomes empty.
    fn deindex_session(&self, session_id: &SessionId, rsync_id: &RsyncId) {
        let drop_bucket = self
            .by_session
            .get_mut(session_id)
            .is_some_and(|mut bucket| {
                bucket.remove(rsync_id);
                bucket.is_empty()
            });
        if drop_bucket {
            self.by_session
                .remove_if(session_id, |_, bucket| bucket.is_empty());
        }
    }
}

impl RsyncRepository for DashMapRsyncRepo {
    async fn insert(&self, entity: Arc<RsyncSession>) -> Result<(), DomainError> {
        let id = entity.id().clone();
        let session_id = entity.session_id().clone();
        match self.by_id.entry(id.clone()) {
            Entry::Vacant(slot) => {
                drop(slot.insert(entity));
                self.index_session(&session_id, &id);
                Ok(())
            }
            Entry::Occupied(_) => Err(DomainError::Internal(format!(
                "rsync session {id} already exists; remove() before re-insert"
            ))),
        }
    }

    async fn insert_if_under_cap(
        &self,
        entity: Arc<RsyncSession>,
        cap: usize,
    ) -> Result<(), DomainError> {
        let id = entity.id().clone();
        let session_id = entity.session_id().clone();

        // Take the session-bucket guard FIRST so the count probe and
        // the membership update happen atomically against any other
        // `insert_if_under_cap` racing on the same session.
        let mut bucket = self.by_session.entry(session_id).or_default();
        if bucket.len() >= cap {
            return Err(DomainError::MaxTransfersExceeded { limit: cap });
        }
        let result = match self.by_id.entry(id.clone()) {
            Entry::Vacant(slot) => {
                drop(slot.insert(entity));
                bucket.insert(id);
                Ok(())
            }
            Entry::Occupied(_) => Err(DomainError::Internal(format!(
                "rsync session {id} already exists; remove() before re-insert"
            ))),
        };
        drop(bucket);
        result
    }

    async fn get(&self, id: &RsyncId) -> Result<Option<Arc<RsyncSession>>, DomainError> {
        Ok(self.by_id.get(id).map(|entry| Arc::clone(entry.value())))
    }

    async fn remove(&self, id: &RsyncId) -> Result<Option<Arc<RsyncSession>>, DomainError> {
        let removed = self.by_id.remove(id).map(|(_, entity)| entity);
        if let Some(entity) = &removed {
            self.deindex_session(entity.session_id(), id);
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
    ) -> Result<Vec<Arc<RsyncSession>>, DomainError> {
        Ok(session_id.map_or_else(
            || {
                let mut out = Vec::with_capacity(self.by_id.len());
                for entry in self.by_id.iter() {
                    out.push(Arc::clone(entry.value()));
                }
                out
            },
            |sid| {
                let ids: Vec<RsyncId> = self
                    .by_session
                    .get(sid)
                    .map(|bucket| bucket.iter().cloned().collect())
                    .unwrap_or_default();
                let mut out = Vec::with_capacity(ids.len());
                for id in &ids {
                    if let Some(entry) = self.by_id.get(id) {
                        out.push(Arc::clone(entry.value()));
                    }
                }
                out
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::DashMapRsyncRepo;
    use crate::domain::error::DomainError;
    use crate::domain::ids::SessionId;
    use crate::domain::rsync::RsyncSession;
    use crate::domain::rsync_ids::RsyncId;
    use crate::ports::rsync_repo::RsyncRepository;
    use std::sync::Arc;

    fn fresh(id: &str, session: &str) -> Arc<RsyncSession> {
        Arc::new(RsyncSession::new(
            RsyncId::new(id.to_string()),
            SessionId::new(session.to_string()),
        ))
    }

    #[tokio::test]
    async fn insert_then_get_round_trip() {
        let repo = DashMapRsyncRepo::new();
        let entity = fresh("rs-1", "s-1");
        repo.insert(Arc::clone(&entity)).await.expect("insert");
        let got = repo
            .get(&RsyncId::new("rs-1".to_string()))
            .await
            .expect("get");
        assert!(got.is_some());
        assert_eq!(got.expect("present").id().as_str(), "rs-1");
    }

    #[tokio::test]
    async fn insert_duplicate_returns_internal_error() {
        let repo = DashMapRsyncRepo::new();
        let entity = fresh("rs-1", "s-1");
        repo.insert(Arc::clone(&entity)).await.expect("first");
        let err = repo.insert(entity).await.expect_err("second must collide");
        match err {
            DomainError::Internal(msg) => assert!(msg.contains("rs-1")),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn remove_clears_indices() {
        let repo = DashMapRsyncRepo::new();
        repo.insert(fresh("rs-1", "s-1")).await.expect("insert");
        let removed = repo
            .remove(&RsyncId::new("rs-1".to_string()))
            .await
            .expect("remove");
        assert!(removed.is_some());
        let after = repo
            .get(&RsyncId::new("rs-1".to_string()))
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
    async fn count_by_session_grows_with_each_insert() {
        let repo = DashMapRsyncRepo::new();
        for i in 0..5_usize {
            repo.insert(fresh(&format!("rs-{i}"), "s-1"))
                .await
                .expect("insert");
        }
        let count = repo
            .count_by_session(&SessionId::new("s-1".to_string()))
            .await
            .expect("count");
        assert_eq!(count, 5);
    }

    #[tokio::test]
    async fn list_filtered_by_session_isolates_buckets() {
        let repo = DashMapRsyncRepo::new();
        repo.insert(fresh("rs-A1", "s-A")).await.expect("a1");
        repo.insert(fresh("rs-A2", "s-A")).await.expect("a2");
        repo.insert(fresh("rs-B1", "s-B")).await.expect("b1");
        let only_a = repo
            .list_filtered(Some(&SessionId::new("s-A".to_string())))
            .await
            .expect("filtered");
        assert_eq!(only_a.len(), 2);
        assert!(only_a.iter().all(|s| s.session_id().as_str() == "s-A"));
    }

    #[tokio::test]
    async fn list_filtered_with_none_returns_all() {
        let repo = DashMapRsyncRepo::new();
        repo.insert(fresh("rs-1", "s-A")).await.expect("rs-1");
        repo.insert(fresh("rs-2", "s-B")).await.expect("rs-2");
        let all = repo.list_filtered(None).await.expect("list all");
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn insert_if_under_cap_rejects_at_cap() {
        let repo = DashMapRsyncRepo::new();
        let cap = 3_usize;
        for i in 0..cap {
            repo.insert_if_under_cap(fresh(&format!("rs-{i}"), "s-cap"), cap)
                .await
                .expect("under cap");
        }
        let err = repo
            .insert_if_under_cap(fresh("rs-overflow", "s-cap"), cap)
            .await
            .expect_err("must reject at cap");
        match err {
            DomainError::MaxTransfersExceeded { limit } => assert_eq!(limit, cap),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn arc_sharing_lets_producer_and_reader_observe_same_counters() {
        let repo = DashMapRsyncRepo::new();
        let entity = fresh("rs-1", "s-1");
        entity.with_files_total(7);
        repo.insert(Arc::clone(&entity)).await.expect("insert");
        // Producer-side increments propagate through Arc sharing.
        entity.record_file_done(1024, 256);
        let via_repo = repo
            .get(&RsyncId::new("rs-1".to_string()))
            .await
            .expect("get")
            .expect("present");
        let snapshot = via_repo.snapshot();
        assert_eq!(snapshot.files_total, 7);
        assert_eq!(snapshot.files_done, 1);
        assert_eq!(snapshot.bytes_transferred, 1024);
        assert_eq!(snapshot.bytes_skipped, 256);
    }

    #[tokio::test]
    async fn clone_shares_state_via_arc() {
        let repo = DashMapRsyncRepo::new();
        let twin = repo.clone();
        repo.insert(fresh("rs-1", "s-1")).await.expect("insert");
        let via_twin = twin
            .get(&RsyncId::new("rs-1".to_string()))
            .await
            .expect("get")
            .expect("present");
        assert_eq!(via_twin.id().as_str(), "rs-1");
    }

    #[tokio::test]
    async fn repo_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DashMapRsyncRepo>();
    }
}
