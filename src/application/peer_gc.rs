//! Use case: drop every subscriber whose underlying transport has closed.
//!
//! Translates the v3 `gc_closed_peers` background task (driven by
//! `spawn_peer_gc` in `src/lib.rs`) into a hexagonal use case that the
//! composition root can schedule on a [`tokio::time::interval`].
//!
//! # Orchestration shape
//!
//! 1. Delegate the entire scan to
//!    [`SubscriberRegistryPort::gc_closed_peers`]. The port returns the
//!    number of peers dropped, which the use case re-surfaces as
//!    [`PeerGcOutcome::peers_dropped`].
//!
//! The port-level scan is intentionally a single sync call so the
//! background task does not stall on per-peer asynchronous work; the
//! `MemoryRegistry` adapter walks every URI bucket without `.await` and
//! drops closed peers in place. Future remote-backed registries can
//! decide whether to fan out per-peer asynchronous probes inside the
//! port impl rather than escaping into the use case.

use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::ports::subscriber_registry::SubscriberRegistryPort;

/// Inbound DTO. Empty by design — the GC pass takes no parameters.
#[derive(Debug, Default, Clone, Copy)]
pub struct PeerGcRequest;

/// Outbound DTO carrying the scan tally.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PeerGcOutcome {
    /// Number of peers whose transport had closed and were dropped.
    pub peers_dropped: usize,
}

/// Peer-GC use case generic over the subscriber registry port.
#[derive(Debug)]
pub struct PeerGcUseCase<Sub>
where
    Sub: SubscriberRegistryPort,
{
    subscribers: Arc<Sub>,
}

impl<Sub> PeerGcUseCase<Sub>
where
    Sub: SubscriberRegistryPort,
{
    /// Wire the use case from an already-shared adapter handle.
    #[must_use]
    pub const fn new(subscribers: Arc<Sub>) -> Self {
        Self { subscribers }
    }

    /// Drive a single GC pass.
    ///
    /// # Errors
    ///
    /// Currently never fails — the port surface is sync and infallible.
    /// The result type is kept as `Result` so future port evolutions
    /// (e.g. a remote-backed registry that propagates a transport
    /// fault) do not require a SemVer-breaking change.
    #[allow(
        clippy::unused_async,
        reason = "kept async for future remote-backed registries that may need to await"
    )]
    pub async fn execute(&self, _req: PeerGcRequest) -> Result<PeerGcOutcome, DomainError> {
        let peers_dropped = self.subscribers.gc_closed_peers();
        Ok(PeerGcOutcome { peers_dropped })
    }
}

