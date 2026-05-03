//! `#[tool_router]` impl + `ServerHandler` impl for [`super::server::McpSshServer`].
//!
//! 18 tool fns + 4 resource methods all delegate to the use case container
//! sitting on [`super::server::McpSshServer::use_cases`]. H16 ships
//! placeholder responses (`TODO H17: render`) so the v4 path is reachable
//! end-to-end before the markdown rendering ports across in H17.
//!
//! ## Generic shape
//!
//! The impl block specialises on the production-shaped
//! [`crate::composition::UseCases`] type. Every generic adapter
//! parameter is forwarded as-is; the test harness (H18) builds a
//! `UseCases<Fakes...>` instance with the exact same shape.
//!
//! ## Args
//!
//! Per-tool stub args live in [`super::args`]; H17 fills v3 parity (every
//! optional tuning knob, env-var fallbacks, validation tables, etc.).

use std::sync::Arc;

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
use crate::application::connect_session::ConnectRequest;
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
use crate::domain::policy::ReusePolicy;
use crate::mcp::keys::{KeyModifiers, ShellKey};
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

#[cfg(feature = "port_forward")]
use super::args::SshForwardArgs;
use super::args::{
    SshCancelCommandArgs, SshConnectArgs, SshDisconnectAgentArgs, SshDisconnectArgs,
    SshDownloadArgs, SshExecuteArgs, SshGetCommandOutputArgs, SshGetTransferProgressArgs,
    SshListCommandsArgs, SshListSessionsArgs, SshShellCloseArgs, SshShellOpenArgs,
    SshShellReadArgs, SshShellSendKeyArgs, SshShellWaitForArgs, SshShellWriteArgs, SshUploadArgs,
};
use super::peer_handle::PeerTable;
use super::resource_handlers;
use super::server::McpSshServer;

/// Map a [`DomainError`] onto an [`McpError`] for tool calls.
///
/// Mirrors the resource-handler mapping but wraps validation errors as
/// `invalid_params` so the rmcp client surfaces the schema-level problem
/// to the LLM rather than as an opaque server failure.
fn map_tool_error(err: DomainError) -> McpError {
    match err {
        DomainError::InvalidArgument(reason) => McpError::invalid_params(reason, None),
        DomainError::SessionNotFound(_)
        | DomainError::ShellNotFound(_)
        | DomainError::CommandNotFound(_)
        | DomainError::TransferNotFound(_)
        | DomainError::ForwardNotFound(_) => McpError::resource_not_found(err.to_string(), None),
        DomainError::Auth(_)
        | DomainError::ConnectFailed(_)
        | DomainError::Transport(_)
        | DomainError::Timeout(_)
        | DomainError::Storage(_)
        | DomainError::Sftp(_)
        | DomainError::PortInUse(_)
        | DomainError::Internal(_)
        | DomainError::MaxCommandsExceeded { .. }
        | DomainError::MaxShellsExceeded { .. }
        | DomainError::MaxTransfersExceeded { .. } => {
            McpError::internal_error(err.to_string(), None)
        }
    }
}

