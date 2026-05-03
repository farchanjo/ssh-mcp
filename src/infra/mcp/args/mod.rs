//! Stub argument types for the v4 rmcp tool wrappers.
//!
//! Each [`Deserialize`] + [`JsonSchema`] struct is the inbound DTO the
//! `#[tool_router]` macro deserialises from the MCP JSON-RPC payload. H16
//! deliberately ships **minimum** fields so the use cases compile and
//! deliver placeholder responses; H17 fills v3 parity (every optional
//! tuning knob, env-var fallbacks, etc.).
//!
//! The tool implementations live in [`super::tool_router`].

use schemars::JsonSchema;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Connection domain
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_connect` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshConnectArgs {
    /// SSH server address as `host:port` (e.g. `192.168.1.1:22`).
    pub address: String,
    /// SSH username.
    pub username: String,
}

/// Arguments for the `ssh_disconnect` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshDisconnectArgs {
    /// `SESSION_ID` returned from a previous `ssh_connect`.
    pub session_id: String,
}

/// Arguments for the `ssh_list_sessions` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshListSessionsArgs {
    /// Optional `AGENT_ID` filter.
    pub agent_id: Option<String>,
}

/// Arguments for the `ssh_disconnect_agent` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshDisconnectAgentArgs {
    /// `AGENT_ID` returned from a previous `ssh_connect`.
    pub agent_id: String,
}

// ---------------------------------------------------------------------------
// Execute domain
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_execute` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshExecuteArgs {
    /// Owning session.
    pub session_id: String,
    /// Shell command line to run on the remote host.
    pub command: String,
}

/// Arguments for the `ssh_get_command_output` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshGetCommandOutputArgs {
    /// `COMMAND_ID` returned from `ssh_execute`.
    pub command_id: String,
}

/// Arguments for the `ssh_list_commands` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshListCommandsArgs {
    /// Optional `SESSION_ID` filter.
    pub session_id: Option<String>,
}

/// Arguments for the `ssh_cancel_command` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshCancelCommandArgs {
    /// `COMMAND_ID` to cancel.
    pub command_id: String,
}

// ---------------------------------------------------------------------------
// Shell domain
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_shell_open` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellOpenArgs {
    /// Owning session.
    pub session_id: String,
}

/// Arguments for the `ssh_shell_write` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellWriteArgs {
    /// Target shell.
    pub shell_id: String,
    /// Bytes to send to the PTY.
    pub input: String,
}

/// Arguments for the `ssh_shell_send_key` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellSendKeyArgs {
    /// Target shell.
    pub shell_id: String,
    /// Named keystroke (snake-case label, e.g. `ctrl_c`, `arrow_up`, `f5`).
    pub key: String,
}

/// Arguments for the `ssh_shell_read` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellReadArgs {
    /// Target shell.
    pub shell_id: String,
}

/// Arguments for the `ssh_shell_wait_for` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellWaitForArgs {
    /// Target shell.
    pub shell_id: String,
    /// Substring patterns to wait for (1..=16, ≤1024 bytes each).
    pub patterns: Vec<String>,
}

/// Arguments for the `ssh_shell_close` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellCloseArgs {
    /// Shell to close.
    pub shell_id: String,
}

// ---------------------------------------------------------------------------
// SFTP domain
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_upload` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshUploadArgs {
    /// Owning session.
    pub session_id: String,
    /// Local source path.
    pub local_path: String,
    /// Remote destination path.
    pub remote_path: String,
}

/// Arguments for the `ssh_download` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshDownloadArgs {
    /// Owning session.
    pub session_id: String,
    /// Remote source path.
    pub remote_path: String,
    /// Local destination path.
    pub local_path: String,
}

/// Arguments for the `ssh_get_transfer_progress` MCP tool (H16 stub).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshGetTransferProgressArgs {
    /// Transfer to query.
    pub transfer_id: String,
}

// ---------------------------------------------------------------------------
// Forward domain (feature-gated)
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_forward` MCP tool (H16 stub, feature-gated).
#[cfg(feature = "port_forward")]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshForwardArgs {
    /// Owning session.
    pub session_id: String,
    /// Local listener port.
    pub local_port: u16,
    /// Remote target host.
    pub remote_address: String,
    /// Remote target port.
    pub remote_port: u16,
}
