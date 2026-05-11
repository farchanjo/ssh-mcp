//! Subscription registry port.
//!
//! Split into a sync half (`SubscriberRegistryPort`) for read-only access
//! and a sync-only mutation surface used by use cases that subscribe /
//! unsubscribe peers, plus `SubscriberRegistryAsync` for any operation that
//! must `await` (e.g. spawning the debouncer task).
//!
//! All sequence allocation and snapshot reads stay sync so the v3 lock-free
//! registry semantics carry over unchanged.

use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::ids::PeerId;

use super::notifier::PeerHandle;

/// Resource scheme handled by the registry. Mirrors the v3
/// [`crate::adapters::subscription::legacy::ResourceKind`] but lives in the port layer
/// to avoid leaking adapter types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// `shell://<id>/output`
    Shell,
    /// `command://<id>/output`
    Command,
    /// `transfer://<id>/progress`
    Transfer,
    /// `session://<id>/health`
    Session,
    /// `forward://<id>/events`
    Forward,
    /// `serial://<id>/output` — UART / TTY / COM serial-port output
    /// (v5.2; ADR 0009).
    Serial,
    /// `rsync://<id>/progress` — rsync hybrid-transport sync progress
    /// (v7.0; ADR 0011).
    Rsync,
}

/// Snapshot of a single subscription record returned by the registry.
#[derive(Debug, Clone)]
pub struct SubscriberSnapshot {
    /// Stable peer identifier.
    pub peer_id: PeerId,
    /// Subscribed URI (e.g. `shell://abc/output`).
    pub uri: String,
    /// Resource scheme.
    pub kind: ResourceKind,
    /// Resource id portion of the URI.
    pub resource_id: String,
}

/// Sync, dyn-safe slice of the subscription registry.
///
/// Methods that "wake the debouncer" are sync because the v3 registry
/// implements them via `Notify::notify_one` (no `.await`).
pub trait SubscriberRegistryPort: Send + Sync + 'static {
    /// Allocate the next sequence number for `(kind, resource_id)`.
    fn next_seq(&self, kind: ResourceKind, resource_id: &str) -> u64;

    /// Read the latest allocated sequence number (`0` if never allocated).
    fn current_seq(&self, kind: ResourceKind, resource_id: &str) -> u64;

    /// Wake the debouncer for `(kind, resource_id)`. No-op when no
    /// subscribers exist.
    fn poke(&self, kind: ResourceKind, resource_id: &str);

    /// Record `bytes_added` newly produced bytes for `(kind, resource_id)`.
    ///
    /// ADR 0006 Amendment 1 — accumulates against the per-resource
    /// byte counter. When the counter crosses
    /// `SSH_NOTIFY_FLUSH_BYTES`, the registry forces an immediate
    /// debouncer flush (skipping the debounce sleep). When the
    /// threshold is `0` (disabled) or no debouncer is running for the
    /// resource, this is a near-zero-cost no-op (single relaxed
    /// `fetch_add`). Producers (russh shell stdout, async command
    /// stdout, SFTP transfer progress) call this every time they
    /// append to the resource's ring buffer.
    ///
    /// Default impl is a no-op so test fixtures and the legacy
    /// (Phase 1) adapter compile unchanged. The production
    /// [`crate::adapters::subscription::memory_registry::MemoryRegistry`]
    /// overrides this with the threshold + counter + `flush_now`
    /// pipeline.
    fn record_bytes(&self, _kind: ResourceKind, _resource_id: &str, _bytes_added: usize) {
        // No-op default. Production registry overrides; test stubs and
        // the legacy adapter inherit the no-op so they keep their v4
        // semantics.
    }

    /// ADR 0012 phase 9 — byte-tail variant. Producers that hold the
    /// real bytes in scope (shell PTY, command stdout/stderr, serial
    /// reader) call this so the registry can fan the raw tail out to
    /// every opt-in inline lane before feeding the debouncer.
    ///
    /// Default impl forwards to [`Self::record_bytes`] with the byte
    /// count only, preserving the v4 / Phase 4 semantics for stubs and
    /// the legacy adapter. The production `MemoryRegistry` overrides
    /// to also call the installed `LaneNotifierBridge`.
    fn record_bytes_with_tail(&self, kind: ResourceKind, resource_id: &str, bytes_added: &[u8]) {
        self.record_bytes(kind, resource_id, bytes_added.len());
    }

    /// Decrement every peer cursor on `uri` by `bytes_dropped` (saturating).
    /// Called when the underlying ring buffer drops bytes from the head.
    fn compensate_truncation(&self, uri: &str, bytes_dropped: u64);

    /// Snapshot all subscribers currently bound to `uri`.
    fn snapshot_subscribers(&self, uri: &str) -> Vec<SubscriberSnapshot>;

    /// Read the current per-peer byte cursor for `(peer_id, uri)`.
    /// Returns `0` when no cursor has ever been allocated.
    fn peer_byte_cursor(&self, peer_id: &PeerId, uri: &str) -> u64;

    /// Advance the per-peer byte cursor for `(peer_id, uri)` to at least
    /// `target` (atomic max). Returns the cursor value AFTER the bump.
    fn advance_peer_byte_cursor(&self, peer_id: &PeerId, uri: &str, target: u64) -> u64;

    /// Walk every subscriber and drop the ones whose transport has
    /// closed. Returns the number of peers dropped. Used by the
    /// [`crate::application::peer_gc`] use case as a periodic GC pass
    /// (rmcp 1.6 does not raise a peer-disconnect callback).
    fn gc_closed_peers(&self) -> usize;
}

