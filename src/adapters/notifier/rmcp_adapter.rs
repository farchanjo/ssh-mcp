//! Production [`NotifierPort`] adapter that fans out
//! `notifications/resources/updated` over rmcp.
//!
//! The adapter receives the type-erased subscriber as
//! `Arc<dyn PeerHandle>` (per the port). It resolves it to the live
//! [`rmcp::Peer<RoleServer>`] through the shared
//! [`crate::adapters::notifier::rmcp_peer::PeerTable`] populated by
//! [`crate::adapters::notifier::rmcp_peer::RmcpPeerHandle`]. A miss means
//! the peer's [`crate::adapters::notifier::rmcp_peer::RmcpPeerHandle`]
//! has already been dropped (transport closed); we treat that as an
//! at-most-once delivery and short-circuit with [`Ok`].

use std::sync::Arc;

use rmcp::model::ResourceUpdatedNotificationParam;

use crate::domain::error::DomainError;
use crate::ports::notifier::{NotifierPort, PeerHandle};

use super::rmcp_peer::PeerTable;

/// Production [`crate::ports::notifier::NotifierPort`] adapter backed by rmcp.
///
/// Constructed once at the composition root with the shared
/// [`PeerTable`] and cloned freely (the inner [`Arc`] is cheap to clone).
#[derive(Debug, Clone)]
pub struct RmcpNotifier {
    /// Shared lookup [`crate::domain::ids::PeerId`] -> live rmcp peer.
    /// Written by [`crate::adapters::notifier::rmcp_peer::RmcpPeerHandle::new`].
    peers: Arc<PeerTable>,
}

impl RmcpNotifier {
    /// Build a notifier that resolves peers through the shared `peers`
    /// table.
    #[must_use]
    pub const fn new(peers: Arc<PeerTable>) -> Self {
        Self { peers }
    }
}

impl NotifierPort for RmcpNotifier {
    async fn notify_resource_updated(
        &self,
        peer: Arc<dyn PeerHandle>,
        uri: &str,
    ) -> Result<(), DomainError> {
        let peer_id = peer.id();
        // Snapshot the rmcp peer out of the shared table BEFORE awaiting so
        // we never hold a DashMap shard guard across `.await`. `Peer` is
        // cheap to clone (mpsc::Sender + Arc).
        let Some(rmcp_peer) = self.peers.get(&peer_id).map(|entry| entry.value().clone()) else {
            // The matching `RmcpPeerHandle` has already been dropped — the
            // transport is gone. Resource notifications are at-most-once,
            // so a miss is not an error.
            return Ok(());
        };
        let params = ResourceUpdatedNotificationParam {
            uri: uri.to_string(),
        };
        rmcp_peer
            .notify_resource_updated(params)
            .await
            .map_err(|err| DomainError::Transport(err.to_string()))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests use unwrap for brevity per CLAUDE.md test policy"
)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::RmcpNotifier;
    use crate::adapters::notifier::rmcp_peer::new_peer_table;
    use crate::domain::ids::PeerId;
    use crate::ports::notifier::{NotifierPort, PeerHandle};
    // `NotifierPort` is the `Send`-bounded alias minted by `trait_variant`;
    // `notify_resource_updated` lives on it.

    fn _assert_send_sync<T: Send + Sync>() {}

    #[derive(Debug)]
    struct StubPeer {
        id: PeerId,
        closed: AtomicBool,
    }

    impl StubPeer {
        fn new(id: &str) -> Self {
            Self {
                id: PeerId::new(id.to_string()),
                closed: AtomicBool::new(false),
            }
        }
    }

    impl PeerHandle for StubPeer {
        fn id(&self) -> PeerId {
            self.id.clone()
        }
        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::Relaxed)
        }
    }

    #[test]
    fn rmcp_notifier_is_send_sync() {
        _assert_send_sync::<RmcpNotifier>();
    }

    #[test]
    fn rmcp_notifier_compiles_as_notifier_port() {
        // Trait bound check: `RmcpNotifier` must satisfy `NotifierPort`
        // (the public `Send`-bounded alias minted by `trait_variant`).
        fn _accepts<T: NotifierPort + Send + Sync>(_t: T) {}
        let table = new_peer_table();
        let notifier = RmcpNotifier::new(table);
        _accepts(notifier);
    }

    #[tokio::test]
    async fn notify_unknown_peer_returns_ok_silently() {
        // Peer is not registered in the shared table — the adapter must
        // treat the miss as at-most-once and return Ok.
        let table = new_peer_table();
        let notifier = RmcpNotifier::new(table);
        let peer: Arc<dyn PeerHandle> = Arc::new(StubPeer::new("ghost"));
        let res = notifier
            .notify_resource_updated(peer, "shell://abc/output")
            .await;
        assert!(res.is_ok(), "missing peer must not error: {res:?}");
    }
}
