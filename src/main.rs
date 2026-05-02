//! HTTP transport entry point for ssh-mcp.
//!
//! Hosts `McpSshServer` (rmcp `ServerHandler`) behind an axum router using
//! rmcp's `StreamableHttpService` (Streamable HTTP MCP transport with SSE).
//!
//! Endpoint: `POST/GET /` (configurable via `MCP_HTTP_PATH`).
//!
//! Notifications (`notifications/resources/updated` etc.) are pushed over the
//! SSE channel established by `StreamableHttpService` per session.
//!
//! A background peer-GC task scans `SUBSCRIPTION_REGISTRY` on the interval
//! configured by `SSH_MCP_PEER_GC_INTERVAL_S` and drops peers whose rmcp
//! transport has closed (rmcp 1.6 does not surface a peer-disconnect
//! callback). The task is cancelled cleanly on Ctrl-C.

#![deny(warnings)]
#![deny(clippy::unwrap_used)]

use std::sync::Arc;

use axum::Router;
use dotenvy::dotenv;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use ssh_mcp::mcp::config::resolve_peer_gc_interval_s;
use ssh_mcp::mcp::server::McpSshServer;
use ssh_mcp::mcp::subscription::spawn_peer_gc;
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let host = std::env::var("MCP_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port: u16 = std::env::var("MCP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8000);
    let path = std::env::var("MCP_HTTP_PATH").unwrap_or_else(|_| "/".to_string());
    let bind_addr = format!("{host}:{port}");

    info!(addr = %bind_addr, %path, "starting ssh-mcp HTTP transport (rmcp 1.6)");

    let service = StreamableHttpService::new(
        || Ok(McpSshServer::new()),
        Arc::new(
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
        ),
        StreamableHttpServerConfig::default(),
    );

    let app = Router::new()
        .nest_service(&path, service)
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("ssh-mcp listening on {bind_addr}{path}");

    let gc_cancel = CancellationToken::new();
    let gc_interval = resolve_peer_gc_interval_s();
    let gc_task = spawn_peer_gc(gc_interval, gc_cancel.clone());
    info!("peer GC task spawned (interval = {gc_interval}s)");

    let shutdown = async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => info!("Ctrl-C received, shutting down"),
            Err(err) => tracing::warn!("ctrl_c handler failed: {err}"),
        }
        gc_cancel.cancel();
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;

    if let Err(err) = gc_task.await {
        tracing::warn!("peer GC task join failed: {err}");
    }

    Ok(())
}
