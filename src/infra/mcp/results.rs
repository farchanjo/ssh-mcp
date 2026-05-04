//! Typed Rust output structs for the v4.8 `output_schema` advertisement.
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
//! v4.8 lifts schema coverage to **all 21 MCP tools** (or 20 without the
//! `port_forward` Cargo feature). v4.7 shipped only six (`ssh_connect`,
//! `ssh_execute`, `ssh_get_command_output`, `ssh_shell_open`,
//! `ssh_shell_read`, `ssh_get_transfer_progress`) plus the three
//! v4.7-step3 additions (`ssh_run`, `ssh_execute_batch`,
//! `ssh_disconnect_many`); the remaining 12 tools now advertise typed
//! schemas mirroring their `structured_content` payload byte-for-byte.
//! The Markdown body is unchanged.
//!
//! ## Stability
//!
//! Every struct is `#[non_exhaustive]` so callers cannot match
//! exhaustively across versions; new optional fields can be added
//! without bumping the major version. Optional fields use
//! `#[serde(skip_serializing_if = "Option::is_none")]` so absent values
//! are not surfaced as JSON `null` on the wire.

use schemars::JsonSchema;
use serde::Serialize;

/// One per-session entry surfaced in `ssh_list_sessions` (and embedded
/// in [`SshConnectResult`] when `status = "suggested"`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SessionEntry {
    /// Stable session id.
    pub session_id: String,
    /// Resolved host portion of the SSH endpoint.
    pub host: String,
    /// Resolved port (defaults to 22).
    pub port: u16,
    /// SSH login user.
    pub username: String,
    /// Optional grouping identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Optional caller-supplied display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// RFC 3339 timestamp of when the session was first established.
    pub connected_at: String,
    /// Last health-check verdict; `null` until the first check fires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub healthy: Option<bool>,
    /// Whether SSH zlib compression was negotiated.
    pub compression_enabled: bool,
}

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
    /// Optional caller-supplied display name echoed from the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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
    /// Number of stale duplicate sessions evicted on the connect path
    /// (only emitted on `status = "ok"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaced: Option<usize>,
    /// Match list surfaced when `status = "suggested"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<Vec<SessionEntry>>,
    /// Count of entries in [`Self::matches`] (only set on `"suggested"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    /// Successor tool calls advertised to the LLM.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Vec<String>>,
}

/// `ssh_disconnect` payload — single-session teardown summary.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshDisconnectResult {
    /// Discriminator: always `"ssh_disconnect"`.
    pub tool: String,
    /// Always `"ok"` on the success path.
    pub status: String,
    /// Echoed session id.
    pub session_id: String,
    /// Number of async commands cancelled by the disconnect.
    pub commands_cancelled: usize,
    /// Number of interactive shells closed by the disconnect.
    pub shells_closed: usize,
    /// Number of in-flight transfers aborted by the disconnect.
    pub transfers_aborted: usize,
}

/// `ssh_list_sessions` payload — current healthy sessions plus an
/// optional bulk-cleanup hint.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshListSessionsResult {
    /// Discriminator: always `"ssh_list_sessions"`.
    pub tool: String,
    /// Always `"ok"` on the success path.
    pub status: String,
    /// Echoed `agent_id` filter (`None` when the caller did not narrow
    /// the request).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id_filter: Option<String>,
    /// Healthy sessions, ordered by the repository's natural order.
    pub sessions: Vec<SessionEntry>,
    /// Number of entries in [`Self::sessions`].
    pub count: usize,
    /// Total session count before the request `max_items` cap was
    /// applied (may exceed `count`).
    pub total: usize,
    /// Anti-leak nudge surfaced when one agent owns more than the
    /// configured threshold of sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Successor tool calls advertised to the LLM.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Vec<String>>,
}

/// `ssh_disconnect_agent` payload — bulk-by-agent teardown summary.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshDisconnectAgentResult {
    /// Discriminator: always `"ssh_disconnect_agent"`.
    pub tool: String,
    /// Always `"ok"` on the success path.
    pub status: String,
    /// Echoed agent id.
    pub agent_id: String,
    /// Number of sessions closed.
    pub sessions_closed: usize,
    /// Number of async commands cancelled.
    pub commands_cancelled: usize,
    /// Number of interactive shells closed.
    pub shells_closed: usize,
    /// Number of in-flight transfers aborted.
    pub transfers_aborted: usize,
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

/// One per-command entry surfaced by [`SshListCommandsResult`].
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct CommandEntry {
    /// Stable command id.
    pub command_id: String,
    /// Owning session id.
    pub session_id: String,
    /// Verbatim command line.
    pub command: String,
    /// One of `"running"`, `"completed"`, `"cancelled"`, `"failed"`.
    pub status: String,
    /// RFC 3339 timestamp of when the command was spawned.
    pub started_at: String,
}

