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
#[cfg(feature = "port_forward")]
use crate::adapters::repo::dashmap::forward::DashMapForwardRepo;
use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
#[cfg(feature = "port_forward")]
use crate::adapters::ssh::internal::status_sink::ForwardRegistrationSink;
use crate::adapters::ssh::internal::status_sink::{
    CommandRegistrationSink, CommandStatusSink, ShellRegistrationSink, ShellStatusSink,
    TransferRegistrationSink, TransferStatusSink,
};
use crate::domain::command::CommandEntity;
#[cfg(feature = "port_forward")]
use crate::domain::forward::ForwardEntity;
#[cfg(feature = "port_forward")]
use crate::domain::ids::ForwardId;
use crate::domain::ids::{CommandId, ShellId, TransferId};
use crate::domain::shell::ShellEntity;
use crate::domain::transfer::TransferEntity;
use crate::ports::command_repo::CommandRepository;
use crate::ports::config::ConfigPort;
#[cfg(feature = "port_forward")]
use crate::ports::forward_repo::ForwardRepository;
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

// ---------------------------------------------------------------------------
// Registration sinks (v4.3 fix: registration parity with adapter tables)
// ---------------------------------------------------------------------------
//
// Bridge the adapter-side `RunningCommand` / `RunningShell` /
// `TransferShared` / `ForwardHandle` lifecycle into the matching domain
// repositories. The composition root passes the same `Arc<DashMap*Repo>`
// handle to both these sinks and the use cases consuming the repos, so
// `resources/subscribe shell://X/output` (and the friends) observe the
// row the moment the adapter has bound it — no race between the SSH-side
// `do_open_shell` returning and the use case-side `insert` landing.

/// Production [`CommandRegistrationSink`] backed by a shared
/// [`DashMapCommandRepo`].
#[derive(Debug, Clone)]
pub struct RepoCommandRegistrationSink {
    repo: Arc<DashMapCommandRepo>,
}

impl RepoCommandRegistrationSink {
    /// Wire the sink to an already-shared repository handle.
    #[must_use]
    pub const fn new(repo: Arc<DashMapCommandRepo>) -> Self {
        Self { repo }
    }
}

impl CommandRegistrationSink for RepoCommandRegistrationSink {
    fn register(&self, entity: CommandEntity) -> SinkFuture<'_> {
        Box::pin(async move {
            let id = entity.id.clone();
            // The use case (`execute_command`) is the canonical writer of
            // this row. The sink only fills the race window between the
            // adapter binding the command and the use case's insert
            // landing — so we never overwrite an existing row, just make
            // sure one exists.
            match self.repo.get(&id).await {
                Ok(Some(_)) => {
                    debug!(
                        command_id = %id,
                        "command registration sink: row already present (use case already inserted)"
                    );
                }
                Ok(None) => {
                    if let Err(err) = self.repo.insert(entity).await {
                        warn!(
                            command_id = %id,
                            ?err,
                            "command registration sink: insert failed (race observed entity then lost it)"
                        );
                    }
                }
                Err(err) => {
                    warn!(
                        command_id = %id,
                        ?err,
                        "command registration sink: get failed before register"
                    );
                }
            }
        })
    }

    fn unregister<'a>(&'a self, command_id: &'a CommandId) -> SinkFuture<'a> {
        Box::pin(async move {
            // `remove` returns `Ok(None)` when the row is already gone;
            // surface only true storage failures.
            if let Err(err) = self.repo.remove(command_id).await {
                warn!(
                    command_id = %command_id,
                    ?err,
                    "command registration sink: remove failed (entity may be orphaned)"
                );
            }
        })
    }
}

/// Production [`ShellRegistrationSink`] backed by a shared
/// [`DashMapShellRepo`].
#[derive(Debug, Clone)]
pub struct RepoShellRegistrationSink {
    repo: Arc<DashMapShellRepo>,
}

impl RepoShellRegistrationSink {
    /// Wire the sink to an already-shared repository handle.
    #[must_use]
    pub const fn new(repo: Arc<DashMapShellRepo>) -> Self {
        Self { repo }
    }
}

