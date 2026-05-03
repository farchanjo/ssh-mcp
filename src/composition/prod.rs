//! Production wiring.
//!
//! Pins concrete adapter types and exposes the two transport entry points
//! [`run_http`] and [`run_stdio`]. After H16 both entry points instantiate
//! the v4 [`crate::infra::mcp::server::McpSshServer`] over the
//! [`crate::composition::UseCases`] container; H17 fills the rendering and
//! H17.5 deletes the legacy v3 transport runtime.
//!
//! The adapters are pinned via [`type ConcreteX`] aliases so the
//! [`build_use_cases`] helper (and the [`build_server`] factory) keeps a
//! single, readable signature.

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

use crate::adapters::auth::chain::AuthChainAdapter;
use crate::adapters::clock::system::SystemClock;
use crate::adapters::config::env::EnvConfig;
use crate::adapters::id_generator::uuid::UuidIds;
use crate::adapters::notifier::rmcp_adapter::RmcpNotifier;
use crate::adapters::output_stream::russh_output::RusshOutputAdapter;
use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
#[cfg(feature = "port_forward")]
use crate::adapters::repo::dashmap::forward::DashMapForwardRepo;
use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
use crate::adapters::sftp::russh_sftp_adapter::{RusshSftpAdapter, SshHandleRegistry};
use crate::adapters::ssh::russh_adapter::RusshAdapter;
use crate::adapters::subscription::memory_registry::MemoryRegistry;
use crate::application::cancel_command::CancelCommandUseCase;
use crate::application::close_shell::CloseShellUseCase;
use crate::application::connect_session::ConnectSessionUseCase;
use crate::application::disconnect_agent::DisconnectAgentUseCase;
use crate::application::disconnect_session::DisconnectSessionUseCase;
use crate::application::download_file::DownloadFileUseCase;
use crate::application::execute_command::ExecuteCommandUseCase;
#[cfg(feature = "port_forward")]
use crate::application::forward_port::ForwardPortUseCase;
use crate::application::get_command_output::GetCommandOutputUseCase;
use crate::application::get_transfer_progress::GetTransferProgressUseCase;
use crate::application::list_commands::ListCommandsUseCase;
use crate::application::list_resources::ListResourcesUseCase;
use crate::application::list_sessions::ListSessionsUseCase;
use crate::application::open_shell::OpenShellUseCase;
use crate::application::peer_gc::PeerGcUseCase;
use crate::application::read_resource::ReadResourceUseCase;
use crate::application::read_shell::ReadShellUseCase;
use crate::application::send_key::SendKeyUseCase;
use crate::application::subscribe_resource::SubscribeResourceUseCase;
use crate::application::unsubscribe_resource::UnsubscribeResourceUseCase;
use crate::application::upload_file::UploadFileUseCase;
use crate::application::wait_for_pattern::WaitForPatternUseCase;
use crate::application::write_shell::WriteShellUseCase;
use crate::composition::UseCases;
use crate::infra::mcp::peer_handle::{PeerTable, new_peer_table};
use crate::infra::mcp::server::McpSshServer;
use crate::mcp::config::resolve_peer_gc_interval_s;
use crate::mcp::subscription::spawn_peer_gc;

/// Boxed transport error returned by the v4 runtime helpers. Same shape
/// as the legacy v3 `RuntimeError` so binaries do not need to change.
pub type RuntimeError = Box<dyn Error + Send + Sync>;

const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 8000;
const DEFAULT_HTTP_PATH: &str = "/";

// ---------------------------------------------------------------------------
// Concrete adapter type aliases
// ---------------------------------------------------------------------------

