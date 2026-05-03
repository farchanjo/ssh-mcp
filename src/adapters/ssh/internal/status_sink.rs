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

use crate::domain::ids::{CommandId, ShellId, TransferId};

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

#[cfg(test)]
mod tests {
    use super::{
        CommandStatusSink, NoopCommandStatusSink, NoopShellStatusSink, NoopTransferStatusSink,
        ShellStatusSink, TransferStatusSink,
    };
    use crate::domain::ids::{CommandId, ShellId, TransferId};

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
    }
}