/// `ssh_list_commands` payload — async command inventory snapshot.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshListCommandsResult {
    /// Discriminator: always `"ssh_list_commands"`.
    pub tool: String,
    /// Always `"ok"` on the success path.
    pub status: String,
    /// Tracked commands matching the optional filters.
    pub commands: Vec<CommandEntry>,
    /// Number of entries in [`Self::commands`].
    pub count: usize,
    /// Total count before the request `max_items` cap was applied
    /// (may exceed `count`).
    pub total: usize,
}

/// `ssh_cancel_command` payload — covers both the `ok` path
/// (cancelled with partial stdout/stderr capture) and the `noop` path
/// (command already terminal).
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshCancelCommandResult {
    /// Discriminator: always `"ssh_cancel_command"`.
    pub tool: String,
    /// `"ok"` when the command was running and got cancelled, `"noop"`
    /// when the command had already reached a terminal state.
    pub status: String,
    /// Echoed command id.
    pub command_id: String,
    /// Stdout snapshot at cancellation time (head-truncated). Absent on
    /// the `"noop"` branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    /// Stderr snapshot at cancellation time (head-truncated). Absent on
    /// the `"noop"` branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    /// `true` when the stdout snapshot dropped trailing bytes. Absent on
    /// the `"noop"` branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_truncated: Option<bool>,
    /// `true` when the stderr snapshot dropped trailing bytes. Absent on
    /// the `"noop"` branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_truncated: Option<bool>,
    /// Status of the command at the moment the cancel arrived; only set
    /// on the `"noop"` branch (one of `"running"`, `"completed"`,
    /// `"cancelled"`, `"failed"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
    /// UTF-8 lossy snapshot of any stdout the PTY emitted within the
    /// initial peek budget. Omitted when the peek returned empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_buffer: Option<String>,
    /// Successor tool calls advertised to the LLM.
    pub next: Vec<String>,
}

/// `ssh_shell_write` payload.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshShellWriteResult {
    /// Discriminator: always `"ssh_shell_write"`.
    pub tool: String,
    /// Always `"ok"` on the success path.
    pub status: String,
    /// Echoed shell id.
    pub shell_id: String,
    /// Number of bytes written to the PTY.
    pub bytes_sent: usize,
    /// Successor tool calls advertised to the LLM.
    pub next: Vec<String>,
}

/// `ssh_shell_send_key` payload.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshShellSendKeyResult {
    /// Discriminator: always `"ssh_shell_send_key"`.
    pub tool: String,
    /// Always `"ok"` on the success path.
    pub status: String,
    /// Echoed shell id.
    pub shell_id: String,
    /// Canonical name of the key sent (e.g. `"arrow_up"`, `"enter"`).
    pub key: String,
    /// Modifier list parsed from the request (`"shift"`, `"alt"`,
    /// `"ctrl"`); empty when no modifier was requested.
    pub modifiers: Vec<String>,
    /// Repeat count applied to the keystroke (1..=64).
    pub repeat: u8,
    /// Number of bytes written to the PTY for the entire (modifiers +
    /// key + repeat) expansion.
    pub bytes_sent: usize,
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

/// `ssh_shell_wait_for` payload — pattern-gated PTY snapshot.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshShellWaitForResult {
    /// Discriminator: always `"ssh_shell_wait_for"`.
    pub tool: String,
    /// One of `"matched"`, `"timeout"`, `"closed"`.
    pub status: String,
    /// Echoed shell id.
    pub shell_id: String,
    /// Pattern that matched (only set on `"matched"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_pattern: Option<String>,
    /// Buffer slice rendered into the response (head-truncated to 16
    /// KiB).
    pub stdout: String,
    /// Number of bytes returned in [`Self::stdout`].
    pub bytes_returned: usize,
    /// Successor tool calls advertised to the LLM (only set on
    /// `"matched"` / `"timeout"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<Vec<String>>,
}

/// `ssh_shell_close` payload.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshShellCloseResult {
    /// Discriminator: always `"ssh_shell_close"`.
    pub tool: String,
    /// Always `"ok"` on the success path.
    pub status: String,
    /// Echoed shell id.
    pub shell_id: String,
}

/// `ssh_upload` payload — SFTP upload `STARTED` snapshot.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshUploadResult {
    /// Discriminator: always `"ssh_upload"`.
    pub tool: String,
    /// Lifecycle status; `"started"` for the async transfer path.
    pub status: String,
    /// Newly-minted transfer id.
    pub transfer_id: String,
    /// Owning session.
    pub session_id: String,
    /// Inherited grouping id, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Source path (local for upload).
    pub from: String,
    /// Destination path (remote for upload).
    pub to: String,
    /// Total payload size in bytes.
    pub size_bytes: u64,
    /// Successor tool calls advertised to the LLM.
    pub next: Vec<String>,
}

/// `ssh_download` payload — SFTP download `STARTED` snapshot.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshDownloadResult {
    /// Discriminator: always `"ssh_download"`.
    pub tool: String,
    /// Lifecycle status; `"started"` for the async transfer path.
    pub status: String,
    /// Newly-minted transfer id.
    pub transfer_id: String,
    /// Owning session.
    pub session_id: String,
    /// Inherited grouping id, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Source path (remote for download).
    pub from: String,
    /// Destination path (local for download).
    pub to: String,
    /// Total payload size in bytes.
    pub size_bytes: u64,
    /// Successor tool calls advertised to the LLM.
    pub next: Vec<String>,
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

