//! [`crate::ports::shell_repo::ShellRepository`] adapter backed by
//! [`dashmap::DashMap`].
//!
//! Two maps:
//!
//! - `by_id: DashMap<ShellId, ShellEntity>` — primary store of
//!   shell aggregates keyed by id.
//! - `by_session: DashMap<SessionId, HashSet<ShellId>>` — secondary
//!   index that powers `list_filtered(Some(session))` /
//!   `count_by_session` in `O(1)` after the shard lookup.
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
//! - `update` mutates the stored entity in place. If the new entity
//!   carries a different `session_id` than the one currently stored,
//!   the secondary index is rebalanced to keep `count_by_session`
//!   correct.
//! - `remove` cleans the session bucket entry and drops the bucket
//!   when its last shell is removed so empty buckets never linger.
//! - `list_filtered(None)` returns every stored entity; the
//!   `Some(session)` path goes through the secondary index for `O(1)`
//!   dispatch.
//! - No `await` is performed while a `DashMap` shard guard is alive.

use std::collections::HashSet;
use std::sync::Arc;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use crate::domain::error::DomainError;
use crate::domain::ids::{SessionId, ShellId};
use crate::domain::shell::ShellEntity;
use crate::ports::shell_repo::ShellRepository;

/// In-process [`ShellRepository`] backed by [`DashMap`].
#[derive(Debug, Default, Clone)]
pub struct DashMapShellRepo {
    by_id: Arc<DashMap<ShellId, ShellEntity>>,
    by_session: Arc<DashMap<SessionId, HashSet<ShellId>>>,
}

impl DashMapShellRepo {
    /// Build an empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `shell_id` into the secondary index under `session_id`.
    fn index_insert(&self, session_id: &SessionId, shell_id: &ShellId) {
        self.by_session
            .entry(session_id.clone())
            .or_default()
            .insert(shell_id.clone());
    }

    /// Remove `shell_id` from the secondary index under `session_id`,
    /// dropping the bucket when it becomes empty.
    fn index_remove(&self, session_id: &SessionId, shell_id: &ShellId) {
        let drop_bucket = self
            .by_session
            .get_mut(session_id)
            .is_some_and(|mut bucket| {
                bucket.remove(shell_id);
                bucket.is_empty()
            });
        if drop_bucket {
            // Re-check inside `remove_if` so a concurrent `index_insert`
            // cannot lose the entry.
            self.by_session
                .remove_if(session_id, |_, bucket| bucket.is_empty());
        }
    }
}

impl ShellRepository for DashMapShellRepo {
    async fn insert(&self, entity: ShellEntity) -> Result<(), DomainError> {
        let id = entity.id.clone();
        let session_id = entity.session_id.clone();
        // `entry` keeps the insert atomic per shard so a concurrent
        // `insert` for the same id cannot both succeed.
        match self.by_id.entry(id.clone()) {
            Entry::Vacant(slot) => {
                slot.insert(entity);
                self.index_insert(&session_id, &id);
                Ok(())
            }
            Entry::Occupied(_) => Err(DomainError::Internal(format!(
                "shell {id} already exists; remove() before re-insert"
            ))),
        }
    }

    async fn update(&self, entity: ShellEntity) -> Result<(), DomainError> {
        let id = entity.id.clone();
        let new_session = entity.session_id.clone();
        // Capture the previous session id (if any) to rebalance the
        // secondary index while the shard guard is held briefly.
        let previous_session = self.by_id.get_mut(&id).map(|mut slot| {
            let old_session = slot.session_id.clone();
            *slot = entity;
            old_session
        });
        match previous_session {
            Some(old_session) => {
                if old_session != new_session {
                    self.index_remove(&old_session, &id);
                    self.index_insert(&new_session, &id);
                }
                Ok(())
            }
            None => Err(DomainError::ShellNotFound(id)),
        }
    }

    async fn get(&self, id: &ShellId) -> Result<Option<ShellEntity>, DomainError> {
        // Clone out before dropping the shard guard; never hold a guard
        // across `.await` (no await follows here, but the rule keeps
        // future maintainers honest).
        Ok(self.by_id.get(id).map(|entry| entry.value().clone()))
    }

    async fn remove(&self, id: &ShellId) -> Result<Option<ShellEntity>, DomainError> {
        let removed = self.by_id.remove(id).map(|(_, entity)| entity);
        if let Some(entity) = &removed {
            self.index_remove(&entity.session_id, id);
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
    ) -> Result<Vec<ShellEntity>, DomainError> {
        Ok(session_id.map_or_else(|| self.collect_all(), |sid| self.collect_by_session(sid)))
    }
}

impl DashMapShellRepo {
    /// Snapshot every stored entity.
    fn collect_all(&self) -> Vec<ShellEntity> {
        let mut out = Vec::with_capacity(self.by_id.len());
        for entry in self.by_id.iter() {
            out.push(entry.value().clone());
        }
        out
    }

