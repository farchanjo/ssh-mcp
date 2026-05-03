//! Adapter-internal **status sinks**.
//!
//! Bridge the live status transitions emitted by the SSH/SFTP runtime
//! tasks (the `tokio::sync::watch::Sender`s living on
//! [`crate::adapters::ssh::internal::async_command::RunningCommand`],
//! [`crate::adapters::ssh::internal::shell::RunningShell`] and
//! [`crate::adapters::sftp::internal::sftp::TransferShared`]) into the
//! domain-level repositories used by the v4 use cases (`CommandRepository`,
//! `ShellRepository`, `TransferRepository`).
//!
//! ## Why a dedicated sink trait?
//!
//! The repository ports declared in [`crate::ports`] are AFIT (`async fn
//! in trait`), so they are **not** object-safe. The production [`super::super::russh_adapter::RusshAdapter`]
//! is intentionally non-generic so the composition root can pin a single
//! [`type ConcreteSsh = RusshAdapter`] alias without dragging the repo
//! generics through every type-list. Squaring those two constraints
//! requires a small purpose-built **`Send + Sync`** trait that returns
//! `Pin<Box<dyn Future<Output = ()> + Send + '_>>` — a manual flavour of
//! AFIT that **is** object-safe and lets the adapter hold a
//! `Arc<dyn CommandStatusSink + Send + Sync>` field with the
//! [`NoopCommandStatusSink`] default.
//!
//! ## Concurrency contract
//!
//! Every sink method:
//! - is invoked from a **dedicated background task** spawned per command /
//!   shell / transfer — no shared lock is held across the future,
//! - resolves the entity through the repository port (cloned `Arc`) and
//!   issues a write-back via the existing `update` method,
//! - swallows `DomainError` failures via `tracing::warn!` because the
//!   sink is best-effort: a missing entity is acceptable (it means the
//!   use case already removed the row), and any other failure is logged
//!   so operators can spot it without taking the runtime down.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::domain::command::CommandEntity;
#[cfg(feature = "port_forward")]
use crate::domain::forward::ForwardEntity;
#[cfg(feature = "port_forward")]
use crate::domain::ids::ForwardId;
use crate::domain::ids::{CommandId, ShellId, TransferId};
use crate::domain::shell::ShellEntity;
use crate::domain::transfer::TransferEntity;

/// Future returned by every sink method. Boxed so the trait stays
/// object-safe.
type SinkFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Bridge between live `RunningCommand` status transitions and the
/// domain `CommandRepository`.
///
/// Implemented in the composition root by [`crate::composition::prod`]
/// so the SSH adapter stays free of repo generics.
pub trait CommandStatusSink: Send + Sync {
    /// Persist `Completed` along with the captured exit code and
    /// timed-out flag.
    fn mark_completed<'a>(
        &'a self,
        command_id: &'a CommandId,
        exit_code: i32,
        timed_out: bool,
    ) -> SinkFuture<'a>;

    /// Persist `Failed`. The optional error string mirrors the value
    /// stored on `RunningCommand.error` so the use case can surface it
    /// later (the current `CommandEntity` schema does not carry the
    /// error string but the field is preserved for forward
    /// compatibility — see [`crate::application::get_command_output`]).
    fn mark_failed<'a>(
        &'a self,
        command_id: &'a CommandId,
        error: Option<String>,
    ) -> SinkFuture<'a>;

    /// Persist `Cancelled`.
    fn mark_cancelled<'a>(&'a self, command_id: &'a CommandId) -> SinkFuture<'a>;
}

/// Bridge between live `RunningShell` status transitions and the
/// domain `ShellRepository`.
///
/// The current production runtime never sends to
/// `RunningShell.status_tx`, so the sink only sees the explicit
/// `closed()` notification fired from
/// [`super::super::russh_adapter::RusshAdapter::close_shell`] — keeping
/// the pump symmetric with the command/transfer story.
pub trait ShellStatusSink: Send + Sync {
    /// Persist `Closed`.
    fn mark_closed<'a>(&'a self, shell_id: &'a ShellId) -> SinkFuture<'a>;
}

