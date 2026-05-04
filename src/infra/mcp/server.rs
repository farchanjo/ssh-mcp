//! `McpSshServer<UC>` — v4 inbound MCP entry point.
//!
//! Generic over the concrete [`crate::composition::UseCases`] container so
//! the production binary picks one set of adapters and the (future H18)
//! test harness picks another. The struct owns:
//!
//! - `use_cases: Arc<UC>` — every use case `execute(...)` call lives
//!   behind this handle. The Arc shares cheaply across the rmcp tool /
//!   resource fan-out without forcing the `UseCases` container to be
//!   `Clone`.
//! - `peer_table: Arc<crate::adapters::notifier::rmcp_peer::PeerTable>`
//!   — the shared lookup table the [`crate::infra::mcp::peer_handle`]
//!   wrapper writes into. `subscribe`/`read_resource` need to wrap the
//!   incoming `rmcp::Peer<RoleServer>` into a
//!   [`crate::infra::mcp::peer_handle::RmcpPeerHandle`] before handing
//!   it to the use cases; the table owned here gives the
//!   [`crate::adapters::notifier::rmcp_adapter::RmcpNotifier`] a stable
//!   way to resolve the peer back to its rmcp handle.
//!
//! ## Why no `tool_router` field
//!
//! The rmcp 1.5 `#[tool_router]` macro generates a `Self::tool_router()`
//! associated function the `#[tool_handler]` macro calls directly; the
//! field is no longer required (see the upstream macro doc:
//! "in most cases you do not need to store the router in a field"). We
//! drop the field outright instead of carrying a dead one to satisfy the
//! `dead_code` lint.
//!
//! The `ServerHandler` impl + `tool_router` impl live in
//! [`super::tool_router`] alongside the per-tool stub args.

use core::fmt;
use std::sync::Arc;

use crate::adapters::lifecycle::leak_watcher::{LeakWatcher, LeakWatcherProbe};

use super::idempotency::IdempotencyCache;
use super::peer_handle::PeerTable;
use super::tool_router::{IdLister, noop_id_lister};

/// v4 MCP server handler. Generic over the concrete [`crate::composition::UseCases`]
/// container. The production binary instantiates one wiring; tests (H18) pick
/// fakes.
pub struct McpSshServer<UC>
where
    UC: Send + Sync + 'static,
{
    /// Shared use cases container. `Arc` so the rmcp fan-out across
    /// concurrent JSON-RPC requests is cheap.
    pub(super) use_cases: Arc<UC>,
    /// Shared peer-id -> rmcp peer table (shared with the
    /// [`crate::adapters::notifier::rmcp_adapter::RmcpNotifier`]). Wrapping
    /// `rmcp::Peer<RoleServer>` into
    /// [`crate::infra::mcp::peer_handle::RmcpPeerHandle`] mutates this
    /// table, so `subscribe` and `read_resource` share the handle.
    pub(super) peer_table: Arc<PeerTable>,
    /// v4.7-step5 dedup cache for mutating tool calls. Shared across
    /// every tool fan-out so two requests with the same
    /// `_meta.idempotency_key` hit the same entry.
    pub(super) idempotency: Arc<IdempotencyCache>,
    /// v4.7-step6 closest-match adapter. Used by the tool router to
    /// enumerate live `SESSION` / `SHELL` / `COMMAND` / `TRANSFER` /
    /// `FORWARD` ids when augmenting `NOT_FOUND` error details.
    /// Defaults to a no-op lister so tests can construct the server
    /// without repository wiring.
    pub(super) id_lister: Arc<dyn IdLister>,
    /// v5 Phase 3 — production [`LeakWatcher`] handle the rmcp tool
    /// layer pumps onto the originating `progressToken` channel via
    /// [`crate::infra::mcp::leak_warn_bridge`]. `None` when the
    /// transport is not wired with a watcher (e.g. unit tests or the
    /// HTTP entry point that builds one server per session).
    pub(super) leak_watcher: Option<Arc<LeakWatcher>>,
    /// v5 Phase 3 — read-only probe consumed by the list renderers
    /// (`ssh_list_sessions`, `ssh_list_commands`, `resources/list`).
    /// Defaults to `None` so tests construct the server without
    /// touching the watcher; production wiring shares the
    /// [`LeakWatcher`] handle behind both this and `leak_watcher`.
    pub(super) leak_probe: Option<Arc<dyn LeakWatcherProbe>>,
}

