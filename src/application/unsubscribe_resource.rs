//! Use case: unsubscribe a peer from a ssh-mcp resource URI.
//!
//! Translates the legacy v3 `unsubscribe_impl` into a
//! hexagonal use case orchestrating the URI parser and the
//! [`SubscriberRegistryAsync`] port.
//!
//! # Orchestration shape
//!
//! 1. Parse the URI via [`crate::application::read_resource::parse_uri`].
//! 2. Delegate to [`SubscriberRegistryAsync::unsubscribe`] with the
//!    canonical URI form.
//!
//! # Idempotency
//!
//! `unsubscribe` is intentionally a no-op when the `(peer_id, uri)` pair
//! is unknown — the v3 surface mirrors MCP semantics: unsubscribing a
//! peer that is not subscribed is not an error. The use case therefore
//! never returns `Err` for unknown URIs (apart from parse errors).

use std::sync::Arc;

use crate::application::read_resource::{canonical_uri, parse_uri};
use crate::domain::error::DomainError;
use crate::domain::ids::PeerId;
use crate::ports::lifecycle_policy::LifecyclePolicyPort;
use crate::ports::subscriber_registry::SubscriberRegistryAsync;

/// Inbound DTO.
#[derive(Debug, Clone)]
pub struct UnsubscribeResourceRequest {
    /// Caller-supplied URI.
    pub uri: String,
    /// Peer id (must match the id used at subscribe time).
    pub peer_id: PeerId,
}

/// Outbound DTO surfacing the canonical URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsubscribeResourceOutcome {
    /// Canonical URI used as the registry key.
    pub uri: String,
}

/// Unsubscribe-resource use case.
#[derive(Debug)]
pub struct UnsubscribeResourceUseCase<Sub>
where
    Sub: SubscriberRegistryAsync + Send + Sync,
{
    subscribers: Arc<Sub>,
    lifecycle: Arc<dyn LifecyclePolicyPort>,
}

impl<Sub> UnsubscribeResourceUseCase<Sub>
where
    Sub: SubscriberRegistryAsync + Send + Sync,
{
    /// Wire the use case from an already-shared adapter handle.
    #[must_use]
    pub const fn new(subscribers: Arc<Sub>, lifecycle: Arc<dyn LifecyclePolicyPort>) -> Self {
        Self {
            subscribers,
            lifecycle,
        }
    }

    /// Drive the unsubscribe orchestration. See module-level docs.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidArgument`] when the URI fails to
    /// parse. Unknown URIs / not-subscribed peers are intentionally a
    /// silent success.
    pub async fn execute(
        &self,
        req: UnsubscribeResourceRequest,
    ) -> Result<UnsubscribeResourceOutcome, DomainError> {
        let parsed =
            parse_uri(&req.uri).map_err(|e| DomainError::InvalidArgument(e.to_string()))?;
        let canonical = canonical_uri(parsed.kind, &parsed.id);
        let was_subscribed = self.subscribers.unsubscribe(&req.peer_id, &canonical).await;
        // v5 lifecycle: debit the resource refcount ONLY when the
        // registry actually removed an entry. Calling on_unsubscribe
        // unconditionally causes sub_count underflow on idempotent
        // (peer, uri) pairs that were not subscribed — the second
        // resources/unsubscribe for the same URI would decrement below
        // zero, returning DomainError::Internal and surfacing as an
        // internal-error RPC failure to the caller.
        if was_subscribed {
            self.lifecycle.on_unsubscribe(parsed.kind, &parsed.id)?;
            // The sync `on_unsubscribe` only stamps the grace deadline
            // when the last subscriber leaves a `release_when_no_subs`
            // resource. Spawn the grace timer here so the resource
            // actually advances `Releasing -> Closed`, firing the cascade
            // that debits the owning session's refcount. No-op unless the
            // resource is in `Releasing` (a concurrent resubscribe may
            // have already reverted it to `Observed`).
            self.lifecycle.arm_release_timer(parsed.kind, &parsed.id);
        }
        Ok(UnsubscribeResourceOutcome { uri: canonical })
    }
}