/// Bridge between the live `TransferShared` status transitions and the
/// domain `TransferRepository`.
pub trait TransferStatusSink: Send + Sync {
    /// Persist `Completed` along with the final byte count.
    fn mark_completed<'a>(
        &'a self,
        transfer_id: &'a TransferId,
        bytes_transferred: u64,
    ) -> SinkFuture<'a>;

    /// Persist `Failed` with an optional error description.
    fn mark_failed<'a>(
        &'a self,
        transfer_id: &'a TransferId,
        error: Option<String>,
    ) -> SinkFuture<'a>;

    /// Persist `Cancelled`.
    fn mark_cancelled<'a>(&'a self, transfer_id: &'a TransferId) -> SinkFuture<'a>;

    /// Update the running byte counter without changing status. Used for
    /// progress watchers that want the repository to reflect partial
    /// completion (so `wait`-mode polls show the latest bytes).
    fn record_progress<'a>(
        &'a self,
        transfer_id: &'a TransferId,
        bytes_transferred: u64,
    ) -> SinkFuture<'a>;
}

// ---------------------------------------------------------------------------
// Registration sinks (v4.3)
// ---------------------------------------------------------------------------
//
// The status sinks above only flip an *existing* repository row to a
// terminal state. They do **not** create or destroy rows. Until v4.3 the
// only writers were the use cases (`open_shell`, `execute_command`,
// `upload_file`, `download_file`, `forward_port`), which inserted **after**
// the SSH/SFTP adapter had already bound the entity into its private
// DashMap. That created two problems:
//
// 1. **Race window.** Subscribers calling `resources/subscribe shell://X`
//    immediately after `ssh_shell_open` could observe the adapter row
//    but no repo row, surfacing `ShellNotFound` even though the shell
//    was alive on the wire.
// 2. **Adapter-driven teardown is invisible to the repo.** When the
//    adapter removes an entity (cancel, close, transfer task end, etc.)
//    the repository keeps the stale row until a use case eventually runs
//    `remove`.
//
// The registration sinks close that gap: the adapter calls `register`
// the moment it inserts into its own table and `unregister` on every
// removal path. The composition root binds these sinks to the same
// repository handles that downstream use cases query. Like the status
// sinks, every method is best-effort and logs (rather than panics) on
// repository faults so a transient storage hiccup never tears the
// runtime down.

/// Bridge between adapter-side `RunningCommand` lifecycle and the
/// domain `CommandRepository`.
pub trait CommandRegistrationSink: Send + Sync {
    /// Persist a fresh `CommandEntity` matching the live adapter row.
    /// Idempotent — the production sink swallows duplicate-id errors so
    /// a use-case-side insert that lands first is harmless.
    fn register(&self, entity: CommandEntity) -> SinkFuture<'_>;
    /// Remove the row when the adapter destroys its in-memory record.
    fn unregister<'a>(&'a self, command_id: &'a CommandId) -> SinkFuture<'a>;
}

/// Bridge between adapter-side `RunningShell` lifecycle and the domain
/// `ShellRepository`. See [`CommandRegistrationSink`].
pub trait ShellRegistrationSink: Send + Sync {
    /// Persist a fresh `ShellEntity` matching the live adapter row.
    fn register(&self, entity: ShellEntity) -> SinkFuture<'_>;
    /// Remove the row when the adapter destroys its in-memory record.
    fn unregister<'a>(&'a self, shell_id: &'a ShellId) -> SinkFuture<'a>;
}

/// Bridge between adapter-side `TransferShared` lifecycle and the domain
/// `TransferRepository`. See [`CommandRegistrationSink`].
pub trait TransferRegistrationSink: Send + Sync {
    /// Persist a fresh `TransferEntity` matching the live adapter row.
    fn register(&self, entity: TransferEntity) -> SinkFuture<'_>;
    /// Remove the row when the adapter destroys its in-memory record.
    fn unregister<'a>(&'a self, transfer_id: &'a TransferId) -> SinkFuture<'a>;
}

