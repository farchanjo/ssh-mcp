//! `McpSshServer` — primary MCP entry point implementing `rmcp::ServerHandler`.
//!
//! In v3.0.0 this struct owns:
//! - The `ToolRouter<Self>` aggregating the 16 SSH tools (E3 wired
//!   `ssh_connect`; E4 wires the remaining 15).
//! - Resource handlers for `shell://`, `command://`, `transfer://`,
//!   `session://`, and `forward://` URIs (subscribe-first realtime streams,
//!   landing in E13).

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ErrorData as McpError, Implementation, ListResourcesResult,
    PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResult,
    ServerCapabilities, ServerInfo, SubscribeRequestParams, UnsubscribeRequestParams,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, tool, tool_handler, tool_router};

use crate::mcp::resources::{
    list_resources_impl, peer_id_for, read_resource_impl, subscribe_impl, unsubscribe_impl,
};

use super::tools::connection::{
    SshConnectArgs, SshDisconnectAgentArgs, SshDisconnectArgs, SshListSessionsArgs,
    ssh_connect_impl, ssh_disconnect_agent_impl, ssh_disconnect_impl, ssh_list_sessions_impl,
};
use super::tools::execute::{
    SshCancelCommandArgs, SshExecuteArgs, SshGetCommandOutputArgs, SshListCommandsArgs,
    ssh_cancel_command_impl, ssh_execute_impl, ssh_get_command_output_impl, ssh_list_commands_impl,
};
use super::tools::forward::{SshForwardArgs, ssh_forward_impl};
use super::tools::sftp::{
    SshDownloadArgs, SshGetTransferProgressArgs, SshUploadArgs, ssh_download_impl,
    ssh_get_transfer_progress_impl, ssh_upload_impl,
};
use super::tools::shell::{
    SshShellCloseArgs, SshShellOpenArgs, SshShellReadArgs, SshShellSendKeyArgs,
    SshShellWaitForArgs, SshShellWriteArgs, ssh_shell_close_impl, ssh_shell_open_impl,
    ssh_shell_read_impl, ssh_shell_send_key_impl, ssh_shell_wait_for_impl, ssh_shell_write_impl,
};

/// Primary MCP server handler.
#[derive(Debug, Clone)]
pub struct McpSshServer {
    tool_router: ToolRouter<Self>,
}

