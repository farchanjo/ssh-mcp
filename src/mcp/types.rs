//! Internal data types used by the MCP SSH module.
//!
//! Response types that used to be returned directly by MCP tools have been
//! removed in v2.0 — all handlers now return plain markdown `String`s built
//! by `super::message::builder`. The types that remain here are pure
//! internal data carriers used by storage and by the list-rendering paths.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Session metadata for tracking connection information.
///
/// Stored in `SessionStorage` and rendered into `ssh_list_sessions`
/// markdown via `message::builder::ListSessionsBuilder`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionInfo {
    pub session_id: String,
    /// Optional human-readable name for the session (useful for LLM identification)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional agent identifier for grouping sessions by agent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub host: String,
    pub username: String,
    pub connected_at: String,
    /// Default timeout in seconds used for this session's connection
    pub default_timeout_secs: u64,
    /// Number of retry attempts needed to establish the connection
    pub retry_attempts: u32,
    /// Whether compression is enabled for this session
    pub compression_enabled: bool,
    /// Timestamp of last health check (RFC3339 format)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_health_check: Option<String>,
    /// Whether session passed last health check
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthy: Option<bool>,
}

/// Internal representation of a command's one-shot result (used by the
/// health-check path that executes `echo 1` on idle sessions).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub(crate) struct SshCommandResponse {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    /// Whether the command timed out (partial output may be available)
    #[serde(default)]
    pub timed_out: bool,
}

/// Status of an async command execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AsyncCommandStatus {
    /// Command is currently running
    Running,
    /// Command has completed (check `exit_code`)
    Completed,
    /// Command was cancelled by user
    Cancelled,
    /// Command failed to start (check error field)
    Failed,
}

impl fmt::Display for AsyncCommandStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// Information about a single async command. Stored in `CommandStorage` and
/// rendered into `ssh_list_commands` markdown.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AsyncCommandInfo {
    /// Unique identifier for this command
    pub command_id: String,
    /// Session ID where the command is running
    pub session_id: String,
    /// The command being executed
    pub command: String,
    /// Current status of the command
    pub status: AsyncCommandStatus,
    /// When the command was started (RFC3339 format)
    pub started_at: String,
}

/// Status of an interactive shell session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShellStatus {
    /// Shell is open and accepting input
    Open,
    /// Shell has been closed
    Closed,
}

impl fmt::Display for ShellStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open => write!(f, "open"),
            Self::Closed => write!(f, "closed"),
        }
    }
}

/// Metadata for an interactive shell session.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ShellInfo {
    /// Unique identifier for this shell
    pub shell_id: String,
    /// Session ID where the shell is running
    pub session_id: String,
    /// Terminal type (e.g., "xterm", "vt100")
    pub term_type: String,
    /// Terminal width in columns
    pub cols: u32,
    /// Terminal height in rows
    pub rows: u32,
    /// When the shell was opened (RFC3339 format)
    pub opened_at: String,
}

/// Live transition event broadcast by `RunningTransfer`. Subscribers consume
/// these via `progress_tx` to drive the future `transfer://<id>/progress`
/// MCP resource (E13).
///
/// Each variant carries a `seq` allocated by
/// [`crate::mcp::subscription::SubscriptionRegistry::next_seq`] so subscribers
/// recovering from `Lagged` can detect gaps.
#[derive(Debug, Clone, Copy)]
pub enum ProgressEvent {
    /// Transfer made progress — `bytes_transferred` was just updated.
    Tick {
        seq: u64,
        bytes_transferred: u64,
        total_bytes: u64,
    },
    /// Transfer terminated successfully.
    Completed { seq: u64, bytes_transferred: u64 },
    /// Transfer failed (the failure reason is on `RunningTransfer.error`).
    Failed { seq: u64 },
    /// Transfer was cancelled by caller.
    Cancelled { seq: u64 },
}

