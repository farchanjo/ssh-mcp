//! SSH client port.
//!
//! Abstracts the russh-backed connection so use cases can drive
//! command execution, PTY shells, and disconnect without depending on
//! `russh::client::Handle` directly. Each method returns a domain-typed
//! result that the v3 adapter (etapa H6) translates from the underlying
//! transport errors.

use std::time::Duration;

use bytes::Bytes;

use crate::domain::command::{CommandEntity, CommandRequest};
use crate::domain::error::DomainError;
use crate::domain::identity::{Address, Credentials};
use crate::domain::ids::{CommandId, SessionId, ShellId};
use crate::domain::session::SessionEntity;
use crate::domain::shell::{ShellEntity, ShellTerminal};

/// One-shot result of a synchronous SSH command (no streaming).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// Captured exit code (`None` when the command did not yield a status).
    pub exit_code: Option<i32>,
    /// Concatenated stdout bytes.
    pub stdout: Bytes,
    /// Concatenated stderr bytes.
    pub stderr: Bytes,
    /// Whether the command exited because the configured timeout fired.
    pub timed_out: bool,
}

/// Streaming handle returned by `execute_async`.
///
/// Use cases consume both the snapshot entity and a stream of
/// [`OutputChunk`] frames; the concrete stream type is opaque (the use
/// case adapter holds it) so the port stays `Send`-bounded without
/// leaking tokio.
#[derive(Debug)]
pub struct CommandHandle {
    /// Initial entity snapshot (status starts as `Running`).
    pub entity: CommandEntity,
}

/// SSH client port. Implementations are async (network I/O + handshake).
#[trait_variant::make(SshClientPort: Send)]
pub trait LocalSshClientPort: Sync {
    /// Open a new SSH session against `address` using `credentials`.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::ConnectFailed` after retry budget exhaustion,
    /// `DomainError::Auth` for credential failures, or
    /// `DomainError::Transport` for protocol-level errors.
    async fn connect(
        &self,
        session_id: SessionId,
        address: Address,
        credentials: Credentials,
        connect_timeout: Duration,
    ) -> Result<SessionEntity, DomainError>;

    /// Tear down a session previously returned by `connect`.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Transport` if the underlying transport rejects
    /// the close handshake.
    async fn disconnect(&self, session_id: &SessionId) -> Result<(), DomainError>;

    /// Run a one-shot command and return its terminal outcome.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Timeout` when the configured timeout fires
    /// before the command completes, or `DomainError::Transport` for
    /// channel-level errors.
    async fn execute(&self, request: CommandRequest) -> Result<CommandOutcome, DomainError>;

    /// Spawn an async command and return a streaming handle.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::MaxCommandsExceeded` when the per-session cap
    /// is hit, or `DomainError::Transport` for channel allocation failures.
    async fn execute_async(
        &self,
        command_id: CommandId,
        request: CommandRequest,
    ) -> Result<CommandHandle, DomainError>;

    /// Cancel a running async command.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::CommandNotFound` if the command id does not
    /// match a live channel, or `DomainError::Transport` for transport faults.
    async fn cancel(&self, command_id: &CommandId) -> Result<(), DomainError>;

    /// Open a fresh interactive PTY shell.
    ///
    /// `max_buffer_size` overrides the per-shell rolling-buffer cap when
    /// supplied (in bytes); `None` falls back to the adapter's default
    /// (10 MiB for the production russh adapter). The cap is wired
    /// directly into the runtime [`std::sync::atomic::AtomicU64`] backing
    /// the reader task's flush threshold so caller overrides actually
    /// bound the buffer at runtime — not just on the persisted entity.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::MaxShellsExceeded`, `DomainError::Transport`,
    /// or `DomainError::Timeout` depending on the failure point.
    async fn open_shell(
        &self,
        session_id: &SessionId,
        terminal: ShellTerminal,
        max_buffer_size: Option<u64>,
    ) -> Result<ShellEntity, DomainError>;

    /// Write raw bytes to an interactive shell (text input or escape sequences).
    ///
    /// # Errors
    ///
    /// Returns `DomainError::ShellNotFound` if the shell is unknown to the
    /// adapter, or `DomainError::WriteFailed` if the channel rejects the data.
    async fn write_shell(&self, shell_id: &ShellId, bytes: Bytes) -> Result<usize, DomainError>;

    /// Close an interactive shell (cancel reader task + drop channel).
    ///
    /// Idempotent — calling on an already-closed shell returns
    /// `DomainError::ShellNotFound`.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::ShellNotFound` when the shell id is unknown.
    async fn close_shell(&self, shell_id: &ShellId) -> Result<(), DomainError>;

    /// Probe the connection liveness with a minimal command (e.g. `echo 1`).
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Transport` when the keepalive channel fails to
    /// open or the probe command does not return a successful exit code.
    async fn health_check(&self, session_id: &SessionId) -> Result<(), DomainError>;

    /// Spawn the local listener + per-connection direct-tcpip channels for
    /// `local_port → remote_address:remote_port` over the SSH session.
    /// The implementation owns the listener and pumps bytes both ways
    /// for every accepted connection. Returns once the listener is
    /// bound and accepting; per-connection lifetimes are independent.
    ///
    /// `forward_id` is the already-minted [`crate::domain::ids::ForwardId`]
    /// (as a plain `String` to keep this port free of the domain-id
    /// newtype) so the adapter can drive `forward://<id>/events` push
    /// notifications for accept / channel-open / close lifecycle events.
    ///
    /// # Errors
    ///
    /// - [`DomainError::SessionNotFound`] when the session is unknown.
    /// - [`DomainError::PortInUse`] when the local bind fails on
    ///   `EADDRINUSE`.
    /// - [`DomainError::Transport`] for any other I/O / russh failure.
    async fn open_forward(
        &self,
        session_id: &SessionId,
        local_port: u16,
        remote_address: String,
        remote_port: u16,
        forward_id: String,
    ) -> Result<ForwardHandle, DomainError>;

    /// Stop the listener + cancel every in-flight forwarded connection
    /// for `local_port`. Idempotent — already-closed forwards return
    /// `Ok(())`.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Transport`] when the underlying cancel
    /// signal cannot be delivered.
    async fn close_forward(&self, local_port: u16) -> Result<(), DomainError>;
}

/// Live handle returned by [`SshClientPort::open_forward`].
///
/// The adapter owns the listener + per-connection tasks; the handle
/// exists so the use case can persist `bound_addr` for diagnostics
/// without depending on the concrete adapter type.
#[derive(Debug, Clone)]
pub struct ForwardHandle {
    /// Address the listener actually bound to (e.g. `0.0.0.0:8080`).
    pub bound_addr: String,
}

#[cfg(test)]
mod tests {
    use super::SshClientPort;

    fn _assert_port<T: SshClientPort>() {}
}
