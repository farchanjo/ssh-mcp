//! Authentication outcome and error variants surfaced by the
//! [`crate::ports::auth_strategy::AuthStrategyPort`] chain.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Result of a single authentication attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthOutcome {
    /// Server accepted the credentials.
    Authenticated,
    /// Server rejected the credentials (continue down the strategy chain).
    Rejected,
}

impl fmt::Display for AuthOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authenticated => f.write_str("authenticated"),
            Self::Rejected => f.write_str("rejected"),
        }
    }
}

/// Domain-level authentication failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// Provided credentials were rejected by the server.
    #[error("authentication rejected by server")]
    Rejected,
    /// No strategy in the chain succeeded for the supplied credentials.
    #[error("no authentication strategy succeeded")]
    Exhausted,
    /// Required credential material was missing or malformed.
    #[error("invalid credential payload: {0}")]
    InvalidCredential(String),
    /// SSH agent socket could not be reached.
    #[error("agent unavailable: {0}")]
    AgentUnavailable(String),
    /// Underlying transport / handshake error reported by the adapter.
    #[error("authentication transport error: {0}")]
    Transport(String),
}

#[cfg(test)]
mod tests {
    use super::{AuthError, AuthOutcome};

    #[test]
    fn auth_outcome_display() {
        assert_eq!(AuthOutcome::Authenticated.to_string(), "authenticated");
        assert_eq!(AuthOutcome::Rejected.to_string(), "rejected");
    }

    #[test]
    fn auth_error_messages_are_descriptive() {
        assert_eq!(
            AuthError::Rejected.to_string(),
            "authentication rejected by server"
        );
        assert_eq!(
            AuthError::Exhausted.to_string(),
            "no authentication strategy succeeded"
        );
        assert_eq!(
            AuthError::InvalidCredential("missing pem".to_string()).to_string(),
            "invalid credential payload: missing pem"
        );
        assert_eq!(
            AuthError::AgentUnavailable("ENOENT".to_string()).to_string(),
            "agent unavailable: ENOENT"
        );
        assert_eq!(
            AuthError::Transport("eof".to_string()).to_string(),
            "authentication transport error: eof"
        );
    }
}
