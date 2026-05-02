//! HTTP transport entry point for ssh-mcp.
//!
//! Hosts `McpSshServer` (rmcp `ServerHandler`) behind an axum router using
//! rmcp's `StreamableHttpService` (Streamable HTTP MCP transport with SSE).
//!
//! Endpoint: `POST/GET /` (configurable via `MCP_HTTP_PATH`).
//!
//! Notifications (`notifications/resources/updated` etc.) are pushed over the
//! SSE channel established by `StreamableHttpService` per session.

#![deny(warnings)]
#![deny(clippy::unwrap_used)]

use std::sync::Arc;

use axum::Router;
use dotenvy::dotenv;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use ssh_mcp::mcp::server::McpSshServer;
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

    axum::serve(listener, app).await?;

    Ok(())
}