/// Async slice of the subscription registry. `subscribe` may spawn a
/// debouncer task on the runtime and is therefore async.
#[trait_variant::make(SubscriberRegistryAsync: Send)]
pub trait LocalSubscriberRegistryAsync: Sync {
    /// Register `peer` as a subscriber to `uri`.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` when the registry refuses the
    /// subscription (capacity exhaustion, internal lock poison, etc).
    async fn subscribe(
        &self,
        kind: ResourceKind,
        resource_id: String,
        uri: String,
        peer: Arc<dyn PeerHandle>,
    ) -> Result<(), DomainError>;

    /// Unregister `peer_id` from `uri`.
    ///
    /// Returns `true` when the peer was actually subscribed and has now been
    /// removed; `false` when the `(peer_id, uri)` pair was not found in the
    /// registry (idempotent no-op). Callers that drive lifecycle refcount
    /// bookkeeping MUST only call
    /// [`crate::ports::lifecycle_policy::LifecyclePolicyPort::on_unsubscribe`]
    /// when this returns `true` — calling it unconditionally causes a
    /// `sub_count` underflow on repeated unsubscribe.
    async fn unsubscribe(&self, peer_id: &PeerId, uri: &str) -> bool;

    /// Drop every subscription owned by `peer_id` across all URIs.
    async fn drop_peer(&self, peer_id: &PeerId);
}

#[cfg(test)]
mod tests {
    use super::{
        ResourceKind, SubscriberRegistryAsync, SubscriberRegistryPort, SubscriberSnapshot,
    };

    fn _assert_sync_dyn_safe(_p: &dyn SubscriberRegistryPort) {}

    fn _assert_async_port<T: SubscriberRegistryAsync>() {}

    #[test]
    fn snapshot_struct_round_trip() {
        let snap = SubscriberSnapshot {
            peer_id: crate::domain::ids::PeerId::new("p".to_string()),
            uri: "shell://abc/output".to_string(),
            kind: ResourceKind::Shell,
            resource_id: "abc".to_string(),
        };
        assert_eq!(snap.peer_id.as_str(), "p");
        assert_eq!(snap.kind, ResourceKind::Shell);
    }

    #[test]
    fn resource_kind_is_hashable() {
        let mut set = std::collections::HashSet::new();
        set.insert(ResourceKind::Shell);
        set.insert(ResourceKind::Shell);
        set.insert(ResourceKind::Command);
        assert_eq!(set.len(), 2);
    }
}
