//! [`PeerHandle`] adapter that wraps [`rmcp::Peer<RoleServer>`] and a
//! transport-keyed [`PeerTable`] that hands out a stable [`PeerId`] per
//! peer.
//!
//! ## Why a stable [`PeerId`]
//!
//! `subscribe`/`unsubscribe` are routed independently through rmcp; both
//! handlers need to reach the same registry entry. Minting a fresh
//! `UUIDv4` per [`RmcpPeerHandle`] would assign two distinct ids to the
//! same client, leaving `unsubscribe` as a no-op. The fix is a small
//! per-process side table keyed by something stable on the transport:
//!
//! - HTTP Streamable: the `Mcp-Session-Id` header injected by rmcp's
//!   `StreamableHttpService` into `RequestContext::extensions` via
//!   [`http::request::Parts`]. Always present after the initial
//!   handshake.
//! - Stdio: a singleton key — only one peer talks to the process.
//!
//! ## Side-effects
//!
//! - [`RmcpPeerHandle::resolve`] looks the [`PeerKey`] up in the shared
//!   [`PeerTable`] and either returns the existing [`PeerId`] or mints a
//!   fresh one and registers the live rmcp peer.
//! - The matching [`super::rmcp_adapter::RmcpNotifier`] reads from the
//!   same [`PeerTable`] via [`PeerTable::lookup_peer`] when fanning out
//!   `notifications/resources/updated`.
//! - The peer-GC pump removes closed peers via [`PeerTable::drop_by_id`].

use std::sync::Arc;

use axum::http::request::Parts as HttpParts;
use dashmap::DashMap;
use rmcp::Peer;
use rmcp::RoleServer;
use rmcp::service::RequestContext;
use uuid::Uuid;

use crate::domain::ids::PeerId;
use crate::ports::notifier::PeerHandle;

/// Stable transport-level identity used to derive a single [`PeerId`]
/// across every rmcp call mounted on the same connection.
///
/// HTTP Streamable peers are keyed by their `Mcp-Session-Id` header
/// (case-insensitive — `HeaderMap::get` does the comparison for us).
/// The Stdio transport is singleton: one peer per process.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PeerKey {
    /// HTTP Streamable peer keyed by `Mcp-Session-Id` header value.
    Http(String),
    /// Stdio singleton.
    Stdio,
}

/// Derive a [`PeerKey`] from an inbound rmcp [`RequestContext`].
///
/// Reads the `Mcp-Session-Id` header out of the
/// [`http::request::Parts`] extension that
/// `rmcp::transport::streamable_http_server::StreamableHttpService`
/// injects per request. Falls back to [`PeerKey::Stdio`] when the
/// extension is absent (stdio transport, in-process tests, ...).
#[must_use]
pub fn peer_key_from_context(ctx: &RequestContext<RoleServer>) -> PeerKey {
    if let Some(parts) = ctx.extensions.get::<HttpParts>()
        && let Some(sid) = parts.headers.get("mcp-session-id")
        && let Ok(s) = sid.to_str()
    {
        return PeerKey::Http(s.to_string());
    }
    PeerKey::Stdio
}

/// Shared transport-keyed lookup serving both directions:
///
/// - forward: [`PeerKey`] -> stable [`PeerId`] (so subscribe / unsubscribe
///   addressed to the same connection share the same id).
/// - reverse: [`PeerId`] -> live [`rmcp::Peer<RoleServer>`] (so the
///   notifier can fan out without depending on the dyn-erased
///   [`PeerHandle`] surface).
///
/// One [`PeerTable`] is created per process at the composition root and
/// shared via [`Arc`]. Construction is intentionally cheap so tests can
/// build their own side tables without setup.
#[derive(Debug, Default)]
pub struct PeerTable {
    /// Forward map: stable [`PeerKey`] -> minted [`PeerId`].
    by_key: DashMap<PeerKey, PeerId>,
    /// Reverse map: [`PeerId`] -> live rmcp [`Peer`].
    by_id: DashMap<PeerId, Peer<RoleServer>>,
}

impl PeerTable {
    /// Build an empty [`PeerTable`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the [`PeerId`] for `key`, minting and registering a new one
    /// when none exists yet. Idempotent: repeat calls with the same
    /// [`PeerKey`] (regardless of the `peer` argument) return the same
    /// [`PeerId`].
    #[must_use]
    pub fn get_or_mint(&self, key: PeerKey, peer: Peer<RoleServer>) -> PeerId {
        if let Some(existing) = self.by_key.get(&key) {
            return existing.value().clone();
        }
        let id = PeerId::new(Uuid::new_v4().to_string());
        // Race-safe insert: if another thread won the mint we re-fetch the
        // winner so every caller agrees on a single id per key.
        let entry = self.by_key.entry(key).or_insert_with(|| id.clone());
        let canonical = entry.value().clone();
        drop(entry);
        self.by_id.entry(canonical.clone()).or_insert(peer);
        canonical
    }

    /// Look the live rmcp [`Peer`] up by [`PeerId`].
    ///
    /// Returns `None` when the peer has already been GC-collected (its
    /// transport closed). The notifier treats a miss as at-most-once
    /// delivery and short-circuits with [`Ok`].
    #[must_use]
    pub fn lookup_peer(&self, id: &PeerId) -> Option<Peer<RoleServer>> {
        self.by_id.get(id).map(|kv| kv.value().clone())
    }

