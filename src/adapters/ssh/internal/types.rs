//! Internal data types used by the SSH adapter (v3 leftover, relocated in
//! H17.6 P1 from `crate::mcp::types` to `crate::adapters::ssh::internal::types`).
//!
//! Response types that used to be returned directly by MCP tools were
//! removed in v2.0. The types that remain here are pure internal data
//! carriers used by the SSH adapter's runtime state (commands + shells),
//! plus a one-shot `SshCommandResponse` used by the health-check path that
//! executes `echo 1` on idle sessions.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

/// Information about a single async command. Stored in the v3 command
/// storage and rendered into `ssh_list_commands` markdown.
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

#[cfg(test)]
mod tests {
    use super::{AsyncCommandInfo, AsyncCommandStatus, ShellInfo, ShellStatus};

    mod async_command_status {
        use super::AsyncCommandStatus;

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
        use super::ShellStatus;

        #[test]
        fn display_trait() {
            assert_eq!(format!("{}", ShellStatus::Open), "open");
            assert_eq!(format!("{}", ShellStatus::Closed), "closed");
        }
    }

    mod async_command_info {
        use super::{AsyncCommandInfo, AsyncCommandStatus};

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
        use super::ShellInfo;

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