/// Bridge between adapter-side `ForwardHandle` lifecycle and the domain
/// `ForwardRepository`. Feature-gated alongside the forward repo port.
#[cfg(feature = "port_forward")]
pub trait ForwardRegistrationSink: Send + Sync {
    /// Persist a fresh `ForwardEntity` matching the live adapter row.
    fn register(&self, entity: ForwardEntity) -> SinkFuture<'_>;
    /// Remove the row when the adapter destroys its in-memory record.
    fn unregister<'a>(&'a self, forward_id: &'a ForwardId) -> SinkFuture<'a>;
}

// ---------------------------------------------------------------------------
// No-op implementations
// ---------------------------------------------------------------------------

/// No-op [`CommandStatusSink`].
///
/// Used when the adapter is built without a repository (tests,
/// fixtures, internal smoke). Keeps the runtime behaviour identical to
/// the v4.1 default so existing tests continue to compile without
/// wiring a repo handle.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopCommandStatusSink;

impl CommandStatusSink for NoopCommandStatusSink {
    fn mark_completed<'a>(
        &'a self,
        _command_id: &'a CommandId,
        _exit_code: i32,
        _timed_out: bool,
    ) -> SinkFuture<'a> {
        Box::pin(async {})
    }

    fn mark_failed<'a>(
        &'a self,
        _command_id: &'a CommandId,
        _error: Option<String>,
    ) -> SinkFuture<'a> {
        Box::pin(async {})
    }

    fn mark_cancelled<'a>(&'a self, _command_id: &'a CommandId) -> SinkFuture<'a> {
        Box::pin(async {})
    }
}

/// No-op [`ShellStatusSink`] mirroring the rationale of
/// [`NoopCommandStatusSink`].
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopShellStatusSink;

impl ShellStatusSink for NoopShellStatusSink {
    fn mark_closed<'a>(&'a self, _shell_id: &'a ShellId) -> SinkFuture<'a> {
        Box::pin(async {})
    }
}

/// No-op [`TransferStatusSink`] mirroring the rationale of
/// [`NoopCommandStatusSink`].
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopTransferStatusSink;

impl TransferStatusSink for NoopTransferStatusSink {
    fn mark_completed<'a>(
        &'a self,
        _transfer_id: &'a TransferId,
        _bytes_transferred: u64,
    ) -> SinkFuture<'a> {
        Box::pin(async {})
    }

    fn mark_failed<'a>(
        &'a self,
        _transfer_id: &'a TransferId,
        _error: Option<String>,
    ) -> SinkFuture<'a> {
        Box::pin(async {})
    }

    fn mark_cancelled<'a>(&'a self, _transfer_id: &'a TransferId) -> SinkFuture<'a> {
        Box::pin(async {})
    }

    fn record_progress<'a>(
        &'a self,
        _transfer_id: &'a TransferId,
        _bytes_transferred: u64,
    ) -> SinkFuture<'a> {
        Box::pin(async {})
    }
}

/// No-op [`CommandRegistrationSink`] used as the adapter default until
/// the composition root wires a real bridge.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopCommandRegistrationSink;

impl CommandRegistrationSink for NoopCommandRegistrationSink {
    fn register(&self, _entity: CommandEntity) -> SinkFuture<'_> {
        Box::pin(async {})
    }

    fn unregister<'a>(&'a self, _command_id: &'a CommandId) -> SinkFuture<'a> {
        Box::pin(async {})
    }
}

/// No-op [`ShellRegistrationSink`] mirroring [`NoopCommandRegistrationSink`].
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopShellRegistrationSink;

impl ShellRegistrationSink for NoopShellRegistrationSink {
    fn register(&self, _entity: ShellEntity) -> SinkFuture<'_> {
        Box::pin(async {})
    }

    fn unregister<'a>(&'a self, _shell_id: &'a ShellId) -> SinkFuture<'a> {
        Box::pin(async {})
    }
}

/// No-op [`TransferRegistrationSink`] mirroring [`NoopCommandRegistrationSink`].
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopTransferRegistrationSink;