    /// Remove every entry that maps to `id`. Called by the peer-GC pump
    /// when an rmcp transport closes.
    pub fn drop_by_id(&self, id: &PeerId) {
        self.by_id.remove(id);
        self.by_key.retain(|_k, v| v != id);
    }

    /// Walk the reverse map and drop every entry whose underlying rmcp
    /// transport has closed. Returns the number of peers dropped.
    ///
    /// Called periodically by the binary entry points
    /// (`spawn_peer_gc`) since rmcp 1.6 does not raise a callback on
    /// peer disconnect.
    #[must_use]
    pub fn gc_closed_peers(&self) -> usize {
        self.gc_closed_peers_with(|_id| {})
    }

    /// Walk the reverse map and drop every entry whose underlying rmcp
    /// transport has closed. Fires `on_drop` once per evicted [`PeerId`]
    /// (used by ADR 0012 Phase 3 to plug
    /// [`crate::adapters::capability::CapabilityRegistry::forget_peer`]
    /// into the GC pump). Returns the number of peers dropped.
    pub fn gc_closed_peers_with<F>(&self, mut on_drop: F) -> usize
    where
        F: FnMut(&PeerId),
    {
        // Snapshot ids before mutating to avoid `await_holding_lock`-style
        // shard contention; `Peer::is_transport_closed` is sync but we
        // still don't want to hold a shard guard across the prune.
        let to_drop: Vec<PeerId> = self
            .by_id
            .iter()
            .filter(|kv| kv.value().is_transport_closed())
            .map(|kv| kv.key().clone())
            .collect();
        let dropped = to_drop.len();
        for id in to_drop {
            on_drop(&id);
            self.drop_by_id(&id);
        }
        dropped
    }

    /// Number of registered peers. Test helper.
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }
}

/// Build a fresh, empty [`PeerTable`] wrapped in an [`Arc`] for sharing.
#[must_use]
pub fn new_peer_table() -> Arc<PeerTable> {
    Arc::new(PeerTable::new())
}

/// Production [`PeerHandle`] adapter wrapping a live
/// [`rmcp::Peer<RoleServer>`].
///
/// Created via [`RmcpPeerHandle::resolve`] from a [`RequestContext`];
/// every call resolves the same [`PeerId`] for the same transport-level
/// identity through the shared [`PeerTable`]. The handle itself owns
/// nothing — the [`PeerTable`] is the source of truth for live peers.
#[derive(Debug)]
pub struct RmcpPeerHandle {
    /// Stable identifier shared with every other handle resolved on the
    /// same connection.
    id: PeerId,
    /// Live rmcp peer (also stored in the shared table; we keep a copy
    /// here so [`PeerHandle::is_closed`] does not need a table lookup).
    peer: Peer<RoleServer>,
}

impl RmcpPeerHandle {
    /// Resolve the [`RmcpPeerHandle`] for an inbound rmcp request.
    ///
    /// Looks the request up in `table` by its [`PeerKey`] and either
    /// reuses the existing [`PeerId`] or registers a fresh one. Multiple
    /// rmcp calls mounted on the same transport (subscribe + unsubscribe,
    /// read + subscribe, ...) all see the same [`PeerId`].
    #[must_use]
    pub fn resolve(ctx: &RequestContext<RoleServer>, table: &Arc<PeerTable>) -> Self {
        let key = peer_key_from_context(ctx);
        let id = table.get_or_mint(key, ctx.peer.clone());
        Self {
            id,
            peer: ctx.peer.clone(),
        }
    }

    /// Borrow the wrapped rmcp peer. Internal helper.
    #[must_use]
    pub const fn peer(&self) -> &Peer<RoleServer> {
        &self.peer
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
    use super::{PeerKey, PeerTable, new_peer_table};
    use crate::domain::ids::PeerId;
    use crate::ports::notifier::PeerHandle;

    fn _assert_peer_handle_dyn_safe() {
        fn _accepts(_h: &dyn PeerHandle) {}
    }

    #[test]
    fn new_peer_table_starts_empty() {
        let table: std::sync::Arc<PeerTable> = new_peer_table();
        assert_eq!(table.len(), 0);
    }

    // The next two tests exercise the table directly — no rmcp Peer is
    // available without a live transport, but `get_or_mint` only needs a
    // sentinel value for the reverse map. We construct one through the
    // table's internal API by leaning on `Default` for an empty table and
    // skipping the reverse-map assertion (covered by integration tests in
    // `tests/v4_smoke.rs`).
    #[test]
    fn peer_key_eq_and_hash_distinguishes_variants() {
        use std::collections::HashSet;
        let mut set: HashSet<PeerKey> = HashSet::new();
        set.insert(PeerKey::Http("a".to_string()));
        set.insert(PeerKey::Http("b".to_string()));
        set.insert(PeerKey::Stdio);
        assert_eq!(set.len(), 3);
        assert!(set.contains(&PeerKey::Http("a".to_string())));
        assert!(set.contains(&PeerKey::Stdio));
    }

    #[test]
    fn drop_by_id_clears_both_maps() {
        // Build a side table and emulate `get_or_mint` by inserting
        // directly through DashMap — we cannot construct a real Peer here.
        let table = PeerTable::new();
        let id = PeerId::new("p1".to_string());
        table
            .by_key
            .insert(PeerKey::Http("s".to_string()), id.clone());
        // Skip the reverse-map insert (no Peer in scope) — `drop_by_id`
        // tolerates a missing entry.
        table.drop_by_id(&id);
        assert_eq!(table.by_key.len(), 0);
        assert_eq!(table.by_id.len(), 0);
    }
}
