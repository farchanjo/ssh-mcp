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
use tokio::sync::mpsc;

use crate::adapters::config::internal::{
    resolve_lane_buffer, resolve_max_subs_per_uri, resolve_max_subs_total, resolve_mux_buffer,
    resolve_peer_gc_interval_s,
};
use crate::adapters::id_generator::uuid::UuidIds;
use crate::adapters::lifecycle::cascade::CascadeCoordinator;
use crate::adapters::lifecycle::leak_watcher::{
    LeakWatcher, LeakWatcherHandle, LeakWatcherProbe, spawn_default,
};
use crate::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
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
use crate::adapters::subscription::channel_mux::ChannelMuxAdapter;
use crate::adapters::subscription::lane_bridge::LaneFanoutBridge;
use crate::adapters::subscription::legacy::{SUBSCRIPTION_REGISTRY, spawn_peer_gc};
use crate::adapters::subscription::memory_registry::MemoryRegistry;
use crate::adapters::subscription::subscriber_lane::SubscriberLaneAdapter;
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
use crate::application::subscription_admin::{
    DaemonStatsUseCase, ListSubsUseCase, PauseSubUseCase, ReplaySubUseCase, ResumeSubUseCase,
    SetFilterUseCase, SubStatsUseCase, SubscribeUseCase, UnsubscribeUseCase,
};
use crate::application::unsubscribe_resource::UnsubscribeResourceUseCase;
use crate::application::upload_file::UploadFileUseCase;
use crate::application::wait_for_pattern::WaitForPatternUseCase;
use crate::application::write_shell::WriteShellUseCase;
use crate::composition::UseCases;
use crate::composition::id_lister::ProdIdLister;
use crate::composition::status_sinks::{
    RepoCommandRegistrationSink, RepoCommandStatusSink, RepoShellRegistrationSink,
    RepoShellStatusSink, RepoTransferRegistrationSink, RepoTransferStatusSink,
};
use crate::infra::mcp::idempotency::IdempotencyCache;
use crate::infra::mcp::peer_handle::{PeerTable, new_peer_table};
use crate::infra::mcp::server::McpSshServer;
use crate::infra::mcp::tool_router::IdLister;
use crate::ports::lifecycle_policy::LifecyclePolicyPort;
use crate::ports::notifier::{DebouncerActivator, ProducerForwarder};
use crate::ports::subscriber_lane::LaneAdmin;

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

/// Bundle returned by [`build_use_cases`] — production wiring plus the
/// per-process side tables the [`McpSshServer`] constructor needs.
///
/// Returned as a tuple-struct via the deconstruction pattern in
/// [`build_use_cases`] callers; the type alias keeps signatures readable.
///
/// The fifth tuple member is the concrete [`RefcountedLifecycleAdapter`]
/// handle the binaries hand to the v5 Phase 3 `SUB_LEAK_RISK` leak
/// watcher (the trait-object handle inside the use case container is
/// dyn-safe but cannot drive `LeakWatcher::spawn` directly).
type ProdWiring = (
    Arc<ProdUseCases>,
    Arc<PeerTable>,
    Arc<IdempotencyCache>,
    Arc<dyn IdLister>,
    Arc<RefcountedLifecycleAdapter<SystemClock>>,
);

