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
//!
//! A background peer-GC task scans `SUBSCRIPTION_REGISTRY` on the interval
//! configured by `SSH_MCP_PEER_GC_INTERVAL_S` and drops peers whose rmcp
//! transport has closed (rmcp 1.6 does not surface a peer-disconnect
//! callback). The task is cancelled cleanly when stdin closes.

#![deny(warnings)]
#![deny(clippy::unwrap_used)]

use rmcp::ServiceExt;
use rmcp::transport::io::stdio;
use ssh_mcp::mcp::config::resolve_peer_gc_interval_s;
use ssh_mcp::mcp::server::McpSshServer;
use ssh_mcp::mcp::subscription::spawn_peer_gc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("starting ssh-mcp stdio transport (rmcp 1.6)");

    let gc_cancel = CancellationToken::new();
    let gc_interval = resolve_peer_gc_interval_s();
    let gc_task = spawn_peer_gc(gc_interval, gc_cancel.clone());
    tracing::info!("peer GC task spawned (interval = {gc_interval}s)");

    let service = McpSshServer::new().serve(stdio()).await?;
    let waiting_result = service.waiting().await;

    gc_cancel.cancel();
    if let Err(err) = gc_task.await {
        tracing::warn!("peer GC task join failed: {err}");
    }

    waiting_result?;
    Ok(())
}
