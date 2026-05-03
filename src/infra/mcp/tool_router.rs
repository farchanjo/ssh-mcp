//! `#[tool_router]` impl + `ServerHandler` impl for [`super::server::McpSshServer`].
//!
//! 18 tool fns + 4 resource methods all delegate to the use case container
//! sitting on [`super::server::McpSshServer::use_cases`]. H17 wires the
//! v3-equivalent block-style markdown bodies through the
//! [`super::render`] helpers; the v4 path is now end-to-end functional.
//!
//! ## Generic shape
//!
//! The impl block specialises on the production-shaped
//! [`crate::composition::UseCases`] type. Every generic adapter
//! parameter is forwarded as-is; the test harness (H18) builds a
//! `UseCases<Fakes...>` instance with the exact same shape.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ListResourcesResult, PaginatedRequestParams,
    ProtocolVersion, ReadResourceRequestParams, ReadResourceResult, ServerCapabilities, ServerInfo,
    SubscribeRequestParams, UnsubscribeRequestParams,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, tool, tool_handler, tool_router};

use crate::application::cancel_command::CancelCommandRequest;
use crate::application::close_shell::CloseShellRequest;
use crate::application::connect_session::{ConnectRequest, ConnectSessionUseCase};
use crate::application::disconnect_agent::DisconnectAgentRequest;
use crate::application::disconnect_session::DisconnectRequest;
use crate::application::download_file::DownloadRequest;
use crate::application::execute_command::ExecuteRequest;
#[cfg(feature = "port_forward")]
use crate::application::forward_port::ForwardPortRequest;
use crate::application::get_command_output::GetCommandOutputRequest;
use crate::application::get_transfer_progress::GetTransferProgressRequest;
use crate::application::list_commands::ListCommandsRequest;
use crate::application::list_sessions::ListSessionsRequest;
use crate::application::open_shell::OpenShellRequest;
use crate::application::read_shell::ReadShellRequest;
use crate::application::send_key::SendKeyRequest;
use crate::application::upload_file::UploadRequest;
use crate::application::wait_for_pattern::WaitForPatternRequest;
use crate::application::write_shell::WriteShellRequest;
use crate::composition::UseCases;
use crate::domain::error::DomainError;
use crate::domain::identity::{Address, Credentials};
use crate::domain::ids::{AgentId, CommandId, SessionId, ShellId, TransferId};
use crate::domain::policy::ReusePolicy as DomainReusePolicy;
use crate::infra::mcp::helpers::error::format_error;
use crate::infra::mcp::render;
use crate::mcp::keys::KeyModifiers;
use crate::ports::auth_strategy::AuthStrategyPort;
use crate::ports::clock::ClockPort;
use crate::ports::command_repo::CommandRepository;
use crate::ports::config::ConfigPort;
#[cfg(feature = "port_forward")]
use crate::ports::forward_repo::ForwardRepository;
use crate::ports::id_generator::IdGeneratorPort;
use crate::ports::notifier::NotifierPort;
use crate::ports::output_stream::OutputStreamPort;
use crate::ports::session_repo::SessionRepository;
use crate::ports::sftp_client::SftpClientPort;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::ssh_client::SshClientPort;
use crate::ports::subscriber_registry::{SubscriberRegistryAsync, SubscriberRegistryPort};
use crate::ports::transfer_repo::TransferRepository;

use super::args::connection::{
    SshConnectArgs, SshDisconnectAgentArgs, SshDisconnectArgs, SshListSessionsArgs,
};
use super::args::execute::{
    SshCancelCommandArgs, SshExecuteArgs, SshGetCommandOutputArgs, SshListCommandsArgs,
};
#[cfg(feature = "port_forward")]
use super::args::forward::SshForwardArgs;
use super::args::sftp::{SshDownloadArgs, SshGetTransferProgressArgs, SshUploadArgs};
use super::args::shell::{
    SshShellCloseArgs, SshShellOpenArgs, SshShellReadArgs, SshShellSendKeyArgs,
    SshShellWaitForArgs, SshShellWriteArgs,
};
use super::peer_handle::PeerTable;
use super::resource_handlers;
use super::server::McpSshServer;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Build a `CallToolResult::error` body using the v3 standardized
/// `TOOL: ERROR / REASON: [CODE] message` format. The mapping picks a
/// stable code per [`DomainError`] variant so the LLM can branch on it.
fn render_tool_error(tool: &str, err: &DomainError) -> CallToolResult {
    let (code, reason, detail) = classify_error(err);
    let body = format_error(tool, code, &reason, detail.as_deref());
    CallToolResult::error(vec![Content::text(body)])
}