impl ShellRegistrationSink for RepoShellRegistrationSink {
    fn register(&self, entity: ShellEntity) -> SinkFuture<'_> {
        Box::pin(async move {
            let id = entity.id.clone();
            // See the rationale on `RepoCommandRegistrationSink::register`.
            match self.repo.get(&id).await {
                Ok(Some(_)) => {
                    debug!(
                        shell_id = %id,
                        "shell registration sink: row already present (use case already inserted)"
                    );
                }
                Ok(None) => {
                    if let Err(err) = self.repo.insert(entity).await {
                        warn!(
                            shell_id = %id,
                            ?err,
                            "shell registration sink: insert failed (race observed entity then lost it)"
                        );
                    }
                }
                Err(err) => {
                    warn!(
                        shell_id = %id,
                        ?err,
                        "shell registration sink: get failed before register"
                    );
                }
            }
        })
    }

    fn unregister<'a>(&'a self, shell_id: &'a ShellId) -> SinkFuture<'a> {
        Box::pin(async move {
            if let Err(err) = self.repo.remove(shell_id).await {
                warn!(
                    shell_id = %shell_id,
                    ?err,
                    "shell registration sink: remove failed (entity may be orphaned)"
                );
            }
        })
    }
}

/// Production [`TransferRegistrationSink`] backed by a shared
/// [`DashMapTransferRepo`].
///
/// v4.3 fix: takes a cloned [`Arc<dyn ConfigPort>`] so adapter-side
/// `register` calls go through
/// [`crate::ports::transfer_repo::TransferRepository::insert_if_under_cap`]
/// — a swarm of 50 concurrent `ssh_upload` calls cannot land 50 rows even
/// when the SFTP adapter spawns the registration sink before the use case
/// has had a chance to insert. Without this gate, the adapter-side
/// register would slip past the per-session transfer cap during heavy
/// chaos contention (TOCTOU on `count_by_session` + `insert`).
#[derive(Debug, Clone)]
pub struct RepoTransferRegistrationSink {
    repo: Arc<DashMapTransferRepo>,
    config: Arc<crate::adapters::config::env::EnvConfig>,
}

impl RepoTransferRegistrationSink {
    /// Wire the sink to an already-shared repository handle and an
    /// already-shared config handle (used to read the live per-session
    /// cap on every register call).
    #[must_use]
    pub const fn new(
        repo: Arc<DashMapTransferRepo>,
        config: Arc<crate::adapters::config::env::EnvConfig>,
    ) -> Self {
        Self { repo, config }
    }
}

impl TransferRegistrationSink for RepoTransferRegistrationSink {
    fn register(&self, entity: TransferEntity) -> SinkFuture<'_> {
        Box::pin(async move {
            let id = entity.id.clone();
            // See the rationale on `RepoCommandRegistrationSink::register`
            // for the get-then-insert structure. The cap-aware insert
            // closes the TOCTOU window described on the struct doc.
            match self.repo.get(&id).await {
                Ok(Some(_)) => {
                    debug!(
                        transfer_id = %id,
                        "transfer registration sink: row already present (use case already inserted)"
                    );
                }
                Ok(None) => {
                    let cap = self.config.max_transfers_per_session();
                    match self.repo.insert_if_under_cap(entity, cap).await {
                        Ok(()) => {}
                        Err(crate::domain::error::DomainError::MaxTransfersExceeded {
                            limit,
                        }) => {
                            // Use case will surface the user-visible
                            // error; the sink merely steps aside.
                            debug!(
                                transfer_id = %id,
                                limit,
                                "transfer registration sink: cap reached (use case will reject)"
                            );
                        }
                        Err(err) => {
                            warn!(
                                transfer_id = %id,
                                ?err,
                                "transfer registration sink: insert_if_under_cap failed"
                            );
                        }
                    }
                }
                Err(err) => {
                    warn!(
                        transfer_id = %id,
                        ?err,
                        "transfer registration sink: get failed before register"
                    );
                }
            }
        })
    }

    fn unregister<'a>(&'a self, transfer_id: &'a TransferId) -> SinkFuture<'a> {
        Box::pin(async move {
            if let Err(err) = self.repo.remove(transfer_id).await {
                warn!(
                    transfer_id = %transfer_id,
                    ?err,
                    "transfer registration sink: remove failed (entity may be orphaned)"
                );
            }
        })
    }
}

/// Production [`ForwardRegistrationSink`] backed by a shared
/// [`DashMapForwardRepo`]. Feature-gated alongside the forward repo port.
#[cfg(feature = "port_forward")]
#[derive(Debug, Clone)]
pub struct RepoForwardRegistrationSink {
    repo: Arc<DashMapForwardRepo>,
}

#[cfg(feature = "port_forward")]
impl RepoForwardRegistrationSink {
    /// Wire the sink to an already-shared repository handle.
    #[must_use]
    pub const fn new(repo: Arc<DashMapForwardRepo>) -> Self {
        Self { repo }
    }
}

