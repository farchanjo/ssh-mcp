//! [`PeerHandle`] adapter that wraps [`rmcp::Peer<RoleServer>`].
//!
//! The hexagonal port [`crate::ports::notifier::PeerHandle`] only exposes
//! `id` + `is_closed` so it stays dyn-safe and free of any rmcp dependency.
//! That leaves the production [`crate::adapters::notifier::rmcp_adapter::RmcpNotifier`]
//! with no way to recover the live `Peer<RoleServer>` from an
//! `Arc<dyn PeerHandle>` (Rust trait objects cannot be downcast without an
//! [`std::any::Any`] super-trait).
//!
//! The fix is a small per-process **side table**: every [`RmcpPeerHandle`]
//! registers its underlying rmcp peer in a shared [`PeerTable`] at
//! construction and removes itself on `Drop`. The notifier then resolves
//! `peer.id()` -> rmcp peer in O(1) via the same table without touching
//! the port surface.

use std::sync::Arc;

use dashmap::DashMap;
use rmcp::Peer;
use rmcp::RoleServer;
use uuid::Uuid;

use crate::domain::ids::PeerId;
use crate::ports::notifier::PeerHandle;

/// Shared lookup from a stable [`PeerId`] to the live
/// [`rmcp::Peer<RoleServer>`] that backs it.
///
/// The composition root creates exactly one [`PeerTable`] per process and
/// shares the [`Arc`] with both [`RmcpPeerHandle::new`] (writer) and
/// [`crate::adapters::notifier::rmcp_adapter::RmcpNotifier::new`] (reader).
pub type PeerTable = DashMap<PeerId, Peer<RoleServer>>;

/// Build a fresh, empty [`PeerTable`] wrapped in an [`Arc`] for sharing.
#[must_use]
pub fn new_peer_table() -> Arc<PeerTable> {
    Arc::new(DashMap::new())
}

/// Production [`PeerHandle`] adapter wrapping a live
/// [`rmcp::Peer<RoleServer>`].
///
/// The struct registers itself in the shared [`PeerTable`] on
/// construction and removes itself on [`Drop`], so the matching
/// [`crate::adapters::notifier::rmcp_adapter::RmcpNotifier`] can resolve
/// the rmcp peer through the type-erased
/// [`crate::ports::notifier::PeerHandle::id`] alone.
#[derive(Debug)]
pub struct RmcpPeerHandle {
    /// Stable identifier minted at construction time. Survives the
    /// lifetime of this [`RmcpPeerHandle`] (and therefore the underlying
    /// rmcp peer).
    id: PeerId,
    /// Live rmcp peer the registry calls `notify_resource_updated` on.
    peer: Peer<RoleServer>,
    /// Shared side table the notifier reads from. Holding the `Arc` here
    /// guarantees the [`Drop`] hook can unregister even if the original
    /// composition-root reference is gone.
    table: Arc<PeerTable>,
}

impl RmcpPeerHandle {
    /// Wrap `peer`, mint a fresh [`PeerId`], and register the pair in
    /// `table` so [`crate::adapters::notifier::rmcp_adapter::RmcpNotifier`]
    /// can fan out notifications to it.
    #[must_use]
    pub fn new(peer: Peer<RoleServer>, table: Arc<PeerTable>) -> Self {
        let id = PeerId::new(Uuid::new_v4().to_string());
        table.insert(id.clone(), peer.clone());
        Self { id, peer, table }
    }

    /// Borrow the wrapped rmcp peer. Internal helper — the rmcp adapter
    /// uses it when it already holds the concrete handle (e.g. tests).
    #[must_use]
    pub const fn peer(&self) -> &Peer<RoleServer> {
        &self.peer
    }
}

impl Drop for RmcpPeerHandle {
    fn drop(&mut self) {
        self.table.remove(&self.id);
    }
}

impl PeerHandle for RmcpPeerHandle {
    fn id(&self) -> PeerId {
        self.id.clone()
    }

    fn is_closed(&self) -> bool {
        self.peer.is_transport_closed()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests use unwrap for brevity per CLAUDE.md test policy"
)]
mod tests {
    use super::{PeerTable, new_peer_table};
    use crate::ports::notifier::PeerHandle;

    fn _assert_peer_handle_dyn_safe() {
        fn _accepts(_h: &dyn PeerHandle) {}
    }

    #[test]
    fn new_peer_table_starts_empty() {
        let table: std::sync::Arc<PeerTable> = new_peer_table();
        assert_eq!(table.len(), 0);
    }
}
