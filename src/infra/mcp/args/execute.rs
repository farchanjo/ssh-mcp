//! Async-command argument types.
//!
//! Mirrors v3 `src/mcp/tools/execute.rs::Ssh*Args` exactly.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::domain::policy::CommandStatusFilter;

/// Async command status filter used by `ssh_list_commands`.
///
/// Replaces the v2.0.1 `Option<String>` filter (which silently ignored
/// invalid values) with a tagged enum that errors at deserialization
/// time for typos such as `"runing"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// The command is currently executing.
    Running,
    /// The command terminated successfully.
    Completed,
    /// The command was cancelled by the caller.
    Cancelled,
    /// The command failed (transport error, channel error, ...).
    Failed,
}

impl CommandStatus {
    /// Translate the wire enum into the domain
    /// [`CommandStatusFilter`].
    #[must_use]
    pub const fn into_domain(self) -> CommandStatusFilter {
        match self {
            Self::Running => CommandStatusFilter::Running,
            Self::Completed => CommandStatusFilter::Completed,
            Self::Cancelled => CommandStatusFilter::Cancelled,
            Self::Failed => CommandStatusFilter::Failed,
        }
    }
}

/// Arguments for the `ssh_execute` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshExecuteArgs {
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,

    /// Shell command to execute on the remote host.
    pub command: String,

    /// Command timeout in seconds. Default: 180. Env:
    /// `SSH_COMMAND_TIMEOUT`.
    pub timeout_secs: Option<u64>,

    /// Allocate a pseudo-terminal (PTY) for the command. Default: false.
    /// Use for commands that require a controlling terminal (e.g. `sudo`,
    /// `top`). All output is merged into stdout in PTY mode (no stderr
    /// separation).
    pub pty: Option<bool>,
}

/// Arguments for the `ssh_get_command_output` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshGetCommandOutputArgs {
    /// `COMMAND_ID` returned from `ssh_execute`.
    pub command_id: String,

    /// Block until completion or `wait_timeout_secs` expires. Default:
    /// false.
    pub wait: Option<bool>,

    /// Maximum seconds to block when `wait=true`. Default: 30. Cap: 300.
    pub wait_timeout_secs: Option<u64>,

    /// Maximum bytes shown in stdout/stderr. Default: 16384. Cap:
    /// 1048576. Content head-truncated; tail (most recent output)
    /// preserved. Env: `SSH_MCP_OUTPUT_DEFAULT_BYTES` /
    /// `SSH_MCP_OUTPUT_MAX_BYTES_CAP`.
    pub max_output_bytes: Option<usize>,
}

/// Arguments for the `ssh_list_commands` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshListCommandsArgs {
    /// `SESSION_ID` returned from `ssh_connect`. Optional filter; when
    /// omitted returns commands across all sessions.
    pub session_id: Option<String>,

    /// Filter by command status. Values: `running`, `completed`,
    /// `cancelled`, `failed`. When omitted returns every status.
    pub status: Option<CommandStatus>,

    /// Maximum entries returned. Default: 500. Cap: 10000. Env:
    /// `SSH_MCP_LIST_MAX_ITEMS` / `SSH_MCP_LIST_MAX_ITEMS_CAP`.
    pub max_items: Option<usize>,
}

/// Arguments for the `ssh_cancel_command` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshCancelCommandArgs {
    /// `COMMAND_ID` returned from `ssh_execute`.
    pub command_id: String,

    /// Maximum bytes shown in stdout/stderr. Default: 16384. Cap:
    /// 1048576. Content head-truncated; tail (most recent output)
    /// preserved. Env: `SSH_MCP_OUTPUT_DEFAULT_BYTES` /
    /// `SSH_MCP_OUTPUT_MAX_BYTES_CAP`.
    pub max_output_bytes: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::CommandStatus;
    use crate::domain::policy::CommandStatusFilter;

    #[test]
    fn command_status_serde_round_trip() {
        let raw = serde_json::json!("cancelled");
        let parsed: CommandStatus = serde_json::from_value(raw).expect("parse");
        assert_eq!(parsed, CommandStatus::Cancelled);
    }

    #[test]
    fn command_status_into_domain_maps_every_variant() {
        assert_eq!(
            CommandStatus::Running.into_domain(),
            CommandStatusFilter::Running
        );
        assert_eq!(
            CommandStatus::Completed.into_domain(),
            CommandStatusFilter::Completed
        );
        assert_eq!(
            CommandStatus::Cancelled.into_domain(),
            CommandStatusFilter::Cancelled
        );
        assert_eq!(
            CommandStatus::Failed.into_domain(),
            CommandStatusFilter::Failed
        );
    }
}