impl TransferRegistrationSink for NoopTransferRegistrationSink {
    fn register(&self, _entity: TransferEntity) -> SinkFuture<'_> {
        Box::pin(async {})
    }

    fn unregister<'a>(&'a self, _transfer_id: &'a TransferId) -> SinkFuture<'a> {
        Box::pin(async {})
    }
}

/// No-op [`ForwardRegistrationSink`] mirroring [`NoopCommandRegistrationSink`].
#[cfg(feature = "port_forward")]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopForwardRegistrationSink;

#[cfg(feature = "port_forward")]
impl ForwardRegistrationSink for NoopForwardRegistrationSink {
    fn register(&self, _entity: ForwardEntity) -> SinkFuture<'_> {
        Box::pin(async {})
    }

    fn unregister<'a>(&'a self, _forward_id: &'a ForwardId) -> SinkFuture<'a> {
        Box::pin(async {})
    }
}

// ---------------------------------------------------------------------------
// Convenience aliases
// ---------------------------------------------------------------------------

/// Shared handle to the command sink. The composition root publishes the
/// same handle to both the SSH adapter and the use cases.
pub type SharedCommandStatusSink = Arc<dyn CommandStatusSink>;

/// Shared handle to the shell sink. See [`SharedCommandStatusSink`].
pub type SharedShellStatusSink = Arc<dyn ShellStatusSink>;

/// Shared handle to the transfer sink. See [`SharedCommandStatusSink`].
pub type SharedTransferStatusSink = Arc<dyn TransferStatusSink>;

/// Shared handle to the command registration sink (v4.3). See
/// [`SharedCommandStatusSink`] for the rationale.
pub type SharedCommandRegistrationSink = Arc<dyn CommandRegistrationSink>;

/// Shared handle to the shell registration sink (v4.3).
pub type SharedShellRegistrationSink = Arc<dyn ShellRegistrationSink>;

/// Shared handle to the transfer registration sink (v4.3).
pub type SharedTransferRegistrationSink = Arc<dyn TransferRegistrationSink>;

/// Shared handle to the forward registration sink (v4.3, feature-gated).
#[cfg(feature = "port_forward")]
pub type SharedForwardRegistrationSink = Arc<dyn ForwardRegistrationSink>;

#[cfg(test)]
mod tests {
    use super::{
        CommandRegistrationSink, CommandStatusSink, NoopCommandRegistrationSink,
        NoopCommandStatusSink, NoopShellRegistrationSink, NoopShellStatusSink,
        NoopTransferRegistrationSink, NoopTransferStatusSink, ShellRegistrationSink,
        ShellStatusSink, TransferRegistrationSink, TransferStatusSink,
    };
    use crate::domain::command::CommandEntity;
    use crate::domain::ids::{CommandId, SessionId, ShellId, TransferId};
    use crate::domain::shell::{ShellEntity, ShellTerminal};
    use crate::domain::transfer::{TransferDirection, TransferEntity};
    use chrono::Utc;
    use std::time::Duration;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn noop_command_sink_is_send_sync_clone_default() {
        assert_send_sync::<NoopCommandStatusSink>();
    }

    #[test]
    fn noop_shell_sink_is_send_sync_clone_default() {
        assert_send_sync::<NoopShellStatusSink>();
    }

    #[test]
    fn noop_transfer_sink_is_send_sync_clone_default() {
        assert_send_sync::<NoopTransferStatusSink>();
    }

    #[tokio::test]
    async fn noop_command_sink_mark_completed_returns_immediately() {
        let sink = NoopCommandStatusSink;
        let id = CommandId::new("c1".to_string());
        sink.mark_completed(&id, 0, false).await;
        sink.mark_failed(&id, Some("boom".to_string())).await;
        sink.mark_cancelled(&id).await;
    }

    #[tokio::test]
    async fn noop_shell_sink_mark_closed_returns_immediately() {
        let sink = NoopShellStatusSink;
        let id = ShellId::new("sh1".to_string());
        sink.mark_closed(&id).await;
    }