/// Pick a v3-compatible error code, reason and optional detail per
/// [`DomainError`] variant.
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive match over the 17 DomainError variants is naturally long; restructuring to a HashMap or sub-helper hurts readability"
)]
fn classify_error(err: &DomainError) -> (&'static str, String, Option<String>) {
    match err {
        DomainError::InvalidArgument(reason) => ("INVALID_ARGUMENT", reason.clone(), None),
        DomainError::SessionNotFound(id) => (
            "SESSION_NOT_FOUND",
            "no active SSH session with the given ID".to_string(),
            Some(id.as_str().to_string()),
        ),
        DomainError::ShellNotFound(id) => (
            "SHELL_NOT_FOUND",
            "no active shell with the given ID".to_string(),
            Some(id.as_str().to_string()),
        ),
        DomainError::CommandNotFound(id) => (
            "COMMAND_NOT_FOUND",
            "no async command with the given ID".to_string(),
            Some(id.as_str().to_string()),
        ),
        DomainError::TransferNotFound(id) => (
            "TRANSFER_NOT_FOUND",
            "no transfer with the given ID".to_string(),
            Some(id.as_str().to_string()),
        ),
        DomainError::ForwardNotFound(id) => (
            "FORWARD_NOT_FOUND",
            "no forwarder with the given ID".to_string(),
            Some(id.as_str().to_string()),
        ),
        DomainError::Auth(reason) => ("AUTH_FAILED", reason.to_string(), None),
        DomainError::ConnectFailed(reason) => ("CONNECTION_FAILED", reason.clone(), None),
        DomainError::Transport(reason) => ("TRANSPORT_ERROR", reason.clone(), None),
        DomainError::Timeout(reason) => ("TIMEOUT", reason.clone(), None),
        DomainError::Storage(reason) => ("STORAGE_ERROR", reason.clone(), None),
        DomainError::Sftp(reason) => ("SFTP_ERROR", reason.clone(), None),
        DomainError::PortInUse(port) => (
            "PORT_IN_USE",
            format!("local port {port} already in use"),
            Some(format!("port={port}")),
        ),
        DomainError::Internal(reason) => ("INTERNAL_ERROR", reason.clone(), None),
        DomainError::MaxCommandsExceeded { limit } => (
            "MAX_COMMANDS_EXCEEDED",
            "maximum running async commands per session reached".to_string(),
            Some(format!("limit={limit}")),
        ),
        DomainError::MaxShellsExceeded { limit } => (
            "MAX_SHELLS_EXCEEDED",
            "maximum shells per session reached".to_string(),
            Some(format!("limit={limit}")),
        ),
        DomainError::MaxTransfersExceeded { limit } => (
            "MAX_TRANSFERS_EXCEEDED",
            "maximum transfers per session reached".to_string(),
            Some(format!("limit={limit}")),
        ),
    }
}

/// Wrap a successful render body in [`CallToolResult::success`].
fn ok_text(body: String) -> CallToolResult {
    CallToolResult::success(vec![Content::text(body)])
}

/// Convert a string `host[:port]` into a domain [`Address`].
fn parse_address(input: &str) -> Result<Address, DomainError> {
    if let Some((host, port_str)) = input.rsplit_once(':') {
        let port = port_str
            .parse::<u16>()
            .map_err(|e| DomainError::InvalidArgument(format!("invalid port {port_str:?}: {e}")))?;
        Address::new(host.to_string(), port)
            .map_err(|e| DomainError::InvalidArgument(e.to_string()))
    } else {
        Address::with_default_port(input.to_string())
            .map_err(|e| DomainError::InvalidArgument(e.to_string()))
    }
}

/// Build the [`Credentials`] DTO from the rmcp connect args following the
/// v3 priority: explicit password takes precedence over key path; an
/// empty payload falls back to the agent placeholder so the auth chain
/// can wire SSH-agent lookup.
///
/// The v3 SSH adapter overloads the [`Credentials::PrivateKey::key_pem`]
/// slot to carry a *file path* (not PEM material) until the H8 auth
/// chain finishes its full bridge — see
/// `src/adapters/ssh/russh_adapter.rs::split_credentials` for the
/// matching reader side.
fn pick_credentials(username: &str, password: Option<&str>, key_path: Option<&str>) -> Credentials {
    if let Some(pw) = password.filter(|s| !s.is_empty()) {
        return Credentials::Password {
            username: username.to_string(),
            password: pw.to_string(),
        };
    }
    if let Some(path) = key_path.filter(|s| !s.is_empty()) {
        return Credentials::PrivateKey {
            username: username.to_string(),
            key_pem: path.to_string(),
            passphrase: None,
        };
    }
    Credentials::Agent {
        username: username.to_string(),
        socket: None,
    }
}

/// Build a [`KeyModifiers`] bag from the optional flags carried on the
/// rmcp `ssh_shell_send_key` payload. Mirrors the v3 fall-back-to-`false`
/// semantics.
fn pick_modifiers(shift: Option<bool>, alt: Option<bool>, ctrl: Option<bool>) -> KeyModifiers {
    KeyModifiers {
        shift: shift.unwrap_or(false),
        alt: alt.unwrap_or(false),
        ctrl: ctrl.unwrap_or(false),
    }
}

// ---------------------------------------------------------------------------
// `#[tool_router]` impl — `port_forward` enabled
// ---------------------------------------------------------------------------

#[cfg(feature = "port_forward")]
#[tool_router]
impl<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg>
    McpSshServer<UseCases<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg>>
