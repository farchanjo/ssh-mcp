//! [`crate::ports::command_repo::CommandRepository`] adapter backed by
//! [`dashmap::DashMap`].
//!
//! Two maps:
//!
//! - `by_id: DashMap<CommandId, CommandEntity>` — primary store of
//!   command snapshots keyed by id.
//! - `by_session: DashMap<SessionId, HashSet<CommandId>>` — secondary
//!   index that powers `count_by_session` /
//!   `count_running_by_session` / `list_filtered` (with a session
//!   constraint) in `O(1)` after the shard lookup.
//!
//! ## Ownership and concurrency
//!
//! Both maps are wrapped in `Arc` so cloning the adapter is a pointer
//! bump. The trait stays `async` so a future remote-backed
//! implementation fits without breaking the API.
//!
//! ## Invariants
//!
//! - `insert` rejects re-binding an existing id with
//!   [`DomainError::Internal`]; callers must `remove` first.
//! - `update` requires the id to exist; missing ids resolve to
//!   [`DomainError::CommandNotFound`].
//! - The `by_session` index is kept consistent with the primary store
//!   on every `insert` / `update` (when the owning session changes,
//!   which never happens in practice but is handled defensively) /
//!   `remove`. Empty buckets are dropped immediately.
//! - `list_filtered` returns entities sorted by `started_at` ascending
//!   so list endpoints are stable across calls.
//! - No `await` is performed while a `DashMap` shard guard is alive.

use std::collections::HashSet;
use std::sync::Arc;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use crate::domain::command::CommandEntity;
use crate::domain::error::DomainError;
use crate::domain::ids::{CommandId, SessionId};
use crate::domain::policy::CommandStatusFilter;
use crate::ports::command_repo::CommandRepository;

/// In-process [`CommandRepository`] backed by [`DashMap`].
#[derive(Debug, Default, Clone)]
pub struct DashMapCommandRepo {
    by_id: Arc<DashMap<CommandId, CommandEntity>>,
    by_session: Arc<DashMap<SessionId, HashSet<CommandId>>>,
}

impl DashMapCommandRepo {
    /// Build an empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop a `(session, command)` binding from the secondary index,
    /// pruning the empty bucket so dangling sessions never linger.
    fn unindex(&self, session_id: &SessionId, command_id: &CommandId) {
        let drop_bucket = self
            .by_session
            .get_mut(session_id)
            .is_some_and(|mut bucket| {
                bucket.remove(command_id);
                bucket.is_empty()
            });
        if drop_bucket {
            // Re-check inside `remove_if` so a concurrent `insert`
            // for the same session cannot lose the entry.
            self.by_session
                .remove_if(session_id, |_, bucket| bucket.is_empty());
        }
    }
}

impl CommandRepository for DashMapCommandRepo {
    async fn insert(&self, entity: CommandEntity) -> Result<(), DomainError> {
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
                    "command {id} already exists; remove() before re-insert"
                )));
            }
        }
        self.by_session.entry(session_id).or_default().insert(id);
        Ok(())
    }

    async fn update(&self, entity: CommandEntity) -> Result<(), DomainError> {
        let id = entity.id.clone();
        let new_session = entity.session_id.clone();
        let Some(mut slot) = self.by_id.get_mut(&id) else {
            return Err(DomainError::CommandNotFound(id));
        };
        let previous_session = slot.session_id.clone();
        *slot = entity;
        // Drop the shard guard before touching the secondary index so
        // we never hold two `DashMap` guards simultaneously.
        drop(slot);
        // Defensive: in current use cases the owning session never
        // changes mid-life, but treat a moved entity as a re-binding so
        // the secondary index stays consistent regardless.
        if previous_session != new_session {
            self.unindex(&previous_session, &id);
            self.by_session.entry(new_session).or_default().insert(id);
        }
        Ok(())
    }

    async fn get(&self, id: &CommandId) -> Result<Option<CommandEntity>, DomainError> {
        Ok(self.by_id.get(id).map(|entry| entry.value().clone()))
    }

    async fn remove(&self, id: &CommandId) -> Result<Option<CommandEntity>, DomainError> {
        let removed = self.by_id.remove(id).map(|(_, entity)| entity);
        if let Some(entity) = &removed {
            self.unindex(&entity.session_id, id);
        }
        Ok(removed)
    }

    async fn count_by_session(&self, session_id: &SessionId) -> Result<usize, DomainError> {
        Ok(self
            .by_session
            .get(session_id)
            .map_or(0, |bucket| bucket.len()))
    }

    async fn count_running_by_session(&self, session_id: &SessionId) -> Result<usize, DomainError> {
        let ids: Vec<CommandId> = self
            .by_session
            .get(session_id)
            .map(|bucket| bucket.iter().cloned().collect())
            .unwrap_or_default();
        // Drop the bucket guard before re-entering the primary map so
        // we never hold two shard guards simultaneously.
        let mut count = 0_usize;
        for id in &ids {
            if self
                .by_id
                .get(id)
                .is_some_and(|entry| CommandStatusFilter::Running.matches(entry.value().status))
            {
                count = count.saturating_add(1);
            }
        }
        Ok(count)
    }

    async fn list_filtered(
        &self,
        session_id: Option<&SessionId>,
        status: Option<CommandStatusFilter>,
    ) -> Result<Vec<CommandEntity>, DomainError> {
        let mut out = session_id.map_or_else(
            || self.collect_all(status),
            |session| self.collect_by_session(session, status),
        );
        out.sort_by_key(|entity| entity.started_at);
        Ok(out)
    }
}

