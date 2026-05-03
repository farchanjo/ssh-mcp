//! Typed Rust output structs for the v4.7 `output_schema` advertisement.
//!
//! Each struct mirrors the runtime `structured_content` payload of the
//! associated tool's success path. Wired through the rmcp `#[tool]`
//! macro's `output_schema = ...` attribute to publish a JSON Schema on
//! the `tools/list` response — smaller LLMs can validate the structured
//! payload they receive against the advertised schema without hard-coding
//! any field names.
//!
//! ## Coverage
//!
//! v4.7 ships typed schemas only for the six most-used tools — connect,
//! execute, get-command-output, shell-open, shell-read,
//! get-transfer-progress. The remaining 12 tools still emit a full
//! structured payload, but their `output_schema` is not advertised; the
//! payload shape is documented in the per-render `*_structured` doc
//! comments under [`super::render`]. Lifting the remaining tools is a
//! mechanical follow-up tracked in v4.8.
//!
//! ## Stability
//!
//! Every struct is `#[non_exhaustive]` so callers cannot match
//! exhaustively across versions; new optional fields can be added
//! without bumping the major version.

use schemars::JsonSchema;
use serde::Serialize;

/// Successful `ssh_connect` payload (covers `ok`, `reused`, `suggested`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshConnectResult {
    /// Discriminator: always `"ssh_connect"`.
    pub tool: String,
    /// `"ok"` for a fresh connect, `"reused"` for a healthy match, or
    /// `"suggested"` when the caller must pick from the matches list.
    pub status: String,
    /// Newly-minted (or reused) session id. Absent on `"suggested"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Resolved host portion of the SSH endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Resolved port (defaults to 22).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// SSH login user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Optional grouping identifier for bulk-cleanup via
    /// `ssh_disconnect_agent`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Number of retry attempts consumed during the handshake.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<u32>,
    /// Whether SSH zlib compression was negotiated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compression_enabled: Option<bool>,
    /// Whether the session opted out of the inactivity sweeper.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistent: Option<bool>,
    /// RFC 3339 deadline; `null` when `persistent = true` or the
    /// inactivity timeout is zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Successor tool calls advertised to the LLM.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Vec<String>>,
}

/// `ssh_execute` started payload.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshExecuteResult {
    /// Discriminator: always `"ssh_execute"`.
    pub tool: String,
    /// Lifecycle status; `"started"` for the async path.
    pub status: String,
    /// Owning session.
    pub session_id: String,
    /// Newly-minted command id.
    pub command_id: String,
    /// Inherited grouping id, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Successor tool calls advertised to the LLM.
    pub next: Vec<String>,
}

/// `ssh_get_command_output` payload.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshGetCommandOutputResult {
    /// Discriminator: always `"ssh_get_command_output"`.
    pub tool: String,
    /// One of `"running"`, `"completed"`, `"timeout"`, `"cancelled"`,
    /// `"failed"`.
    pub status: String,
    /// Echoed command id.
    pub command_id: String,
    /// Captured exit code; only set when `status = "completed"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Stdout snapshot (head-truncated to 16 KiB).
    pub stdout: String,
    /// Stderr snapshot (head-truncated to 16 KiB).
    pub stderr: String,
    /// `true` when the snapshot dropped trailing stdout bytes.
    pub stdout_truncated: bool,
    /// `true` when the snapshot dropped trailing stderr bytes.
    pub stderr_truncated: bool,
    /// `true` when the command exited because the configured timeout
    /// fired.
    pub timed_out: bool,
    /// Optional error string set when the command failed before producing
    /// an exit status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Successor tool calls advertised to the LLM (only set while
    /// `status = "running"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Vec<String>>,
}

/// `ssh_shell_open` payload.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshShellOpenResult {
    /// Discriminator: always `"ssh_shell_open"`.
    pub tool: String,
    /// Always `"ok"` on the success path.
    pub status: String,
    /// Owning session.
    pub session_id: String,
    /// Newly-minted shell id.
    pub shell_id: String,
    /// Negotiated `TERM` value (e.g. `"xterm"`).
    pub term: String,
    /// Negotiated terminal column count.
    pub cols: u16,
    /// Negotiated terminal row count.
    pub rows: u16,
    /// Inherited grouping id, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Successor tool calls advertised to the LLM.
    pub next: Vec<String>,
}

/// `ssh_shell_read` payload.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshShellReadResult {
    /// Discriminator: always `"ssh_shell_read"`.
    pub tool: String,
    /// One of `"open"`, `"closed"`, `"timeout"`.
    pub status: String,
    /// Echoed shell id.
    pub shell_id: String,
    /// Buffer slice rendered into the response.
    pub stdout: String,
    /// Number of bytes returned in [`Self::stdout`].
    pub bytes_returned: usize,
    /// Total bytes the buffer held at snapshot time.
    pub buffer_size_at_snapshot: usize,
    /// `true` when the buffer was drained as part of the read.
    pub cleared: bool,
    /// `true` when the snapshot dropped leading bytes.
    pub truncated: bool,
}

/// `ssh_get_transfer_progress` payload.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshGetTransferProgressResult {
    /// Discriminator: always `"ssh_get_transfer_progress"`.
    pub tool: String,
    /// One of `"running"`, `"completed"`, `"failed"`, `"cancelled"`.
    pub status: String,
    /// Echoed transfer id.
    pub transfer_id: String,
    /// `"upload"` or `"download"`.
    pub direction: String,
    /// Progress in 0..=100.
    pub progress_percent: u8,
    /// Bytes transferred so far.
    pub bytes_transferred: u64,
    /// Total bytes the transfer will move.
    pub total_bytes: u64,
    /// Optional terminal failure description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Successor tool calls advertised to the LLM (only set while
    /// `status = "running"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Vec<String>>,
}