/// Build the production [`UseCases`] container plus the shared peer
/// table the rmcp `RmcpPeerHandle` writes into and the dedup cache the
/// v4.7-step5 mutating-tool layer reads.
///
/// All three handles are returned together because the [`McpSshServer`]
/// constructor (`infra::mcp::server`) takes them as a triple — the
/// table feeds the notifier, the cache wraps mutating tools, the use
/// cases drive everything else.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "composition root naturally instantiates every adapter + use case (22 use cases x ~5 lines each); H17 may extract sub-builders per domain"
)]
pub fn build_use_cases() -> ProdWiring {
    let sessions = Arc::new(DashMapSessionRepo::new());
    let commands = Arc::new(DashMapCommandRepo::new());
    let shells = Arc::new(DashMapShellRepo::new());
    let transfers = Arc::new(DashMapTransferRepo::new());

    // Single shared SFTP handle registry: the SSH adapter publishes
    // every session it opens into the registry; the SFTP adapter reads
    // from the same registry. Without sharing the SFTP layer would see
    // an empty session table.
    //
    // Both adapters also receive a status sink so the v4.2 status
    // pump can transition CommandEntity / ShellEntity / TransferEntity
    // into terminal states from the live `RunningCommand` /
    // `RunningShell` / `TransferShared` watch channels owned by the
    // background streaming tasks. Without this bridge `wait`-mode use
    // cases would observe `Running` forever (v4.2 fix).
    let sftp_registry = SshHandleRegistry::new();
    let command_sink = Arc::new(RepoCommandStatusSink::new(Arc::clone(&commands)));
    let shell_sink = Arc::new(RepoShellStatusSink::new(Arc::clone(&shells)));
    let transfer_sink = Arc::new(RepoTransferStatusSink::new(Arc::clone(&transfers)));
    // v4.3 fix: registration sinks bridge the adapter-internal DashMaps
    // into the matching domain repositories so subscribers / readers see
    // the entity the moment it exists. Same handles flow into the use
    // cases (which still insert/remove explicitly), so the sinks
    // intentionally tolerate duplicate-id inserts.
    let command_registration_sink =
        Arc::new(RepoCommandRegistrationSink::new(Arc::clone(&commands)));
    let shell_registration_sink = Arc::new(RepoShellRegistrationSink::new(Arc::clone(&shells)));
    // The transfer registration sink needs a config handle so its
    // adapter-side `register` call honours the per-session transfer cap.
    // Build the config Arc up here (slightly out of order vs the rest of
    // the wiring) so the sink and the use cases share the same handle.
    let config = Arc::new(EnvConfig);
    let transfer_registration_sink = Arc::new(RepoTransferRegistrationSink::new(
        Arc::clone(&transfers),
        Arc::clone(&config),
    ));
    let ssh = Arc::new(
        RusshAdapter::new()
            .with_sftp_registry(sftp_registry.clone())
            .with_command_status_sink(command_sink)
            .with_shell_status_sink(shell_sink)
            .with_command_registration_sink(command_registration_sink)
            .with_shell_registration_sink(shell_registration_sink),
    );
    let sftp = Arc::new(
        RusshSftpAdapter::new(sftp_registry, 256, 10)
            .with_status_sink(transfer_sink)
            .with_registration_sink(transfer_registration_sink),
    );
    #[cfg(feature = "port_forward")]
    let forwards = Arc::new(DashMapForwardRepo::new());

    let peer_table = new_peer_table();
    let notifier = Arc::new(RmcpNotifier::new(Arc::clone(&peer_table)));
    // v5: enforce SSH_MAX_SUBS_PER_URI / SSH_MAX_SUBS_TOTAL on the
    // legacy v4-compat subscribe path so callers that bypass the
    // SubscriberLane layer still hit the documented caps.
    let subscribers = MemoryRegistry::<RmcpNotifier>::from_env(Arc::clone(&notifier));

    let auth = Arc::new(AuthChainAdapter::default_chain());
    let output = Arc::new(RusshOutputAdapter::new(&ssh));

    let clock = Arc::new(SystemClock);
    // `config` was bound earlier (alongside the transfer registration sink)
    // so the sink shares the same handle as every downstream use case.
    let ids = Arc::new(UuidIds);

    // v5 lifecycle adapter: cascade coordinator + refcounted state
    // machine. Held as `Arc<dyn LifecyclePolicyPort>` so use cases stay
    // generics-stable while the v5 wiring rolls out incrementally.
    // Phase 1 leaves the auto-disconnect hook uninstalled — Phase 3
    // wires the DisconnectSessionUseCase callback once the use cases
    // expose a Send + Sync 'static driver.
    let cascade = CascadeCoordinator::new();
    let lifecycle_concrete =
        RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));
    // `Arc<RefcountedLifecycleAdapter<_>>` -> `Arc<dyn LifecyclePolicyPort>`
    // via the standard unsizing coercion. Wrapping via `Arc::clone`
    // would require erasing the type first, so the explicit binding
    // is the cleanest path.
    let lifecycle: Arc<dyn LifecyclePolicyPort> =
        Arc::<RefcountedLifecycleAdapter<SystemClock>>::clone(&lifecycle_concrete);

    // v5 Phase 2: SubscriberLane + ChannelMux. The lane adapter
    // mints per-`SubId` UUIDv7 ids through the same id generator
    // used elsewhere in the wiring. The mux installs an outbound
    // sink that the `ssh-mcp-tail` daemon drains via
    // `composition::embed`; HTTP / stdio binaries keep the receiver
    // alive on this composition root so the mpsc never reports
    // `Closed`.
    let subscriber_lane_adapter = SubscriberLaneAdapter::new(
        Arc::clone(&ids),
        resolve_lane_buffer(),
        resolve_max_subs_per_uri(),
        resolve_max_subs_total(),
    );
    let channel_mux = ChannelMuxAdapter::new();
    let mux_for_sink = Arc::clone(&channel_mux);
    subscriber_lane_adapter.install_rx_sink(Box::new(move |sub_id, rx| {
        mux_for_sink.register_lane(sub_id, rx);
    }));
    let (mux_outbound_tx, _mux_outbound_rx) = mpsc::channel(resolve_mux_buffer());
    channel_mux.install_outbound(mux_outbound_tx);
    let lane_admin: Arc<dyn LaneAdmin> = lane_admin_from(&subscriber_lane_adapter);
    // The `ssh-mcp-tail` daemon (`composition::embed`) drains this
    // receiver into NDJSON stdout. HTTP / stdio binaries keep the
    // receiver alive on this composition root so the mpsc never
    // reports `Closed` and the outbound sink never blocks.
    //
    // v5.3 — install the lane-fanout bridge so the legacy
    // [`MemoryRegistry::broadcast`] pipeline also delivers
    // `notifications/resources/updated` to every `sub_open`
    // lane bound to the broadcast URI. Lane stats (events_sent /
    // bytes_sent) increment on each successful peer notify.
    let lane_bridge =
        LaneFanoutBridge::new(Arc::clone(&subscriber_lane_adapter), Arc::clone(&notifier));
    subscribers.install_lane_bridge(lane_bridge);

    let connect = Arc::new(ConnectSessionUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&sessions),
        Arc::clone(&clock),
        Arc::clone(&ids),
        Arc::clone(&config),
    ));
    let disconnect = Arc::new(DisconnectSessionUseCase::with_lifecycle(
        Arc::clone(&ssh),
        Arc::clone(&sessions),
        Arc::clone(&commands),
        Arc::clone(&shells),
        Arc::clone(&transfers),
        Arc::clone(&lifecycle),
        Arc::clone(&lane_admin),
    ));
    let list_sessions = Arc::new(ListSessionsUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&sessions),
        Arc::clone(&clock),
        Arc::clone(&config),
    ));
    let disconnect_agent = Arc::new(DisconnectAgentUseCase::with_lifecycle(
        Arc::clone(&ssh),
        Arc::clone(&sessions),
        Arc::clone(&commands),
        Arc::clone(&shells),
        Arc::clone(&transfers),
        Arc::clone(&lifecycle),
        Arc::clone(&lane_admin),
    ));

    let execute = Arc::new(
        ExecuteCommandUseCase::new(
            Arc::clone(&ssh),
            Arc::clone(&sessions),
            Arc::clone(&commands),
            Arc::clone(&clock),
            Arc::clone(&ids),
            Arc::clone(&config),
            Arc::clone(&subscribers),
        )
        .with_lifecycle(Arc::clone(&lifecycle)),
    );
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
        Arc::clone(&lifecycle),
    ));

    let open_shell = Arc::new(OpenShellUseCase::new(
        Arc::clone(&ssh),
        Arc::clone(&sessions),
        Arc::clone(&shells),
        Arc::clone(&clock),
        Arc::clone(&ids),
        Arc::clone(&config),
        Arc::clone(&lifecycle),
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
        Arc::clone(&lifecycle),
    ));

    let upload_file = Arc::new(
        UploadFileUseCase::new(
            Arc::clone(&sftp),
            Arc::clone(&sessions),
            Arc::clone(&transfers),
            Arc::clone(&clock),
            Arc::clone(&ids),
            Arc::clone(&config),
            Arc::clone(&subscribers),
        )
        .with_lifecycle(Arc::clone(&lifecycle)),
    );
    let download_file = Arc::new(
        DownloadFileUseCase::new(
            Arc::clone(&sftp),
            Arc::clone(&sessions),
            Arc::clone(&transfers),
            Arc::clone(&clock),
            Arc::clone(&ids),
            Arc::clone(&config),
            Arc::clone(&subscribers),
        )
        .with_lifecycle(Arc::clone(&lifecycle)),
    );
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
    let subscribe_resource = Arc::new(
        SubscribeResourceUseCase::new(
            Arc::clone(&shells),
            Arc::clone(&commands),
            Arc::clone(&transfers),
            Arc::clone(&sessions),
            Arc::clone(&forwards),
            Arc::clone(&subscribers),
            Arc::clone(&lifecycle),
        )
        .with_subscriber_lane(Arc::clone(&lane_admin)),
    );
    #[cfg(not(feature = "port_forward"))]
    let subscribe_resource = Arc::new(
        SubscribeResourceUseCase::new(
            Arc::clone(&shells),
            Arc::clone(&commands),
            Arc::clone(&transfers),
            Arc::clone(&sessions),
            Arc::clone(&subscribers),
            Arc::clone(&lifecycle),
        )
        .with_subscriber_lane(Arc::clone(&lane_admin)),
    );

    let unsubscribe_resource = Arc::new(
        UnsubscribeResourceUseCase::new(Arc::clone(&subscribers), Arc::clone(&lifecycle))
            .with_subscriber_lane(Arc::clone(&lane_admin)),
    );
    let peer_gc = Arc::new(PeerGcUseCase::new(Arc::clone(&subscribers)));

    // v5 Phase 3 — subscription-administration use cases. All share the
    // same `Arc<dyn LaneAdmin>` handle so the channel-mux wiring stays
    // single-sourced.
    let activator: Arc<dyn DebouncerActivator> =
        Arc::<MemoryRegistry<RmcpNotifier>>::clone(&subscribers);
    // Producer-side forwarder: legacy `SUBSCRIPTION_REGISTRY` mirrors
    // every poke / record_bytes into the hexagonal `MemoryRegistry`
    // so `sub_open` lanes track real producer events (not just
    // the 1 s force-flush keepalive tick).
    let forwarder: Arc<dyn ProducerForwarder> =
        Arc::<MemoryRegistry<RmcpNotifier>>::clone(&subscribers);
    SUBSCRIPTION_REGISTRY.install_forwarder(forwarder);
    let sub_subscribe = Arc::new(SubscribeUseCase::with_activator(
        Arc::clone(&lane_admin),
        activator,
    ));
    let sub_unsubscribe = Arc::new(UnsubscribeUseCase::new(Arc::clone(&lane_admin)));
    let sub_pause = Arc::new(PauseSubUseCase::new(Arc::clone(&lane_admin)));
    let sub_resume = Arc::new(ResumeSubUseCase::new(Arc::clone(&lane_admin)));
    let sub_filter = Arc::new(SetFilterUseCase::new(Arc::clone(&lane_admin)));
    let sub_replay = Arc::new(ReplaySubUseCase::new(Arc::clone(&lane_admin)));
    let sub_list = Arc::new(ListSubsUseCase::new(Arc::clone(&lane_admin)));
    let sub_stats = Arc::new(SubStatsUseCase::new(Arc::clone(&lane_admin)));
    let daemon_stats = Arc::new(DaemonStatsUseCase::new(Arc::clone(&lane_admin)));

    let idempotency = Arc::new(IdempotencyCache::from_env());

    #[cfg(feature = "port_forward")]
    let id_lister: Arc<dyn IdLister> = Arc::new(ProdIdLister::new(
        Arc::clone(&sessions),
        Arc::clone(&commands),
        Arc::clone(&shells),
        Arc::clone(&transfers),
        Arc::clone(&forwards),
    ));
    #[cfg(not(feature = "port_forward"))]
    let id_lister: Arc<dyn IdLister> = Arc::new(ProdIdLister::new(
        Arc::clone(&sessions),
        Arc::clone(&commands),
        Arc::clone(&shells),
        Arc::clone(&transfers),
    ));

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
        lifecycle_policy: Arc::clone(&lifecycle),
        sub_subscribe,
        sub_unsubscribe,
        sub_pause,
        sub_resume,
        sub_filter,
        sub_replay,
        sub_list,
        sub_stats,
        daemon_stats,
    });

    (
        use_cases,
        peer_table,
        idempotency,
        id_lister,
        lifecycle_concrete,
    )
}