#[cfg(test)]
mod tests {
    use super::{PeerGcRequest, PeerGcUseCase};
    use crate::adapters::subscription::memory_registry::MemoryRegistry;
    use crate::domain::error::DomainError;
    use crate::domain::ids::PeerId;
    use crate::ports::notifier::{NotifierPort, PeerHandle};
    use crate::ports::subscriber_registry::{
        ResourceKind, SubscriberRegistryAsync, SubscriberRegistryPort, SubscriberSnapshot,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Stub notifier that swallows every fan-out — peer GC tests do
    /// not exercise the broadcast path.
    #[derive(Debug, Default)]
    struct StubNotifier;

    impl NotifierPort for StubNotifier {
        async fn notify_resource_updated(
            &self,
            _peer: Arc<dyn PeerHandle>,
            _uri: &str,
        ) -> Result<(), DomainError> {
            Ok(())
        }

        async fn notify_ssh_output(
            &self,
            _peer: Arc<dyn PeerHandle>,
            _payload: crate::domain::inline_payload::InlinePayload,
        ) -> Result<(), DomainError> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct StubPeer {
        id: PeerId,
        closed: AtomicBool,
    }

    impl StubPeer {
        fn new(id: &str) -> Arc<Self> {
            Arc::new(Self {
                id: PeerId::new(id.to_string()),
                closed: AtomicBool::new(false),
            })
        }

        fn close(&self) {
            self.closed.store(true, Ordering::Relaxed);
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

    /// Fully scriptable registry stub that lets us inject the count of
    /// peers the GC pass should report regardless of internal state.
    #[derive(Debug, Default)]
    struct CountingRegistry {
        next_count: AtomicUsize,
        calls: AtomicUsize,
    }

    impl CountingRegistry {
        fn set(&self, n: usize) {
            self.next_count.store(n, Ordering::Relaxed);
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    impl SubscriberRegistryPort for CountingRegistry {
        fn next_seq(&self, _k: ResourceKind, _id: &str) -> u64 {
            0
        }
        fn current_seq(&self, _k: ResourceKind, _id: &str) -> u64 {
            0
        }
        fn poke(&self, _k: ResourceKind, _id: &str) {}
        fn compensate_truncation(&self, _u: &str, _b: u64) {}
        fn snapshot_subscribers(&self, _u: &str) -> Vec<SubscriberSnapshot> {
            Vec::new()
        }
        fn peer_byte_cursor(&self, _p: &PeerId, _u: &str) -> u64 {
            0
        }
        fn advance_peer_byte_cursor(&self, _p: &PeerId, _u: &str, t: u64) -> u64 {
            t
        }
        fn gc_closed_peers(&self) -> usize {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.next_count.load(Ordering::Relaxed)
        }
    }

    fn build_with_counting() -> (PeerGcUseCase<CountingRegistry>, Arc<CountingRegistry>) {
        let registry = Arc::new(CountingRegistry::default());
        let uc = PeerGcUseCase::new(Arc::clone(&registry));
        (uc, registry)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_with_no_closed_peers_reports_zero() {
        let (uc, registry) = build_with_counting();
        registry.set(0);
        let outcome = uc.execute(PeerGcRequest).await.expect("execute");
        assert_eq!(outcome.peers_dropped, 0);
        assert_eq!(registry.calls(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_with_three_closed_peers_reports_three() {
        let (uc, registry) = build_with_counting();
        registry.set(3);
        let outcome = uc.execute(PeerGcRequest).await.expect("execute");
        assert_eq!(outcome.peers_dropped, 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execute_invokes_registry_exactly_once_per_call() {
        let (uc, registry) = build_with_counting();
        registry.set(1);
        for _ in 0_u8..5 {
            let _ = uc.execute(PeerGcRequest).await.expect("execute");
        }
        assert_eq!(registry.calls(), 5);
    }

    // ---- end-to-end against the real MemoryRegistry --------------------

    fn build_with_memory() -> (
        PeerGcUseCase<MemoryRegistry<StubNotifier>>,
        Arc<MemoryRegistry<StubNotifier>>,
    ) {
        let notifier = Arc::new(StubNotifier);
        let registry = MemoryRegistry::new(notifier);
        let uc = PeerGcUseCase::new(Arc::clone(&registry));
        (uc, registry)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn end_to_end_drops_closed_peer_via_real_registry() {
        let (uc, registry) = build_with_memory();
        let alive = StubPeer::new("alive");
        let dead = StubPeer::new("dead");
        SubscriberRegistryAsync::subscribe(
            registry.as_ref(),
            ResourceKind::Shell,
            "a".to_string(),
            "shell://a/output".to_string(),
            Arc::clone(&alive) as Arc<dyn PeerHandle>,
        )
        .await
        .expect("subscribe alive");
        SubscriberRegistryAsync::subscribe(
            registry.as_ref(),
            ResourceKind::Shell,
            "a".to_string(),
            "shell://a/output".to_string(),
            Arc::clone(&dead) as Arc<dyn PeerHandle>,
        )
        .await
        .expect("subscribe dead");
        dead.close();
        let outcome = uc.execute(PeerGcRequest).await.expect("execute");
        assert_eq!(outcome.peers_dropped, 1);
        let snap = registry.snapshot_subscribers("shell://a/output");
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].peer_id.as_str(), "alive");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn end_to_end_with_no_subscribers_reports_zero() {
        let (uc, _registry) = build_with_memory();
        let outcome = uc.execute(PeerGcRequest).await.expect("execute");
        assert_eq!(outcome.peers_dropped, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn end_to_end_idempotent_after_drop() {
        let (uc, registry) = build_with_memory();
        let dead = StubPeer::new("dead");
        SubscriberRegistryAsync::subscribe(
            registry.as_ref(),
            ResourceKind::Shell,
            "a".to_string(),
            "shell://a/output".to_string(),
            Arc::clone(&dead) as Arc<dyn PeerHandle>,
        )
        .await
        .expect("subscribe dead");
        dead.close();
        // First pass drops the peer.
        let first = uc.execute(PeerGcRequest).await.expect("first");
        assert_eq!(first.peers_dropped, 1);
        // Second pass has nothing left to drop.
        let second = uc.execute(PeerGcRequest).await.expect("second");
        assert_eq!(second.peers_dropped, 0);
    }
}