where
    S: SshClientPort + Send + Sync + 'static,
    F: SftpClientPort + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    CR: CommandRepository + Send + Sync + 'static,
    ShR: ShellRepository + Send + Sync + 'static,
    TR: TransferRepository + Send + Sync + 'static,
    FR: ForwardRepository + Send + Sync + 'static,
    N: NotifierPort + Send + Sync + 'static,
    AS: AuthStrategyPort + Send + Sync + 'static,
    OS: OutputStreamPort + Send + Sync + 'static,
    SubR: SubscriberRegistryPort + SubscriberRegistryAsync + Send + Sync + 'static,
    C: ClockPort + Send + Sync + 'static,
    Cfg: ConfigPort + Send + Sync + 'static,
    Idg: IdGeneratorPort + Send + Sync + 'static,
{
    /// Build an [`McpSshServer`] with the provided container + peer table.
    #[must_use]
    #[allow(
        clippy::type_complexity,
        reason = "the Arc<UseCases<...>> generic surface is the natural shape of the production wiring; the prod alias `ProdUseCases` collapses it at the call site"
    )]
    pub const fn new(
        use_cases: Arc<UseCases<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg>>,
        peer_table: Arc<PeerTable>,
    ) -> Self {
        Self::from_parts(use_cases, peer_table)
    }

    // ---------- Connection domain ------------------------------------

    #[tool(
        description = "Connect to an SSH server and store the session.\n\nWhen to use:\n- Establishing a new SSH connection to run commands, open shells, or transfer files.\n- Reusing an already-connected session by passing its `session_id`.\n\nImportant identifiers in response:\n- `SESSION_ID`: passed to ssh_execute, ssh_shell_open, ssh_upload, ssh_download, ssh_disconnect, ssh_forward.\n- `AGENT_ID`: optional grouping; passed to ssh_list_sessions (filter) and ssh_disconnect_agent (cleanup).\n\nWorkflow:\n1. Call ssh_connect once per remote host.\n2. Use the returned SESSION_ID for subsequent tool calls.\n3. Call ssh_disconnect (or ssh_disconnect_agent) when done.\n\nStatus values: OK, REUSED, SUGGESTED.\n\nErrors: CONNECTION_FAILED, AUTH_FAILED."
    )]
    async fn ssh_connect(
        &self,
        Parameters(args): Parameters<SshConnectArgs>,
    ) -> Result<CallToolResult, McpError> {
        run_connect(self.use_cases.connect.as_ref(), args).await
    }

    #[tool(
        description = "Disconnect an SSH session.\n\nWhen to use:\n- Tearing down a single SSH session previously opened with ssh_connect.\n- Cancels every async command, closes every PTY, and aborts every in-flight SFTP transfer for the session.\n\nWorkflow:\n1. Pass the `session_id` returned from ssh_connect.\n2. Subsequent tool calls against that id return SESSION_NOT_FOUND.\n\nStatus values: OK.\n\nErrors: SESSION_NOT_FOUND, TRANSPORT_ERROR."
    )]
    async fn ssh_disconnect(
        &self,
        Parameters(args): Parameters<SshDisconnectArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .use_cases
            .disconnect
            .execute(DisconnectRequest {
                session_id: SessionId::new(args.session_id),
            })
            .await
        {
            Ok(outcome) => Ok(ok_text(render::connection::disconnect_render(&outcome))),
            Err(err) => Ok(render_tool_error("SSH_DISCONNECT", &err)),
        }
    }

    #[tool(
        description = "List active SSH sessions on the server.\n\nWhen to use:\n- Inspecting sessions known to this server (optionally narrowed to one agent).\n\nWorkflow:\n1. Optional `agent_id` filter to scope the list to sessions tagged with that AGENT_ID.\n2. Optional `max_items` cap (default 500, env `SSH_MCP_LIST_MAX_ITEMS`).\n\nStatus values: OK.\n\nErrors: STORAGE_ERROR."
    )]
    async fn ssh_list_sessions(
        &self,
        Parameters(args): Parameters<SshListSessionsArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .use_cases
            .list_sessions
            .execute(ListSessionsRequest {
                filter_agent_id: args.agent_id.map(AgentId::new),
                max_items: args.max_items,
            })
            .await
        {
            Ok(outcome) => Ok(ok_text(render::connection::list_sessions_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_LIST_SESSIONS", &err)),
        }
    }

    #[tool(
        description = "Disconnect every session bound to a given agent.\n\nWhen to use:\n- Bulk-cleanup of every SSH session tagged with a given AGENT_ID.\n- Cancels async commands, closes shells, and aborts transfers per disconnected session.\n\nWorkflow:\n1. Pass the AGENT_ID returned from a previous ssh_connect.\n2. Sessions owned by other agents are not affected.\n\nStatus values: OK.\n\nErrors: STORAGE_ERROR."
    )]
    async fn ssh_disconnect_agent(
        &self,
        Parameters(args): Parameters<SshDisconnectAgentArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .use_cases
            .disconnect_agent
            .execute(DisconnectAgentRequest {
                agent_id: AgentId::new(args.agent_id),
            })
            .await
        {
            Ok(outcome) => Ok(ok_text(render::connection::disconnect_agent_render(
                &outcome,
            ))),
            Err(err) => Ok(render_tool_error("SSH_DISCONNECT_AGENT", &err)),
        }
    }

    // ---------- Execute domain ---------------------------------------

    #[tool(
        description = "Spawn an asynchronous command on an SSH session.\n\nWhen to use:\n- Starting a command and polling its output via ssh_get_command_output.\n- Set `pty=true` for commands requiring a controlling terminal (e.g. sudo).\n\nImportant identifiers in response:\n- `COMMAND_ID`: passed to ssh_get_command_output, ssh_cancel_command.\n\nWorkflow:\n1. Call ssh_execute with the SESSION_ID and command line.\n2. Use ssh_get_command_output to fetch progress / completion.\n3. Optional ssh_cancel_command to interrupt.\n\nStatus values: STARTED.\n\nErrors: SESSION_NOT_FOUND, MAX_COMMANDS_EXCEEDED, TRANSPORT_ERROR."
    )]
    async fn ssh_execute(
        &self,
        Parameters(args): Parameters<SshExecuteArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = ExecuteRequest {
            session_id: SessionId::new(args.session_id),
            command: args.command,
            timeout: args.timeout_secs.map(Duration::from_secs),
            use_pty: args.pty.unwrap_or(false),
        };
        match self.use_cases.execute.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::execute::execute_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_EXECUTE", &err)),
        }
    }

    #[tool(
        description = "Fetch the current output of an asynchronous command.\n\nWhen to use:\n- Polling stdout/stderr for a command spawned with ssh_execute.\n- Optionally blocking until the command completes (`wait=true`).\n\nWorkflow:\n1. Pass the COMMAND_ID returned from ssh_execute.\n2. Set `wait=true` to block; capped at `wait_timeout_secs` (default 30, max 300).\n3. `max_output_bytes` head-truncates very large outputs (default 16384).\n\nStatus values: RUNNING, COMPLETED, TIMEOUT, CANCELLED, FAILED.\n\nErrors: COMMAND_NOT_FOUND."
    )]
    async fn ssh_get_command_output(
        &self,
        Parameters(args): Parameters<SshGetCommandOutputArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = GetCommandOutputRequest {
            command_id: CommandId::new(args.command_id),
            wait: args.wait.unwrap_or(false),
            wait_timeout: args.wait_timeout_secs.map(Duration::from_secs),
            max_output_bytes: args.max_output_bytes,
        };
        match self.use_cases.get_command_output.execute(req).await {
            Ok(result) => Ok(ok_text(render::execute::get_command_output_render(result))),
            Err(err) => Ok(render_tool_error("SSH_GET_COMMAND_OUTPUT", &err)),
        }
    }

    #[tool(
        description = "List asynchronous commands tracked on the server.\n\nWhen to use:\n- Inspecting every command (optionally filtered by session and/or status).\n\nWorkflow:\n1. Optional `session_id` to narrow to one session.\n2. Optional `status` filter (`running`, `completed`, `cancelled`, `failed`).\n3. Optional `max_items` cap (default 500).\n\nStatus values: OK.\n\nErrors: STORAGE_ERROR."
    )]
    async fn ssh_list_commands(
        &self,
        Parameters(args): Parameters<SshListCommandsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = ListCommandsRequest {
            filter_session_id: args.session_id.map(SessionId::new),
            filter_status: args
                .status
                .map(super::args::execute::CommandStatus::into_domain),
            max_items: args.max_items,
        };
        match self.use_cases.list_commands.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::execute::list_commands_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_LIST_COMMANDS", &err)),
        }
    }

    #[tool(
        description = "Cancel an asynchronous command.\n\nWhen to use:\n- Interrupting a long-running command spawned with ssh_execute.\n- Returns the partial stdout/stderr captured so far when the command was running.\n\nStatus values: CANCELLED, NOOP.\n\nErrors: COMMAND_NOT_FOUND."
    )]
    async fn ssh_cancel_command(
        &self,
        Parameters(args): Parameters<SshCancelCommandArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = CancelCommandRequest {
            command_id: CommandId::new(args.command_id),
            max_output_bytes: args.max_output_bytes,
        };
        match self.use_cases.cancel_command.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::execute::cancel_command_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_CANCEL_COMMAND", &err)),
        }
    }

    // ---------- Shell domain -----------------------------------------

    #[tool(
        description = "Open an interactive PTY shell on an SSH session.\n\nWhen to use:\n- Driving an interactive program (vim, htop, REPL, sudo prompt) that needs a TTY.\n- Prefer subscribing to `shell://<shell_id>/output` over polling ssh_shell_read.\n\nImportant identifiers in response:\n- `SHELL_ID`: passed to ssh_shell_write, ssh_shell_send_key, ssh_shell_read, ssh_shell_wait_for, ssh_shell_close.\n\nStatus values: OK.\n\nErrors: SESSION_NOT_FOUND, MAX_SHELLS_EXCEEDED, TRANSPORT_ERROR."
    )]
    async fn ssh_shell_open(
        &self,
        Parameters(args): Parameters<SshShellOpenArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = OpenShellRequest {
            session_id: SessionId::new(args.session_id),
            term: args.term,
            cols: args.cols,
            rows: args.rows,
            inactivity_ttl_secs: args.inactivity_ttl,
            max_buffer_size: parse_human_bytes(args.max_buffer_size.as_deref()),
        };
        match self.use_cases.open_shell.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::shell::shell_open_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_SHELL_OPEN", &err)),
        }
    }

    #[tool(
        description = "Write raw bytes to a PTY shell.\n\nWhen to use:\n- Submitting a typed command (append `\\n`).\n- Sending raw control sequences (e.g. `\\x03` for Ctrl+C, `\\x1b[A` for arrow up).\n- Prefer ssh_shell_send_key for named keystrokes.\n\nStatus values: OK.\n\nErrors: SHELL_NOT_FOUND, TRANSPORT_ERROR."
    )]
    async fn ssh_shell_write(
        &self,
        Parameters(args): Parameters<SshShellWriteArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = WriteShellRequest {
            shell_id: ShellId::new(args.shell_id),
            bytes: Bytes::from(args.input.into_bytes()),
        };
        match self.use_cases.write_shell.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::shell::shell_write_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_SHELL_WRITE", &err)),
        }
    }

    #[tool(
        description = "Send a named keystroke (with optional modifiers) to a PTY shell.\n\nWhen to use:\n- Sending arrows, function keys, control codes, navigation keys without crafting the bytes manually.\n- Optional Shift / Alt / Ctrl modifiers; optional `repeat` (1..=64).\n\nStatus values: OK.\n\nErrors: SHELL_NOT_FOUND, INVALID_ARGUMENT (bad repeat / modifier combination), TRANSPORT_ERROR."
    )]
    async fn ssh_shell_send_key(
        &self,
        Parameters(args): Parameters<SshShellSendKeyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = SendKeyRequest {
            shell_id: ShellId::new(args.shell_id),
            key: args.key,
            modifiers: pick_modifiers(args.shift, args.alt, args.ctrl),
            repeat: args.repeat.unwrap_or(1),
        };
        match self.use_cases.send_key.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::shell::shell_send_key_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_SHELL_SEND_KEY", &err)),
        }
    }

    #[tool(
        description = "Read the buffered output of a PTY shell.\n\nWhen to use:\n- FALLBACK polling when subscribing to `shell://<shell_id>/output` is not feasible.\n- `clear=true` (default) drains the rendered head; `clear=false` keeps the buffer for re-inspection.\n- Optional long-poll via `wait=true` (`min_bytes` / `wait_timeout_secs`).\n\nStatus values: OPEN, CLOSED, TIMEOUT.\n\nErrors: SHELL_NOT_FOUND."
    )]
    async fn ssh_shell_read(
        &self,
        Parameters(args): Parameters<SshShellReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = ReadShellRequest {
            shell_id: ShellId::new(args.shell_id),
            clear: args.clear.unwrap_or(true),
            max_output_bytes: args.max_output_bytes,
            wait: args.wait.unwrap_or(false),
            wait_timeout: args.wait_timeout_secs.map(Duration::from_secs),
            min_bytes: args.min_bytes,
        };
        match self.use_cases.read_shell.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::shell::shell_read_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_SHELL_READ", &err)),
        }
    }

    #[tool(
        description = "Block until a substring pattern appears in the shell output.\n\nWhen to use:\n- Single-shot prompt gating before issuing the next command (e.g. wait for `\"$ \"`).\n- Up to 16 patterns (≤1024 bytes each); first match wins.\n- Prefer subscribing to `shell://<shell_id>/output` for realtime push.\n\nStatus values: MATCHED, TIMEOUT, CLOSED.\n\nErrors: SHELL_NOT_FOUND, INVALID_ARGUMENT."
    )]
    async fn ssh_shell_wait_for(
        &self,
        Parameters(args): Parameters<SshShellWaitForArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = WaitForPatternRequest {
            shell_id: ShellId::new(args.shell_id),
            patterns: args.patterns,
            timeout: args.timeout_secs.map(Duration::from_secs),
            max_output_bytes: args.max_output_bytes,
            clear: args.clear.unwrap_or(true),
        };
        match self.use_cases.wait_for_pattern.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::shell::shell_wait_for_render(&outcome))),
            Err(err) => Ok(render_tool_error("SSH_SHELL_WAIT_FOR", &err)),
        }
    }

    #[tool(
        description = "Close a PTY shell and free its resources.\n\nStatus values: OK.\n\nErrors: SHELL_NOT_FOUND, TRANSPORT_ERROR."
    )]
    async fn ssh_shell_close(
        &self,
        Parameters(args): Parameters<SshShellCloseArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .use_cases
            .close_shell
            .execute(CloseShellRequest {
                shell_id: ShellId::new(args.shell_id),
            })
            .await
        {
            Ok(outcome) => Ok(ok_text(render::shell::shell_close_render(&outcome))),
            Err(err) => Ok(render_tool_error("SSH_SHELL_CLOSE", &err)),
        }
    }

    // ---------- SFTP domain ------------------------------------------

    #[tool(
        description = "Upload a local file to the remote host via SFTP.\n\nWhen to use:\n- Streaming a local file to the remote host in 32 KiB chunks.\n- Subscribe to `transfer://<transfer_id>/progress` for live progress events.\n\nImportant identifiers in response:\n- `TRANSFER_ID`: passed to ssh_get_transfer_progress.\n\nStatus values: STARTED.\n\nErrors: SESSION_NOT_FOUND, MAX_TRANSFERS_EXCEEDED, SFTP_ERROR."
    )]
    async fn ssh_upload(
        &self,
        Parameters(args): Parameters<SshUploadArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = UploadRequest {
            session_id: SessionId::new(args.session_id),
            local_path: args.local_path,
            remote_path: args.remote_path,
        };
        match self.use_cases.upload_file.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::sftp::upload_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_UPLOAD", &err)),
        }
    }

    #[tool(
        description = "Download a remote file via SFTP.\n\nWhen to use:\n- Streaming a remote file to the local host in 32 KiB chunks.\n- Subscribe to `transfer://<transfer_id>/progress` for live progress events.\n\nStatus values: STARTED.\n\nErrors: SESSION_NOT_FOUND, MAX_TRANSFERS_EXCEEDED, SFTP_ERROR."
    )]
    async fn ssh_download(
        &self,
        Parameters(args): Parameters<SshDownloadArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = DownloadRequest {
            session_id: SessionId::new(args.session_id),
            remote_path: args.remote_path,
            local_path: args.local_path,
        };
        match self.use_cases.download_file.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::sftp::download_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_DOWNLOAD", &err)),
        }
    }

    #[tool(
        description = "Snapshot the progress of an SFTP transfer.\n\nWhen to use:\n- Polling progress for an upload/download.\n- Optional `wait=true` blocks until the transfer reaches a terminal state.\n\nStatus values: RUNNING, COMPLETED, FAILED, CANCELLED.\n\nErrors: TRANSFER_NOT_FOUND."
    )]
    async fn ssh_get_transfer_progress(
        &self,
        Parameters(args): Parameters<SshGetTransferProgressArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = GetTransferProgressRequest {
            transfer_id: TransferId::new(args.transfer_id),
            wait: args.wait.unwrap_or(false),
            wait_timeout: args.wait_timeout_secs.map(Duration::from_secs),
        };
        match self.use_cases.get_transfer_progress.execute(req).await {
            Ok(result) => Ok(ok_text(render::sftp::transfer_progress_render(&result))),
            Err(err) => Ok(render_tool_error("SSH_GET_TRANSFER_PROGRESS", &err)),
        }
    }

    // ---------- Forward domain --------------------------------------

    #[tool(
        description = "Set up a TCP port forwarder backed by an SSH session.\n\nWhen to use:\n- Tunnelling local TCP traffic over the SSH transport to a remote host:port.\n- Available only when the `port_forward` Cargo feature is enabled.\n\nStatus values: OK.\n\nErrors: SESSION_NOT_FOUND, PORT_IN_USE."
    )]
    async fn ssh_forward(
        &self,
        Parameters(args): Parameters<SshForwardArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = ForwardPortRequest {
            session_id: SessionId::new(args.session_id),
            local_port: args.local_port,
            remote_address: args.remote_address,
            remote_port: args.remote_port,
        };
        match self.use_cases.forward_port.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::forward::forward_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_FORWARD", &err)),
        }
    }
}