/// `ssh_forward` payload — TCP port-forward `OK` snapshot. Only emitted
/// when the `port_forward` Cargo feature is enabled.
#[cfg(feature = "port_forward")]
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshForwardResult {
    /// Discriminator: always `"ssh_forward"`.
    pub tool: String,
    /// Always `"ok"` on the success path.
    pub status: String,
    /// Newly-minted forward id.
    pub forward_id: String,
    /// Owning session.
    pub session_id: String,
    /// Local listener address (e.g. `"0.0.0.0:8080"`).
    pub local: String,
    /// Remote endpoint (e.g. `"example.com:3306"`).
    pub remote: String,
    /// Always `true` on the success path; the value is included so the
    /// schema documents the field shape.
    pub active: bool,
    /// Successor tool calls advertised to the LLM.
    pub next: Vec<String>,
}

/// `ssh_run` payload — surfaces the resolved session id, the captured
/// exit code, and the truncated stdout/stderr blocks.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the four bool fields each describe a distinct observable: disconnected = lifecycle, stdout_truncated/stderr_truncated = output integrity, timed_out = wait budget; collapsing them hurts the wire schema"
)]
pub struct SshRunResult {
    /// Discriminator: always `"ssh_run"`.
    pub tool: String,
    /// One of `"completed"`, `"timeout"`, `"failed"`, `"cancelled"`.
    pub status: String,
    /// Resolved session id (newly minted or reused).
    pub session_id: String,
    /// Newly minted command id.
    pub command_id: String,
    /// `true` when the session was disconnected after the command
    /// finished (default), `false` when the caller opted to keep the
    /// session alive for follow-up calls.
    pub disconnected: bool,
    /// Captured exit code; `None` for terminal states other than
    /// `"completed"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Stdout snapshot.
    pub stdout: String,
    /// Stderr snapshot.
    pub stderr: String,
    /// `true` when the snapshot dropped trailing stdout bytes.
    pub stdout_truncated: bool,
    /// `true` when the snapshot dropped trailing stderr bytes.
    pub stderr_truncated: bool,
    /// `true` when the wait budget fired before completion.
    pub timed_out: bool,
    /// Optional error description set when the command failed before
    /// producing an exit status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One per-command entry surfaced by [`SshExecuteBatchResult`].
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
#[allow(
    clippy::struct_excessive_bools,
    reason = "stdout_truncated/stderr_truncated/timed_out describe distinct observables on a single per-command snapshot; collapsing them hurts the wire schema"
)]
pub struct SshExecuteBatchEntry {
    /// 0-based index into the input commands array.
    pub index: usize,
    /// Verbatim command line.
    pub command: String,
    /// One of `"completed"`, `"timeout"`, `"failed"`, `"cancelled"`,
    /// `"skipped"`.
    pub status: String,
    /// Newly minted command id (absent on `"skipped"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    /// Captured exit code (absent for non-`"completed"` entries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Stdout snapshot.
    pub stdout: String,
    /// Stderr snapshot.
    pub stderr: String,
    /// `true` when the stdout snapshot dropped trailing bytes.
    pub stdout_truncated: bool,
    /// `true` when the stderr snapshot dropped trailing bytes.
    pub stderr_truncated: bool,
    /// `true` when the per-command wait budget fired before
    /// completion.
    pub timed_out: bool,
    /// Optional error description when the command failed before
    /// producing an exit status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `ssh_execute_batch` payload — sequential execution of multiple
/// commands against a single session.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshExecuteBatchResult {
    /// Discriminator: always `"ssh_execute_batch"`.
    pub tool: String,
    /// `"ok"` when every command ran (regardless of exit code) or
    /// `"halted"` when stop-on-failure short-circuited the loop.
    pub status: String,
    /// Owning session.
    pub session_id: String,
    /// Per-command outcome list, ordered by `index`.
    pub results: Vec<SshExecuteBatchEntry>,
    /// Total number of input commands.
    pub total: usize,
    /// Number of commands actually executed (may be less than
    /// `total` when `status = "halted"`).
    pub executed: usize,
}

/// One per-id entry surfaced by [`SshDisconnectManyResult`].
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshDisconnectManyEntry {
    /// Echoed session id.
    pub session_id: String,
    /// `"ok"` on a successful disconnect, `"error"` otherwise.
    pub status: String,
    /// v4.5 wire error code when `status = "error"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Human-readable reason when `status = "error"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `ssh_disconnect_many` payload — best-effort bulk disconnect.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct SshDisconnectManyResult {
    /// Discriminator: always `"ssh_disconnect_many"`.
    pub tool: String,
    /// Always `"ok"` — per-id failures surface inside `results`.
    pub status: String,
    /// Per-id outcome list.
    pub results: Vec<SshDisconnectManyEntry>,
    /// Number of successfully disconnected sessions.
    pub disconnected: usize,
    /// Number of sessions that returned an error.
    pub failed: usize,
}
