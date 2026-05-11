//! Per-peer experimental client capability registry (ADR 0012 Phase 3).
//!
//! Tracks lightweight feature flags negotiated during the MCP
//! `initialize` handshake (see ADR 0012 Phase 6 for the recorder side).
//! Today the registry exposes a single flag —
//! [`CapabilityFlag::InlinePush`] — but the [`PeerCapabilities`] POD
//! struct is `#[repr(C)]` so additional flags drop in at zero cost.
//!
//! ## Lock-free contract
//!
//! - `peers: DashMap<PeerId, Arc<PeerCapabilities>>` — shard-locked
//!   O(1) insert/lookup/remove. Hot path NEVER holds a shard guard
//!   across `.await`.
//! - `PeerCapabilities.<flag>: AtomicBool` — single-word loads/stores.
//!   `Release` on store and `Acquire` on load so any future
//!   read-side reasoning about a peer transitioning to "capability X
//!   on" happens-after the recording task. The handshake recorder is
//!   the only writer per peer; concurrent flips are race-free under
//!   the chosen ordering pair.
//! - Zero `Mutex`, zero `RwLock`, zero `.await` inside the registry.
//!
//! ## Wiring
//!
//! - Composition root (`composition::prod` / `composition::embed`)
//!   constructs a single `Arc<CapabilityRegistry>` per process.
//! - The `initialize` handler (Phase 6) calls
//!   [`CapabilityRegistry::record_capability`] when it sees
//!   `experimental.ssh_inline_push = true` on the client info.
//! - The peer-GC pump (`adapters::subscription::legacy::spawn_peer_gc`)
//!   calls [`CapabilityRegistry::forget_peer`] for every evicted
//!   peer so the registry never out-lives the peer's transport.
//! - Phase 4 lane fan-out (`LaneFanoutBridge`) reads capability state
//!   via [`CapabilityRegistry::peer_has_capability`] to decide
//!   between the byte-payload inline push and the legacy
//!   `notifications/resources/updated` fan-out.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dashmap::DashMap;

use crate::domain::ids::PeerId;

/// Experimental client capability flag tracked per peer.
///
/// Single-variant today; new variants are additive — the matching
/// `AtomicBool` field on [`PeerCapabilities`] grows alongside the
/// enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CapabilityFlag {
    /// `experimental.ssh_inline_push` — when set, the lane fan-out
    /// inlines the resource payload (base64-encoded bytes) directly
    /// on the `notifications/resources/updated` message instead of
    /// the "URI changed, call resources/read" indirection.
    InlinePush,
}

/// Atomic capability bit-bag. One `AtomicBool` per supported flag.
///
/// `#[repr(C)]` keeps the field layout stable so a future
/// `repr(transparent)` newtype wrapper or FFI export does not have
/// to re-derive offsets.
#[repr(C)]
#[derive(Debug, Default)]
pub struct PeerCapabilities {
    /// Mirror of [`CapabilityFlag::InlinePush`].
    pub inline_push: AtomicBool,
}

/// Lock-free per-peer capability registry.
///
/// See module docs for the full design rationale.
#[derive(Debug, Default)]
pub struct CapabilityRegistry {
    peers: DashMap<PeerId, Arc<PeerCapabilities>>,
}

impl CapabilityRegistry {
    /// Build an empty registry. Composition root constructs a single
    /// `Arc<CapabilityRegistry>` per process.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record (or update) one capability flag for `peer_id`.
    ///
    /// Insertion of the per-peer bit-bag is `entry().or_default()` —
    /// idempotent. The actual flag write is a single `AtomicBool::store`
    /// with `Release` ordering so any subsequent `Acquire` load on the
    /// same flag observes the new value.
    pub fn record_capability(&self, peer_id: PeerId, flag: CapabilityFlag, enabled: bool) {
        let entry = self.peers.entry(peer_id).or_default();
        match flag {
            CapabilityFlag::InlinePush => {
                entry.value().inline_push.store(enabled, Ordering::Release);
            }
        }
    }

    /// Read one capability flag for `peer_id`.
    ///
    /// Returns `false` when the peer has never been recorded — equivalent
    /// to "the client did not opt in". The atomic load uses `Acquire`
    /// so it pairs with the `Release` store from
    /// [`Self::record_capability`].
    #[must_use]
    pub fn peer_has_capability(&self, peer_id: &PeerId, flag: CapabilityFlag) -> bool {
        let Some(entry) = self.peers.get(peer_id) else {
            return false;
        };
        match flag {
            CapabilityFlag::InlinePush => entry.value().inline_push.load(Ordering::Acquire),
        }
    }

