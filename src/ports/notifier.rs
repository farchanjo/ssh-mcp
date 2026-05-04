//! Subscriber notification port.
//!
//! `PeerHandle` is the sync, dyn-safe handle the registry stores per
//! connected MCP peer. `NotifierPort` is the async port use cases call to
//! deliver `notifications/resources/updated`. `PeerHandle` is intentionally
//! sync (no AFIT methods) so it remains erasable behind `Arc<dyn PeerHandle>`.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::ids::PeerId;

/// Stable, dyn-safe handle to a connected MCP peer.
///
/// Implementations wrap the underlying transport (e.g. `rmcp::Peer`) so the
/// registry can keep holding a closed-transport detection method without
/// pulling rmcp into the port crate.
pub trait PeerHandle: Send + Sync + Debug {
    /// Stable identifier minted at subscribe time.
    fn id(&self) -> PeerId;

    /// `true` once the underlying transport has been closed.
    fn is_closed(&self) -> bool;
}

/// Async notifier port. Adapters fan out `resources/updated` notifications.
#[trait_variant::make(NotifierPort: Send)]
pub trait LocalNotifierPort: Sync {
    /// Notify a single peer that `uri` has changed.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Transport` when the underlying transport
    /// rejects the notification (closed channel, IO failure, etc).
    async fn notify_resource_updated(
        &self,
        peer: Arc<dyn PeerHandle>,
        uri: &str,
    ) -> Result<(), DomainError>;
}

/// Sync, dyn-safe activator for the per-resource debouncer task.
///
/// Lets `sub_open`-only flows wake the same debouncer the
/// legacy `resources/subscribe` path uses, so producer pokes /
/// byte-threshold flushes drive the URI broadcast pipeline. Composition
/// root wires the production [`MemoryRegistry`] as the impl.
pub trait DebouncerActivator: Send + Sync + Debug + 'static {
    /// Ensure a debouncer task is running for the resource URI.
    /// Idempotent — re-calling on a live debouncer is a no-op.
    fn ensure_for_uri(&self, uri: &str);
}

/// Sync, dyn-safe forwarder for legacy producer pokes.
///
/// The runtime-side adapters (russh, sftp, serial) feed the legacy
/// `SUBSCRIPTION_REGISTRY` singleton via `poke` / `record_bytes`. The
/// hexagonal [`MemoryRegistry`] runs alongside — without forwarding,
/// `sub_open` lanes only see the 1 s force-flush tick. Composition
/// root installs an implementation backed by the hexagonal registry so
/// every legacy producer event also wakes the lane bridge.
pub trait ProducerForwarder: Send + Sync + Debug + 'static {
    /// Forward a producer poke (resource updated, no byte counter
    /// change) to the hexagonal registry.
    fn forward_poke(&self, kind: super::subscriber_registry::ResourceKind, id: &str);

    /// Forward a producer byte delta to the hexagonal registry's
    /// per-URI byte counter.
    fn forward_record_bytes(
        &self,
        kind: super::subscriber_registry::ResourceKind,
        id: &str,
        bytes_added: usize,
    );
}

/// URI-keyed bridge for the legacy broadcast pipeline.
///
/// Fans `resources/updated` notifications out to every per-`SubId`
/// lane bound to a URI. Composition root installs an implementation
/// on the `MemoryRegistry` so legacy broadcast wakes up
/// `sub_open`-created lanes without a dedicated Phase 4 drain
/// task. Implementations also increment per-lane atomics
/// (`events_sent`, `bytes_sent`). Boxed-future return so the trait
/// stays dyn-safe — `MemoryRegistry` stores it as
/// `ArcSwap<Option<Arc<dyn LaneNotifierBridge>>>`.
pub trait LaneNotifierBridge: Send + Sync + Debug + 'static {
    /// Notify every lane bound to `uri`. `bytes_added` is the byte
    /// count that triggered the broadcast (used to increment lane
    /// `bytes_sent`).
    fn notify_lanes<'a>(
        &'a self,
        uri: &'a str,
        bytes_added: usize,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

#[cfg(test)]
mod tests {
    use super::{NotifierPort, PeerHandle};

    fn _assert_peer_dyn_safe(_h: &dyn PeerHandle) {}

    fn _assert_notifier_port<T: NotifierPort>() {}

    #[test]
    fn dyn_peer_handle_compiles_in_arc() {
        // The `Arc<dyn PeerHandle>` shape used by NotifierPort must be
        // constructable in test code — proves dyn-safety.
        #[derive(Debug)]
        struct Stub;
        impl PeerHandle for Stub {
            fn id(&self) -> crate::domain::ids::PeerId {
                crate::domain::ids::PeerId::new("p".to_string())
            }
            fn is_closed(&self) -> bool {
                false
            }
        }
        let handle: std::sync::Arc<dyn PeerHandle> = std::sync::Arc::new(Stub);
        assert_eq!(handle.id().as_str(), "p");
        assert!(!handle.is_closed());
    }
}
