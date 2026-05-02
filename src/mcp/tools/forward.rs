//! Port forwarding tool: `ssh_forward`.
//!
//! Sets up a local TCP listener that forwards every accepted connection
//! through the SSH transport to `remote_address:remote_port`. Available only
//! when the `port_forward` Cargo feature is enabled (the default).

use rmcp::model::{CallToolResult, Content, ErrorData as McpError};
use schemars::JsonSchema;
use serde::Deserialize;

use super::super::message::helpers::format_error;

#[cfg(feature = "port_forward")]
use std::sync::Arc;
#[cfg(feature = "port_forward")]
use tracing::{error, info};

#[cfg(feature = "port_forward")]
use super::super::forward::setup_port_forwarding;
#[cfg(feature = "port_forward")]
use super::super::message::builder::render_forward_ok;
#[cfg(feature = "port_forward")]
use super::super::storage::session::SESSION_STORAGE;
#[cfg(feature = "port_forward")]
use super::super::storage::traits::SessionStorage;
#[cfg(feature = "port_forward")]
use super::legacy_helpers::err_session_not_found;

/// Arguments for the `ssh_forward` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshForwardArgs {
    /// SESSION_ID returned from ssh_connect.
    pub session_id: String,

    /// Local TCP port to listen on (e.g. `8080`).
    pub local_port: u16,

    /// Remote host on the server side to forward to (e.g. `localhost` or `10.0.0.1`).
    pub remote_address: String,

    /// Remote TCP port to forward to (e.g. `3306` for MySQL).
    pub remote_port: u16,
}

/// Implementation of `ssh_forward`.
#[cfg(feature = "port_forward")]
pub async fn ssh_forward_impl(args: SshForwardArgs) -> Result<CallToolResult, McpError> {
    let SshForwardArgs {
        session_id,
        local_port,
        remote_address,
        remote_port,
    } = args;
    info!(
        "Setting up port forwarding from local port {local_port} to {remote_address}:{remote_port} using session {session_id}"
    );

    let handle_arc = match SESSION_STORAGE
        .get(&session_id)
        .map(|s| Arc::clone(&s.handle))
    {
        Some(h) => h,
        None => {
            return Ok(CallToolResult::error(vec![Content::text(
                err_session_not_found("SSH_FORWARD", &session_id),
            )]));
        }
    };

    match setup_port_forwarding(handle_arc, local_port, &remote_address, remote_port).await {
        Ok(local_addr) => {
            let body = render_forward_ok(
                &local_addr.to_string(),
                &format!("{remote_address}:{remote_port}"),
                true,
            );
            Ok(CallToolResult::success(vec![Content::text(body)]))
        }
        Err(e) => {
            error!("Port forwarding setup failed: {e}");
            Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_FORWARD",
                "FORWARD_FAILED",
                &e,
                None,
            ))]))
        }
    }
}

/// Stub used when the `port_forward` Cargo feature is disabled.
#[cfg(not(feature = "port_forward"))]
#[expect(
    clippy::unused_async,
    reason = "rmcp tool entrypoints are uniformly async even when the body itself is sync"
)]
pub async fn ssh_forward_impl(args: SshForwardArgs) -> Result<CallToolResult, McpError> {
    let _ = args;
    Ok(CallToolResult::error(vec![Content::text(format_error(
        "SSH_FORWARD",
        "FEATURE_DISABLED",
        "port forwarding feature is not enabled",
        Some("rebuild with --features port_forward"),
    ))]))
}