impl DashMapCommandRepo {
    /// Collect commands bound to `session` that match `status`.
    fn collect_by_session(
        &self,
        session: &SessionId,
        status: Option<CommandStatusFilter>,
    ) -> Vec<CommandEntity> {
        let ids: Vec<CommandId> = self
            .by_session
            .get(session)
            .map(|bucket| bucket.iter().cloned().collect())
            .unwrap_or_default();
        let mut acc = Vec::with_capacity(ids.len());
        for id in &ids {
            if let Some(entry) = self.by_id.get(id) {
                let entity = entry.value().clone();
                if status.is_none_or(|filter| filter.matches(entity.status)) {
                    acc.push(entity);
                }
            }
        }
        acc
    }

    /// Collect every command in the primary store that matches `status`.
    fn collect_all(&self, status: Option<CommandStatusFilter>) -> Vec<CommandEntity> {
        let mut acc = Vec::with_capacity(self.by_id.len());
        for entry in self.by_id.iter() {
            let entity = entry.value().clone();
            if status.is_none_or(|filter| filter.matches(entity.status)) {
                acc.push(entity);
            }
        }
        acc
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, TimeZone, Utc};

    use super::DashMapCommandRepo;
    use crate::domain::command::{CommandEntity, CommandStatus};
    use crate::domain::error::DomainError;
    use crate::domain::ids::{CommandId, SessionId};
    use crate::domain::policy::CommandStatusFilter;
    use crate::ports::command_repo::CommandRepository;