// ---------------------------------------------------------------------------
// `#[tool_router]` impl — `port_forward` disabled
// ---------------------------------------------------------------------------

#[cfg(not(feature = "port_forward"))]
#[tool_router]
impl<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg>
    McpSshServer<UseCases<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg>>
where
    S: SshClientPort + Send + Sync + 'static,
    F: SftpClientPort + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    CR: CommandRepository + Send + Sync + 'static,
    ShR: ShellRepository + Send + Sync + 'static,
    TR: TransferRepository + Send + Sync + 'static,
    N: NotifierPort + Send + Sync + 'static,
    AS: AuthStrategyPort + Send + Sync + 'static,
    OS: OutputStreamPort + Send + Sync + 'static,
    SubR: SubscriberRegistryPort + SubscriberRegistryAsync + Send + Sync + 'static,
    C: ClockPort + Send + Sync + 'static,
    Cfg: ConfigPort + Send + Sync + 'static,
    Idg: IdGeneratorPort + Send + Sync + 'static,
{
    /// Build an [`McpSshServer`] with the provided container + peer table.
    #[must_use]
    #[allow(
        clippy::type_complexity,
        reason = "the Arc<UseCases<...>> generic surface is the natural shape of the production wiring; the prod alias `ProdUseCases` collapses it at the call site"
    )]
    pub const fn new(
        use_cases: Arc<UseCases<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg>>,
        peer_table: Arc<PeerTable>,
    ) -> Self {
        Self::from_parts(use_cases, peer_table)
    }

    // ---------- Connection domain ------------------------------------

    #[tool(description = "Connect to an SSH server and store the session.")]
    async fn ssh_connect(
        &self,
        Parameters(args): Parameters<SshConnectArgs>,
    ) -> Result<CallToolResult, McpError> {
        run_connect(self.use_cases.connect.as_ref(), args).await
    }

    #[tool(description = "Disconnect an SSH session.")]
    async fn ssh_disconnect(
        &self,
        Parameters(args): Parameters<SshDisconnectArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .use_cases
            .disconnect
            .execute(DisconnectRequest {
                session_id: SessionId::new(args.session_id),
            })
            .await
        {
            Ok(outcome) => Ok(ok_text(render::connection::disconnect_render(&outcome))),
            Err(err) => Ok(render_tool_error("SSH_DISCONNECT", &err)),
        }
    }

    #[tool(description = "List active SSH sessions.")]
    async fn ssh_list_sessions(
        &self,
        Parameters(args): Parameters<SshListSessionsArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .use_cases
            .list_sessions
            .execute(ListSessionsRequest {
                filter_agent_id: args.agent_id.map(AgentId::new),
                max_items: args.max_items,
            })
            .await
        {
            Ok(outcome) => Ok(ok_text(render::connection::list_sessions_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_LIST_SESSIONS", &err)),
        }
    }

    #[tool(description = "Disconnect every session bound to a given agent.")]
    async fn ssh_disconnect_agent(
        &self,
        Parameters(args): Parameters<SshDisconnectAgentArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .use_cases
            .disconnect_agent
            .execute(DisconnectAgentRequest {
                agent_id: AgentId::new(args.agent_id),
            })
            .await
        {
            Ok(outcome) => Ok(ok_text(render::connection::disconnect_agent_render(
                &outcome,
            ))),
            Err(err) => Ok(render_tool_error("SSH_DISCONNECT_AGENT", &err)),
        }
    }

    // ---------- Execute domain ---------------------------------------

    #[tool(description = "Spawn an asynchronous command on an SSH session.")]
    async fn ssh_execute(
        &self,
        Parameters(args): Parameters<SshExecuteArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = ExecuteRequest {
            session_id: SessionId::new(args.session_id),
            command: args.command,
            timeout: args.timeout_secs.map(Duration::from_secs),
            use_pty: args.pty.unwrap_or(false),
        };
        match self.use_cases.execute.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::execute::execute_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_EXECUTE", &err)),
        }
    }

    #[tool(description = "Fetch the current output of an asynchronous command.")]
    async fn ssh_get_command_output(
        &self,
        Parameters(args): Parameters<SshGetCommandOutputArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = GetCommandOutputRequest {
            command_id: CommandId::new(args.command_id),
            wait: args.wait.unwrap_or(false),
            wait_timeout: args.wait_timeout_secs.map(Duration::from_secs),
            max_output_bytes: args.max_output_bytes,
        };
        match self.use_cases.get_command_output.execute(req).await {
            Ok(result) => Ok(ok_text(render::execute::get_command_output_render(result))),
            Err(err) => Ok(render_tool_error("SSH_GET_COMMAND_OUTPUT", &err)),
        }
    }

    #[tool(description = "List asynchronous commands tracked on the server.")]
    async fn ssh_list_commands(
        &self,
        Parameters(args): Parameters<SshListCommandsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = ListCommandsRequest {
            filter_session_id: args.session_id.map(SessionId::new),
            filter_status: args
                .status
                .map(super::args::execute::CommandStatus::into_domain),
            max_items: args.max_items,
        };
        match self.use_cases.list_commands.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::execute::list_commands_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_LIST_COMMANDS", &err)),
        }
    }

    #[tool(description = "Cancel an asynchronous command.")]
    async fn ssh_cancel_command(
        &self,
        Parameters(args): Parameters<SshCancelCommandArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = CancelCommandRequest {
            command_id: CommandId::new(args.command_id),
            max_output_bytes: args.max_output_bytes,
        };
        match self.use_cases.cancel_command.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::execute::cancel_command_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_CANCEL_COMMAND", &err)),
        }
    }

    // ---------- Shell domain -----------------------------------------

    #[tool(description = "Open an interactive PTY shell.")]
    async fn ssh_shell_open(
        &self,
        Parameters(args): Parameters<SshShellOpenArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = OpenShellRequest {
            session_id: SessionId::new(args.session_id),
            term: args.term,
            cols: args.cols,
            rows: args.rows,
            inactivity_ttl_secs: args.inactivity_ttl,
            max_buffer_size: parse_human_bytes(args.max_buffer_size.as_deref()),
        };
        match self.use_cases.open_shell.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::shell::shell_open_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_SHELL_OPEN", &err)),
        }
    }

    #[tool(description = "Write raw bytes to a PTY shell.")]
    async fn ssh_shell_write(
        &self,
        Parameters(args): Parameters<SshShellWriteArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = WriteShellRequest {
            shell_id: ShellId::new(args.shell_id),
            bytes: Bytes::from(args.input.into_bytes()),
        };
        match self.use_cases.write_shell.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::shell::shell_write_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_SHELL_WRITE", &err)),
        }
    }

    #[tool(description = "Send a named keystroke (with optional modifiers) to a PTY shell.")]
    async fn ssh_shell_send_key(
        &self,
        Parameters(args): Parameters<SshShellSendKeyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = SendKeyRequest {
            shell_id: ShellId::new(args.shell_id),
            key: args.key,
            modifiers: pick_modifiers(args.shift, args.alt, args.ctrl),
            repeat: args.repeat.unwrap_or(1),
        };
        match self.use_cases.send_key.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::shell::shell_send_key_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_SHELL_SEND_KEY", &err)),
        }
    }

    #[tool(description = "Read the buffered output of a PTY shell.")]
    async fn ssh_shell_read(
        &self,
        Parameters(args): Parameters<SshShellReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = ReadShellRequest {
            shell_id: ShellId::new(args.shell_id),
            clear: args.clear.unwrap_or(true),
            max_output_bytes: args.max_output_bytes,
            wait: args.wait.unwrap_or(false),
            wait_timeout: args.wait_timeout_secs.map(Duration::from_secs),
            min_bytes: args.min_bytes,
        };
        match self.use_cases.read_shell.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::shell::shell_read_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_SHELL_READ", &err)),
        }
    }

    #[tool(description = "Block until a substring pattern appears in the shell output.")]
    async fn ssh_shell_wait_for(
        &self,
        Parameters(args): Parameters<SshShellWaitForArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = WaitForPatternRequest {
            shell_id: ShellId::new(args.shell_id),
            patterns: args.patterns,
            timeout: args.timeout_secs.map(Duration::from_secs),
            max_output_bytes: args.max_output_bytes,
            clear: args.clear.unwrap_or(true),
        };
        match self.use_cases.wait_for_pattern.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::shell::shell_wait_for_render(&outcome))),
            Err(err) => Ok(render_tool_error("SSH_SHELL_WAIT_FOR", &err)),
        }
    }

    #[tool(description = "Close a PTY shell.")]
    async fn ssh_shell_close(
        &self,
        Parameters(args): Parameters<SshShellCloseArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .use_cases
            .close_shell
            .execute(CloseShellRequest {
                shell_id: ShellId::new(args.shell_id),
            })
            .await
        {
            Ok(outcome) => Ok(ok_text(render::shell::shell_close_render(&outcome))),
            Err(err) => Ok(render_tool_error("SSH_SHELL_CLOSE", &err)),
        }
    }

    // ---------- SFTP domain ------------------------------------------

    #[tool(description = "Upload a local file to the remote host via SFTP.")]
    async fn ssh_upload(
        &self,
        Parameters(args): Parameters<SshUploadArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = UploadRequest {
            session_id: SessionId::new(args.session_id),
            local_path: args.local_path,
            remote_path: args.remote_path,
        };
        match self.use_cases.upload_file.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::sftp::upload_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_UPLOAD", &err)),
        }
    }

    #[tool(description = "Download a remote file via SFTP.")]
    async fn ssh_download(
        &self,
        Parameters(args): Parameters<SshDownloadArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = DownloadRequest {
            session_id: SessionId::new(args.session_id),
            remote_path: args.remote_path,
            local_path: args.local_path,
        };
        match self.use_cases.download_file.execute(req).await {
            Ok(outcome) => Ok(ok_text(render::sftp::download_render(outcome))),
            Err(err) => Ok(render_tool_error("SSH_DOWNLOAD", &err)),
        }
    }

    #[tool(description = "Snapshot the progress of an SFTP transfer.")]
    async fn ssh_get_transfer_progress(
        &self,
        Parameters(args): Parameters<SshGetTransferProgressArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = GetTransferProgressRequest {
            transfer_id: TransferId::new(args.transfer_id),
            wait: args.wait.unwrap_or(false),
            wait_timeout: args.wait_timeout_secs.map(Duration::from_secs),
        };
        match self.use_cases.get_transfer_progress.execute(req).await {
            Ok(result) => Ok(ok_text(render::sftp::transfer_progress_render(&result))),
            Err(err) => Ok(render_tool_error("SSH_GET_TRANSFER_PROGRESS", &err)),
        }
    }
}

