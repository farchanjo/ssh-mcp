//! Stdio transport entry point for ssh-mcp.
//!
//! Uses rmcp's `transport::io::stdio()` and `ServiceExt::serve` to handle the
//! full JSON-RPC pipeline including:
//! - `notifications/cancelled` (native rmcp routing)
//! - `notifications/initialized`
//! - `resources/templates/list`, `resources/read`, `resources/subscribe` etc.
//!
//! All ~250 LOC of custom cancel/fallback hacks present in v2.0 are removed —
//! rmcp 1.6 handles them natively.

#![deny(warnings)]
#![deny(clippy::unwrap_used)]

use rmcp::ServiceExt;
use rmcp::transport::io::stdio;
use ssh_mcp::mcp::server::McpSshServer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("starting ssh-mcp stdio transport (rmcp 1.6)");

    let service = McpSshServer::new().serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}
