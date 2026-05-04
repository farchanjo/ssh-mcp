//! Async-command argument types.
//!
//! Mirrors v3 `src/mcp/tools/execute.rs::Ssh*Args` exactly.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::domain::policy::CommandStatusFilter;

/// Async command status filter used by `ssh_commands`.
///
/// Replaces the v2.0.1 `Option<String>` filter (which silently ignored
/// invalid values) with a tagged enum that errors at deserialization
/// time for typos such as `"runing"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
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

// Schemars 1.2 default-fn helpers — see `connection.rs` for the
// rationale on the `() -> Option<T>` signature requirement.
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_command_timeout_secs() -> Option<u64> {
    Some(180)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_pty() -> Option<bool> {
    Some(false)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_wait() -> Option<bool> {
    Some(false)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_wait_timeout_secs() -> Option<u64> {
    Some(30)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_max_output_bytes() -> Option<usize> {
    Some(16384)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_max_items() -> Option<usize> {
    Some(500)
}

// ssh_run / ssh_exec_batch defaults --------------------------------
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_run_timeout_secs() -> Option<u64> {
    Some(30)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_release_when_no_subs() -> Option<bool> {
    Some(false)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_lifecycle_grace_ms() -> Option<u32> {
    Some(2_000)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_run_max_output_bytes() -> Option<usize> {
    Some(16384)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_disconnect_after_run() -> Option<bool> {
    Some(true)
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_stop_on_failure() -> Option<bool> {
    Some(true)
}

/// Arguments for the `ssh_exec` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshExecArgs {
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,

    /// Shell command to execute on the remote host.
    pub command: String,

    /// Command timeout in seconds. Default: 180. Env:
    /// `SSH_COMMAND_TIMEOUT`.
    #[schemars(default = "default_command_timeout_secs")]
    pub timeout_secs: Option<u64>,

    /// Allocate a pseudo-terminal (PTY) for the command. Default: false.
    /// Use for commands that require a controlling terminal (e.g. `sudo`,
    /// `top`). All output is merged into stdout in PTY mode (no stderr
    /// separation).
    #[schemars(default = "default_pty")]
    pub pty: Option<bool>,

    /// v5 Phase 3 — auto-release when the command resource has zero
    /// subscribers. Default: false (legacy v4 behaviour).
    #[schemars(default = "default_release_when_no_subs")]
    pub release_when_no_subs: Option<bool>,

    /// v5 Phase 3 — grace window in ms before auto-release fires.
    /// Default: 2000.
    #[schemars(default = "default_lifecycle_grace_ms")]
    pub grace_ms: Option<u32>,
}

/// Arguments for the `ssh_exec_output` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshExecOutputArgs {
    /// `COMMAND_ID` returned from `ssh_exec`.
    pub command_id: String,

    /// Block until completion or `wait_timeout_secs` expires. Default:
    /// false.
    #[schemars(default = "default_wait")]
    pub wait: Option<bool>,

    /// Maximum seconds to block when `wait=true`. Default: 30. Cap: 300.
    #[schemars(default = "default_wait_timeout_secs")]
    pub wait_timeout_secs: Option<u64>,

    /// Maximum bytes shown in stdout/stderr. Default: 16384. Cap:
    /// 1048576. Content head-truncated; tail (most recent output)
    /// preserved. Env: `SSH_MCP_OUTPUT_DEFAULT_BYTES` /
    /// `SSH_MCP_OUTPUT_MAX_BYTES_CAP`.
    #[schemars(default = "default_max_output_bytes")]
    pub max_output_bytes: Option<usize>,
}

/// Arguments for the `ssh_commands` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshCommandsArgs {
    /// `SESSION_ID` returned from `ssh_connect`. Optional filter; when
    /// omitted returns commands across all sessions.
    pub session_id: Option<String>,

    /// Filter by command status. Values: `running`, `completed`,
    /// `cancelled`, `failed`. When omitted returns every status.
    pub status: Option<CommandStatus>,

    /// Maximum entries returned. Default: 500. Cap: 10000. Env:
    /// `SSH_MCP_LIST_MAX_ITEMS` / `SSH_MCP_LIST_MAX_ITEMS_CAP`.
    #[schemars(default = "default_max_items")]
    pub max_items: Option<usize>,
}

/// Arguments for the `ssh_exec_cancel` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshExecCancelArgs {
    /// `COMMAND_ID` returned from `ssh_exec`.
    pub command_id: String,

    /// Maximum bytes shown in stdout/stderr. Default: 16384. Cap:
    /// 1048576. Content head-truncated; tail (most recent output)
    /// preserved. Env: `SSH_MCP_OUTPUT_DEFAULT_BYTES` /
    /// `SSH_MCP_OUTPUT_MAX_BYTES_CAP`.
    #[schemars(default = "default_max_output_bytes")]
    pub max_output_bytes: Option<usize>,
}

/// Arguments for the `ssh_run` MCP tool.
///
/// One-shot orchestration of `ssh_connect` + `ssh_exec` +
/// (optional) `ssh_disconnect`. Avoids the three-round-trip
/// `connect -> execute -> wait` choreography for short atomic
/// commands like `uptime`, `hostname`, etc.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshRunArgs {
    /// SSH server address in the form `host:port` (e.g.
    /// `192.168.1.1:22`, `example.com:2222`). Port defaults to 22.
    pub address: String,

    /// SSH login username.
    pub username: String,

    /// Command to run on the remote host.
    pub command: String,

    /// Password for password-based authentication. Optional.
    pub password: Option<String>,

    /// Path to a private key file. Optional. Auth chain: key ->
    /// password -> agent (`SSH_AUTH_SOCK`).
    pub key_path: Option<String>,

    /// Optional `AGENT_ID` for grouping the underlying session.
    pub agent_id: Option<String>,

    /// Allocate a pseudo-terminal for the command. Default: false.
    #[schemars(default = "default_pty")]
    pub pty: Option<bool>,

    /// Maximum seconds to wait for the command to complete. Default:
    /// 30. Capped at 300 by the inbound adapter.
    #[schemars(default = "default_run_timeout_secs")]
    pub timeout_secs: Option<u64>,

    /// Maximum bytes shown in stdout/stderr. Default: 16384. Cap:
    /// 1048576.
    #[schemars(default = "default_run_max_output_bytes")]
    pub max_output_bytes: Option<usize>,

    /// Disconnect the session after the command finishes. Default:
    /// true (one-shot mode). Set false to keep the session open and
    /// reuse it for subsequent `ssh_exec` calls.
    #[schemars(default = "default_disconnect_after_run")]
    pub disconnect_after: Option<bool>,
}

/// Arguments for the `ssh_exec_batch` MCP tool — sequential
/// execution of multiple commands on the same session, with optional
/// stop-on-failure semantics.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshExecBatchArgs {
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,

    /// Commands to execute, in order. 1..=16 entries.
    pub commands: Vec<String>,

    /// Halt the loop on the first non-zero exit code. Default: true.
    #[schemars(default = "default_stop_on_failure")]
    pub stop_on_failure: Option<bool>,

    /// Per-command wait timeout in seconds. Default: 30. Cap: 300.
    #[schemars(default = "default_run_timeout_secs")]
    pub timeout_secs_per_command: Option<u64>,

    /// Per-command max bytes shown in stdout/stderr. Default: 16384.
    /// Cap: 1048576.
    #[schemars(default = "default_run_max_output_bytes")]
    pub max_output_bytes_per_command: Option<usize>,

    /// Allocate a PTY for each command. Default: false.
    #[schemars(default = "default_pty")]
    pub pty: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::{
        CommandStatus, SshCommandsArgs, SshExecArgs, SshExecCancelArgs, SshExecOutputArgs,
    };
    use crate::domain::policy::CommandStatusFilter;
    use schemars::schema_for;
    use serde_json::Value;

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

    /// See `connection::tests::property_default` for the helper rationale.
    fn property_default<'a>(schema_json: &'a Value, field: &str) -> Option<&'a Value> {
        let property = schema_json.get("properties")?.get(field)?;
        property.get("default")
    }

    #[test]
    fn ssh_execute_schema_emits_documented_defaults() {
        let schema = schema_for!(SshExecArgs);
        let schema_json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&schema_json, "timeout_secs"),
            Some(&Value::from(180_u64))
        );
        assert_eq!(
            property_default(&schema_json, "pty"),
            Some(&Value::Bool(false))
        );
    }

    #[test]
    fn ssh_get_command_output_schema_emits_documented_defaults() {
        let schema = schema_for!(SshExecOutputArgs);
        let schema_json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&schema_json, "wait"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            property_default(&schema_json, "wait_timeout_secs"),
            Some(&Value::from(30_u64))
        );
        assert_eq!(
            property_default(&schema_json, "max_output_bytes"),
            Some(&Value::from(16384_usize))
        );
    }

    #[test]
    fn ssh_list_commands_schema_emits_max_items_default() {
        let schema = schema_for!(SshCommandsArgs);
        let schema_json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&schema_json, "max_items"),
            Some(&Value::from(500_usize))
        );
    }

    #[test]
    fn ssh_cancel_command_schema_emits_documented_defaults() {
        let schema = schema_for!(SshExecCancelArgs);
        let schema_json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&schema_json, "max_output_bytes"),
            Some(&Value::from(16384_usize))
        );
    }

    // ---------- v4.7-step3 ssh_run / ssh_exec_batch arg tests ------

    #[test]
    fn ssh_run_schema_emits_documented_defaults() {
        use super::SshRunArgs;
        let schema = schema_for!(SshRunArgs);
        let schema_json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&schema_json, "timeout_secs"),
            Some(&Value::from(30_u64))
        );
        assert_eq!(
            property_default(&schema_json, "max_output_bytes"),
            Some(&Value::from(16384_usize))
        );
        assert_eq!(
            property_default(&schema_json, "disconnect_after"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            property_default(&schema_json, "pty"),
            Some(&Value::Bool(false))
        );
    }

    #[test]
    fn ssh_execute_batch_schema_emits_documented_defaults() {
        use super::SshExecBatchArgs;
        let schema = schema_for!(SshExecBatchArgs);
        let schema_json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&schema_json, "stop_on_failure"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            property_default(&schema_json, "timeout_secs_per_command"),
            Some(&Value::from(30_u64))
        );
        assert_eq!(
            property_default(&schema_json, "max_output_bytes_per_command"),
            Some(&Value::from(16384_usize))
        );
    }

    #[test]
    fn ssh_run_args_round_trip() {
        use super::SshRunArgs;
        let raw = serde_json::json!({
            "address": "h.example.com:22",
            "username": "alice",
            "command": "uptime",
            "pty": true,
            "timeout_secs": 5,
            "disconnect_after": false,
        });
        let parsed: SshRunArgs = serde_json::from_value(raw).expect("parse");
        assert_eq!(parsed.address, "h.example.com:22");
        assert_eq!(parsed.username, "alice");
        assert_eq!(parsed.command, "uptime");
        assert_eq!(parsed.pty, Some(true));
        assert_eq!(parsed.timeout_secs, Some(5));
        assert_eq!(parsed.disconnect_after, Some(false));
    }

    #[test]
    fn ssh_execute_batch_args_round_trip() {
        use super::SshExecBatchArgs;
        let raw = serde_json::json!({
            "session_id": "sess-1",
            "commands": ["uptime", "hostname"],
            "stop_on_failure": false,
        });
        let parsed: SshExecBatchArgs = serde_json::from_value(raw).expect("parse");
        assert_eq!(parsed.session_id, "sess-1");
        assert_eq!(parsed.commands.len(), 2);
        assert_eq!(parsed.stop_on_failure, Some(false));
    }
}