    fn base_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
    }

    fn entity(id: &str, session: &str, command: &str, offset_secs: i64) -> CommandEntity {
        CommandEntity::new(
            CommandId::new(id.to_string()),
            SessionId::new(session.to_string()),
            command.to_string(),
            base_time() + Duration::seconds(offset_secs),
        )
    }

    #[tokio::test]
    async fn insert_then_get_round_trip() {
        let repo = DashMapCommandRepo::new();
        let e = entity("c-1", "s-1", "echo", 0);
        repo.insert(e.clone()).await.expect("insert");
        let got = repo
            .get(&CommandId::new("c-1".to_string()))
            .await
            .expect("get");
        assert_eq!(got, Some(e));
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let repo = DashMapCommandRepo::new();
        let got = repo
            .get(&CommandId::new("absent".to_string()))
            .await
            .expect("get");
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn insert_duplicate_returns_internal_error() {
        let repo = DashMapCommandRepo::new();
        let e = entity("c-1", "s-1", "echo", 0);
        repo.insert(e.clone()).await.expect("first insert");
        let err = repo
            .insert(e)
            .await
            .expect_err("second insert must fail with Internal");
        match err {
            DomainError::Internal(msg) => assert!(msg.contains("c-1")),
            DomainError::SessionNotFound(_)
            | DomainError::CommandNotFound(_)
            | DomainError::ShellNotFound(_)
            | DomainError::TransferNotFound(_)
            | DomainError::ForwardNotFound(_)
            | DomainError::SerialNotFound(_)
            | DomainError::Serial(_)
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
            | DomainError::Storage(_)
            | DomainError::ResourceGone(_)
            | DomainError::LifecycleStateConflict { .. }
            | DomainError::SessionRefcountUnderflow(_)
            | DomainError::SubNotFound(_)
            | DomainError::MaxSubsPerUriExceeded { .. }
            | DomainError::MaxSubsTotalExceeded { .. }
            | DomainError::LaneBufferFull { .. }
            | DomainError::LagDetected { .. }
            | DomainError::MuxBackpressure
            | DomainError::InvalidLagPolicy(_)
            | DomainError::InvalidLifetime(_)
            | DomainError::RsyncNotFound(_)
            | DomainError::RsyncVersionTooOld(_)
            | DomainError::RsyncProtocolError(_)
            | DomainError::RsyncFileListTooLarge { .. }
            | DomainError::RsyncPartialTransfer(_)
            | DomainError::SftpFeatureMissing(_) => {
                panic!("unexpected error variant: {err:?}")
            }
        }
    }

    #[tokio::test]
    async fn remove_returns_entity_then_get_returns_none() {
        let repo = DashMapCommandRepo::new();
        let e = entity("c-1", "s-1", "echo", 0);
        repo.insert(e.clone()).await.expect("insert");
        let removed = repo
            .remove(&CommandId::new("c-1".to_string()))
            .await
            .expect("remove");
        assert_eq!(removed, Some(e));
        let after = repo
            .get(&CommandId::new("c-1".to_string()))
            .await
            .expect("get");
        assert!(after.is_none());
    }

    #[tokio::test]
    async fn remove_missing_returns_none() {
        let repo = DashMapCommandRepo::new();
        let removed = repo
            .remove(&CommandId::new("absent".to_string()))
            .await
            .expect("remove");
        assert!(removed.is_none());
    }

    #[tokio::test]
    async fn double_remove_is_idempotent() {
        let repo = DashMapCommandRepo::new();
        repo.insert(entity("c-1", "s-1", "echo", 0))
            .await
            .expect("insert");
        let id = CommandId::new("c-1".to_string());
        let first = repo.remove(&id).await.expect("remove 1");
        assert!(first.is_some());
        let second = repo.remove(&id).await.expect("remove 2");
        assert!(second.is_none());
    }

    #[tokio::test]
    async fn update_replaces_stored_entity_and_status() {
        let repo = DashMapCommandRepo::new();
        let e = entity("c-1", "s-1", "echo", 0);
        repo.insert(e.clone()).await.expect("insert");
        let completed = e.complete(0, false);
        repo.update(completed.clone()).await.expect("update");
        let got = repo
            .get(&CommandId::new("c-1".to_string()))
            .await
            .expect("get")
            .expect("present");
        assert_eq!(got.status, CommandStatus::Completed);
        assert_eq!(got.exit_code, Some(0));
        assert_eq!(got, completed);
    }

    #[tokio::test]
    async fn update_missing_id_returns_command_not_found() {
        let repo = DashMapCommandRepo::new();
        let absent = entity("absent", "s-1", "echo", 0);
        let err = repo
            .update(absent)
            .await
            .expect_err("update on missing id must error");
        assert_eq!(
            err,
            DomainError::CommandNotFound(CommandId::new("absent".to_string()))
        );
    }

    #[tokio::test]
    async fn count_by_session_tracks_inserts_and_removes() {
        let repo = DashMapCommandRepo::new();
        let session = SessionId::new("s-1".to_string());
        repo.insert(entity("c-1", "s-1", "a", 0))
            .await
            .expect("insert 1");
        repo.insert(entity("c-2", "s-1", "b", 1))
            .await
            .expect("insert 2");
        assert_eq!(repo.count_by_session(&session).await.expect("count"), 2);
        repo.remove(&CommandId::new("c-1".to_string()))
            .await
            .expect("remove");
        assert_eq!(repo.count_by_session(&session).await.expect("count"), 1);
    }

    #[tokio::test]
    async fn count_by_session_drops_empty_bucket() {
        let repo = DashMapCommandRepo::new();
        let session = SessionId::new("s-1".to_string());
        repo.insert(entity("c-1", "s-1", "a", 0))
            .await
            .expect("insert");
        repo.remove(&CommandId::new("c-1".to_string()))
            .await
            .expect("remove");
        assert_eq!(repo.count_by_session(&session).await.expect("count"), 0);
    }

    #[tokio::test]
    async fn count_running_by_session_filters_by_status() {
        let repo = DashMapCommandRepo::new();
        let session = SessionId::new("s-1".to_string());
        // 1 running, 1 completed, 1 cancelled, 1 failed.
        repo.insert(entity("running", "s-1", "a", 0))
            .await
            .expect("running");
        repo.insert(entity("completed", "s-1", "b", 1).complete(0, false))
            .await
            .expect("completed");
        repo.insert(entity("cancelled", "s-1", "c", 2).cancel())
            .await
            .expect("cancelled");
        repo.insert(entity("failed", "s-1", "d", 3).fail())
            .await
            .expect("failed");
        assert_eq!(repo.count_by_session(&session).await.expect("count"), 4);
        assert_eq!(
            repo.count_running_by_session(&session)
                .await
                .expect("count_running"),
            1
        );
    }

    #[tokio::test]
    async fn count_running_by_session_unknown_session_is_zero() {
        let repo = DashMapCommandRepo::new();
        let session = SessionId::new("absent".to_string());
        assert_eq!(
            repo.count_running_by_session(&session)
                .await
                .expect("count_running"),
            0
        );
    }

    #[tokio::test]
    async fn list_filtered_by_session_only_returns_session_commands() {
        let repo = DashMapCommandRepo::new();
        repo.insert(entity("c-1", "s-1", "a", 0))
            .await
            .expect("c-1");
        repo.insert(entity("c-2", "s-1", "b", 1))
            .await
            .expect("c-2");
        repo.insert(entity("c-3", "s-2", "c", 2))
            .await
            .expect("c-3");
        let session = SessionId::new("s-1".to_string());
        let listed = repo
            .list_filtered(Some(&session), None)
            .await
            .expect("list");
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|c| c.id.as_str() == "c-1"));
        assert!(listed.iter().any(|c| c.id.as_str() == "c-2"));
    }

    #[tokio::test]
    async fn list_filtered_by_status_only_returns_matching_commands() {
        let repo = DashMapCommandRepo::new();
        repo.insert(entity("running", "s-1", "a", 0))
            .await
            .expect("running");
        repo.insert(entity("completed", "s-1", "b", 1).complete(0, false))
            .await
            .expect("completed");
        let listed = repo
            .list_filtered(None, Some(CommandStatusFilter::Running))
            .await
            .expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id.as_str(), "running");
    }

    #[tokio::test]
    async fn list_filtered_by_session_and_status_intersects_both() {
        let repo = DashMapCommandRepo::new();
        repo.insert(entity("c-1", "s-1", "a", 0))
            .await
            .expect("c-1");
        repo.insert(entity("c-2", "s-1", "b", 1).complete(0, false))
            .await
            .expect("c-2");
        repo.insert(entity("c-3", "s-2", "c", 2))
            .await
            .expect("c-3");
        let session = SessionId::new("s-1".to_string());
        let running = repo
            .list_filtered(Some(&session), Some(CommandStatusFilter::Running))
            .await
            .expect("list");
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id.as_str(), "c-1");
        let completed = repo
            .list_filtered(Some(&session), Some(CommandStatusFilter::Completed))
            .await
            .expect("list");
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id.as_str(), "c-2");
    }

    #[tokio::test]
    async fn list_filtered_sorts_by_started_at_ascending() {
        let repo = DashMapCommandRepo::new();
        // Insert out of chronological order to prove the sort.
        repo.insert(entity("c-late", "s-1", "a", 100))
            .await
            .expect("late");
        repo.insert(entity("c-early", "s-1", "b", 1))
            .await
            .expect("early");
        repo.insert(entity("c-mid", "s-1", "c", 50))
            .await
            .expect("mid");
        let listed = repo
            .list_filtered(Some(&SessionId::new("s-1".to_string())), None)
            .await
            .expect("list");
        let ids: Vec<&str> = listed.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["c-early", "c-mid", "c-late"]);
    }

    #[tokio::test]
    async fn list_filtered_no_filters_returns_all_sorted() {
        let repo = DashMapCommandRepo::new();
        repo.insert(entity("c-1", "s-1", "a", 10))
            .await
            .expect("c-1");
        repo.insert(entity("c-2", "s-2", "b", 5))
            .await
            .expect("c-2");
        let listed = repo.list_filtered(None, None).await.expect("list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id.as_str(), "c-2");
        assert_eq!(listed[1].id.as_str(), "c-1");
    }

    #[tokio::test]
    async fn list_filtered_unknown_session_returns_empty_vec() {
        let repo = DashMapCommandRepo::new();
        let session = SessionId::new("absent".to_string());
        let listed = repo
            .list_filtered(Some(&session), None)
            .await
            .expect("list");
        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn cross_instance_isolation_no_shared_state() {
        let repo_a = DashMapCommandRepo::new();
        let repo_b = DashMapCommandRepo::new();
        repo_a
            .insert(entity("c-1", "s-1", "echo", 0))
            .await
            .expect("insert a");
        let in_b = repo_b
            .get(&CommandId::new("c-1".to_string()))
            .await
            .expect("get b");
        assert!(in_b.is_none(), "fresh repo must not see other repo's data");
        let in_a = repo_a
            .get(&CommandId::new("c-1".to_string()))
            .await
            .expect("get a")
            .expect("present in a");
        assert_eq!(in_a.id.as_str(), "c-1");
    }

    #[tokio::test]
    async fn clone_shares_state_via_arc() {
        let repo = DashMapCommandRepo::new();
        let twin = repo.clone();
        repo.insert(entity("c-1", "s-1", "echo", 0))
            .await
            .expect("insert via repo");
        let via_twin = twin
            .get(&CommandId::new("c-1".to_string()))
            .await
            .expect("get via twin")
            .expect("entity present via twin");
        assert_eq!(via_twin.id.as_str(), "c-1");
    }

    #[tokio::test]
    async fn repo_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DashMapCommandRepo>();
    }
}
