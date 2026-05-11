//! Subscriber notification port.
//!
//! `PeerHandle` is the sync, dyn-safe handle the registry stores per
//! connected MCP peer. `NotifierPort` is the async port use cases call to
//! deliver `notifications/resources/updated` and (since ADR 0012, v7.1)
//! `notifications/ssh/output` for inline-push delivery. `PeerHandle` is
//! intentionally sync (no AFIT methods) so it remains erasable behind
//! `Arc<dyn PeerHandle>`.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::ids::PeerId;
use crate::domain::inline_payload::InlinePayload;

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

    /// Deliver an inline-push fragment to the originating peer as a
    /// `notifications/ssh/output` MCP notification.
    ///
    /// Called by the lane-side bridge for subscriptions that negotiated
    /// inline push at `sub_open` time and that the v7.1 capability
    /// handshake authorised (see ADR 0012). The lane composes the
    /// [`InlinePayload`] from its producer ring buffer; this port is
    /// the pure-delivery boundary — payload composition, splitting, and
    /// per-lane stats live on the lane side.
    ///
    /// `peer` mirrors the `notify_resource_updated` shape so adapters
    /// share a single `PeerHandle`-to-transport resolver. The payload is
    /// moved (not borrowed) so adapters take ownership of `bytes`
    /// without an intermediate copy. Implementations MUST NOT hold any
    /// lock across the underlying `.await`; the production rmcp adapter
    /// snapshots the cheap-to-clone [`rmcp::Peer`] out of its lookup
    /// table before awaiting.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Transport` when the underlying transport
    /// rejects the notification (closed channel, IO failure, ...). The
    /// production rmcp adapter treats a missing peer (already GC'd
    /// because the transport closed) as at-most-once delivery and
    /// returns `Ok(())`.
    async fn notify_ssh_output(
        &self,
        peer: Arc<dyn PeerHandle>,
        payload: InlinePayload,
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

    /// ADR 0012 phase 9 — byte-tail variant. Default impl forwards
    /// to [`Self::forward_record_bytes`] with the byte count, so
    /// implementations that only care about the legacy debouncer
    /// remain unchanged. The composition root forwarder overrides to
    /// also drive the inline-push lane fan-out.
    fn forward_record_bytes_with_tail(
        &self,
        kind: super::subscriber_registry::ResourceKind,
        id: &str,
        bytes_added: &[u8],
    ) {
        self.forward_record_bytes(kind, id, bytes_added.len());
    }
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

    /// ADR 0012 phase 9 — synchronous producer-side hook for the
    /// inline-push path. The producer calls this with the actual byte
    /// tail it just appended; the bridge fans out only to opt-in
    /// inline lanes (legacy `resources/updated` notifications are
    /// still delivered by the debouncer-driven [`Self::notify_lanes`]
    /// at the regular cadence).
    ///
    /// Default impl is a no-op so test bridges that do not care about
    /// the inline path compile without change.
    fn notify_lanes_inline<'a>(
        &'a self,
        _uri: &'a str,
        _bytes_added: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }
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