/// Erase the concrete [`SubscriberLaneAdapter`] handle behind a
/// [`LaneAdmin`] dyn pointer. The conversion uses the implicit
/// `Arc<T>` -> `Arc<dyn Trait>` coercion so the `as_conversions`
/// lint stays happy.
fn lane_admin_from(adapter: &Arc<SubscriberLaneAdapter<UuidIds>>) -> Arc<dyn LaneAdmin> {
    let cloned: Arc<SubscriberLaneAdapter<UuidIds>> = Arc::clone(adapter);
    cloned
}

/// Build a fresh [`McpSshServer`] backed by the production wiring.
///
/// Discards the v5 Phase 3 lifecycle handle. Use [`build_server_with_lifecycle`]
/// when the caller needs to spawn the `SUB_LEAK_RISK` watcher.
#[must_use]
pub fn build_server() -> McpSshServer<ProdUseCases> {
    build_server_with_lifecycle().0
}

/// Build a fresh [`McpSshServer`] and surface the concrete lifecycle
/// adapter for `SUB_LEAK_RISK` watcher wiring.
#[must_use]
pub fn build_server_with_lifecycle() -> (
    McpSshServer<ProdUseCases>,
    Arc<RefcountedLifecycleAdapter<SystemClock>>,
) {
    let (use_cases, peer_table, idempotency, id_lister, lifecycle_concrete) = build_use_cases();
    let server = McpSshServer::<ProdUseCases>::new(use_cases, peer_table, idempotency)
        .with_id_lister(id_lister);
    (server, lifecycle_concrete)
}