/// Build a placeholder `CallToolResult::success` with the given text.
/// H17 fills the real markdown body.
fn placeholder_ok(tool: &str) -> CallToolResult {
    CallToolResult::success(vec![Content::text(format!("TODO H17: render {tool}"))])
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

// ---------------------------------------------------------------------------
// `#[tool_router]` impl
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
    pub fn new(
        use_cases: Arc<UseCases<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg>>,
        peer_table: Arc<PeerTable>,
    ) -> Self {
        Self::from_parts(use_cases, peer_table)
    }

    // -----------------------------------------------------------------------
    // Connection domain
    // -----------------------------------------------------------------------

    #[tool(description = "TODO H17: ssh_connect description")]
    async fn ssh_connect(
        &self,
        Parameters(args): Parameters<SshConnectArgs>,
    ) -> Result<CallToolResult, McpError> {
        let address = parse_address(&args.address).map_err(map_tool_error)?;
        let credentials = Credentials::Password {
            username: args.username.clone(),
            password: String::new(),
        };
        let req = ConnectRequest {
            explicit_session_id: None,
            address,
            username: args.username,
            credentials,
            timeout_secs: None,
            max_retries: None,
            retry_delay_ms: None,
            compress: None,
            name: None,
            persistent: false,
            agent_id: None,
            reuse: ReusePolicy::Suggest,
        };
        self.use_cases
            .connect
            .execute(req)
            .await
            .map(|_outcome| placeholder_ok("ssh_connect"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_disconnect description")]
    async fn ssh_disconnect(
        &self,
        Parameters(args): Parameters<SshDisconnectArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .disconnect
            .execute(DisconnectRequest {
                session_id: SessionId::new(args.session_id),
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_disconnect"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_list_sessions description")]
    async fn ssh_list_sessions(
        &self,
        Parameters(args): Parameters<SshListSessionsArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .list_sessions
            .execute(ListSessionsRequest {
                filter_agent_id: args.agent_id.map(AgentId::new),
                max_items: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_list_sessions"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_disconnect_agent description")]
    async fn ssh_disconnect_agent(
        &self,
        Parameters(args): Parameters<SshDisconnectAgentArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .disconnect_agent
            .execute(DisconnectAgentRequest {
                agent_id: AgentId::new(args.agent_id),
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_disconnect_agent"))
            .map_err(map_tool_error)
    }

    // -----------------------------------------------------------------------
    // Execute domain
    // -----------------------------------------------------------------------

    #[tool(description = "TODO H17: ssh_execute description")]
    async fn ssh_execute(
        &self,
        Parameters(args): Parameters<SshExecuteArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .execute
            .execute(ExecuteRequest {
                session_id: SessionId::new(args.session_id),
                command: args.command,
                timeout: None,
                use_pty: false,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_execute"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_get_command_output description")]
    async fn ssh_get_command_output(
        &self,
        Parameters(args): Parameters<SshGetCommandOutputArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .get_command_output
            .execute(GetCommandOutputRequest {
                command_id: CommandId::new(args.command_id),
                wait: false,
                wait_timeout: None,
                max_output_bytes: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_get_command_output"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_list_commands description")]
    async fn ssh_list_commands(
        &self,
        Parameters(args): Parameters<SshListCommandsArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .list_commands
            .execute(ListCommandsRequest {
                filter_session_id: args.session_id.map(SessionId::new),
                filter_status: None,
                max_items: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_list_commands"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_cancel_command description")]
    async fn ssh_cancel_command(
        &self,
        Parameters(args): Parameters<SshCancelCommandArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .cancel_command
            .execute(CancelCommandRequest {
                command_id: CommandId::new(args.command_id),
                max_output_bytes: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_cancel_command"))
            .map_err(map_tool_error)
    }

    // -----------------------------------------------------------------------
    // Shell domain
    // -----------------------------------------------------------------------

    #[tool(description = "TODO H17: ssh_shell_open description")]
    async fn ssh_shell_open(
        &self,
        Parameters(args): Parameters<SshShellOpenArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .open_shell
            .execute(OpenShellRequest {
                session_id: SessionId::new(args.session_id),
                term: None,
                cols: None,
                rows: None,
                inactivity_ttl_secs: None,
                max_buffer_size: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_shell_open"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_shell_write description")]
    async fn ssh_shell_write(
        &self,
        Parameters(args): Parameters<SshShellWriteArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .write_shell
            .execute(WriteShellRequest {
                shell_id: ShellId::new(args.shell_id),
                bytes: Bytes::from(args.input.into_bytes()),
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_shell_write"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_shell_send_key description")]
    async fn ssh_shell_send_key(
        &self,
        Parameters(args): Parameters<SshShellSendKeyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let key = parse_shell_key(&args.key)?;
        self.use_cases
            .send_key
            .execute(SendKeyRequest {
                shell_id: ShellId::new(args.shell_id),
                key,
                modifiers: KeyModifiers::default(),
                repeat: 1,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_shell_send_key"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_shell_read description")]
    async fn ssh_shell_read(
        &self,
        Parameters(args): Parameters<SshShellReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .read_shell
            .execute(ReadShellRequest {
                shell_id: ShellId::new(args.shell_id),
                clear: true,
                max_output_bytes: None,
                wait: false,
                wait_timeout: None,
                min_bytes: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_shell_read"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_shell_wait_for description")]
    async fn ssh_shell_wait_for(
        &self,
        Parameters(args): Parameters<SshShellWaitForArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .wait_for_pattern
            .execute(WaitForPatternRequest {
                shell_id: ShellId::new(args.shell_id),
                patterns: args.patterns,
                timeout: None,
                max_output_bytes: None,
                clear: false,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_shell_wait_for"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_shell_close description")]
    async fn ssh_shell_close(
        &self,
        Parameters(args): Parameters<SshShellCloseArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .close_shell
            .execute(CloseShellRequest {
                shell_id: ShellId::new(args.shell_id),
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_shell_close"))
            .map_err(map_tool_error)
    }

    // -----------------------------------------------------------------------
    // SFTP domain
    // -----------------------------------------------------------------------

    #[tool(description = "TODO H17: ssh_upload description")]
    async fn ssh_upload(
        &self,
        Parameters(args): Parameters<SshUploadArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .upload_file
            .execute(UploadRequest {
                session_id: SessionId::new(args.session_id),
                local_path: args.local_path,
                remote_path: args.remote_path,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_upload"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_download description")]
    async fn ssh_download(
        &self,
        Parameters(args): Parameters<SshDownloadArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .download_file
            .execute(DownloadRequest {
                session_id: SessionId::new(args.session_id),
                remote_path: args.remote_path,
                local_path: args.local_path,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_download"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_get_transfer_progress description")]
    async fn ssh_get_transfer_progress(
        &self,
        Parameters(args): Parameters<SshGetTransferProgressArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .get_transfer_progress
            .execute(GetTransferProgressRequest {
                transfer_id: TransferId::new(args.transfer_id),
                wait: false,
                wait_timeout: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_get_transfer_progress"))
            .map_err(map_tool_error)
    }

    // -----------------------------------------------------------------------
    // Forward domain (feature-gated)
    // -----------------------------------------------------------------------

    #[tool(description = "TODO H17: ssh_forward description")]
    async fn ssh_forward(
        &self,
        Parameters(args): Parameters<SshForwardArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .forward_port
            .execute(ForwardPortRequest {
                session_id: SessionId::new(args.session_id),
                local_port: args.local_port,
                remote_address: args.remote_address,
                remote_port: args.remote_port,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_forward"))
            .map_err(map_tool_error)
    }
}

// ---------------------------------------------------------------------------
// `#[tool_router]` impl — `port_forward` disabled (no `ssh_forward` tool, no
// `FR` parameter)
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
    pub fn new(
        use_cases: Arc<UseCases<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg>>,
        peer_table: Arc<PeerTable>,
    ) -> Self {
        Self::from_parts(use_cases, peer_table)
    }

    // The 17 non-forward tool fns are duplicated verbatim from the
    // feature-on impl. Keeping them inline here (rather than in a shared
    // helper macro) preserves the rmcp `#[tool]` macro's compile-time
    // schema generation.

    #[tool(description = "TODO H17: ssh_connect description")]
    async fn ssh_connect(
        &self,
        Parameters(args): Parameters<SshConnectArgs>,
    ) -> Result<CallToolResult, McpError> {
        let address = parse_address(&args.address).map_err(map_tool_error)?;
        let credentials = Credentials::Password {
            username: args.username.clone(),
            password: String::new(),
        };
        let req = ConnectRequest {
            explicit_session_id: None,
            address,
            username: args.username,
            credentials,
            timeout_secs: None,
            max_retries: None,
            retry_delay_ms: None,
            compress: None,
            name: None,
            persistent: false,
            agent_id: None,
            reuse: ReusePolicy::Suggest,
        };
        self.use_cases
            .connect
            .execute(req)
            .await
            .map(|_outcome| placeholder_ok("ssh_connect"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_disconnect description")]
    async fn ssh_disconnect(
        &self,
        Parameters(args): Parameters<SshDisconnectArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .disconnect
            .execute(DisconnectRequest {
                session_id: SessionId::new(args.session_id),
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_disconnect"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_list_sessions description")]
    async fn ssh_list_sessions(
        &self,
        Parameters(args): Parameters<SshListSessionsArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .list_sessions
            .execute(ListSessionsRequest {
                filter_agent_id: args.agent_id.map(AgentId::new),
                max_items: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_list_sessions"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_disconnect_agent description")]
    async fn ssh_disconnect_agent(
        &self,
        Parameters(args): Parameters<SshDisconnectAgentArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .disconnect_agent
            .execute(DisconnectAgentRequest {
                agent_id: AgentId::new(args.agent_id),
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_disconnect_agent"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_execute description")]
    async fn ssh_execute(
        &self,
        Parameters(args): Parameters<SshExecuteArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .execute
            .execute(ExecuteRequest {
                session_id: SessionId::new(args.session_id),
                command: args.command,
                timeout: None,
                use_pty: false,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_execute"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_get_command_output description")]
    async fn ssh_get_command_output(
        &self,
        Parameters(args): Parameters<SshGetCommandOutputArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .get_command_output
            .execute(GetCommandOutputRequest {
                command_id: CommandId::new(args.command_id),
                wait: false,
                wait_timeout: None,
                max_output_bytes: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_get_command_output"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_list_commands description")]
    async fn ssh_list_commands(
        &self,
        Parameters(args): Parameters<SshListCommandsArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .list_commands
            .execute(ListCommandsRequest {
                filter_session_id: args.session_id.map(SessionId::new),
                filter_status: None,
                max_items: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_list_commands"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_cancel_command description")]
    async fn ssh_cancel_command(
        &self,
        Parameters(args): Parameters<SshCancelCommandArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .cancel_command
            .execute(CancelCommandRequest {
                command_id: CommandId::new(args.command_id),
                max_output_bytes: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_cancel_command"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_shell_open description")]
    async fn ssh_shell_open(
        &self,
        Parameters(args): Parameters<SshShellOpenArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .open_shell
            .execute(OpenShellRequest {
                session_id: SessionId::new(args.session_id),
                term: None,
                cols: None,
                rows: None,
                inactivity_ttl_secs: None,
                max_buffer_size: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_shell_open"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_shell_write description")]
    async fn ssh_shell_write(
        &self,
        Parameters(args): Parameters<SshShellWriteArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .write_shell
            .execute(WriteShellRequest {
                shell_id: ShellId::new(args.shell_id),
                bytes: Bytes::from(args.input.into_bytes()),
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_shell_write"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_shell_send_key description")]
    async fn ssh_shell_send_key(
        &self,
        Parameters(args): Parameters<SshShellSendKeyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let key = parse_shell_key(&args.key)?;
        self.use_cases
            .send_key
            .execute(SendKeyRequest {
                shell_id: ShellId::new(args.shell_id),
                key,
                modifiers: KeyModifiers::default(),
                repeat: 1,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_shell_send_key"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_shell_read description")]
    async fn ssh_shell_read(
        &self,
        Parameters(args): Parameters<SshShellReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .read_shell
            .execute(ReadShellRequest {
                shell_id: ShellId::new(args.shell_id),
                clear: true,
                max_output_bytes: None,
                wait: false,
                wait_timeout: None,
                min_bytes: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_shell_read"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_shell_wait_for description")]
    async fn ssh_shell_wait_for(
        &self,
        Parameters(args): Parameters<SshShellWaitForArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .wait_for_pattern
            .execute(WaitForPatternRequest {
                shell_id: ShellId::new(args.shell_id),
                patterns: args.patterns,
                timeout: None,
                max_output_bytes: None,
                clear: false,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_shell_wait_for"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_shell_close description")]
    async fn ssh_shell_close(
        &self,
        Parameters(args): Parameters<SshShellCloseArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .close_shell
            .execute(CloseShellRequest {
                shell_id: ShellId::new(args.shell_id),
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_shell_close"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_upload description")]
    async fn ssh_upload(
        &self,
        Parameters(args): Parameters<SshUploadArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .upload_file
            .execute(UploadRequest {
                session_id: SessionId::new(args.session_id),
                local_path: args.local_path,
                remote_path: args.remote_path,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_upload"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_download description")]
    async fn ssh_download(
        &self,
        Parameters(args): Parameters<SshDownloadArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .download_file
            .execute(DownloadRequest {
                session_id: SessionId::new(args.session_id),
                remote_path: args.remote_path,
                local_path: args.local_path,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_download"))
            .map_err(map_tool_error)
    }

    #[tool(description = "TODO H17: ssh_get_transfer_progress description")]
    async fn ssh_get_transfer_progress(
        &self,
        Parameters(args): Parameters<SshGetTransferProgressArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.use_cases
            .get_transfer_progress
            .execute(GetTransferProgressRequest {
                transfer_id: TransferId::new(args.transfer_id),
                wait: false,
                wait_timeout: None,
            })
            .await
            .map(|_outcome| placeholder_ok("ssh_get_transfer_progress"))
            .map_err(map_tool_error)
    }
}

/// Parse a snake-case shell-key label into the domain
/// [`ShellKey`] enum. Goes through serde so the surface stays in lock-step
/// with the JSON schema the v3 wire format already published.
///
/// # Errors
///
/// Returns [`McpError::invalid_params`] when the label does not name a
/// known key.
fn parse_shell_key(label: &str) -> Result<ShellKey, McpError> {
    let json = format!("\"{label}\"");
    serde_json::from_str(&json).map_err(|err| {
        McpError::invalid_params(format!("invalid shell key {label:?}: {err}"), None)
    })
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
             (shell://, command://, transfer://, session://, forward://). \
             H16 ships placeholder responses; H17 fills v3 parity rendering."
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
             (shell://, command://, transfer://, session://). \
             H16 ships placeholder responses; H17 fills v3 parity rendering."
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
    use super::{parse_address, parse_shell_key};
    use crate::mcp::keys::ShellKey;

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
    fn parse_shell_key_round_trips_known_label() {
        let key = parse_shell_key("ctrl_c").expect("ctrl_c");
        assert_eq!(key, ShellKey::CtrlC);
    }

    #[test]
    fn parse_shell_key_rejects_unknown_label() {
        let err = parse_shell_key("not_a_key").expect_err("unknown");
        assert!(err.message.contains("invalid shell key"));
    }
}
