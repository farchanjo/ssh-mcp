//! Chaos 37 — v4 legacy `subscribe(PeerId, Uri)` path coexists with
//! v5 features.
//!
//! The legacy `SubscriberRegistryAsync::subscribe` entry point
//! synthesises a fresh `UUIDv7` `SubId` so v5 hosts that consume
//! `_meta.sub_id` see a stable handle while v4 hosts (that ignore the
//! field) keep their `(peer_id, uri)` cursor semantics. Independently,
//! the lifecycle adapter applies `release_when_no_subs` + lag policy
//! (via the channel mux) to the same resource. Requirement: both
//! surfaces remain self-consistent.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ssh_mcp::adapters::clock::fake::FakeClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::adapters::subscription::memory_registry::MemoryRegistry;
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::ids::{PeerId, SessionId};
use ssh_mcp::domain::lifecycle::{LifecyclePolicy, LifecycleState};
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::notifier::{NotifierPort, PeerHandle};
use ssh_mcp::ports::subscriber_registry::{
    ResourceKind, SubscriberRegistryAsync, SubscriberRegistryPort,
};
use uuid::Uuid;

/// Test notifier that ignores every fan-out — the chaos test does not
/// assert on the debouncer side.
#[derive(Debug, Default)]
struct NoopNotifier;

impl NotifierPort for NoopNotifier {
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
        _payload: ssh_mcp::domain::inline_payload::InlinePayload,
    ) -> Result<(), DomainError> {
        Ok(())
    }
}

#[derive(Debug)]
struct StubPeer {
    id: PeerId,
    closed: AtomicBool,
}

impl PeerHandle for StubPeer {
    fn id(&self) -> PeerId {
        self.id.clone()
    }
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos37_legacy_subscribe_synthesises_sub_id_and_lifecycle_applies() {
    // 1. Lifecycle adapter — v5 release_when_no_subs is in force.
    let clock = Arc::new(FakeClock::new(1_000_000));
    let cascade = CascadeCoordinator::new();
    let lifecycle = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));
    let mut policy = LifecyclePolicy::release_with_default_grace();
    policy.grace_ms = 100;
    lifecycle.track_resource(
        ResourceKind::Shell,
        "compat-shell",
        &SessionId::new("s".to_string()),
        policy,
    );

    // 2. Memory registry — legacy (peer_id, uri) entry point.
    let registry: Arc<MemoryRegistry<NoopNotifier>> = MemoryRegistry::new(Arc::new(NoopNotifier));
    let peer_id = PeerId::new("peer-compat".to_string());
    let peer: Arc<dyn PeerHandle> = Arc::new(StubPeer {
        id: peer_id.clone(),
        closed: AtomicBool::new(false),
    });

    // Legacy path: subscribe with (PeerId, Uri) — registry MUST mint a
    // UUIDv7 SubId behind the scenes.
    registry
        .subscribe(
            ResourceKind::Shell,
            "compat-shell".to_string(),
            "shell://compat-shell/output".to_string(),
            Arc::clone(&peer),
        )
        .await
        .unwrap();
    let sub_id = registry
        .sub_id_for(&peer_id, "shell://compat-shell/output")
        .expect("legacy subscribe must synthesise a SubId");
    let parsed = Uuid::parse_str(sub_id.as_str()).expect("synthesised SubId is a UUID");
    assert_eq!(
        parsed.get_version_num(),
        7,
        "synthesised SubId is not UUIDv7: {sub_id:?}",
    );

    // Legacy snapshot is non-empty.
    let snapshot = registry.snapshot_subscribers("shell://compat-shell/output");
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].peer_id, peer_id);

    // 3. v5 lifecycle still applies — drive subscribe + unsubscribe
    // through the lifecycle adapter and verify the grace timer arms.
    lifecycle
        .on_subscribe(ResourceKind::Shell, "compat-shell")
        .unwrap();
    let snap = lifecycle
        .snapshot(ResourceKind::Shell, "compat-shell")
        .unwrap();
    assert_eq!(snap.state, LifecycleState::Observed);
    lifecycle
        .on_unsubscribe(ResourceKind::Shell, "compat-shell")
        .unwrap();
    let snap = lifecycle
        .snapshot(ResourceKind::Shell, "compat-shell")
        .unwrap();
    // release_when_no_subs flipped state to Releasing on last unsub.
    assert_eq!(
        snap.state,
        LifecycleState::Releasing,
        "v5 lifecycle must apply alongside v4 subscribe path: snap={snap:?}",
    );
    assert!(snap.grace_remaining_ms.is_some());

    // 4. Cleanup: registry unsubscribe + lifecycle force_close.
    registry
        .unsubscribe(&peer_id, "shell://compat-shell/output")
        .await;
    lifecycle
        .force_close(ResourceKind::Shell, "compat-shell")
        .unwrap();
    assert_eq!(
        cascade.session_active_refs(&SessionId::new("s".to_string())),
        0
    );
}