// ---------------------------------------------------------------------------
// Free-standing connect helper shared between the two `tool_router` impls
// ---------------------------------------------------------------------------

/// Drive the connect-session use case from the rmcp `SshConnectArgs`.
/// Pulled out as a free function so both feature flavours of the
/// `#[tool_router]` impl can call it without duplicating the body.
async fn run_connect<S, R, C, I, Cfg>(
    use_case: &ConnectSessionUseCase<S, R, C, I, Cfg>,
    args: SshConnectArgs,
) -> Result<CallToolResult, McpError>
where
    S: SshClientPort + Send + Sync,
    R: SessionRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    I: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    let req = match build_connect_request(args) {
        Ok(r) => r,
        Err(err) => return Ok(render_tool_error("SSH_CONNECT", &err)),
    };
    match use_case.execute(req).await {
        Ok(outcome) => Ok(ok_text(render::connection::connect_render(outcome))),
        Err(err) => Ok(render_tool_error("SSH_CONNECT", &err)),
    }
}

/// Translate the rmcp `SshConnectArgs` payload into a domain
/// [`ConnectRequest`]. Address parsing failures surface as
/// [`DomainError::InvalidArgument`] so the caller renders the
/// canonical error block.
fn build_connect_request(args: SshConnectArgs) -> Result<ConnectRequest, DomainError> {
    let address = parse_address(&args.address)?;
    let credentials = pick_credentials(
        &args.username,
        args.password.as_deref(),
        args.key_path.as_deref(),
    );
    Ok(ConnectRequest {
        explicit_session_id: args.session_id.map(SessionId::new),
        address,
        username: args.username,
        credentials,
        timeout_secs: args.timeout_secs,
        max_retries: args.max_retries,
        retry_delay_ms: args.retry_delay_ms,
        compress: args.compress,
        name: args.name,
        persistent: args.persistent.unwrap_or(false),
        agent_id: args.agent_id.map(AgentId::new),
        reuse: args.reuse.map_or(
            DomainReusePolicy::Suggest,
            super::args::connection::ReusePolicy::into_domain,
        ),
    })
}