impl Default for McpSshServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl McpSshServer {
    /// Create a new server with the v3.0.0 tool router.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    /// Connect to an SSH server and store the session.
    ///
    /// **When to use:**
    /// - Establish a new SSH connection to run commands, open shells, or transfer files.
    /// - Reuse an already-connected session by passing its `session_id`.
    ///
    /// **Important identifiers in response:**
    /// - `SESSION_ID`: pass to ssh_execute, ssh_shell_open, ssh_upload,
    ///   ssh_download, ssh_disconnect, ssh_forward.
    /// - `AGENT_ID`: optional grouping; pass to ssh_list_sessions (filter)
    ///   and ssh_disconnect_agent (bulk cleanup).
    ///
    /// **Workflow:**
    /// 1. Call ssh_connect once per remote host.
    /// 2. Use the returned SESSION_ID for subsequent tool calls.
    /// 3. Call ssh_disconnect (or ssh_disconnect_agent) when done.
    ///
    /// **Limits:** `timeout_secs` default 30 (env SSH_CONNECT_TIMEOUT);
    /// `max_retries` default 3 (env SSH_MAX_RETRIES); `retry_delay_ms`
    /// default 1000 capped at 10s (env SSH_RETRY_DELAY_MS).
    ///
    /// **Status values:**
    /// - `OK`: brand-new session created.
    /// - `REUSED`: existing healthy session returned (reuse=auto, or
    ///   `session_id` provided).
    /// - `SUGGESTED`: matching session(s) exist; LLM picks one or retries
    ///   with `reuse="force_new"`.
    ///
    /// **Errors:**
    /// - `CONNECTION_FAILED`: handshake failed or all retries exhausted.
    #[tool(
        description = "Connect to an SSH server and store the session. Returns SESSION_ID (and optional AGENT_ID). Status: OK, REUSED, SUGGESTED. reuse=suggest (default), auto, force_new. Use SESSION_ID with ssh_execute, ssh_shell_open, ssh_upload, ssh_download, ssh_disconnect, ssh_forward. Errors: CONNECTION_FAILED."
    )]
    async fn ssh_connect(
        &self,
        Parameters(args): Parameters<SshConnectArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_connect_impl(args).await
    }

    /// Disconnect a single SSH session and free its resources.
    ///
    /// **When to use:**
    /// - Done with a session and want a clean teardown.
    ///
    /// **Important identifiers in response:**
    /// - `SESSION_ID`: echoed back for traceability.
    ///
    /// **Workflow:**
    /// 1. Cancel every running async command for the session.
    /// 2. Close every interactive shell.
    /// 3. Abort every in-flight SFTP transfer.
    /// 4. Disconnect the SSH transport.
    ///
    /// **Errors:**
    /// - `SESSION_NOT_FOUND`: no session with the given ID.
    #[tool(
        description = "Disconnect a single SSH session by SESSION_ID. Cancels all running commands, closes all shells, and aborts all transfers for the session. Errors: SESSION_NOT_FOUND."
    )]
    async fn ssh_disconnect(
        &self,
        Parameters(args): Parameters<SshDisconnectArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_disconnect_impl(args).await
    }

    /// List active SSH sessions with health-check metadata.
    ///
    /// **When to use:**
    /// - Discover available SESSION_IDs.
    /// - Audit which agents own which sessions.
    ///
    /// **Important identifiers in response:**
    /// - `SESSION_ID`: pass to other tools.
    /// - `AGENT_ID`: optional owning agent (when set at connect time).
    ///
    /// **Workflow:** runs an `echo 1` health probe against each candidate
    /// session and removes any that fail before returning. Pass `agent_id`
    /// to filter to a single agent.
    ///
    /// **Limits:** `max_items` default 500, cap 10000.
    #[tool(
        description = "List active SSH sessions with health-check metadata. Optional agent_id filter (AGENT_ID). max_items default 500, cap 10000. Returns SESSION_ID, host, username, agent_id, healthy, last_health_check."
    )]
    async fn ssh_list_sessions(
        &self,
        Parameters(args): Parameters<SshListSessionsArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_list_sessions_impl(args).await
    }

    /// Bulk-disconnect every session owned by an agent.
    ///
    /// **When to use:**
    /// - At agent shutdown to release every resource the agent allocated.
    ///
    /// **Important identifiers in response:**
    /// - `AGENT`: echoed back for traceability.
    /// - `SESSIONS`: count of sessions disconnected.
    /// - `COMMANDS`: count of running commands cancelled.
    ///
    /// **Workflow:**
    /// 1. Enumerate sessions owned by `agent_id`.
    /// 2. Cancel running commands, close shells, abort transfers per session.
    /// 3. Disconnect each session's SSH transport.
    ///
    /// Sessions owned by other agents are not affected.
    #[tool(
        description = "Disconnect ALL sessions for a specific AGENT_ID (bulk cleanup). Cancels commands, closes shells, aborts transfers for every owned session. Other agents' sessions are unaffected."
    )]
    async fn ssh_disconnect_agent(
        &self,
        Parameters(args): Parameters<SshDisconnectAgentArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_disconnect_agent_impl(args).await
    }

    /// Execute a shell command asynchronously on a session.
    ///
    /// **When to use:**
    /// - Any command — short or long-running.
    /// - Returns immediately with a COMMAND_ID; output is polled separately.
    ///
    /// **Important identifiers in response:**
    /// - `COMMAND_ID`: pass to ssh_get_command_output and ssh_cancel_command.
    /// - `SESSION_ID`: echoed back for traceability.
    ///
    /// **Workflow:**
    /// 1. ssh_execute -> COMMAND_ID.
    /// 2. ssh_get_command_output(command_id, wait=true) -> final result.
    ///
    /// **Limits:**
    /// - Up to 100 concurrent multiplexed commands per session.
    /// - `timeout_secs` default 180 (env SSH_COMMAND_TIMEOUT).
    ///
    /// **Errors:**
    /// - `SESSION_NOT_FOUND`: no session with the given ID.
    /// - `MAX_COMMANDS_EXCEEDED`: per-session running-command cap reached.
    #[tool(
        description = "Execute a command asynchronously. Returns COMMAND_ID immediately. Pass pty=true for commands needing a TTY (sudo, top). Up to 100 concurrent commands per session. Poll with ssh_get_command_output. Errors: SESSION_NOT_FOUND, MAX_COMMANDS_EXCEEDED."
    )]
    async fn ssh_execute(
        &self,
        Parameters(args): Parameters<SshExecuteArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_execute_impl(args).await
    }

    /// Read the current output and status of an async command.
    ///
    /// **When to use:**
    /// - Poll for progress (`wait=false`).
    /// - Block until completion (`wait=true`).
    ///
    /// **Important identifiers in response:**
    /// - `COMMAND_ID`: echoed back; use it on subsequent polls.
    /// - `EXIT`: present only on COMPLETED.
    ///
    /// **Limits:** stdout/stderr capped by `max_output_bytes` (default 16384,
    /// cap 1048576); content is head-truncated, tail (most recent output)
    /// preserved.
    ///
    /// **Status values:**
    /// - `RUNNING`: command still executing; output marked `(partial)`.
    /// - `COMPLETED`: command finished; `EXIT` line present.
    /// - `TIMEOUT`: long-poll deadline expired (only when `wait=true`).
    ///
    /// **Errors:**
    /// - `COMMAND_NOT_FOUND`: no async command with the given ID.
    #[tool(
        description = "Read output and status of an async command by COMMAND_ID. Set wait=true to block until completion (default 30s, cap 300s). Status: RUNNING, COMPLETED, TIMEOUT. max_output_bytes default 16384, cap 1048576 (head-truncated, tail preserved). Errors: COMMAND_NOT_FOUND."
    )]
    async fn ssh_get_command_output(
        &self,
        Parameters(args): Parameters<SshGetCommandOutputArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_get_command_output_impl(args).await
    }

    /// List async commands across one or all sessions.
    ///
    /// **When to use:**
    /// - Monitor multiple concurrent operations.
    /// - Check what is still running before disconnecting a session.
    ///
    /// **Important identifiers in response:**
    /// - `COMMAND_ID`: pass to ssh_get_command_output / ssh_cancel_command.
    /// - `SESSION_ID`: owning session for each command.
    ///
    /// **Limits:** `max_items` default 500, cap 10000.
    #[tool(
        description = "List async commands across one or all sessions. Optional filters: session_id (SESSION_ID); status (input enum: running|completed|cancelled|failed). max_items default 500, cap 10000. Returns COMMAND_IDs, owning SESSION_ID, status, started_at."
    )]
    async fn ssh_list_commands(
        &self,
        Parameters(args): Parameters<SshListCommandsArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_list_commands_impl(args).await
    }

    /// Cancel a running async command.
    ///
    /// **When to use:**
    /// - Stop a long-running command that is no longer needed.
    /// - Abort a command taking too long.
    ///
    /// **Important identifiers in response:**
    /// - `COMMAND_ID`: echoed back for traceability.
    ///
    /// **Limits:** stdout/stderr capped by `max_output_bytes` (default 16384,
    /// cap 1048576); content head-truncated, tail (most recent output)
    /// preserved.
    ///
    /// **Status values:**
    /// - `CANCELLED`: command was running and the cancellation signal was
    ///   sent; partial stdout/stderr returned.
    /// - `NOOP`: command was not in a running state (already finished).
    ///
    /// **Errors:**
    /// - `COMMAND_NOT_FOUND`: no async command with the given ID.
    #[tool(
        description = "Cancel a running async command by COMMAND_ID. Returns CANCELLED with partial stdout/stderr (head-truncated to max_output_bytes; default 16384, cap 1048576) or NOOP if not running. Errors: COMMAND_NOT_FOUND."
    )]
    async fn ssh_cancel_command(
        &self,
        Parameters(args): Parameters<SshCancelCommandArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_cancel_command_impl(args).await
    }

    /// Open an interactive PTY shell on a session.
    ///
    /// **When to use:**
    /// - Serial Over LAN (SOL/IPMI/OOB) consoles — use `term="vt100"` 80x24.
    /// - Multi-step workflows that need persistent shell state.
    /// - Commands requiring terminal interaction.
    ///
    /// **Important identifiers in response:**
    /// - `SHELL_ID`: pass to ssh_shell_write, ssh_shell_send_key,
    ///   ssh_shell_read, ssh_shell_wait_for, ssh_shell_close.
    ///
    /// **Prefer `resources/subscribe shell://<id>/output` (realtime push) over polling.**
    ///
    /// **Limits:**
    /// - Up to 10 shells per session.
    /// - `inactivity_ttl` default 600s (env SSH_SHELL_INACTIVITY_TTL).
    /// - `max_buffer_size` default `10m` (env SSH_SHELL_MAX_BUFFER_SIZE).
    ///
    /// **Errors:**
    /// - `SESSION_NOT_FOUND`: no session with the given ID.
    /// - `MAX_SHELLS_EXCEEDED`: 10 shells already open on this session.
    /// - `CHANNEL_FAILED`: russh failed to open the PTY channel.
    #[tool(
        description = "Open an interactive PTY shell. Returns SHELL_ID. Defaults: term=xterm 80x24 (use vt100 for SOL/IPMI). Up to 10 shells per session. Prefer resources/subscribe shell://<id>/output (realtime push) over polling. Errors: SESSION_NOT_FOUND, MAX_SHELLS_EXCEEDED, CHANNEL_FAILED."
    )]
    async fn ssh_shell_open(
        &self,
        Parameters(args): Parameters<SshShellOpenArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_shell_open_impl(args).await
    }

    /// Send raw input bytes to an interactive shell.
    ///
    /// **When to use:**
    /// - Type a command line (append `\n` for Enter).
    /// - Send a control character (`\x03` = Ctrl+C, `\x04` = Ctrl+D).
    /// - Send a non-standard escape sequence (`\x1b[A` = arrow up).
    ///
    /// **Prefer `ssh_shell_send_key`** for named keystrokes (ctrl_c,
    /// arrow_up, f-keys, modifiers). Use `ssh_shell_write` only for plain
    /// text or non-standard escape sequences.
    ///
    /// **Prefer `resources/subscribe shell://<id>/output` (realtime push) over polling.**
    ///
    /// **Errors:**
    /// - `SHELL_NOT_FOUND`: no active shell with the given ID.
    /// - `WRITE_FAILED`: the background shell writer task closed.
    #[tool(
        description = "Send raw input bytes to a shell by SHELL_ID. Prefer ssh_shell_send_key for named keystrokes (ctrl_c, arrow_up, f-keys); use ssh_shell_write for plain text or non-standard escape sequences. Append \\n for Enter, \\x03 for Ctrl+C, etc. Prefer resources/subscribe shell://<id>/output (realtime push) over polling. Errors: SHELL_NOT_FOUND, WRITE_FAILED."
    )]
    async fn ssh_shell_write(
        &self,
        Parameters(args): Parameters<SshShellWriteArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_shell_write_impl(args).await
    }

    /// Send a named keystroke to an interactive shell.
    ///
    /// Convenience wrapper over `ssh_shell_write` that maps semantic key
    /// names (e.g. `ctrl_c`, `arrow_up`, `f5`) to xterm-compatible byte
    /// sequences. Use this instead of crafting escape sequences manually.
    ///
    /// **When to use:**
    /// - Interrupt running commands (`ctrl_c`, `ctrl_z`).
    /// - Navigate shell history (`arrow_up`, `arrow_down`).
    /// - Edit input lines (`ctrl_a`, `ctrl_e`, `ctrl_u`, `ctrl_w`, `home`, `end`).
    /// - Send function keys to TUIs (`f1..f12`).
    /// - Back-tab in completion menus (`shift+tab`).
    ///
    /// **Important identifiers in response:**
    /// - `SHELL_ID`: echoed back for traceability.
    ///
    /// **Prefer `resources/subscribe shell://<id>/output` (realtime push) over polling** to observe the shell after sending the key.
    ///
    /// **Modifier rules:**
    /// - Allowed on arrows, navigation keys, F1-F12 (any of `shift`, `alt`, `ctrl`).
    /// - `tab` accepts `shift` only (produces back-tab `\x1b[Z`).
    /// - Rejected on `ctrl_*`, `enter`, `escape`, `backspace`, `space`.
    ///
    /// **Limits:**
    /// - `repeat` range 1..=64.
    ///
    /// **Errors:**
    /// - `SHELL_NOT_FOUND`: no active shell with the given ID.
    /// - `MODIFIER_NOT_ALLOWED`: modifier rejected for the given key.
    /// - `INVALID_REPEAT`: repeat outside 1..=64.
    /// - `WRITE_FAILED`: the background shell writer task closed.
    #[tool(
        description = "Send a named keystroke (ctrl_c, arrow_up, f5, etc.) to a shell by SHELL_ID. Modifier rules: arrows/nav/F-keys accept shift+alt+ctrl; tab accepts shift only (back-tab); ctrl_*/enter/escape/backspace/space reject modifiers. Repeat 1..=64. Prefer resources/subscribe shell://<id>/output (realtime push) over polling. Errors: SHELL_NOT_FOUND, MODIFIER_NOT_ALLOWED, INVALID_REPEAT, WRITE_FAILED."
    )]
    async fn ssh_shell_send_key(
        &self,
        Parameters(args): Parameters<SshShellSendKeyArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_shell_send_key_impl(args).await
    }

    /// Read accumulated output from an interactive shell.
    ///
    /// **When to use:**
    /// - Snapshot read: pull whatever has been buffered so far.
    /// - FALLBACK long-poll (`wait=true`): block until at least `min_bytes`
    ///   of new output arrive, the shell closes, or `wait_timeout_secs`
    ///   expires.
    ///
    /// **Important identifiers in response:**
    /// - `SHELL_ID`: echoed back for traceability.
    ///
    /// **Prefer `resources/subscribe shell://<id>/output` (realtime push) over polling.** The long-poll mode here is a fallback for clients that cannot subscribe.
    ///
    /// **Limits:**
    /// - `max_output_bytes` default 16384, cap 1048576 (tail rendered).
    /// - `wait_timeout_secs` default 30, cap 300.
    /// - `min_bytes` floor 1, capped at resolved `max_output_bytes`.
    ///
    /// **Status values:**
    /// - `OPEN`: shell still alive; data block carries the rendered bytes.
    /// - `CLOSED`: background reader ended; remaining buffer is returned.
    /// - `TIMEOUT`: long-poll deadline expired (only when `wait=true`).
    ///
    /// **Errors:**
    /// - `SHELL_NOT_FOUND`: no active shell with the given ID.
    #[tool(
        description = "Read buffered output from a shell by SHELL_ID. clear=true (default) drains shown bytes (head pagination). max_output_bytes default 16384, cap 1048576. Optional FALLBACK long-poll via wait=true (default 30s, cap 300s; min_bytes floor 1). Prefer resources/subscribe shell://<id>/output (realtime push) over polling. Status: OPEN, CLOSED, TIMEOUT. Errors: SHELL_NOT_FOUND."
    )]
    async fn ssh_shell_read(
        &self,
        Parameters(args): Parameters<SshShellReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_shell_read_impl(args).await
    }

    /// Wait for one of up to 16 substring patterns to appear in shell output.
    ///
    /// **When to use:**
    /// - Gate on prompt patterns (`$ `, `# `, `password:`).
    /// - Branch login flows: pass `["password:", "Permission denied", "$ "]`
    ///   and inspect `MATCHED_PATTERN` to choose the next action.
    /// - Multi-step workflows where each step waits for a completion marker.
    ///
    /// **Important identifiers in response:**
    /// - `SHELL_ID`: echoed back for traceability.
    /// - `MATCHED_PATTERN`: present only on MATCHED — the pattern that hit.
    ///
    /// **Prefer `resources/subscribe shell://<id>/output` (realtime push) over polling** for continuous observation; use this for single-shot prompt gating only.
    ///
    /// **Limits:**
    /// - 1..=16 patterns; each pattern up to 1024 bytes.
    /// - `timeout_secs` default 30, cap 300.
    /// - `max_output_bytes` default 16384, cap 1048576.
    ///
    /// **Status values:**
    /// - `MATCHED`: pattern found; matched prefix drained when `clear=true`.
    /// - `TIMEOUT`: deadline expired without any match; current buffer returned.
    /// - `CLOSED`: shell closed during the wait.
    ///
    /// **Errors:**
    /// - `SHELL_NOT_FOUND`: no active shell with the given ID.
    /// - `EMPTY_PATTERNS`: no patterns supplied.
    /// - `TOO_MANY_PATTERNS`: more than 16 patterns supplied.
    /// - `PATTERN_TOO_LONG`: a pattern exceeded 1024 bytes.
    #[tool(
        description = "Wait for one of up to 16 substring patterns in shell output by SHELL_ID. timeout_secs default 30, cap 300. Each pattern <=1024 bytes. Status: MATCHED (with MATCHED_PATTERN), TIMEOUT (current buffer), CLOSED. Prefer resources/subscribe shell://<id>/output (realtime push) over polling — use this for single-shot prompt gating. Errors: SHELL_NOT_FOUND, EMPTY_PATTERNS, TOO_MANY_PATTERNS, PATTERN_TOO_LONG."
    )]
    async fn ssh_shell_wait_for(
        &self,
        Parameters(args): Parameters<SshShellWaitForArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_shell_wait_for_impl(args).await
    }

    /// Close an interactive shell and release its PTY channel.
    ///
    /// **When to use:**
    /// - The workflow is complete and you want to release the PTY channel.
    ///
    /// **Important identifiers in response:**
    /// - `SHELL_ID`: echoed back for traceability.
    ///
    /// **Prefer `resources/subscribe shell://<id>/output` (realtime push) over polling** while the shell is still alive; this tool finalises the lifecycle.
    ///
    /// **Workflow:** stops the background reader, sends a Close request to
    /// the writer task, and removes the shell from storage. Active
    /// subscribers receive a final closed event.
    ///
    /// **Errors:**
    /// - `SHELL_NOT_FOUND`: no active shell with the given ID.
    #[tool(
        description = "Close an interactive shell by SHELL_ID. Stops the background reader and closes the PTY channel. Prefer resources/subscribe shell://<id>/output (realtime push) over polling while the shell is alive. Errors: SHELL_NOT_FOUND."
    )]
    async fn ssh_shell_close(
        &self,
        Parameters(args): Parameters<SshShellCloseArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_shell_close_impl(args).await
    }

    /// Upload a local file to a remote path via SFTP.
    ///
    /// **When to use:**
    /// - Push a file to the remote host. Streams in 32 KiB chunks for bounded memory.
    ///
    /// **Important identifiers in response:**
    /// - `TRANSFER_ID`: pass to ssh_get_transfer_progress.
    /// - `SESSION_ID`: echoed back for traceability.
    ///
    /// **Limits:** up to 10 concurrent transfers per session.
    ///
    /// **Errors:**
    /// - `SESSION_NOT_FOUND`: no session with the given ID.
    /// - `MAX_TRANSFERS_EXCEEDED`: per-session transfer cap reached.
    /// - `LOCAL_FILE_ERROR`: cannot stat the local file.
    /// - `LOCAL_NOT_FILE`: local path is not a regular file.
    #[tool(
        description = "Upload a local file to remote_path via SFTP. Returns TRANSFER_ID immediately; poll with ssh_get_transfer_progress. Up to 10 transfers per session. Errors: SESSION_NOT_FOUND, MAX_TRANSFERS_EXCEEDED, LOCAL_FILE_ERROR, LOCAL_NOT_FILE."
    )]
    async fn ssh_upload(
        &self,
        Parameters(args): Parameters<SshUploadArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_upload_impl(args).await
    }

    /// Download a remote file to a local path via SFTP.
    ///
    /// **When to use:**
    /// - Pull a file from the remote host. Streams in 32 KiB chunks for bounded memory.
    ///
    /// **Important identifiers in response:**
    /// - `TRANSFER_ID`: pass to ssh_get_transfer_progress.
    /// - `SESSION_ID`: echoed back for traceability.
    ///
    /// **Limits:** up to 10 concurrent transfers per session.
    ///
    /// **Errors:**
    /// - `SESSION_NOT_FOUND`: no session with the given ID.
    /// - `MAX_TRANSFERS_EXCEEDED`: per-session transfer cap reached.
    /// - `SFTP_OPEN_FAILED`: failed to open the SFTP subsystem.
    /// - `REMOTE_METADATA_ERROR`: failed to stat the remote file.
    #[tool(
        description = "Download a remote file from remote_path to local_path via SFTP. Returns TRANSFER_ID immediately; poll with ssh_get_transfer_progress. Up to 10 transfers per session. Errors: SESSION_NOT_FOUND, MAX_TRANSFERS_EXCEEDED, SFTP_OPEN_FAILED, REMOTE_METADATA_ERROR."
    )]
    async fn ssh_download(
        &self,
        Parameters(args): Parameters<SshDownloadArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_download_impl(args).await
    }

    /// Read the current progress of an SFTP transfer.
    ///
    /// **When to use:**
    /// - Monitor an upload/download by polling (`wait=false`).
    /// - Block until termination (`wait=true`).
    ///
    /// **Important identifiers in response:**
    /// - `TRANSFER_ID`: echoed back for traceability.
    ///
    /// **Limits:** `wait_timeout_secs` default 30, cap 300. Terminated
    /// transfers are cleaned from storage after `SSH_TRANSFER_CLEANUP_TTL`
    /// (default 300s).
    ///
    /// **Status values:**
    /// - `RUNNING`: transfer in progress; `PROGRESS` line shows percent.
    /// - `COMPLETED`: 100% transferred.
    /// - `FAILED`: terminated with an error; `REASON` line carries detail.
    ///
    /// **Errors:**
    /// - `TRANSFER_NOT_FOUND`: no transfer with the given ID (or already cleaned up).
    #[tool(
        description = "Read SFTP transfer progress by TRANSFER_ID. Set wait=true to block until termination (default 30s, cap 300s). Status: RUNNING, COMPLETED, FAILED. Terminated transfers cleaned after SSH_TRANSFER_CLEANUP_TTL (default 300s). Errors: TRANSFER_NOT_FOUND."
    )]
    async fn ssh_get_transfer_progress(
        &self,
        Parameters(args): Parameters<SshGetTransferProgressArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_get_transfer_progress_impl(args).await
    }

    /// Set up local-to-remote TCP port forwarding through an SSH session.
    ///
    /// **When to use:**
    /// - Expose a remote port (e.g. internal database) on a local port
    ///   (e.g. 8080).
    ///
    /// **Important identifiers in response:**
    /// - `LOCAL`: local listening address (`host:port`).
    /// - `REMOTE`: remote target (`host:port`).
    ///
    /// **Limits:** available only with the `port_forward` Cargo feature
    /// (default-on).
    ///
    /// **Errors:**
    /// - `SESSION_NOT_FOUND`: no session with the given ID.
    /// - `FORWARD_FAILED`: listener bind or russh stream-open failed.
    /// - `FEATURE_DISABLED`: build was compiled without `port_forward`.
    #[tool(
        description = "Local-to-remote TCP port forwarding through an SSH session by SESSION_ID. Listens on local_port and forwards to remote_address:remote_port. Feature-gated (port_forward). Errors: SESSION_NOT_FOUND, FORWARD_FAILED, FEATURE_DISABLED."
    )]
    async fn ssh_forward(
        &self,
        Parameters(args): Parameters<SshForwardArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_forward_impl(args).await
    }
}