/// Build a fresh [`McpSshServer`] with the v5 Phase 3 leak watcher
/// already plumbed in.
///
/// Returns the server (with the watcher wired into both consumer-side
/// surfaces — progress notifications + list render), the concrete
/// lifecycle adapter, and the spawned [`LeakWatcherHandle`] the binary
/// owns until graceful shutdown.
#[must_use]
pub fn build_server_with_leak_watcher() -> (
    McpSshServer<ProdUseCases>,
    Arc<RefcountedLifecycleAdapter<SystemClock>>,
    LeakWatcherHandle,
) {
    let (use_cases, peer_table, idempotency, id_lister, lifecycle_concrete) = build_use_cases();
    let leak_handle = spawn_default(&lifecycle_concrete);
    let watcher_arc: Arc<LeakWatcher> = Arc::new(leak_handle.watcher.clone());
    let server = McpSshServer::<ProdUseCases>::new(use_cases, peer_table, idempotency)
        .with_id_lister(id_lister)
        .with_leak_watcher(watcher_arc);
    (server, lifecycle_concrete, leak_handle)
}

/// Re-export of the [`LeakWatcherProbe`] dyn alias so tests can build a
/// fake probe without reaching into the adapter layer.
pub type DynLeakProbe = Arc<dyn LeakWatcherProbe>;

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

    let app = if path == "/" {
        // axum 0.8 rejects `nest_service("/", ...)` — use `fallback_service`
        // so a root-mounted MCP server still binds successfully.
        Router::new().fallback_service(build_http_service())
    } else {
        Router::new().nest_service(&path, build_http_service())
    }
    .layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(&bind_addr).await?;
    info!("ssh-mcp listening on {bind_addr}{path}");

    let gc_cancel = CancellationToken::new();
    // Peer GC walks the legacy `adapters::subscription::legacy` global
    // today; the parallel hexagonal `application::peer_gc` use case
    // exists but the SSH/SFTP runtime adapters still poke the legacy
    // global directly, so the spawn stays here until the runtime
    // migration completes.
    //
    // TODO(v4.5): the HTTP entry point creates one `McpSshServer` (and
    // therefore one `PeerTable`) per session via the rmcp factory, so
    // the GC pump does not own a single table to prune. The stale
    // entries are reclaimed naturally when the session-scoped server
    // drops; we still pass `None` here so the GC pump compiles uniformly.
    let gc_interval = resolve_peer_gc_interval_s();
    let gc_task = spawn_peer_gc(gc_interval, gc_cancel.clone(), None);
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
    // Stdio runs a single `McpSshServer` for the whole process — share
    // its `PeerTable` with the GC pump so closed peers in the v4 side
    // table are pruned alongside the legacy registry.
    //
    // v5 Phase 3 — `build_server_with_leak_watcher` plumbs the
    // `SUB_LEAK_RISK` watcher into both consumer-side surfaces:
    //   1. notifications/progress (`leak_warn_bridge`) when the tool
    //      call supplies `_meta.progressToken`.
    //   2. WARN: SUB_LEAK_RISK lines on `ssh_list_*` responses.
    let (server, _lifecycle_concrete, leak_handle) = build_server_with_leak_watcher();
    let peer_table = Arc::clone(server.peer_table());
    tracing::info!("SUB_LEAK_RISK watcher spawned + wired into rmcp surface");
    let gc_interval = resolve_peer_gc_interval_s();
    let gc_task = spawn_peer_gc(gc_interval, gc_cancel.clone(), Some(peer_table));
    tracing::info!("peer GC task spawned (interval = {gc_interval}s)");

    let service = server.serve(stdio()).await?;
    let waiting_result = service.waiting().await;

    gc_cancel.cancel();
    leak_handle.cancel.cancel();
    if let Err(err) = gc_task.await {
        tracing::warn!("peer GC task join failed: {err}");
    }
    if let Err(err) = leak_handle.task.await {
        tracing::warn!("SUB_LEAK_RISK watcher join failed: {err}");
    }

    waiting_result?;
    Ok(())
}
