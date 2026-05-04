//! Subscriber notification port.
//!
//! `PeerHandle` is the sync, dyn-safe handle the registry stores per
//! connected MCP peer. `NotifierPort` is the async port use cases call to
//! deliver `notifications/resources/updated`. `PeerHandle` is intentionally
//! sync (no AFIT methods) so it remains erasable behind `Arc<dyn PeerHandle>`.

use std::fmt::Debug;
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