/// Production SSH client adapter (russh wrapper).
type ConcreteSsh = RusshAdapter;
/// Production SFTP client adapter (russh-sftp wrapper).
type ConcreteSftp = RusshSftpAdapter;
/// Production session repository adapter (`DashMap`).
type ConcreteSessionRepo = DashMapSessionRepo;
/// Production command repository adapter (`DashMap`).
type ConcreteCommandRepo = DashMapCommandRepo;
/// Production shell repository adapter (`DashMap`).
type ConcreteShellRepo = DashMapShellRepo;
/// Production transfer repository adapter (`DashMap`).
type ConcreteTransferRepo = DashMapTransferRepo;
/// Production forward repository adapter (`DashMap`, feature-gated).
#[cfg(feature = "port_forward")]
type ConcreteForwardRepo = DashMapForwardRepo;
/// Production rmcp notifier adapter.
type ConcreteNotifier = RmcpNotifier;
/// Production auth strategy adapter (chain of password/key/agent).
type ConcreteAuth = AuthChainAdapter;
/// Production output-stream adapter (lock-free shared with the russh adapter).
type ConcreteOutput = RusshOutputAdapter;
/// Production subscription registry adapter.
type ConcreteSubscribers = MemoryRegistry<ConcreteNotifier>;
/// Production system clock adapter.
type ConcreteClock = SystemClock;
/// Production environment-backed config adapter.
type ConcreteConfig = EnvConfig;
/// Production UUID-based id generator.
type ConcreteIds = UuidIds;

/// Concrete `UseCases` shape pinned to the production adapters above.
#[cfg(feature = "port_forward")]
pub type ProdUseCases = UseCases<
    ConcreteSsh,
    ConcreteSftp,
    ConcreteSessionRepo,
    ConcreteCommandRepo,
    ConcreteShellRepo,
    ConcreteTransferRepo,
    ConcreteForwardRepo,
    ConcreteNotifier,
    ConcreteAuth,
    ConcreteOutput,
    ConcreteSubscribers,
    ConcreteClock,
    ConcreteConfig,
    ConcreteIds,
>;

#[cfg(not(feature = "port_forward"))]
pub type ProdUseCases = UseCases<
    ConcreteSsh,
    ConcreteSftp,
    ConcreteSessionRepo,
    ConcreteCommandRepo,
    ConcreteShellRepo,
    ConcreteTransferRepo,
    ConcreteNotifier,
    ConcreteAuth,
    ConcreteOutput,
    ConcreteSubscribers,
    ConcreteClock,
    ConcreteConfig,
    ConcreteIds,
>;

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

