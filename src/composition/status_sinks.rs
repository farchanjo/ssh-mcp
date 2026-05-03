//! Production [`crate::adapters::ssh::internal::status_sink`] implementations.
//!
//! Bridge the SSH/SFTP runtime watchers (owned by
//! [`crate::adapters::ssh::russh_adapter::RusshAdapter`] and
//! [`crate::adapters::sftp::russh_sftp_adapter::RusshSftpAdapter`]) to
//! the in-memory v4 repositories. Lives here, in the composition layer,
//! because each sink couples a runtime trait surface to a concrete
//! repository implementation — a coupling that belongs at the wiring
//! seam rather than inside any single adapter.
//!
//! ## Failure handling
//!
//! Every sink call is best-effort. A missing entity (the use case may
//! have removed the row before the watcher fired) is logged at
//! `debug!` level; storage failures surface at `warn!`. Neither
//! propagates into the watcher task — the runtime stays up no matter
//! what the repository does.

use core::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracing::{debug, warn};

use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
use crate::adapters::ssh::internal::status_sink::{
    CommandStatusSink, ShellStatusSink, TransferStatusSink,
};
use crate::domain::command::CommandEntity;
use crate::domain::ids::{CommandId, ShellId, TransferId};
use crate::domain::transfer::TransferEntity;
use crate::ports::command_repo::CommandRepository;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::transfer_repo::TransferRepository;

/// Future returned by every sink method. Boxed so the trait stays
/// object-safe.
type SinkFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

// ---------------------------------------------------------------------------
// Command sink
// ---------------------------------------------------------------------------

/// Production [`CommandStatusSink`] backed by a shared
/// [`DashMapCommandRepo`].
#[derive(Debug, Clone)]
pub struct RepoCommandStatusSink {
    repo: Arc<DashMapCommandRepo>,
}

impl RepoCommandStatusSink {
    /// Wire the sink to an already-shared repository handle.
    #[must_use]
    pub const fn new(repo: Arc<DashMapCommandRepo>) -> Self {
        Self { repo }
    }

    /// Internal helper: load the entity, apply `mutate`, and persist via
    /// `update`. Logs (without panicking) when the entity is missing or
    /// the storage call fails.
    async fn mutate_persist<F>(&self, command_id: &CommandId, mutate: F)
    where
        F: FnOnce(CommandEntity) -> CommandEntity + Send,
    {
        match self.repo.get(command_id).await {
            Ok(Some(entity)) => {
                let next = mutate(entity);
                if let Err(err) = self.repo.update(next).await {
                    warn!(
                        command_id = %command_id,
                        ?err,
                        "command status sink: update failed (entity left in previous state)"
                    );
                }
            }
            Ok(None) => {
                debug!(
                    command_id = %command_id,
                    "command status sink: entity already absent (probably removed by use case)"
                );
            }
            Err(err) => {
                warn!(
                    command_id = %command_id,
                    ?err,
                    "command status sink: get failed before status pump"
                );
            }
        }
    }
}

impl CommandStatusSink for RepoCommandStatusSink {
    fn mark_completed<'a>(
        &'a self,
        command_id: &'a CommandId,
        exit_code: i32,
        timed_out: bool,
    ) -> SinkFuture<'a> {
        Box::pin(async move {
            self.mutate_persist(command_id, |entity| entity.complete(exit_code, timed_out))
                .await;
        })
    }

    fn mark_failed<'a>(
        &'a self,
        command_id: &'a CommandId,
        _error: Option<String>,
    ) -> SinkFuture<'a> {
        // The current `CommandEntity` schema does not carry an error
        // string; callers can still observe the failure via `status =
        // Failed`. Surfacing the error message is a follow-up tracked
        // outside the v4.2 fix scope.
        Box::pin(async move {
            self.mutate_persist(command_id, CommandEntity::fail).await;
        })
    }

    fn mark_cancelled<'a>(&'a self, command_id: &'a CommandId) -> SinkFuture<'a> {
        Box::pin(async move {
            self.mutate_persist(command_id, CommandEntity::cancel).await;
        })
    }
}

