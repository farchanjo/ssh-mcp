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
            .finish()
    }
}