#[cfg(feature = "port_forward")]
impl ForwardRegistrationSink for RepoForwardRegistrationSink {
    fn register(&self, entity: ForwardEntity) -> SinkFuture<'_> {
        Box::pin(async move {
            let id = entity.id.clone();
            // See the rationale on `RepoCommandRegistrationSink::register`.
            match self.repo.get(&id).await {
                Ok(Some(_)) => {
                    debug!(
                        forward_id = %id,
                        "forward registration sink: row already present (use case already inserted)"
                    );
                }
                Ok(None) => {
                    if let Err(err) = self.repo.insert(entity).await {
                        warn!(
                            forward_id = %id,
                            ?err,
                            "forward registration sink: insert failed (race observed entity then lost it)"
                        );
                    }
                }
                Err(err) => {
                    warn!(
                        forward_id = %id,
                        ?err,
                        "forward registration sink: get failed before register"
                    );
                }
            }
        })
    }

    fn unregister<'a>(&'a self, forward_id: &'a ForwardId) -> SinkFuture<'a> {
        Box::pin(async move {
            if let Err(err) = self.repo.remove(forward_id).await {
                warn!(
                    forward_id = %forward_id,
                    ?err,
                    "forward registration sink: remove failed (entity may be orphaned)"
                );
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RepoCommandRegistrationSink, RepoCommandStatusSink, RepoShellRegistrationSink,
        RepoShellStatusSink, RepoTransferRegistrationSink, RepoTransferStatusSink,
    };
    use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
    use crate::adapters::ssh::internal::status_sink::{
        CommandRegistrationSink, CommandStatusSink, ShellRegistrationSink, ShellStatusSink,
        TransferRegistrationSink, TransferStatusSink,
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

    // -------------------------------------------------------------------
    // Registration sink coverage (v4.3)
    // -------------------------------------------------------------------

    fn cmd_entity(id: &str) -> CommandEntity {
        CommandEntity::new(
            CommandId::new(id.to_string()),
            SessionId::new("sess-1".to_string()),
            "true".to_string(),
            Utc::now(),
        )
    }

    fn shell_entity(id: &str) -> ShellEntity {
        ShellEntity::new(
            ShellId::new(id.to_string()),
            SessionId::new("sess-1".to_string()),
            ShellTerminal::new("xterm".to_string(), 80, 24),
            Utc::now(),
            Duration::from_secs(60),
            1024,
        )
    }

    fn transfer_entity(id: &str) -> TransferEntity {
        TransferEntity::new(
            TransferId::new(id.to_string()),
            SessionId::new("sess-1".to_string()),
            TransferDirection::Upload,
            "/tmp/local".to_string(),
            "/srv/remote".to_string(),
            Utc::now(),
            1024,
        )
    }

    #[tokio::test]
    async fn command_registration_sink_inserts_then_removes() {
        let repo = Arc::new(DashMapCommandRepo::new());
        let sink = RepoCommandRegistrationSink::new(Arc::clone(&repo));
        let entity = cmd_entity("c-reg-1");
        let id = entity.id.clone();
        sink.register(entity).await;
        assert!(repo.get(&id).await.expect("get").is_some());
        sink.unregister(&id).await;
        assert!(repo.get(&id).await.expect("get").is_none());
    }

    #[tokio::test]
    async fn command_registration_sink_register_is_idempotent_on_duplicate() {
        let repo = Arc::new(DashMapCommandRepo::new());
        let sink = RepoCommandRegistrationSink::new(Arc::clone(&repo));
        let entity = cmd_entity("c-dup");
        let twin = entity.clone();
        // First register lands the row; second is logged at debug! level
        // and silently returns so an adapter-side register racing with a
        // use-case-side insert never panics.
        sink.register(entity).await;
        sink.register(twin).await;
        assert!(repo.get(&CommandId::new("c-dup".to_string())).await.expect("get").is_some());
    }

    #[tokio::test]
    async fn command_registration_sink_unregister_silent_when_absent() {
        let repo = Arc::new(DashMapCommandRepo::new());
        let sink = RepoCommandRegistrationSink::new(Arc::clone(&repo));
        // Must not panic.
        sink.unregister(&CommandId::new("ghost".to_string())).await;
    }

    #[tokio::test]
    async fn shell_registration_sink_inserts_then_removes() {
        let repo = Arc::new(DashMapShellRepo::new());
        let sink = RepoShellRegistrationSink::new(Arc::clone(&repo));
        let entity = shell_entity("sh-reg-1");
        let id = entity.id.clone();
        sink.register(entity).await;
        assert!(repo.get(&id).await.expect("get").is_some());
        sink.unregister(&id).await;
        assert!(repo.get(&id).await.expect("get").is_none());
    }

    #[tokio::test]
    async fn shell_registration_sink_register_is_idempotent_on_duplicate() {
        let repo = Arc::new(DashMapShellRepo::new());
        let sink = RepoShellRegistrationSink::new(Arc::clone(&repo));
        let entity = shell_entity("sh-dup");
        let twin = entity.clone();
        sink.register(entity).await;
        sink.register(twin).await;
        assert!(repo.get(&ShellId::new("sh-dup".to_string())).await.expect("get").is_some());
    }

    #[tokio::test]
    async fn transfer_registration_sink_inserts_then_removes() {
        let repo = Arc::new(DashMapTransferRepo::new());
        let cfg = Arc::new(crate::adapters::config::env::EnvConfig);
        let sink = RepoTransferRegistrationSink::new(Arc::clone(&repo), cfg);
        let entity = transfer_entity("t-reg-1");
        let id = entity.id.clone();
        sink.register(entity).await;
        assert!(repo.get(&id).await.expect("get").is_some());
        sink.unregister(&id).await;
        assert!(repo.get(&id).await.expect("get").is_none());
    }

    #[tokio::test]
    async fn transfer_registration_sink_register_is_idempotent_on_duplicate() {
        let repo = Arc::new(DashMapTransferRepo::new());
        let cfg = Arc::new(crate::adapters::config::env::EnvConfig);
        let sink = RepoTransferRegistrationSink::new(Arc::clone(&repo), cfg);
        let entity = transfer_entity("t-dup");
        let twin = entity.clone();
        sink.register(entity).await;
        sink.register(twin).await;
        assert!(repo.get(&TransferId::new("t-dup".to_string())).await.expect("get").is_some());
    }

    #[tokio::test]
    async fn transfer_registration_sink_honours_per_session_cap() {
        // EnvConfig defaults `max_transfers_per_session` to 10 — verify
        // 50 concurrent registers all targeting the same session do not
        // breach the cap when the use case has not landed any row yet.
        let repo = Arc::new(DashMapTransferRepo::new());
        let cfg = Arc::new(crate::adapters::config::env::EnvConfig);
        let sink = Arc::new(RepoTransferRegistrationSink::new(
            Arc::clone(&repo),
            Arc::clone(&cfg),
        ));
        let session = SessionId::new("burst-1".to_string());
        let mut joins = Vec::with_capacity(50);
        for i in 0..50_u32 {
            let sink_clone = Arc::clone(&sink);
            let session_id = session.clone();
            joins.push(tokio::spawn(async move {
                let entity = TransferEntity::new(
                    TransferId::new(format!("t-burst-{i}")),
                    session_id,
                    TransferDirection::Upload,
                    "/tmp/local".to_string(),
                    "/srv/remote".to_string(),
                    Utc::now(),
                    1024,
                );
                sink_clone.register(entity).await;
            }));
        }
        for j in joins {
            j.await.expect("join");
        }
        let count = repo.count_by_session(&session).await.expect("count");
        assert!(
            count <= 10,
            "transfer registration sink must not breach per-session cap; got {count}"
        );
    }

    #[cfg(feature = "port_forward")]
    #[tokio::test]
    async fn forward_registration_sink_inserts_then_removes() {
        use super::RepoForwardRegistrationSink;
        use crate::adapters::repo::dashmap::forward::DashMapForwardRepo;
        use crate::adapters::ssh::internal::status_sink::ForwardRegistrationSink;
        use crate::domain::forward::ForwardEntity;
        use crate::domain::ids::ForwardId;
        use crate::ports::forward_repo::ForwardRepository;
        let repo = Arc::new(DashMapForwardRepo::new());
        let sink = RepoForwardRegistrationSink::new(Arc::clone(&repo));
        let id = ForwardId::new("fwd-reg-1".to_string());
        let entity = ForwardEntity::new(
            id.clone(),
            SessionId::new("sess-1".to_string()),
            8080,
            "internal".to_string(),
            80,
            Utc::now(),
        );
        sink.register(entity).await;
        assert!(repo.get(&id).await.expect("get").is_some());
        sink.unregister(&id).await;
        assert!(repo.get(&id).await.expect("get").is_none());
    }
}
