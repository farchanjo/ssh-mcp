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
    CallToolResult, ErrorData as McpError, Implementation, ProtocolVersion, ServerCapabilities,
    ServerInfo,
};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};

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
    SshShellCloseArgs, SshShellOpenArgs, SshShellReadArgs, SshShellWriteArgs, ssh_shell_close_impl,
    ssh_shell_open_impl, ssh_shell_read_impl, ssh_shell_write_impl,
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
    /// - Establishing a new SSH connection to run commands, open shells, or transfer files.
    /// - Reusing an already-connected session by passing its `session_id`.
    ///
    /// **Important identifiers in response:**
    /// - `SESSION_ID`: passed to ssh_execute, ssh_shell_open, ssh_upload, ssh_download,
    ///   ssh_disconnect, ssh_forward.
    /// - `AGENT_ID`: optional grouping; passed to ssh_list_sessions (filter) and
    ///   ssh_disconnect_agent (cleanup).
    ///
    /// **Workflow:**
    /// 1. Call ssh_connect once per remote host.
    /// 2. Use the returned SESSION_ID for subsequent tool calls.
    /// 3. Call ssh_disconnect (or ssh_disconnect_agent) when done.
    ///
    /// **Status values:** OK, REUSED, SUGGESTED.
    ///
    /// **Errors:** CONNECTION_FAILED.
    #[tool(
        description = "Connect to an SSH server and store the session. Returns SESSION_ID and optional AGENT_ID. Status values: OK, REUSED, SUGGESTED. Use SESSION_ID with ssh_execute, ssh_shell_open, ssh_upload, ssh_download, ssh_disconnect, ssh_forward."
    )]
    async fn ssh_connect(
        &self,
        Parameters(args): Parameters<SshConnectArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_connect_impl(args).await
    }

    /// Disconnect a single SSH session and free its resources.
    ///
    /// **When to use:** done with a session and want a clean teardown.
    ///
    /// **Workflow:** automatically cancels every running async command,
    /// closes every interactive shell, and aborts every in-flight SFTP
    /// transfer for the session before disconnecting the SSH transport.
    ///
    /// **Errors:** SESSION_NOT_FOUND.
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
    /// **When to use:** discover available SESSION_IDs or audit which agents
    /// own which sessions.
    ///
    /// **Workflow:** runs an `echo 1` health probe against each session and
    /// removes any that fail before returning. Pass `agent_id` to filter to
    /// a single agent.
    #[tool(
        description = "List active SSH sessions with health-check metadata. Optional agent_id filter. Returns SESSION_ID, host, username, agent_id, healthy, last_health_check."
    )]
    async fn ssh_list_sessions(
        &self,
        Parameters(args): Parameters<SshListSessionsArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_list_sessions_impl(args).await
    }

    /// Bulk-disconnect every session owned by an agent.
    ///
    /// **When to use:** at agent shutdown to release every resource the
    /// agent allocated. Sessions owned by other agents are not affected.
    ///
    /// **Workflow:** cancels commands, closes shells, aborts transfers, and
    /// disconnects each session that belongs to `agent_id`.
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
    /// **When to use:** any command — short or long-running. Returns
    /// immediately with a COMMAND_ID for polling.
    ///
    /// **Important identifiers in response:**
    /// - `COMMAND_ID`: passed to ssh_get_command_output and ssh_cancel_command.
    ///
    /// **Workflow:**
    /// 1. ssh_execute → COMMAND_ID.
    /// 2. ssh_get_command_output(command_id, wait=true) → final result.
    ///
    /// **Limits:** up to 100 concurrent multiplexed commands per session.
    ///
    /// **Errors:** SESSION_NOT_FOUND, MAX_COMMANDS_EXCEEDED.
    #[tool(
        description = "Execute a command asynchronously. Returns COMMAND_ID immediately. Pass pty=true for commands needing a TTY (sudo, top). Poll with ssh_get_command_output. Errors: SESSION_NOT_FOUND, MAX_COMMANDS_EXCEEDED."
    )]
    async fn ssh_execute(
        &self,
        Parameters(args): Parameters<SshExecuteArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_execute_impl(args).await
    }

    /// Read the current output and status of an async command.
    ///
    /// **When to use:** poll for progress (`wait=false`) or block until
    /// completion (`wait=true`).
    ///
    /// **Status values:** running, completed, cancelled, failed, timeout.
    ///
    /// **Errors:** COMMAND_NOT_FOUND, COMMAND_FAILED.
    #[tool(
        description = "Read output and status of an async command by COMMAND_ID. Set wait=true to block until completion (cap 300s). Status: running, completed, cancelled, failed, timeout. Errors: COMMAND_NOT_FOUND."
    )]
    async fn ssh_get_command_output(
        &self,
        Parameters(args): Parameters<SshGetCommandOutputArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_get_command_output_impl(args).await
    }

    /// List async commands across one or all sessions.
    ///
    /// **When to use:** monitor multiple concurrent operations or check
    /// what is still running before disconnecting a session.
    ///
    /// **Filters:** optional `session_id` and/or `status` (running,
    /// completed, cancelled, failed).
    #[tool(
        description = "List async commands. Optional filters: session_id, status (running|completed|cancelled|failed). Returns COMMAND_IDs and metadata."
    )]
    async fn ssh_list_commands(
        &self,
        Parameters(args): Parameters<SshListCommandsArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_list_commands_impl(args).await
    }

    /// Cancel a running async command.
    ///
    /// **When to use:** stop a long-running command that is no longer
    /// needed, or abort one taking too long.
    ///
    /// **Notes:** only running commands can be cancelled. Returns the
    /// output collected so far.
    ///
    /// **Errors:** COMMAND_NOT_FOUND. Already-finished commands return a
    /// no-op success response.
    #[tool(
        description = "Cancel a running async command by COMMAND_ID. Returns partial stdout/stderr collected before cancellation. Errors: COMMAND_NOT_FOUND. Already-finished commands return a no-op success."
    )]
    async fn ssh_cancel_command(
        &self,
        Parameters(args): Parameters<SshCancelCommandArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_cancel_command_impl(args).await
    }

    /// Open an interactive PTY shell on a session.
    ///
    /// **When to use:** Serial Over LAN (SOL/IPMI/OOB), multi-step
    /// workflows that need persistent shell state, or commands requiring
    /// terminal interaction. For SOL/IPMI use `term="vt100"` with 80×24.
    ///
    /// **Important identifiers in response:**
    /// - `SHELL_ID`: passed to ssh_shell_write, ssh_shell_read, ssh_shell_close.
    ///
    /// **Limits:** up to 10 shells per session.
    ///
    /// **Errors:** SESSION_NOT_FOUND, MAX_SHELLS_EXCEEDED, CHANNEL_FAILED.
    #[tool(
        description = "Open an interactive PTY shell. Returns SHELL_ID. Defaults: term=xterm 80x24. For SOL/IPMI use term=vt100. Errors: SESSION_NOT_FOUND, MAX_SHELLS_EXCEEDED, CHANNEL_FAILED."
    )]
    async fn ssh_shell_open(
        &self,
        Parameters(args): Parameters<SshShellOpenArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_shell_open_impl(args).await
    }

    /// Send raw input to an interactive shell.
    ///
    /// **When to use:** type a command (append `\n` for Enter), send a
    /// control character (`\x03` = Ctrl+C, `\x04` = Ctrl+D), or send an
    /// escape sequence (`\x1b[A` = arrow up).
    ///
    /// **Errors:** SHELL_NOT_FOUND, WRITE_FAILED.
    #[tool(
        description = "Send raw input bytes to a shell by SHELL_ID. Append \\n for Enter, send \\x03 for Ctrl+C, etc. Errors: SHELL_NOT_FOUND, WRITE_FAILED."
    )]
    async fn ssh_shell_write(
        &self,
        Parameters(args): Parameters<SshShellWriteArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_shell_write_impl(args).await
    }

    /// Read accumulated output from an interactive shell.
    ///
    /// **When to use:** after writing input, give the shell a beat, then
    /// call this to retrieve the new output.
    ///
    /// **Notes:** with `clear=true` (default) only the rendered bytes are
    /// drained (head-based pagination).
    ///
    /// **Status values:** open, closed.
    ///
    /// **Errors:** SHELL_NOT_FOUND.
    #[tool(
        description = "Read buffered output from a shell by SHELL_ID. clear=true (default) drains shown bytes (head pagination). max_output_bytes default 16384, cap 1048576. Status: open, closed. Errors: SHELL_NOT_FOUND."
    )]
    async fn ssh_shell_read(
        &self,
        Parameters(args): Parameters<SshShellReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_shell_read_impl(args).await
    }

    /// Close an interactive shell.
    ///
    /// **When to use:** the workflow is complete and you want to release
    /// the PTY channel.
    ///
    /// **Errors:** SHELL_NOT_FOUND.
    #[tool(
        description = "Close an interactive shell by SHELL_ID. Stops the background reader and closes the PTY channel. Errors: SHELL_NOT_FOUND."
    )]
    async fn ssh_shell_close(
        &self,
        Parameters(args): Parameters<SshShellCloseArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_shell_close_impl(args).await
    }

    /// Upload a local file to a remote path via SFTP.
    ///
    /// **When to use:** push a file to the remote host. Streams in 32 KiB
    /// chunks for bounded memory.
    ///
    /// **Important identifiers in response:**
    /// - `TRANSFER_ID`: passed to ssh_get_transfer_progress.
    ///
    /// **Limits:** up to 10 transfers per session.
    ///
    /// **Errors:** SESSION_NOT_FOUND, MAX_TRANSFERS_EXCEEDED,
    /// LOCAL_FILE_ERROR, LOCAL_NOT_FILE.
    #[tool(
        description = "Upload a local file to remote_path via SFTP. Returns TRANSFER_ID immediately; poll with ssh_get_transfer_progress. Errors: SESSION_NOT_FOUND, MAX_TRANSFERS_EXCEEDED, LOCAL_FILE_ERROR, LOCAL_NOT_FILE."
    )]
    async fn ssh_upload(
        &self,
        Parameters(args): Parameters<SshUploadArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_upload_impl(args).await
    }

    /// Download a remote file to a local path via SFTP.
    ///
    /// **When to use:** pull a file from the remote host. Streams in 32 KiB
    /// chunks for bounded memory.
    ///
    /// **Important identifiers in response:**
    /// - `TRANSFER_ID`: passed to ssh_get_transfer_progress.
    ///
    /// **Limits:** up to 10 transfers per session.
    ///
    /// **Errors:** SESSION_NOT_FOUND, MAX_TRANSFERS_EXCEEDED,
    /// SFTP_OPEN_FAILED, REMOTE_METADATA_ERROR.
    #[tool(
        description = "Download a remote file from remote_path to local_path via SFTP. Returns TRANSFER_ID immediately; poll with ssh_get_transfer_progress. Errors: SESSION_NOT_FOUND, MAX_TRANSFERS_EXCEEDED, SFTP_OPEN_FAILED, REMOTE_METADATA_ERROR."
    )]
    async fn ssh_download(
        &self,
        Parameters(args): Parameters<SshDownloadArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_download_impl(args).await
    }

    /// Read the current progress of an SFTP transfer.
    ///
    /// **When to use:** monitor an upload/download by polling
    /// (`wait=false`) or block until completion (`wait=true`).
    ///
    /// **Status values:** running, completed, failed, cancelled.
    ///
    /// **Errors:** TRANSFER_NOT_FOUND.
    #[tool(
        description = "Read SFTP transfer progress by TRANSFER_ID. Set wait=true to block until termination (cap 300s). Status: running, completed, failed, cancelled. Errors: TRANSFER_NOT_FOUND."
    )]
    async fn ssh_get_transfer_progress(
        &self,
        Parameters(args): Parameters<SshGetTransferProgressArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_get_transfer_progress_impl(args).await
    }

    /// Set up local-to-remote TCP port forwarding through an SSH session.
    ///
    /// **When to use:** expose a remote port (e.g. internal database) on a
    /// local port (e.g. 8080). Available only with the `port_forward`
    /// Cargo feature (default).
    ///
    /// **Errors:** SESSION_NOT_FOUND, FORWARD_FAILED, FEATURE_DISABLED.
    #[tool(
        description = "Local-to-remote TCP port forwarding through an SSH session. Listens on local_port and forwards to remote_address:remote_port. Errors: SESSION_NOT_FOUND, FORWARD_FAILED, FEATURE_DISABLED."
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
            "SSH MCP server — 16 SSH tools and 5 resource subscribe schemes \
             (shell://, command://, transfer://, session://, forward://). \
             Prefer resources/subscribe + resources/read for realtime output streams \
             over polling-based ssh_shell_read."
                .to_string(),
        );
        info
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
}