    /// Forget every recorded capability for `peer_id`.
    ///
    /// Called by the peer-GC pump when an rmcp transport closes.
    /// Idempotent — silent no-op on never-recorded peers.
    pub fn forget_peer(&self, peer_id: &PeerId) {
        self.peers.remove(peer_id);
    }

    /// Number of peers with at least one recorded capability. Test
    /// helper / observability hook.
    #[must_use]
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// Whether the registry has zero recorded peers. Test helper.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests use unwrap/expect for brevity (production code keeps the lints at forbid)"
)]
mod tests {
    use super::{CapabilityFlag, CapabilityRegistry};
    use crate::domain::ids::PeerId;
    use std::sync::Arc;
    use tokio::task::JoinSet;

    fn pid(s: &str) -> PeerId {
        PeerId::new(s.to_string())
    }

    #[test]
    fn record_then_peer_has() {
        let reg = CapabilityRegistry::new();
        let peer = pid("peer-1");
        reg.record_capability(peer.clone(), CapabilityFlag::InlinePush, true);
        assert!(reg.peer_has_capability(&peer, CapabilityFlag::InlinePush));
    }

    #[test]
    fn record_overwrite() {
        let reg = CapabilityRegistry::new();
        let peer = pid("peer-1");
        reg.record_capability(peer.clone(), CapabilityFlag::InlinePush, true);
        reg.record_capability(peer.clone(), CapabilityFlag::InlinePush, false);
        assert!(!reg.peer_has_capability(&peer, CapabilityFlag::InlinePush));
    }

    #[test]
    fn peer_has_unknown_peer() {
        let reg = CapabilityRegistry::new();
        let peer = pid("never-seen");
        assert!(!reg.peer_has_capability(&peer, CapabilityFlag::InlinePush));
    }

    #[test]
    fn forget_peer_removes_state() {
        let reg = CapabilityRegistry::new();
        let peer = pid("peer-1");
        reg.record_capability(peer.clone(), CapabilityFlag::InlinePush, true);
        assert!(reg.peer_has_capability(&peer, CapabilityFlag::InlinePush));
        reg.forget_peer(&peer);
        assert!(!reg.peer_has_capability(&peer, CapabilityFlag::InlinePush));
        assert!(reg.is_empty());
    }

    #[test]
    fn forget_unknown_peer_noop() {
        let reg = CapabilityRegistry::new();
        let peer = pid("never-seen");
        // Must not panic / not allocate beyond the empty registry.
        reg.forget_peer(&peer);
        assert_eq!(reg.len(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_record_safe() {
        let reg = Arc::new(CapabilityRegistry::new());
        let peer = pid("peer-shared");
        let mut set = JoinSet::new();
        for i in 0_u8..8 {
            let reg = Arc::clone(&reg);
            let peer = peer.clone();
            set.spawn(async move {
                // Each task records once; final value depends on the
                // last writer, but the registry must remain coherent
                // (single live entry, single bit-bag).
                let enabled = (i & 1) == 1;
                reg.record_capability(peer, CapabilityFlag::InlinePush, enabled);
            });
        }
        while let Some(joined) = set.join_next().await {
            joined.expect("task joined cleanly");
        }
        assert_eq!(reg.len(), 1, "exactly one peer entry must exist");
        // `peer_has_capability` must return a stable bool (no torn read).
        let _final_value = reg.peer_has_capability(&peer, CapabilityFlag::InlinePush);
    }

    #[test]
    fn multi_peer_isolation() {
        let reg = CapabilityRegistry::new();
        let alice = pid("alice");
        let bob = pid("bob");
        reg.record_capability(alice.clone(), CapabilityFlag::InlinePush, true);
        reg.record_capability(bob.clone(), CapabilityFlag::InlinePush, false);
        assert!(reg.peer_has_capability(&alice, CapabilityFlag::InlinePush));
        assert!(!reg.peer_has_capability(&bob, CapabilityFlag::InlinePush));
        // Forgetting alice must not touch bob's state.
        reg.forget_peer(&alice);
        assert!(!reg.peer_has_capability(&alice, CapabilityFlag::InlinePush));
        assert!(!reg.peer_has_capability(&bob, CapabilityFlag::InlinePush));
        assert_eq!(reg.len(), 1);
    }
}