/// Parse a v3 human-byte string (`512k`, `10m`, `1g`, `1t`) into an
/// absolute byte count. Returns `None` for an unparseable input or
/// missing argument so the use case falls back to the configured
/// default.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "human-byte parser only operates on integer suffix tables; the cast surface is bounded by the multiplier table and never crosses 2^63"
)]
fn parse_human_bytes(input: Option<&str>) -> Option<u64> {
    let raw = input?.trim();
    if raw.is_empty() {
        return None;
    }
    let (digits, suffix): (String, String) =
        raw.chars().partition(|c| c.is_ascii_digit() || *c == '.');
    let multiplier: u64 = match suffix.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1024,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        "t" | "tb" => 1024_u64 * 1024 * 1024 * 1024,
        _ => return None,
    };
    let value: f64 = digits.parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some((value * multiplier as f64) as u64)
}

// ---------------------------------------------------------------------------
// `#[tool_handler]` impl
// ---------------------------------------------------------------------------

#[cfg(feature = "port_forward")]
#[tool_handler]
impl<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg> ServerHandler
    for McpSshServer<UseCases<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg>>
where
    S: SshClientPort + Send + Sync + 'static,
    F: SftpClientPort + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    CR: CommandRepository + Send + Sync + 'static,
    ShR: ShellRepository + Send + Sync + 'static,
    TR: TransferRepository + Send + Sync + 'static,
    FR: ForwardRepository + Send + Sync + 'static,
    N: NotifierPort + Send + Sync + 'static,
    AS: AuthStrategyPort + Send + Sync + 'static,
    OS: OutputStreamPort + Send + Sync + 'static,
    SubR: SubscriberRegistryPort + SubscriberRegistryAsync + Send + Sync + 'static,
    C: ClockPort + Send + Sync + 'static,
    Cfg: ConfigPort + Send + Sync + 'static,
    Idg: IdGeneratorPort + Send + Sync + 'static,
{
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
            "SSH MCP server (v4 hexagonal). 18 SSH tools and 5 resource subscribe schemes \
             (shell://, command://, transfer://, session://, forward://)."
                .to_string(),
        );
        info
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        resource_handlers::list_resources_impl(&self.use_cases.list_resources).await
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        resource_handlers::read_resource_impl(
            &self.use_cases.read_resource,
            request,
            &context.peer,
            &self.peer_table,
        )
        .await
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        resource_handlers::subscribe_impl(
            &self.use_cases.subscribe_resource,
            request,
            context.peer,
            &self.peer_table,
        )
        .await
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        resource_handlers::unsubscribe_impl(
            &self.use_cases.unsubscribe_resource,
            request,
            &context.peer,
            &self.peer_table,
        )
        .await
    }
}