/// Build the production [`UseCases`] container plus the shared peer
/// table the rmcp `RmcpPeerHandle` writes into.
///
/// Both handles are returned together because the [`McpSshServer`]
/// constructor (`infra::mcp::server`) takes both — the table feeds the
/// notifier, the use cases drive everything else.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "composition root naturally instantiates every adapter + use case (22 use cases x ~5 lines each); H17 may extract sub-builders per domain"
)]
pub fn build_use_cases() -> (Arc<ProdUseCases>, Arc<PeerTable>) {
    let ssh = Arc::new(RusshAdapter::new());
    let sftp_registry = SshHandleRegistry::new();
    let sftp = Arc::new(RusshSftpAdapter::new(sftp_registry, 256, 10));

    let sessions = Arc::new(DashMapSessionRepo::new());
    let commands = Arc::new(DashMapCommandRepo::new());
    let shells = Arc::new(DashMapShellRepo::new());
    let transfers = Arc::new(DashMapTransferRepo::new());
    #[cfg(feature = "port_forward")]
    let forwards = Arc::new(DashMapForwardRepo::new());

    let peer_table = new_peer_table();
    let notifier = Arc::new(RmcpNotifier::new(Arc::clone(&peer_table)));
    let subscribers = MemoryRegistry::<RmcpNotifier>::new(Arc::clone(&notifier));

    let auth = Arc::new(AuthChainAdapter::default_chain());
    let output = Arc::new(RusshOutputAdapter::new(&ssh));

    let clock = Arc::new(SystemClock);
    let config = Arc::new(EnvConfig);
    let ids = Arc::new(UuidIds);

    let connect = Arc::new(ConnectSessionUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&sessions),
        Arc::clone(&clock),
        Arc::clone(&ids),
        Arc::clone(&config),
    ));
    let disconnect = Arc::new(DisconnectSessionUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&sessions),
        Arc::clone(&commands),
        Arc::clone(&shells),
        Arc::clone(&transfers),
    ));
    let list_sessions = Arc::new(ListSessionsUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&sessions),
        Arc::clone(&clock),
        Arc::clone(&config),
    ));
    let disconnect_agent = Arc::new(DisconnectAgentUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&sessions),
        Arc::clone(&commands),
        Arc::clone(&shells),
        Arc::clone(&transfers),
    ));

    let execute = Arc::new(ExecuteCommandUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&sessions),
        Arc::clone(&commands),
        Arc::clone(&clock),
        Arc::clone(&ids),
        Arc::clone(&config),
        Arc::clone(&subscribers),
    ));
    let get_command_output = Arc::new(GetCommandOutputUseCase::new(
        Arc::clone(&commands),
        Arc::clone(&output),
    ));
    let list_commands = Arc::new(ListCommandsUseCase::new(
        Arc::clone(&commands),
        Arc::clone(&config),
    ));
    let cancel_command = Arc::new(CancelCommandUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&commands),
        Arc::clone(&output),
    ));

    let open_shell = Arc::new(OpenShellUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&sessions),
        Arc::clone(&shells),
        Arc::clone(&clock),
        Arc::clone(&ids),
        Arc::clone(&config),
    ));
    let write_shell = Arc::new(WriteShellUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&shells),
        Arc::clone(&clock),
    ));
    let send_key = Arc::new(SendKeyUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&shells),
        Arc::clone(&clock),
    ));
    let read_shell = Arc::new(ReadShellUseCase::new(
        Arc::clone(&shells),
        Arc::clone(&output),
        Arc::clone(&clock),
        Arc::clone(&config),
    ));
    let wait_for_pattern = Arc::new(WaitForPatternUseCase::new(
        Arc::clone(&shells),
        Arc::clone(&output),
        Arc::clone(&clock),
        Arc::clone(&config),
    ));
    let close_shell = Arc::new(CloseShellUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&shells),
    ));

    let upload_file = Arc::new(UploadFileUseCase::new(
        Arc::clone(&sftp),
        Arc::clone(&sessions),
        Arc::clone(&transfers),
        Arc::clone(&clock),
        Arc::clone(&ids),
        Arc::clone(&config),
        Arc::clone(&subscribers),
    ));
    let download_file = Arc::new(DownloadFileUseCase::new(
        Arc::clone(&sftp),
        Arc::clone(&sessions),
        Arc::clone(&transfers),
        Arc::clone(&clock),
        Arc::clone(&ids),
        Arc::clone(&config),
        Arc::clone(&subscribers),
    ));
    let get_transfer_progress = Arc::new(GetTransferProgressUseCase::new(Arc::clone(&transfers)));

    #[cfg(feature = "port_forward")]
    let forward_port = Arc::new(ForwardPortUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&sessions),
        Arc::clone(&forwards),
        Arc::clone(&clock),
        Arc::clone(&ids),
        Arc::clone(&config),
    ));

    #[cfg(feature = "port_forward")]
    let list_resources = Arc::new(ListResourcesUseCase::new(
        Arc::clone(&sessions),
        Arc::clone(&commands),
        Arc::clone(&shells),
        Arc::clone(&transfers),
        Arc::clone(&forwards),
    ));
    #[cfg(not(feature = "port_forward"))]
    let list_resources = Arc::new(ListResourcesUseCase::new(
        Arc::clone(&sessions),
        Arc::clone(&commands),
        Arc::clone(&shells),
        Arc::clone(&transfers),
    ));

    #[cfg(feature = "port_forward")]
    let read_resource = Arc::new(ReadResourceUseCase::new(
        Arc::clone(&shells),
        Arc::clone(&commands),
        Arc::clone(&transfers),
        Arc::clone(&sessions),
        Arc::clone(&forwards),
        Arc::clone(&output),
        Arc::clone(&subscribers),
    ));
    #[cfg(not(feature = "port_forward"))]
    let read_resource = Arc::new(ReadResourceUseCase::new(
        Arc::clone(&shells),
        Arc::clone(&commands),
        Arc::clone(&transfers),
        Arc::clone(&sessions),
        Arc::clone(&output),
        Arc::clone(&subscribers),
    ));

    #[cfg(feature = "port_forward")]
    let subscribe_resource = Arc::new(SubscribeResourceUseCase::new(
        Arc::clone(&shells),
        Arc::clone(&commands),
        Arc::clone(&transfers),
        Arc::clone(&sessions),
        Arc::clone(&forwards),
        Arc::clone(&subscribers),
    ));
    #[cfg(not(feature = "port_forward"))]
    let subscribe_resource = Arc::new(SubscribeResourceUseCase::new(
        Arc::clone(&shells),
        Arc::clone(&commands),
        Arc::clone(&transfers),
        Arc::clone(&sessions),
        Arc::clone(&subscribers),
    ));

    let unsubscribe_resource = Arc::new(UnsubscribeResourceUseCase::new(Arc::clone(&subscribers)));
    let peer_gc = Arc::new(PeerGcUseCase::new(Arc::clone(&subscribers)));

    let use_cases = Arc::new(UseCases {
        connect,
        disconnect,
        list_sessions,
        disconnect_agent,
        execute,
        get_command_output,
        list_commands,
        cancel_command,
        open_shell,
        write_shell,
        send_key,
        read_shell,
        wait_for_pattern,
        close_shell,
        upload_file,
        download_file,
        get_transfer_progress,
        #[cfg(feature = "port_forward")]
        forward_port,
        list_resources,
        read_resource,
        subscribe_resource,
        unsubscribe_resource,
        peer_gc,
        auth,
        notifier,
    });

    (use_cases, peer_table)
}

