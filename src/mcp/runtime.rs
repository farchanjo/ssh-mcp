//! v3 transport runtime — extracted from the legacy binaries so the v4
//! composition root can delegate to it while concrete adapters land
//! incrementally in etapas H3-H9.
//!
//! Both entry points stay fully functional. Once `composition::prod` takes
//! ownership of the bootstrap (etapas H10-H17) these helpers will be
//! removed.

#![deny(clippy::unwrap_used)]

use std::env;
use std::error::Error;
use std::io;
use std::sync::Arc;

use axum::Router;
use dotenvy::dotenv;
use rmcp::ServiceExt;
use rmcp::transport::io::stdio;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

use crate::mcp::config::resolve_peer_gc_interval_s;
use crate::mcp::server::McpSshServer;
use crate::mcp::subscription::spawn_peer_gc;

/// Boxed transport error returned by the v3 runtime helpers.
pub type RuntimeError = Box<dyn Error + Send + Sync>;

const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 8000;
const DEFAULT_HTTP_PATH: &str = "/";

fn install_subscriber<W>(make_writer: W)
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(make_writer)
        .init();
}

fn resolve_http_bind() -> (String, String) {
    let host = env::var("MCP_HOST").unwrap_or_else(|_| DEFAULT_HOST.to_string());
    let port: u16 = env::var("MCP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let path = env::var("MCP_HTTP_PATH").unwrap_or_else(|_| DEFAULT_HTTP_PATH.to_string());
    (format!("{host}:{port}"), path)
}

fn build_http_service() -> StreamableHttpService<McpSshServer, LocalSessionManager> {
    StreamableHttpService::new(
        || Ok(McpSshServer::new()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    )
}

async fn graceful_shutdown(gc_cancel: CancellationToken) {
    match signal::ctrl_c().await {
        Ok(()) => info!("Ctrl-C received, shutting down"),
        Err(err) => tracing::warn!("ctrl_c handler failed: {err}"),
    }
    gc_cancel.cancel();
}

/// Run the legacy v3 HTTP transport (axum + rmcp `StreamableHttpService`).
///
/// Mirrors the previous `src/main.rs` entry point: load `.env`, install the
/// tracing subscriber, bind the configured address, spawn the peer-GC
/// task, and serve until Ctrl-C.
///
/// # Errors
///
/// Returns the underlying axum / IO error if the listener cannot be bound
/// or `axum::serve` fails to graceful-shutdown.
pub async fn run_http() -> Result<(), RuntimeError> {
    dotenv().ok();
    install_subscriber(io::stdout);

    let (bind_addr, path) = resolve_http_bind();
    info!(addr = %bind_addr, %path, "starting ssh-mcp HTTP transport (rmcp 1.6)");

    let app = Router::new()
        .nest_service(&path, build_http_service())
        .layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(&bind_addr).await?;
    info!("ssh-mcp listening on {bind_addr}{path}");

    let gc_cancel = CancellationToken::new();
    let gc_interval = resolve_peer_gc_interval_s();
    let gc_task = spawn_peer_gc(gc_interval, gc_cancel.clone());
    info!("peer GC task spawned (interval = {gc_interval}s)");

    axum::serve(listener, app)
        .with_graceful_shutdown(graceful_shutdown(gc_cancel))
        .await?;

    if let Err(err) = gc_task.await {
        tracing::warn!("peer GC task join failed: {err}");
    }
    Ok(())
}

/// Run the legacy v3 stdio transport.
///
/// Mirrors the previous `src/bin/ssh_mcp_stdio.rs` entry point: install the
/// tracing subscriber on stderr, spawn the peer-GC task, drive
/// `McpSshServer::serve(stdio())`, then wait for the service to terminate.
///
/// # Errors
///
/// Returns the underlying rmcp service error if the transport setup or
/// `serve_loop` fails.
pub async fn run_stdio() -> Result<(), RuntimeError> {
    install_subscriber(io::stderr);
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
