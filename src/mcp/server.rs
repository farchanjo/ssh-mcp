//! `McpSshServer` — primary MCP entry point implementing `rmcp::ServerHandler`.
//!
//! In v3.0.0 this struct owns:
//! - The `ToolRouter<Self>` aggregating the SSH tools (one wired in E3, the
//!   remaining 17 land in E4).
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

use super::tools::connection::{SshConnectArgs, ssh_connect_impl};

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
    #[tool(description = "Connect to an SSH server and store the session. Returns SESSION_ID and optional AGENT_ID. Status values: OK, REUSED, SUGGESTED. Use SESSION_ID with ssh_execute, ssh_shell_open, ssh_upload, ssh_download, ssh_disconnect, ssh_forward.")]
    async fn ssh_connect(
        &self,
        Parameters(args): Parameters<SshConnectArgs>,
    ) -> Result<CallToolResult, McpError> {
        ssh_connect_impl(args).await
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