/// Build a fresh [`McpSshServer`] backed by the production wiring.
#[must_use]
pub fn build_server() -> McpSshServer<ProdUseCases> {
    let (use_cases, peer_table) = build_use_cases();
    McpSshServer::<ProdUseCases>::new(use_cases, peer_table)
}

// ---------------------------------------------------------------------------
// Transport entry points
// ---------------------------------------------------------------------------

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

fn build_http_service() -> StreamableHttpService<McpSshServer<ProdUseCases>, LocalSessionManager> {
    StreamableHttpService::new(
        || Ok(build_server()),
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

/// Run the v4 HTTP transport (axum + rmcp `StreamableHttpService`).
///
/// # Errors
///
/// Propagates any error returned by the transport stack (listener bind
/// failure, axum graceful-shutdown failure, etc).
pub async fn run_http() -> Result<(), RuntimeError> {
    dotenv().ok();
    install_subscriber(io::stdout);

    let (bind_addr, path) = resolve_http_bind();
    info!(addr = %bind_addr, %path, "starting ssh-mcp HTTP transport (v4 hexagonal)");

    let app = Router::new()
        .nest_service(&path, build_http_service())
        .layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(&bind_addr).await?;
    info!("ssh-mcp listening on {bind_addr}{path}");

    let gc_cancel = CancellationToken::new();
    // Peer GC is wired into the v3 `mcp::subscription` global today; H17.5
    // removes the v3 path. Until then the v4 use case
    // `application::peer_gc` is invoked through the same legacy spawn so
    // existing tests stay green.
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

/// Run the v4 stdio transport (rmcp `serve(stdio())`).
///
/// # Errors
///
/// Propagates any error returned by the transport stack (rmcp service
/// errors, transport setup failures, etc).
pub async fn run_stdio() -> Result<(), RuntimeError> {
    install_subscriber(io::stderr);
    tracing::info!("starting ssh-mcp stdio transport (v4 hexagonal)");

    let gc_cancel = CancellationToken::new();
    let gc_interval = resolve_peer_gc_interval_s();
    let gc_task = spawn_peer_gc(gc_interval, gc_cancel.clone());
    tracing::info!("peer GC task spawned (interval = {gc_interval}s)");

    let service = build_server().serve(stdio()).await?;
    let waiting_result = service.waiting().await;

    gc_cancel.cancel();
    if let Err(err) = gc_task.await {
        tracing::warn!("peer GC task join failed: {err}");
    }

    waiting_result?;
    Ok(())
}