/// Live health event broadcast by a `SessionRef`. Subscribers consume these
/// via `health_tx` to drive the future `session://<id>/health` MCP resource.
#[derive(Debug, Clone, Copy)]
pub enum HealthEvent {
    /// Health check passed at this timestamp (epoch milliseconds).
    Healthy { seq: u64, at_ms: u64 },
    /// Health check failed at this timestamp (epoch milliseconds).
    Unhealthy { seq: u64, at_ms: u64 },
    /// Session was disconnected.
    Disconnected { seq: u64 },
}

/// Live event broadcast by an active port-forwarder.
///
/// Subscribers consume these via `events_tx` to drive the future
/// `forward://<id>/events` MCP resource (E13). Per-connection eventing
/// is feature-gated and may be stubbed until the wiring lands in E13.
#[cfg(feature = "port_forward")]
#[derive(Debug, Clone)]
pub enum ForwardEvent {
    /// Local connection accepted from the given remote address.
    Accept {
        seq: u64,
        client_addr: String,
        at_ms: u64,
    },
    /// Forwarded connection closed; aggregated byte counts are reported.
    Close {
        seq: u64,
        client_addr: String,
        bytes_in: u64,
        bytes_out: u64,
        at_ms: u64,
    },
    /// Forwarder task shut down — no further events will be emitted.
    Stopped { seq: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    mod async_command_status {
        use super::*;

        #[test]
        fn display_trait() {
            assert_eq!(format!("{}", AsyncCommandStatus::Running), "running");
            assert_eq!(format!("{}", AsyncCommandStatus::Completed), "completed");
            assert_eq!(format!("{}", AsyncCommandStatus::Cancelled), "cancelled");
            assert_eq!(format!("{}", AsyncCommandStatus::Failed), "failed");
        }

        #[test]
        fn copy_and_equality() {
            let a = AsyncCommandStatus::Running;
            let b = a;
            assert_eq!(a, b);
            assert_ne!(AsyncCommandStatus::Running, AsyncCommandStatus::Completed);
        }
    }

    mod shell_status {
        use super::*;

        #[test]
        fn display_trait() {
            assert_eq!(format!("{}", ShellStatus::Open), "open");
            assert_eq!(format!("{}", ShellStatus::Closed), "closed");
        }
    }

    mod session_info {
        use super::*;

        #[test]
        fn clone_preserves_fields() {
            let info = SessionInfo {
                session_id: "s".to_string(),
                name: Some("n".to_string()),
                agent_id: Some("a".to_string()),
                host: "h".to_string(),
                username: "u".to_string(),
                connected_at: "t".to_string(),
                default_timeout_secs: 30,
                retry_attempts: 0,
                compression_enabled: true,
                last_health_check: None,
                healthy: None,
            };
            let cloned = info.clone();
            assert_eq!(cloned.session_id, "s");
            assert_eq!(cloned.agent_id, Some("a".to_string()));
        }
    }

    mod async_command_info {
        use super::*;

        #[test]
        fn clone_preserves_fields() {
            let info = AsyncCommandInfo {
                command_id: "c".to_string(),
                session_id: "s".to_string(),
                command: "ls".to_string(),
                status: AsyncCommandStatus::Running,
                started_at: "t".to_string(),
            };
            let cloned = info.clone();
            assert_eq!(cloned.command_id, "c");
            assert_eq!(cloned.status, AsyncCommandStatus::Running);
        }
    }

    mod shell_info {
        use super::*;

        #[test]
        fn clone_preserves_fields() {
            let info = ShellInfo {
                shell_id: "sh".to_string(),
                session_id: "s".to_string(),
                term_type: "xterm".to_string(),
                cols: 80,
                rows: 24,
                opened_at: "t".to_string(),
            };
            let cloned = info.clone();
            assert_eq!(cloned.shell_id, "sh");
            assert_eq!(cloned.term_type, "xterm");
        }
    }
}