// ---------------------------------------------------------------------------
// Shell sink
// ---------------------------------------------------------------------------

/// Production [`ShellStatusSink`] backed by a shared [`DashMapShellRepo`].
#[derive(Debug, Clone)]
pub struct RepoShellStatusSink {
    repo: Arc<DashMapShellRepo>,
}

impl RepoShellStatusSink {
    /// Wire the sink to an already-shared repository handle.
    #[must_use]
    pub const fn new(repo: Arc<DashMapShellRepo>) -> Self {
        Self { repo }
    }
}

impl ShellStatusSink for RepoShellStatusSink {
    fn mark_closed<'a>(&'a self, shell_id: &'a ShellId) -> SinkFuture<'a> {
        Box::pin(async move {
            match self.repo.get(shell_id).await {
                Ok(Some(entity)) => {
                    let closed = entity.close();
                    if let Err(err) = self.repo.update(closed).await {
                        warn!(
                            shell_id = %shell_id,
                            ?err,
                            "shell status sink: update failed (entity left in previous state)"
                        );
                    }
                }
                Ok(None) => {
                    debug!(
                        shell_id = %shell_id,
                        "shell status sink: entity already absent (probably removed by use case)"
                    );
                }
                Err(err) => {
                    warn!(
                        shell_id = %shell_id,
                        ?err,
                        "shell status sink: get failed before status pump"
                    );
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Transfer sink
// ---------------------------------------------------------------------------

/// Production [`TransferStatusSink`] backed by a shared
/// [`DashMapTransferRepo`].
#[derive(Debug, Clone)]
pub struct RepoTransferStatusSink {
    repo: Arc<DashMapTransferRepo>,
}

impl RepoTransferStatusSink {
    /// Wire the sink to an already-shared repository handle.
    #[must_use]
    pub const fn new(repo: Arc<DashMapTransferRepo>) -> Self {
        Self { repo }
    }

    /// Internal helper: load the entity, apply `mutate`, and persist via
    /// `update`. Logs (without panicking) when the entity is missing or
    /// the storage call fails.
    async fn mutate_persist<F>(&self, transfer_id: &TransferId, mutate: F)
    where
        F: FnOnce(TransferEntity) -> TransferEntity + Send,
    {
        match self.repo.get(transfer_id).await {
            Ok(Some(entity)) => {
                let next = mutate(entity);
                if let Err(err) = self.repo.update(next).await {
                    warn!(
                        transfer_id = %transfer_id,
                        ?err,
                        "transfer status sink: update failed (entity left in previous state)"
                    );
                }
            }
            Ok(None) => {
                debug!(
                    transfer_id = %transfer_id,
                    "transfer status sink: entity already absent (probably removed by use case)"
                );
            }
            Err(err) => {
                warn!(
                    transfer_id = %transfer_id,
                    ?err,
                    "transfer status sink: get failed before status pump"
                );
            }
        }
    }
}

impl TransferStatusSink for RepoTransferStatusSink {
    fn mark_completed<'a>(
        &'a self,
        transfer_id: &'a TransferId,
        bytes_transferred: u64,
    ) -> SinkFuture<'a> {
        Box::pin(async move {
            self.mutate_persist(transfer_id, |entity| entity.complete(bytes_transferred))
                .await;
        })
    }

    fn mark_failed<'a>(
        &'a self,
        transfer_id: &'a TransferId,
        error: Option<String>,
    ) -> SinkFuture<'a> {
        Box::pin(async move {
            let reason = error.unwrap_or_else(|| "transfer failed".to_string());
            self.mutate_persist(transfer_id, |entity| entity.fail(reason))
                .await;
        })
    }

    fn mark_cancelled<'a>(&'a self, transfer_id: &'a TransferId) -> SinkFuture<'a> {
        Box::pin(async move {
            self.mutate_persist(transfer_id, TransferEntity::cancel)
                .await;
        })
    }

    fn record_progress<'a>(
        &'a self,
        transfer_id: &'a TransferId,
        bytes_transferred: u64,
    ) -> SinkFuture<'a> {
        Box::pin(async move {
            self.mutate_persist(transfer_id, |entity| {
                entity.with_progress(bytes_transferred)
            })
            .await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{RepoCommandStatusSink, RepoShellStatusSink, RepoTransferStatusSink};
    use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
    use crate::adapters::ssh::internal::status_sink::{
        CommandStatusSink, ShellStatusSink, TransferStatusSink,
    };
    use crate::domain::command::{CommandEntity, CommandStatus};
    use crate::domain::ids::{CommandId, SessionId, ShellId, TransferId};
    use crate::domain::shell::{ShellEntity, ShellStatus, ShellTerminal};
    use crate::domain::transfer::{TransferDirection, TransferEntity, TransferStatus};
    use crate::ports::command_repo::CommandRepository;
    use crate::ports::shell_repo::ShellRepository;
    use crate::ports::transfer_repo::TransferRepository;
    use chrono::Utc;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn command_sink_completes_existing_entity() {
        let repo = Arc::new(DashMapCommandRepo::new());
        let sink = RepoCommandStatusSink::new(Arc::clone(&repo));
        let id = CommandId::new("c1".to_string());
        repo.insert(CommandEntity::new(
            id.clone(),
            SessionId::new("s1".to_string()),
            "true".to_string(),
            Utc::now(),
        ))
        .await
        .expect("insert");
        sink.mark_completed(&id, 0, false).await;
        let after = repo.get(&id).await.expect("get").expect("present");
        assert_eq!(after.status, CommandStatus::Completed);
        assert_eq!(after.exit_code, Some(0_i32));
    }

    #[tokio::test]
    async fn command_sink_marks_cancelled() {
        let repo = Arc::new(DashMapCommandRepo::new());
        let sink = RepoCommandStatusSink::new(Arc::clone(&repo));
        let id = CommandId::new("c2".to_string());
        repo.insert(CommandEntity::new(
            id.clone(),
            SessionId::new("s1".to_string()),
            "sleep 60".to_string(),
            Utc::now(),
        ))
        .await
        .expect("insert");
        sink.mark_cancelled(&id).await;
        let after = repo.get(&id).await.expect("get").expect("present");
        assert_eq!(after.status, CommandStatus::Cancelled);
    }

    #[tokio::test]
    async fn command_sink_marks_failed_without_error_string() {
        let repo = Arc::new(DashMapCommandRepo::new());
        let sink = RepoCommandStatusSink::new(Arc::clone(&repo));
        let id = CommandId::new("c3".to_string());
        repo.insert(CommandEntity::new(
            id.clone(),
            SessionId::new("s1".to_string()),
            "foo".to_string(),
            Utc::now(),
        ))
        .await
        .expect("insert");
        sink.mark_failed(&id, Some("boom".to_string())).await;
        let after = repo.get(&id).await.expect("get").expect("present");
        assert_eq!(after.status, CommandStatus::Failed);
    }

    #[tokio::test]
    async fn command_sink_silent_when_entity_absent() {
        let repo = Arc::new(DashMapCommandRepo::new());
        let sink = RepoCommandStatusSink::new(Arc::clone(&repo));
        let id = CommandId::new("ghost".to_string());
        // Must not panic.
        sink.mark_completed(&id, 0, false).await;
    }

    #[tokio::test]
    async fn shell_sink_marks_closed_existing_entity() {
        let repo = Arc::new(DashMapShellRepo::new());
        let sink = RepoShellStatusSink::new(Arc::clone(&repo));
        let id = ShellId::new("sh1".to_string());
        repo.insert(ShellEntity::new(
            id.clone(),
            SessionId::new("s1".to_string()),
            ShellTerminal::new("xterm".to_string(), 80, 24),
            Utc::now(),
            Duration::from_secs(60),
            1024,
        ))
        .await
        .expect("insert");
        sink.mark_closed(&id).await;
        let after = repo.get(&id).await.expect("get").expect("present");
        assert_eq!(after.status, ShellStatus::Closed);
    }

    #[tokio::test]
    async fn shell_sink_silent_when_entity_absent() {
        let repo = Arc::new(DashMapShellRepo::new());
        let sink = RepoShellStatusSink::new(Arc::clone(&repo));
        let id = ShellId::new("ghost".to_string());
        sink.mark_closed(&id).await;
    }

    #[tokio::test]
    async fn transfer_sink_completes_existing_entity_with_byte_count() {
        let repo = Arc::new(DashMapTransferRepo::new());
        let sink = RepoTransferStatusSink::new(Arc::clone(&repo));
        let id = TransferId::new("t1".to_string());
        repo.insert(TransferEntity::new(
            id.clone(),
            SessionId::new("s1".to_string()),
            TransferDirection::Upload,
            "/tmp/local".to_string(),
            "/srv/remote".to_string(),
            Utc::now(),
            1024,
        ))
        .await
        .expect("insert");
        sink.mark_completed(&id, 1024).await;
        let after = repo.get(&id).await.expect("get").expect("present");
        assert_eq!(after.status, TransferStatus::Completed);
        assert_eq!(after.bytes_transferred, 1024_u64);
    }

    #[tokio::test]
    async fn transfer_sink_marks_failed_with_reason() {
        let repo = Arc::new(DashMapTransferRepo::new());
        let sink = RepoTransferStatusSink::new(Arc::clone(&repo));
        let id = TransferId::new("t2".to_string());
        repo.insert(TransferEntity::new(
            id.clone(),
            SessionId::new("s1".to_string()),
            TransferDirection::Upload,
            "/tmp/local".to_string(),
            "/srv/remote".to_string(),
            Utc::now(),
            1024,
        ))
        .await
        .expect("insert");
        sink.mark_failed(&id, Some("disk full".to_string())).await;
        let after = repo.get(&id).await.expect("get").expect("present");
        assert_eq!(after.status, TransferStatus::Failed);
        assert_eq!(after.error.as_deref(), Some("disk full"));
    }

    #[tokio::test]
    async fn transfer_sink_marks_cancelled() {
        let repo = Arc::new(DashMapTransferRepo::new());
        let sink = RepoTransferStatusSink::new(Arc::clone(&repo));
        let id = TransferId::new("t3".to_string());
        repo.insert(TransferEntity::new(
            id.clone(),
            SessionId::new("s1".to_string()),
            TransferDirection::Download,
            "/tmp/local".to_string(),
            "/srv/remote".to_string(),
            Utc::now(),
            1024,
        ))
        .await
        .expect("insert");
        sink.mark_cancelled(&id).await;
        let after = repo.get(&id).await.expect("get").expect("present");
        assert_eq!(after.status, TransferStatus::Cancelled);
    }

    #[tokio::test]
    async fn transfer_sink_records_progress_without_status_change() {
        let repo = Arc::new(DashMapTransferRepo::new());
        let sink = RepoTransferStatusSink::new(Arc::clone(&repo));
        let id = TransferId::new("t4".to_string());
        repo.insert(TransferEntity::new(
            id.clone(),
            SessionId::new("s1".to_string()),
            TransferDirection::Upload,
            "/tmp/local".to_string(),
            "/srv/remote".to_string(),
            Utc::now(),
            1024,
        ))
        .await
        .expect("insert");
        sink.record_progress(&id, 256).await;
        let after = repo.get(&id).await.expect("get").expect("present");
        assert_eq!(after.status, TransferStatus::Running);
        assert_eq!(after.bytes_transferred, 256_u64);
    }
}
