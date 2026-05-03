//! Type-alias surface for the v4 [`RmcpPeerHandle`] adapter used by the
//! rmcp MCP transport.
//!
//! The adapter itself lives under
//! [`crate::adapters::notifier::rmcp_peer`] (it is paired with the
//! [`crate::adapters::notifier::rmcp_adapter::RmcpNotifier`] and shares
//! the per-process
//! [`crate::adapters::notifier::rmcp_peer::PeerTable`]). This module
//! surfaces the same symbols under the `infra::mcp` namespace so the H16
//! wiring code stays self-contained: every other MCP-facing type the
//! layer needs lives in [`super`].
//!
//! The wrapper performs three side-effects:
//!
//! 1. Mints a fresh [`crate::domain::ids::PeerId`] (`UUIDv4`) at
//!    construction.
//! 2. Registers the live [`rmcp::Peer<rmcp::RoleServer>`] in the
//!    composition-root [`PeerTable`] so the
//!    [`crate::adapters::notifier::rmcp_adapter::RmcpNotifier`] can fan
//!    out `notifications/resources/updated` deliveries.
//! 3. Removes itself from the table on `Drop` so a closed transport
//!    naturally turns into a not-found in the notifier.
//!
//! Implements [`crate::ports::notifier::PeerHandle`] so the use-case
//! layer only ever sees the dyn-safe port surface.
//!
//! ## Why type aliases instead of `pub use`
//!
//! The crate-wide [`clippy::pub_use`] lint forbids `pub use`
//! re-exports; we therefore use [`type`] aliases for the two structs
//! plus a thin wrapper for the constructor function. The aliased path
//! resolves to the same nominal type so callers can interchange the
//! `crate::adapters::...` form and the `crate::infra::mcp::peer_handle`
//! form without losing trait bounds.

use std::sync::Arc;

use crate::adapters::notifier::rmcp_peer;

/// Alias for the shared peer-id -> rmcp peer lookup table.
pub type PeerTable = rmcp_peer::PeerTable;

/// Alias for the production [`crate::ports::notifier::PeerHandle`] adapter.
pub type RmcpPeerHandle = rmcp_peer::RmcpPeerHandle;

/// Build a fresh, empty [`PeerTable`] wrapped in an [`Arc`].
///
/// Thin wrapper around
/// [`crate::adapters::notifier::rmcp_peer::new_peer_table`] kept here so
/// the composition root can address the constructor through the
/// `infra::mcp` module path.
#[must_use]
pub fn new_peer_table() -> Arc<PeerTable> {
    rmcp_peer::new_peer_table()
}