#[cfg(not(feature = "port_forward"))]
#[tool_handler]
impl<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg> ServerHandler
    for McpSshServer<UseCases<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg>>
where
    S: SshClientPort + Send + Sync + 'static,
    F: SftpClientPort + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    CR: CommandRepository + Send + Sync + 'static,
    ShR: ShellRepository + Send + Sync + 'static,
    TR: TransferRepository + Send + Sync + 'static,
    N: NotifierPort + Send + Sync + 'static,
    AS: AuthStrategyPort + Send + Sync + 'static,
    OS: OutputStreamPort + Send + Sync + 'static,
    SubR: SubscriberRegistryPort + SubscriberRegistryAsync + Send + Sync + 'static,
    C: ClockPort + Send + Sync + 'static,
    Cfg: ConfigPort + Send + Sync + 'static,
    Idg: IdGeneratorPort + Send + Sync + 'static,
{
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
            "SSH MCP server (v4 hexagonal). 17 SSH tools and 4 resource subscribe schemes \
             (shell://, command://, transfer://, session://)."
                .to_string(),
        );
        info
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        resource_handlers::list_resources_impl(&self.use_cases.list_resources).await
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        resource_handlers::read_resource_impl(
            &self.use_cases.read_resource,
            request,
            &context.peer,
            &self.peer_table,
        )
        .await
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        resource_handlers::subscribe_impl(
            &self.use_cases.subscribe_resource,
            request,
            context.peer,
            &self.peer_table,
        )
        .await
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        resource_handlers::unsubscribe_impl(
            &self.use_cases.unsubscribe_resource,
            request,
            &context.peer,
            &self.peer_table,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_address, parse_human_bytes};

    #[test]
    fn parse_address_with_explicit_port() {
        let addr = parse_address("example.com:2222").expect("address");
        assert_eq!(addr.host(), "example.com");
        assert_eq!(addr.port(), 2222);
    }

    #[test]
    fn parse_address_defaults_to_22_when_no_port() {
        let addr = parse_address("example.com").expect("address");
        assert_eq!(addr.host(), "example.com");
        assert_eq!(addr.port(), 22);
    }

    #[test]
    fn parse_address_rejects_non_numeric_port() {
        let err = parse_address("example.com:abc").expect_err("non-numeric port");
        assert!(err.to_string().contains("invalid port"));
    }

    #[test]
    fn parse_human_bytes_handles_kb_mb_gb() {
        assert_eq!(parse_human_bytes(Some("512k")), Some(512 * 1024));
        assert_eq!(parse_human_bytes(Some("10m")), Some(10 * 1024 * 1024));
        assert_eq!(parse_human_bytes(Some("1g")), Some(1024_u64 * 1024 * 1024));
        assert_eq!(
            parse_human_bytes(Some("1t")),
            Some(1024_u64 * 1024 * 1024 * 1024)
        );
    }

    #[test]
    fn parse_human_bytes_rejects_garbage() {
        assert_eq!(parse_human_bytes(Some("nope")), None);
        assert_eq!(parse_human_bytes(Some("")), None);
        assert_eq!(parse_human_bytes(None), None);
    }
}