    #[tokio::test]
    async fn noop_transfer_sink_methods_return_immediately() {
        let sink = NoopTransferStatusSink;
        let id = TransferId::new("t1".to_string());
        sink.mark_completed(&id, 1024).await;
        sink.mark_failed(&id, Some("disk full".to_string())).await;
        sink.mark_cancelled(&id).await;
        sink.record_progress(&id, 512).await;
    }

    #[test]
    fn shared_handles_are_dyn_safe_pointer_types() {
        // Compile-time guard that the type aliases really are dyn-safe.
        let _cmd: super::SharedCommandStatusSink = std::sync::Arc::new(NoopCommandStatusSink);
        let _shell: super::SharedShellStatusSink = std::sync::Arc::new(NoopShellStatusSink);
        let _xfer: super::SharedTransferStatusSink = std::sync::Arc::new(NoopTransferStatusSink);
        let _cmd_reg: super::SharedCommandRegistrationSink =
            std::sync::Arc::new(NoopCommandRegistrationSink);
        let _shell_reg: super::SharedShellRegistrationSink =
            std::sync::Arc::new(NoopShellRegistrationSink);
        let _xfer_reg: super::SharedTransferRegistrationSink =
            std::sync::Arc::new(NoopTransferRegistrationSink);
    }

    #[test]
    fn noop_registration_sinks_are_send_sync() {
        assert_send_sync::<NoopCommandRegistrationSink>();
        assert_send_sync::<NoopShellRegistrationSink>();
        assert_send_sync::<NoopTransferRegistrationSink>();
    }

    fn sample_command_entity() -> CommandEntity {
        CommandEntity::new(
            CommandId::new("c-1".to_string()),
            SessionId::new("s-1".to_string()),
            "echo hi".to_string(),
            Utc::now(),
        )
    }

    fn sample_shell_entity() -> ShellEntity {
        ShellEntity::new(
            ShellId::new("sh-1".to_string()),
            SessionId::new("s-1".to_string()),
            ShellTerminal::new("xterm".to_string(), 80, 24),
            Utc::now(),
            Duration::from_secs(60),
            1024,
        )
    }

    fn sample_transfer_entity() -> TransferEntity {
        TransferEntity::new(
            TransferId::new("t-1".to_string()),
            SessionId::new("s-1".to_string()),
            TransferDirection::Upload,
            "/tmp/local".to_string(),
            "/srv/remote".to_string(),
            Utc::now(),
            1024,
        )
    }

    #[tokio::test]
    async fn noop_command_registration_sink_returns_immediately() {
        let sink = NoopCommandRegistrationSink;
        let id = CommandId::new("c-1".to_string());
        sink.register(sample_command_entity()).await;
        sink.unregister(&id).await;
    }

    #[tokio::test]
    async fn noop_shell_registration_sink_returns_immediately() {
        let sink = NoopShellRegistrationSink;
        let id = ShellId::new("sh-1".to_string());
        sink.register(sample_shell_entity()).await;
        sink.unregister(&id).await;
    }

    #[tokio::test]
    async fn noop_transfer_registration_sink_returns_immediately() {
        let sink = NoopTransferRegistrationSink;
        let id = TransferId::new("t-1".to_string());
        sink.register(sample_transfer_entity()).await;
        sink.unregister(&id).await;
    }

    #[cfg(feature = "port_forward")]
    #[tokio::test]
    async fn noop_forward_registration_sink_returns_immediately() {
        use super::{ForwardRegistrationSink, NoopForwardRegistrationSink};
        use crate::domain::forward::ForwardEntity;
        use crate::domain::ids::ForwardId;
        let sink = NoopForwardRegistrationSink;
        let entity = ForwardEntity::new(
            ForwardId::new("fwd-1".to_string()),
            SessionId::new("s-1".to_string()),
            8080,
            "internal".to_string(),
            80,
            Utc::now(),
        );
        let id = entity.id.clone();
        sink.register(entity).await;
        sink.unregister(&id).await;
    }
}