impl<UC> McpSshServer<UC>
where
    UC: Send + Sync + 'static,
{
    /// Build the server with an already-shared use case container, the
    /// per-process peer table, and the idempotency cache. Uses a
    /// no-op id lister; call [`Self::with_id_lister`] to plug in a
    /// repository-backed lister for closest-match suggestions.
    #[must_use]
    pub fn from_parts(
        use_cases: Arc<UC>,
        peer_table: Arc<PeerTable>,
        idempotency: Arc<IdempotencyCache>,
    ) -> Self {
        Self {
            use_cases,
            peer_table,
            idempotency,
            id_lister: noop_id_lister(),
            leak_watcher: None,
            leak_probe: None,
        }
    }

    /// Replace the (default no-op) id lister with a repository-backed
    /// adapter. Used by the production composition root to wire the
    /// v4.7-step6 closest-match suggestions on `NOT_FOUND`.
    #[must_use]
    pub fn with_id_lister(mut self, lister: Arc<dyn IdLister>) -> Self {
        self.id_lister = lister;
        self
    }

    /// Plug a [`LeakWatcher`] into the server.
    ///
    /// Sharing the watcher here installs **both** consumer-side
    /// wirings at once:
    ///
    /// 1. The rmcp tool layer pumps `notifications/progress` carrying
    ///    `WARN: SUB_LEAK_RISK <uri>` lines through
    ///    [`crate::infra::mcp::leak_warn_bridge`] for tool calls that
    ///    supply `_meta.progressToken`.
    /// 2. The list renderers (`ssh_list_sessions`, `ssh_list_commands`,
    ///    `resources/list`) append `WARN: SUB_LEAK_RISK <uri>` lines
    ///    for resources currently flagged by the watcher.
    #[must_use]
    pub fn with_leak_watcher(mut self, watcher: Arc<LeakWatcher>) -> Self {
        // Implicit `Arc<T>` -> `Arc<dyn Trait>` coercion via a typed
        // local binding keeps the `as_conversions` lint silent. The
        // same pattern is used elsewhere in the wiring (see
        // `composition::prod::lane_admin_from`).
        let probe: Arc<dyn LeakWatcherProbe> = Arc::<LeakWatcher>::clone(&watcher);
        self.leak_probe = Some(probe);
        self.leak_watcher = Some(watcher);
        self
    }

    /// Plug a custom [`LeakWatcherProbe`]. Useful for tests that want to
    /// drive the list-render injection path without spawning a real
    /// watcher.
    #[must_use]
    pub fn with_leak_probe(mut self, probe: Arc<dyn LeakWatcherProbe>) -> Self {
        self.leak_probe = Some(probe);
        self
    }

    /// Borrow the underlying use case container. Test/observability helper.
    #[must_use]
    pub const fn use_cases(&self) -> &Arc<UC> {
        &self.use_cases
    }

    /// Borrow the shared peer table. Test/observability helper.
    #[must_use]
    pub const fn peer_table(&self) -> &Arc<PeerTable> {
        &self.peer_table
    }

    /// Borrow the shared idempotency cache. Test/observability helper.
    #[must_use]
    pub const fn idempotency(&self) -> &Arc<IdempotencyCache> {
        &self.idempotency
    }

    /// Borrow the id lister adapter.
    #[must_use]
    pub const fn id_lister(&self) -> &Arc<dyn IdLister> {
        &self.id_lister
    }

    /// Borrow the production leak watcher when wired. Used by the rmcp
    /// tool layer to spawn the `notifications/progress` bridge per tool
    /// call.
    #[must_use]
    pub const fn leak_watcher(&self) -> Option<&Arc<LeakWatcher>> {
        self.leak_watcher.as_ref()
    }

    /// Borrow the read-only leak probe when wired. Used by the list
    /// renderers to inject `WARN: SUB_LEAK_RISK <uri>` lines into
    /// `ssh_list_sessions`, `ssh_list_commands`, and the resource list
    /// path.
    #[must_use]
    pub const fn leak_probe(&self) -> Option<&Arc<dyn LeakWatcherProbe>> {
        self.leak_probe.as_ref()
    }
}

impl<UC> fmt::Debug for McpSshServer<UC>
where
    UC: Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpSshServer")
            .field("use_cases", &"<elided>")
            .field("peer_table", &"<elided>")
            .field("idempotency", &self.idempotency)
            .field("id_lister", &"<dyn IdLister>")
            .field(
                "leak_watcher",
                &self.leak_watcher.as_ref().map_or("<unset>", |_| "<wired>"),
            )
            .field(
                "leak_probe",
                &self.leak_probe.as_ref().map_or("<unset>", |_| "<wired>"),
            )
            .finish()
    }
}
