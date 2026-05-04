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
        self.subscribers.unsubscribe(&req.peer_id, &canonical).await;
        // v5 lifecycle: debit the resource refcount. Errors here are
        // bug-class (underflow on a never-tracked resource); surface
        // them so the caller can report the regression rather than
        // silently swallow it. The legacy registry tolerates unknown
        // (peer, uri) pairs, but the lifecycle adapter is permissive
        // for unknown resources too — see
        // `RefcountedLifecycleAdapter::on_unsubscribe`.
        self.lifecycle.on_unsubscribe(parsed.kind, &parsed.id)?;
        Ok(UnsubscribeResourceOutcome { uri: canonical })
    }
}

#[cfg(test)]
mod tests {
    use super::{UnsubscribeResourceRequest, UnsubscribeResourceUseCase};
    use crate::domain::error::DomainError;
    use crate::domain::ids::PeerId;
    use crate::ports::notifier::PeerHandle;
    use crate::ports::subscriber_registry::{
        ResourceKind, SubscriberRegistryAsync, SubscriberRegistryPort, SubscriberSnapshot,
    };
    use std::sync::Arc;
    use std::sync::Mutex;

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
        ) -> Result<(), DomainError> {
            Ok(())
        }
        async fn unsubscribe(&self, peer_id: &PeerId, uri: &str) {
            if let Ok(mut g) = self.unsubs.lock() {
                g.push((peer_id.clone(), uri.to_string()));
            }
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
}
