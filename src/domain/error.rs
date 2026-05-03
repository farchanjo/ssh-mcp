//! Domain-level error type returned by use cases.
//!
//! Variants are designed to be exhaustively matched at the adapter layer so
//! HTTP / stdio responders can render a stable error code per case without
//! string parsing.

use thiserror::Error;

use super::auth::AuthError;
use super::ids::{CommandId, ForwardId, SessionId, ShellId, TransferId};

/// Top-level error variant produced by use cases.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DomainError {
    /// A session referenced by id was not found.
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),

    /// A command referenced by id was not found.
    #[error("command not found: {0}")]
    CommandNotFound(CommandId),

    /// A shell referenced by id was not found.
    #[error("shell not found: {0}")]
    ShellNotFound(ShellId),

    /// A transfer referenced by id was not found.
    #[error("transfer not found: {0}")]
    TransferNotFound(TransferId),

    /// A forwarder referenced by id was not found.
    #[error("forward not found: {0}")]
    ForwardNotFound(ForwardId),

    /// Per-session command capacity was exhausted.
    #[error("max commands per session exceeded (limit {limit})")]
    MaxCommandsExceeded {
        /// Configured per-session command cap.
        limit: usize,
    },

    /// Per-session shell capacity was exhausted.
    #[error("max shells per session exceeded (limit {limit})")]
    MaxShellsExceeded {
        /// Configured per-session shell cap.
        limit: usize,
    },

    /// Per-session transfer capacity was exhausted.
    #[error("max transfers per session exceeded (limit {limit})")]
    MaxTransfersExceeded {
        /// Configured per-session transfer cap.
        limit: usize,
    },

    /// Caller-supplied input failed validation before reaching the adapter.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// SSH connection establishment failed (retry budget exhausted).
    #[error("connect failed: {0}")]
    ConnectFailed(String),

    /// Authentication chain failed.
    #[error(transparent)]
    Auth(#[from] AuthError),

    /// An operation timed out before completing.
    #[error("operation timed out: {0}")]
    Timeout(String),

    /// SFTP-level error surfaced by the adapter.
    #[error("sftp error: {0}")]
    Sftp(String),

    /// A configured local port was already in use.
    #[error("port already in use: {0}")]
    PortInUse(u16),

    /// SSH transport error not covered by a more specific variant.
    #[error("ssh transport error: {0}")]
    Transport(String),

    /// Storage / repository fault (e.g. lock poisoning, IO failure).
    #[error("storage error: {0}")]
    Storage(String),

    /// Internal invariant violated.
    #[error("internal error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::{AuthError, DomainError, SessionId};

    #[test]
    fn auth_error_converts_via_from() {
        let auth_err = AuthError::Rejected;
        let domain: DomainError = auth_err.into();
        assert_eq!(domain, DomainError::Auth(AuthError::Rejected));
    }

    #[test]
    fn limit_variants_carry_value() {
        let err = DomainError::MaxCommandsExceeded { limit: 100 };
        assert_eq!(
            err.to_string(),
            "max commands per session exceeded (limit 100)"
        );
    }

    #[test]
    fn not_found_messages_include_id() {
        let err = DomainError::SessionNotFound(SessionId::new("sess-1".to_string()));
        assert_eq!(err.to_string(), "session not found: sess-1");
    }

    #[test]
    fn port_in_use_includes_port() {
        assert_eq!(
            DomainError::PortInUse(8080).to_string(),
            "port already in use: 8080"
        );
    }
}