#[cfg(test)]
mod tests {
    use super::{UnsubscribeResourceRequest, UnsubscribeResourceUseCase};
    use crate::adapters::id_generator::uuid::UuidIds;
    use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
    use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
    use crate::adapters::subscription::memory_registry::MemoryRegistry;
    use crate::adapters::subscription::subscriber_lane::SubscriberLaneAdapter;
    use crate::application::subscribe_resource::{
        SubscribeResourceRequest, SubscribeResourceUseCase,
    };
    use crate::domain::error::DomainError;
    use crate::domain::ids::{PeerId, SessionId, ShellId};
    use crate::domain::lifecycle::LifecyclePolicy;
    use crate::domain::shell::{ShellEntity, ShellTerminal};
    use crate::ports::lifecycle_policy::LifecyclePolicyPort;
    use crate::ports::notifier::{NotifierPort, PeerHandle};
    use crate::ports::shell_repo::ShellRepository;
    use crate::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
    use crate::ports::subscriber_registry::{
        ResourceKind, SubscriberRegistryAsync, SubscriberRegistryPort, SubscriberSnapshot,
    };
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Debug, Default)]
    struct RecordingRegistry {
        unsubs: Mutex<Vec<(PeerId, String)>>,
        drops: Mutex<Vec<PeerId>>,
    }

    impl RecordingRegistry {
        fn unsubs(&self) -> Vec<(PeerId, String)> {
            self.unsubs
                .lock()
                .map_or_else(|p| p.into_inner().clone(), |g| g.clone())
        }
    }

    impl SubscriberRegistryPort for RecordingRegistry {
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
            0
        }
    }

    impl SubscriberRegistryAsync for RecordingRegistry {
        async fn subscribe(
            &self,
            _kind: ResourceKind,
            _resource_id: String,
            _uri: String,
            _peer: Arc<dyn PeerHandle>,
        ) -> Result<crate::ports::subscriber_registry::SubscribeDisposition, DomainError> {
            Ok(crate::ports::subscriber_registry::SubscribeDisposition::Created)
        }
        async fn unsubscribe(&self, peer_id: &PeerId, uri: &str) -> bool {
            if let Ok(mut g) = self.unsubs.lock() {
                g.push((peer_id.clone(), uri.to_string()));
            }
            // Test stub: always report that the peer was subscribed so
            // the use case exercises the lifecycle on_unsubscribe path.
            true
        }
        async fn drop_peer(&self, peer_id: &PeerId) {
            if let Ok(mut g) = self.drops.lock() {
                g.push(peer_id.clone());
            }
        }
    }

    fn build() -> (
        UnsubscribeResourceUseCase<RecordingRegistry>,
        Arc<RecordingRegistry>,
    ) {
        let registry = Arc::new(RecordingRegistry::default());
        let cascade = crate::adapters::lifecycle::cascade::CascadeCoordinator::new();
        let clock = Arc::new(crate::adapters::clock::system::SystemClock);
        let lifecycle: Arc<dyn crate::ports::lifecycle_policy::LifecyclePolicyPort> =
            crate::adapters::lifecycle::refcount::RefcountedLifecycleAdapter::new(cascade, clock);
        let uc = UnsubscribeResourceUseCase::new(Arc::clone(&registry), lifecycle);
        (uc, registry)
    }

    fn req(uri: &str, peer: &str) -> UnsubscribeResourceRequest {
        UnsubscribeResourceRequest {
            uri: uri.to_string(),
            peer_id: PeerId::new(peer.to_string()),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsubscribe_routes_to_canonical_uri() {
        let (uc, registry) = build();
        let outcome = uc
            .execute(req("shell://sh-1/output?cursor=auto", "peer-1"))
            .await
            .expect("execute");
        assert_eq!(outcome.uri, "shell://sh-1/output");
        let calls = registry.unsubs();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.as_str(), "peer-1");
        assert_eq!(calls[0].1, "shell://sh-1/output");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsubscribe_is_idempotent_for_unknown_peer() {
        // The registry's `unsubscribe` is a silent no-op for unknown
        // pairs; the use case therefore returns `Ok` even when nothing
        // was registered. We exercise the call twice with the same args
        // to confirm the second call also succeeds.
        let (uc, registry) = build();
        for _ in 0_u8..2 {
            let _ = uc
                .execute(req("shell://sh-1/output", "peer-ghost"))
                .await
                .expect("execute");
        }
        let calls = registry.unsubs();
        assert_eq!(calls.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn malformed_uri_returns_invalid_argument() {
        let (uc, registry) = build();
        let err = uc
            .execute(req("ftp://x/output", "peer-1"))
            .await
            .expect_err("must fail");
        match err {
            DomainError::InvalidArgument(msg) => assert!(msg.contains("ftp")),
            other => panic!("unexpected: {other:?}"),
        }
        // Registry must not be invoked when the URI fails to parse.
        assert!(registry.unsubs().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsubscribe_routes_each_scheme_to_canonical_form() {
        let (uc, registry) = build();
        let cases = [
            ("shell://x/output", "shell://x/output"),
            ("command://x/output", "command://x/output"),
            ("transfer://x/progress", "transfer://x/progress"),
            ("session://x/health", "session://x/health"),
            ("forward://x/events", "forward://x/events"),
        ];
        for (uri, expected) in cases {
            let outcome = uc.execute(req(uri, "peer-1")).await.expect("execute");
            assert_eq!(outcome.uri, expected);
        }
        assert_eq!(registry.unsubs().len(), 5);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_id_returns_invalid_argument() {
        let (uc, _registry) = build();
        let err = uc
            .execute(req("shell:///output", "peer-1"))
            .await
            .expect_err("must fail");
        match err {
            DomainError::InvalidArgument(msg) => assert!(msg.to_lowercase().contains("missing")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsubscribe_carries_peer_id_through_to_registry() {
        let (uc, registry) = build();
        let _ = uc
            .execute(req("session://s/health", "peer-9"))
            .await
            .expect("execute");
        let calls = registry.unsubs();
        assert_eq!(calls[0].0.as_str(), "peer-9");
    }

    // --- BUG #5: grace timer armed on the production unsubscribe path ---

    #[tokio::test(flavor = "multi_thread")]
    async fn release_resource_reaches_closed_via_production_unsubscribe_path() {
        // A `release_when_no_subs` resource whose last subscriber leaves
        // via the production `execute` path must actually advance to
        // Closed: the use case has to spawn the grace timer (the sync
        // `on_unsubscribe` only stamps the deadline). Before the fix the
        // resource stayed stuck in Releasing forever (session-refcount
        // leak).
        use std::time::Duration;

        use crate::adapters::clock::fake::FakeClock;
        use crate::adapters::lifecycle::cascade::CascadeCoordinator;
        use crate::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
        use crate::domain::ids::SessionId;
        use crate::domain::lifecycle::{LifecyclePolicy, LifecycleState};
        use crate::ports::lifecycle_policy::LifecyclePolicyPort;
        use crate::ports::subscriber_registry::ResourceKind;

        let registry = Arc::new(RecordingRegistry::default());
        let cascade = CascadeCoordinator::new();
        let clock = Arc::new(FakeClock::new(1_000_000));
        let lifecycle_concrete =
            RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));

        // Track + subscribe a release_when_no_subs resource so the last
        // unsubscribe drives Observed -> Releasing.
        let mut policy = LifecyclePolicy::release_with_default_grace();
        policy.grace_ms = 1_000;
        let session = SessionId::new("s-1".to_string());
        lifecycle_concrete.track_resource(ResourceKind::Shell, "sh-1", &session, policy);
        lifecycle_concrete
            .on_subscribe(ResourceKind::Shell, "sh-1")
            .expect("sub");

        let lifecycle: Arc<dyn LifecyclePolicyPort> =
            Arc::<RefcountedLifecycleAdapter<FakeClock>>::clone(&lifecycle_concrete);
        let uc = UnsubscribeResourceUseCase::new(Arc::clone(&registry), lifecycle);

        uc.execute(UnsubscribeResourceRequest {
            uri: "shell://sh-1/output".to_string(),
            peer_id: PeerId::new("peer-1".to_string()),
        })
        .await
        .expect("execute");

        // The production path armed the grace timer. Advance the clock past
        // the deadline and wait for the detached timer to fire the close.
        clock.advance(Duration::from_millis(5_000));
        let mut closed = false;
        for _ in 0_u8..60 {
            if lifecycle_concrete
                .snapshot(ResourceKind::Shell, "sh-1")
                .expect("snap")
                .state
                == LifecycleState::Closed
            {
                closed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            closed,
            "release resource must reach Closed via the production unsubscribe path"
        );
        assert_eq!(
            cascade.session_active_refs(&session),
            0,
            "cascade must debit the session refcount once the resource closes"
        );
    }

    const UNSUB_URI: &str = "shell://sh-unsub/output";

    #[derive(Debug, Default)]
    struct UnsubNotifier;

    impl NotifierPort for UnsubNotifier {
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
        closed: std::sync::atomic::AtomicBool,
    }

    impl StubPeer {
        fn new(id: &str) -> Arc<Self> {
            Arc::new(Self {
                id: PeerId::new(id.to_string()),
                closed: std::sync::atomic::AtomicBool::new(false),
            })
        }
    }

    impl PeerHandle for StubPeer {
        fn id(&self) -> PeerId {
            self.id.clone()
        }
        fn is_closed(&self) -> bool {
            self.closed.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    struct UnsubHarness {
        subscribe: SubscribeResourceUseCase<
            DashMapShellRepo,
            DashMapCommandRepo,
            DashMapTransferRepo,
            DashMapSessionRepo,
            MemoryRegistry<UnsubNotifier>,
        >,
        unsubscribe: UnsubscribeResourceUseCase<MemoryRegistry<UnsubNotifier>>,
        registry: Arc<MemoryRegistry<UnsubNotifier>>,
        lanes: Arc<SubscriberLaneAdapter<UuidIds>>,
        lifecycle: Arc<dyn LifecyclePolicyPort>,
        shells: Arc<DashMapShellRepo>,
    }

    fn unsub_harness() -> UnsubHarness {
        let shells = Arc::new(DashMapShellRepo::new());
        let commands = Arc::new(DashMapCommandRepo::new());
        let transfers = Arc::new(DashMapTransferRepo::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        let registry = MemoryRegistry::new(Arc::new(UnsubNotifier));
        let cascade = crate::adapters::lifecycle::cascade::CascadeCoordinator::new();
        let clock = Arc::new(crate::adapters::clock::system::SystemClock);
        let lifecycle_concrete =
            crate::adapters::lifecycle::refcount::RefcountedLifecycleAdapter::new(cascade, clock);
        let lifecycle: Arc<dyn LifecyclePolicyPort> =
            Arc::clone(&lifecycle_concrete) as Arc<dyn LifecyclePolicyPort>;
        let lanes = SubscriberLaneAdapter::new(Arc::new(UuidIds), 1, 16, 64);
        let subscribe = SubscribeResourceUseCase::new(
            Arc::clone(&shells),
            Arc::clone(&commands),
            Arc::clone(&transfers),
            Arc::clone(&sessions),
            Arc::clone(&registry),
            Arc::clone(&lifecycle),
        );
        let unsubscribe =
            UnsubscribeResourceUseCase::new(Arc::clone(&registry), Arc::clone(&lifecycle));
        UnsubHarness {
            subscribe,
            unsubscribe,
            registry,
            lanes,
            lifecycle,
            shells,
        }
    }

    async fn seed_unsub_shell(h: &UnsubHarness) {
        h.shells
            .insert(ShellEntity::new(
                ShellId::new("sh-unsub".to_string()),
                SessionId::new("s".to_string()),
                ShellTerminal::new("xterm".to_string(), 80, 24),
                Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
                Duration::from_secs(60),
                10 * 1024 * 1024,
            ))
            .await
            .unwrap();
        h.lifecycle.track_resource(
            ResourceKind::Shell,
            "sh-unsub",
            &SessionId::new("s".to_string()),
            LifecyclePolicy::default(),
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resources_unsubscribe_removes_the_registry_entry() {
        let h = unsub_harness();
        seed_unsub_shell(&h).await;
        h.subscribe
            .execute(SubscribeResourceRequest {
                uri: UNSUB_URI.to_string(),
                peer: StubPeer::new("peer-unsub") as Arc<dyn PeerHandle>,
            })
            .await
            .unwrap();
        h.unsubscribe
            .execute(UnsubscribeResourceRequest {
                uri: UNSUB_URI.to_string(),
                peer_id: PeerId::new("peer-unsub".to_string()),
            })
            .await
            .unwrap();
        assert!(h.registry.snapshot_subscribers(UNSUB_URI).is_empty());
        h.unsubscribe
            .execute(UnsubscribeResourceRequest {
                uri: UNSUB_URI.to_string(),
                peer_id: PeerId::new("peer-unsub".to_string()),
            })
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resources_unsubscribe_leaves_a_sub_open_lane_open() {
        let h = unsub_harness();
        seed_unsub_shell(&h).await;
        let sub_id = h
            .lanes
            .open_lane(
                UNSUB_URI.to_string(),
                ResourceKind::Shell,
                "sh-unsub".to_string(),
                LanePolicy::default(),
            )
            .await
            .unwrap();
        h.subscribe
            .execute(SubscribeResourceRequest {
                uri: UNSUB_URI.to_string(),
                peer: StubPeer::new("peer-keep") as Arc<dyn PeerHandle>,
            })
            .await
            .unwrap();
        h.unsubscribe
            .execute(UnsubscribeResourceRequest {
                uri: UNSUB_URI.to_string(),
                peer_id: PeerId::new("peer-keep".to_string()),
            })
            .await
            .unwrap();
        assert!(h.lanes.stats_snapshot(&sub_id).is_some());
    }
}
