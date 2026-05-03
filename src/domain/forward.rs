//! Port-forward aggregate value objects.
//!
//! The forwarding feature is gated by the `port_forward` Cargo feature in
//! adapters/use cases, but the domain entity itself is always compiled —
//! keeping it always-on means downstream test code can refer to the type
//! without juggling cfg flags.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ids::{ForwardId, SessionId};

/// Lifecycle states of a port forwarder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardStatus {
    /// Forwarder is accepting connections.
    Running,
    /// Forwarder was stopped (either by the operator or by session shutdown).
    Stopped,
    /// Forwarder failed to start or terminated due to an error.
    Failed,
}

impl fmt::Display for ForwardStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => f.write_str("running"),
            Self::Stopped => f.write_str("stopped"),
            Self::Failed => f.write_str("failed"),
        }
    }
}

/// Snapshot of a single port-forwarder aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardEntity {
    /// Stable identifier.
    pub id: ForwardId,
    /// Owning session.
    pub session_id: SessionId,
    /// Local port the listener is bound to.
    pub local_port: u16,
    /// Remote address (host portion) the connections are forwarded to.
    pub remote_host: String,
    /// Remote port.
    pub remote_port: u16,
    /// Wall-clock timestamp the forwarder was started.
    pub started_at: DateTime<Utc>,
    /// Lifecycle state.
    pub status: ForwardStatus,
}

impl ForwardEntity {
    /// Build a fresh forwarder snapshot in [`ForwardStatus::Running`].
    #[must_use]
    pub const fn new(
        id: ForwardId,
        session_id: SessionId,
        local_port: u16,
        remote_host: String,
        remote_port: u16,
        started_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            session_id,
            local_port,
            remote_host,
            remote_port,
            started_at,
            status: ForwardStatus::Running,
        }
    }

    /// Flip status to [`ForwardStatus::Stopped`].
    #[must_use]
    pub const fn stop(mut self) -> Self {
        self.status = ForwardStatus::Stopped;
        self
    }

    /// Flip status to [`ForwardStatus::Failed`].
    #[must_use]
    pub const fn fail(mut self) -> Self {
        self.status = ForwardStatus::Failed;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{ForwardEntity, ForwardId, ForwardStatus, SessionId};
    use chrono::Utc;

    #[test]
    fn forward_status_display() {
        assert_eq!(ForwardStatus::Running.to_string(), "running");
        assert_eq!(ForwardStatus::Stopped.to_string(), "stopped");
        assert_eq!(ForwardStatus::Failed.to_string(), "failed");
    }

    #[test]
    fn lifecycle_transitions() {
        let f = ForwardEntity::new(
            ForwardId::new("f".to_string()),
            SessionId::new("s".to_string()),
            8080,
            "internal".to_string(),
            80,
            Utc::now(),
        );
        assert_eq!(f.status, ForwardStatus::Running);
        assert_eq!(f.clone().stop().status, ForwardStatus::Stopped);
        assert_eq!(f.fail().status, ForwardStatus::Failed);
    }
}