#[tool_handler]
impl ServerHandler for McpSshServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::V_2025_06_18;
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .enable_resources()
            .enable_resources_subscribe()
            .enable_resources_list_changed()
            .build();
        let mut implementation = Implementation::default();
        implementation.name = "ssh-mcp".to_string();
        implementation.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info = implementation;
        info.instructions = Some(
            "SSH MCP server — 18 SSH tools and 5 resource subscribe schemes \
             (shell://, command://, transfer://, session://, forward://). \
             Prefer `resources/subscribe shell://<shell_id>/output` for realtime \
             PTY streams over polling-based ssh_shell_read; the long-poll variants \
             (ssh_shell_read.wait, ssh_shell_wait_for) are FALLBACKS for clients \
             that cannot subscribe. Use resources/list to enumerate active shells, \
             commands, transfers, and sessions."
                .to_string(),
        );
        info
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(list_resources_impl())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        read_resource_impl(request, &context.peer).await
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        subscribe_impl(request, context.peer).await
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let peer_id = peer_id_for(&context.peer);
        unsubscribe_impl(request, &peer_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_info_advertises_subscribe_capability() {
        let info = McpSshServer::new().get_info();
        assert!(
            info.capabilities
                .resources
                .as_ref()
                .and_then(|r| r.subscribe)
                .unwrap_or(false),
            "resources.subscribe must be advertised as true"
        );
    }

    #[test]
    fn server_info_advertises_resources_list_changed() {
        let info = McpSshServer::new().get_info();
        assert!(
            info.capabilities
                .resources
                .as_ref()
                .and_then(|r| r.list_changed)
                .unwrap_or(false),
            "resources.list_changed must be advertised as true"
        );
    }

    #[test]
    fn server_info_advertises_tool_list_changed() {
        let info = McpSshServer::new().get_info();
        assert!(
            info.capabilities
                .tools
                .as_ref()
                .and_then(|t| t.list_changed)
                .unwrap_or(false),
            "tools.list_changed must be advertised as true"
        );
    }

    #[test]
    fn server_info_protocol_version_is_2025_06_18() {
        let info = McpSshServer::new().get_info();
        assert_eq!(info.protocol_version, ProtocolVersion::V_2025_06_18);
    }

    #[test]
    fn instructions_mention_resources_subscribe_and_shell_scheme() {
        let info = McpSshServer::new().get_info();
        let instructions = info
            .instructions
            .as_deref()
            .expect("get_info() must populate instructions");
        assert!(
            instructions.contains("subscribe"),
            "instructions must mention `subscribe`: {instructions}"
        );
        assert!(
            instructions.contains("shell://"),
            "instructions must reference the `shell://` scheme: {instructions}"
        );
    }

    #[test]
    fn tool_router_advertises_eighteen_tools() {
        let server = McpSshServer::new();
        // 18 = 4 connection + 4 commands + 6 shell + 3 sftp + 1 forward
        // (the forward tool is feature-gated; it ships under the default
        // feature set used by `cargo test`).
        let tools = server.tool_router.list_all();
        assert_eq!(
            tools.len(),
            18,
            "tool_router must advertise 18 tools, found {}",
            tools.len()
        );
    }
}