    /// Snapshot only the entities indexed under `session_id`.
    ///
    /// The secondary-index shard guard is dropped before we touch
    /// `by_id`, preserving the "no `await` while holding a guard"
    /// invariant and avoiding nested-shard contention.
    fn collect_by_session(&self, session_id: &SessionId) -> Vec<ShellEntity> {
        let ids: Vec<ShellId> = self
            .by_session
            .get(session_id)
            .map(|bucket| bucket.iter().cloned().collect())
            .unwrap_or_default();
        let mut out = Vec::with_capacity(ids.len());
        for id in &ids {
            if let Some(entry) = self.by_id.get(id) {
                out.push(entry.value().clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::{TimeZone, Utc};

    use super::DashMapShellRepo;
    use crate::domain::error::DomainError;
    use crate::domain::ids::{SessionId, ShellId};
    use crate::domain::shell::{ShellEntity, ShellStatus, ShellTerminal};
    use crate::ports::shell_repo::ShellRepository;

    fn entity(id: &str, session: &str) -> ShellEntity {
        ShellEntity::new(
            ShellId::new(id.to_string()),
            SessionId::new(session.to_string()),
            ShellTerminal::new("xterm-256color".to_string(), 80, 24),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            Duration::from_secs(600),
            10 * 1024 * 1024,
        )
    }

    #[tokio::test]
    async fn insert_then_get_round_trip() {
        let repo = DashMapShellRepo::new();
        let e = entity("sh-1", "s-1");
        repo.insert(e.clone()).await.expect("insert");
        let got = repo
            .get(&ShellId::new("sh-1".to_string()))
            .await
            .expect("get");
        assert_eq!(got, Some(e));
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let repo = DashMapShellRepo::new();
        let got = repo
            .get(&ShellId::new("absent".to_string()))
            .await
            .expect("get");
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn insert_duplicate_returns_internal_error() {
        let repo = DashMapShellRepo::new();
        let e = entity("sh-1", "s-1");
        repo.insert(e.clone()).await.expect("first insert");
        let err = repo
            .insert(e)
            .await
            .expect_err("second insert must fail with Internal");
        match err {
            DomainError::Internal(msg) => assert!(msg.contains("sh-1")),
            DomainError::SessionNotFound(_)
            | DomainError::CommandNotFound(_)
            | DomainError::ShellNotFound(_)
            | DomainError::TransferNotFound(_)
            | DomainError::ForwardNotFound(_)
            | DomainError::MaxCommandsExceeded { .. }
            | DomainError::MaxShellsExceeded { .. }
            | DomainError::MaxTransfersExceeded { .. }
            | DomainError::InvalidArgument(_)
            | DomainError::ConnectFailed(_)
            | DomainError::Auth(_)
            | DomainError::Timeout(_)
            | DomainError::Sftp(_)
            | DomainError::PortInUse(_)
            | DomainError::Transport(_)
            | DomainError::Storage(_) => panic!("unexpected error variant: {err:?}"),
        }
    }

    #[tokio::test]
    async fn remove_returns_entity_then_get_returns_none() {
        let repo = DashMapShellRepo::new();
        let e = entity("sh-1", "s-1");
        repo.insert(e.clone()).await.expect("insert");
        let removed = repo
            .remove(&ShellId::new("sh-1".to_string()))
            .await
            .expect("remove");
        assert_eq!(removed, Some(e));
        let after = repo
            .get(&ShellId::new("sh-1".to_string()))
            .await
            .expect("get");
        assert!(after.is_none());
    }

    #[tokio::test]
    async fn remove_missing_returns_none() {
        let repo = DashMapShellRepo::new();
        let removed = repo
            .remove(&ShellId::new("absent".to_string()))
            .await
            .expect("remove");
        assert!(removed.is_none());
    }

    #[tokio::test]
    async fn update_mutates_stored_entity() {
        let repo = DashMapShellRepo::new();
        repo.insert(entity("sh-1", "s-1")).await.expect("insert");
        let mut next = entity("sh-1", "s-1");
        next = next.close();
        repo.update(next.clone()).await.expect("update");
        let got = repo
            .get(&ShellId::new("sh-1".to_string()))
            .await
            .expect("get")
            .expect("entity present");
        assert_eq!(got.status, ShellStatus::Closed);
        assert_eq!(got, next);
    }

    #[tokio::test]
    async fn update_on_missing_id_returns_shell_not_found() {
        let repo = DashMapShellRepo::new();
        let id = ShellId::new("absent".to_string());
        let err = repo
            .update(entity("absent", "s-1"))
            .await
            .expect_err("expected ShellNotFound");
        assert_eq!(err, DomainError::ShellNotFound(id));
    }

    #[tokio::test]
    async fn update_rebalances_index_when_session_changes() {
        let repo = DashMapShellRepo::new();
        repo.insert(entity("sh-1", "s-1")).await.expect("insert");
        // Re-bind sh-1 under a different session id.
        repo.update(entity("sh-1", "s-2"))
            .await
            .expect("update with new session");
        let count_old = repo
            .count_by_session(&SessionId::new("s-1".to_string()))
            .await
            .expect("count s-1");
        let count_new = repo
            .count_by_session(&SessionId::new("s-2".to_string()))
            .await
            .expect("count s-2");
        assert_eq!(count_old, 0, "old session bucket must be drained");
        assert_eq!(count_new, 1, "new session bucket must own sh-1");
    }

    #[tokio::test]
    async fn count_by_session_reflects_inserts_and_removes() {
        let repo = DashMapShellRepo::new();
        let session = SessionId::new("s-1".to_string());
        assert_eq!(
            repo.count_by_session(&session).await.expect("count empty"),
            0
        );
        repo.insert(entity("sh-1", "s-1")).await.expect("insert 1");
        repo.insert(entity("sh-2", "s-1")).await.expect("insert 2");
        assert_eq!(
            repo.count_by_session(&session)
                .await
                .expect("count after 2 inserts"),
            2
        );
        repo.remove(&ShellId::new("sh-1".to_string()))
            .await
            .expect("remove 1");
        assert_eq!(
            repo.count_by_session(&session)
                .await
                .expect("count after 1 remove"),
            1
        );
        repo.remove(&ShellId::new("sh-2".to_string()))
            .await
            .expect("remove 2");
        assert_eq!(
            repo.count_by_session(&session)
                .await
                .expect("count after both removes"),
            0
        );
    }

    #[tokio::test]
    async fn list_filtered_none_returns_every_entity() {
        let repo = DashMapShellRepo::new();
        repo.insert(entity("sh-1", "s-1")).await.expect("insert 1");
        repo.insert(entity("sh-2", "s-2")).await.expect("insert 2");
        repo.insert(entity("sh-3", "s-1")).await.expect("insert 3");
        let mut all = repo.list_filtered(None).await.expect("list all");
        all.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id.as_str(), "sh-1");
        assert_eq!(all[1].id.as_str(), "sh-2");
        assert_eq!(all[2].id.as_str(), "sh-3");
    }

    #[tokio::test]
    async fn list_filtered_some_session_returns_only_that_bucket() {
        let repo = DashMapShellRepo::new();
        repo.insert(entity("sh-1", "s-1")).await.expect("insert 1");
        repo.insert(entity("sh-2", "s-2")).await.expect("insert 2");
        repo.insert(entity("sh-3", "s-1")).await.expect("insert 3");
        let session = SessionId::new("s-1".to_string());
        let mut filtered = repo.list_filtered(Some(&session)).await.expect("filtered");
        filtered.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].id.as_str(), "sh-1");
        assert_eq!(filtered[1].id.as_str(), "sh-3");
    }

    #[tokio::test]
    async fn list_filtered_unknown_session_returns_empty_vec() {
        let repo = DashMapShellRepo::new();
        repo.insert(entity("sh-1", "s-1")).await.expect("insert");
        let session = SessionId::new("absent".to_string());
        let filtered = repo.list_filtered(Some(&session)).await.expect("filtered");
        assert!(filtered.is_empty());
    }

    #[tokio::test]
    async fn session_index_segregates_distinct_sessions() {
        let repo = DashMapShellRepo::new();
        repo.insert(entity("sh-1", "s-1")).await.expect("insert 1");
        repo.insert(entity("sh-2", "s-2")).await.expect("insert 2");
        // Removing one shell leaves the other bucket intact.
        repo.remove(&ShellId::new("sh-1".to_string()))
            .await
            .expect("remove 1");
        let count_s1 = repo
            .count_by_session(&SessionId::new("s-1".to_string()))
            .await
            .expect("count s-1");
        let count_s2 = repo
            .count_by_session(&SessionId::new("s-2".to_string()))
            .await
            .expect("count s-2");
        assert_eq!(count_s1, 0);
        assert_eq!(count_s2, 1);
    }

    #[tokio::test]
    async fn cross_instance_isolation_no_shared_state() {
        let repo_a = DashMapShellRepo::new();
        let repo_b = DashMapShellRepo::new();
        repo_a
            .insert(entity("sh-1", "s-1"))
            .await
            .expect("insert a");
        let in_b = repo_b
            .get(&ShellId::new("sh-1".to_string()))
            .await
            .expect("get b");
        assert!(in_b.is_none(), "fresh repo must not see other repo's data");
        let in_a = repo_a
            .get(&ShellId::new("sh-1".to_string()))
            .await
            .expect("get a")
            .expect("present in a");
        assert_eq!(in_a.id.as_str(), "sh-1");
    }

    #[tokio::test]
    async fn clone_shares_state_via_arc() {
        // Cloning the adapter must share the underlying maps so the
        // composition root can hand the same repo to multiple use cases.
        let repo = DashMapShellRepo::new();
        let twin = repo.clone();
        repo.insert(entity("sh-1", "s-1"))
            .await
            .expect("insert via repo");
        let via_twin = twin
            .get(&ShellId::new("sh-1".to_string()))
            .await
            .expect("get via twin")
            .expect("entity present via twin");
        assert_eq!(via_twin.id.as_str(), "sh-1");
        // Counts via the twin must also reflect the shared secondary index.
        let count_via_twin = twin
            .count_by_session(&SessionId::new("s-1".to_string()))
            .await
            .expect("count via twin");
        assert_eq!(count_via_twin, 1);
    }

    #[tokio::test]
    async fn repo_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DashMapShellRepo>();
    }
}
