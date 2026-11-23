//! Use case: register a peer for `resources/updated` notifications on a
//! ssh-mcp resource URI.
//!
//! Translates the legacy v3 `subscribe_impl` into a
//! hexagonal use case orchestrating the URI parser, the four/five
//! repositories (to confirm the resource exists), and the
//! [`SubscriberRegistryAsync`] port.
//!
//! # Orchestration shape
//!
//! 1. Parse the URI via [`crate::application::read_resource::parse_uri`]
//!    (re-exported here through `super::read_resource`).
//! 2. Confirm the resource exists by issuing a `get`-style call to the
//!    relevant repository (forwards always succeed because the v3 surface
//!    keeps the door open even before the producer publishes the first
//!    event — the registry can hold the subscription waiting for events).
//! 3. Delegate to [`SubscriberRegistryAsync::subscribe`] with the
//!    canonical URI form so callers always cursor against the same key.
//!
//! # Concurrency contract
//!
//! Every `.await` happens against owned types — no shard guards leak.

use std::sync::Arc;

use crate::application::read_resource::{canonical_uri, parse_uri};
use crate::domain::error::DomainError;
use crate::domain::ids::{CommandId, SessionId, ShellId, TransferId};
use crate::ports::command_repo::CommandRepository;
use crate::ports::lifecycle_policy::LifecyclePolicyPort;
use crate::ports::notifier::PeerHandle;
use crate::ports::session_repo::SessionRepository;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::subscriber_registry::{
    ResourceKind, SubscribeDisposition, SubscriberRegistryAsync,
};
use crate::ports::transfer_repo::TransferRepository;

/// Inbound DTO. The peer handle is wrapped in [`Arc`] so the registry
/// can keep its own clone alongside the use-case's.
#[derive(Debug, Clone)]
pub struct SubscribeResourceRequest {
    /// Caller-supplied URI.
    pub uri: String,
    /// Live peer handle the registry will use to fan out notifications.
    pub peer: Arc<dyn PeerHandle>,
}

/// Outbound DTO surfacing the canonical URI the inbound adapter must
/// echo back to the client. The subscription is peer-keyed and owns no
/// lane, so no `SubId` is minted for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribeResourceOutcome {
    /// Canonical URI (no `?cursor=` — the registry keys against this form).
    pub uri: String,
}

/// Subscribe-resource use case.
///
/// The forward repository is intentionally **not** a type parameter: the
/// subscribe path never queries it (forward URIs are accepted permissively
/// so subscribers can attach before the producer publishes), so carrying
/// `FR` here would be dead plumbing. This keeps a single shape across both
/// `port_forward` feature configurations.
#[derive(Debug)]
pub struct SubscribeResourceUseCase<ShR, CR, TR, SR, Sub>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    Sub: SubscriberRegistryAsync + Send + Sync,
{
    shells: Arc<ShR>,
    commands: Arc<CR>,
    transfers: Arc<TR>,
    sessions: Arc<SR>,
    subscribers: Arc<Sub>,
    lifecycle: Arc<dyn LifecyclePolicyPort>,
}

impl<ShR, CR, TR, SR, Sub> SubscribeResourceUseCase<ShR, CR, TR, SR, Sub>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    Sub: SubscriberRegistryAsync + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(
        shells: Arc<ShR>,
        commands: Arc<CR>,
        transfers: Arc<TR>,
        sessions: Arc<SR>,
        subscribers: Arc<Sub>,
        lifecycle: Arc<dyn LifecyclePolicyPort>,
    ) -> Self {
        Self {
            shells,
            commands,
            transfers,
            sessions,
            subscribers,
            lifecycle,
        }
    }

    /// Drive the subscribe orchestration.
    ///
    /// # Errors
    ///
    /// - [`DomainError::InvalidArgument`] when the URI fails to parse.
    /// - Resource-specific not-found errors when the URI does not
    ///   resolve to a live aggregate (forward URIs always succeed —
    ///   subscribers can attach before the producer publishes).
    pub async fn execute(
        &self,
        req: SubscribeResourceRequest,
    ) -> Result<SubscribeResourceOutcome, DomainError> {
        let parsed =
            parse_uri(&req.uri).map_err(|e| DomainError::InvalidArgument(e.to_string()))?;
        ensure_exists(
            parsed.kind,
            &parsed.id,
            &*self.shells,
            &*self.commands,
            &*self.transfers,
            &*self.sessions,
        )
        .await?;
        let canonical = canonical_uri(parsed.kind, &parsed.id);
        let parsed_kind = parsed.kind;
        let parsed_id = parsed.id.clone();
        let peer_id = req.peer.id();
        let disposition = self
            .subscribers
            .subscribe(parsed.kind, parsed.id, canonical.clone(), req.peer)
            .await?;
        // v5 lifecycle: the refcount is per `(peer, uri)` entry, so only a
        // created entry increments it. A refresh replaces the record in
        // place and leaves the count alone.
        if disposition == SubscribeDisposition::Created
            && let Err(err) = self.lifecycle.on_subscribe(parsed_kind, &parsed_id)
        {
            // The call is about to fail; do not leave a live subscription
            // behind. The lifecycle has already restored its count.
            let _ = self.subscribers.unsubscribe(&peer_id, &canonical).await;
            return Err(err);
        }
        Ok(SubscribeResourceOutcome { uri: canonical })
    }
}

pub(crate) async fn ensure_exists<ShR, CR, TR, SR>(
    kind: ResourceKind,
    id: &str,
    shells: &ShR,
    commands: &CR,
    transfers: &TR,
    sessions: &SR,
) -> Result<(), DomainError>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
{
    match kind {
        ResourceKind::Shell => ensure_shell(id, shells).await,
        ResourceKind::Command => ensure_command(id, commands).await,
        ResourceKind::Transfer => ensure_transfer(id, transfers).await,
        ResourceKind::Session => ensure_session(id, sessions).await,
        // v3 default: subscribers may attach to a forward URI
        // before the producer publishes the first event.
        // v5.2 (ADR 0009): serial state lives outside the
        // application repositories — the infra-layer
        // `serial_open` already validates that the SerialId
        // exists before any subscribe call reaches this path,
        // so we accept the attach with the same forward-style
        // semantics.
        // v7.0 (ADR 0011): rsync sessions live outside the application
        // repositories — the infra-layer `ssh_rsync` mints the rsync_id
        // and registers the lane before any subscribe call reaches this
        // path, so we accept the attach with the same forward-style
        // semantics.
        ResourceKind::Forward | ResourceKind::Serial | ResourceKind::Rsync => Ok(()),
    }
}

async fn ensure_shell<ShR: ShellRepository + Send + Sync>(
    id: &str,
    shells: &ShR,
) -> Result<(), DomainError> {
    let id = ShellId::new(id.to_string());
    if shells.get(&id).await?.is_none() {
        return Err(DomainError::ShellNotFound(id));
    }
    Ok(())
}

async fn ensure_command<CR: CommandRepository + Send + Sync>(
    id: &str,
    commands: &CR,
) -> Result<(), DomainError> {
    let id = CommandId::new(id.to_string());
    if commands.get(&id).await?.is_none() {
        return Err(DomainError::CommandNotFound(id));
    }
    Ok(())
}

async fn ensure_transfer<TR: TransferRepository + Send + Sync>(
    id: &str,
    transfers: &TR,
) -> Result<(), DomainError> {
    let id = TransferId::new(id.to_string());
    if transfers.get(&id).await?.is_none() {
        return Err(DomainError::TransferNotFound(id));
    }
    Ok(())
}

async fn ensure_session<SR: SessionRepository + Send + Sync>(
    id: &str,
    sessions: &SR,
) -> Result<(), DomainError> {
    let id = SessionId::new(id.to_string());
    if sessions.get(&id).await?.is_none() {
        return Err(DomainError::SessionNotFound(id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{SubscribeResourceRequest, SubscribeResourceUseCase};
    use crate::adapters::id_generator::uuid::UuidIds;
    use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
    use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
    use crate::adapters::subscription::memory_registry::MemoryRegistry;
    use crate::adapters::subscription::subscriber_lane::SubscriberLaneAdapter;
    use crate::domain::command::CommandEntity;
    use crate::domain::error::DomainError;
    use crate::domain::identity::Address;
    use crate::domain::ids::{CommandId, PeerId, SessionId, ShellId, TransferId};
    use crate::domain::lifecycle::LifecyclePolicy;
    use crate::domain::session::SessionEntity;
    use crate::domain::shell::{ShellEntity, ShellTerminal};
    use crate::domain::transfer::{TransferDirection, TransferEntity};
    use crate::ports::command_repo::CommandRepository;
    use crate::ports::lifecycle_policy::LifecyclePolicyPort;
    use crate::ports::notifier::{NotifierPort, PeerHandle};
    use crate::ports::session_repo::SessionRepository;
    use crate::ports::shell_repo::ShellRepository;
    use crate::ports::subscriber_lane::SubscriberLanePort;
    use crate::ports::subscriber_registry::{
        ResourceKind, SubscriberRegistryAsync, SubscriberRegistryPort, SubscriberSnapshot,
    };
    use crate::ports::transfer_repo::TransferRepository;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

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

    #[derive(Debug, Default)]
    struct RecordingRegistry {
        subs: Mutex<Vec<(ResourceKind, String, String, PeerId)>>,
    }

    impl RecordingRegistry {
        fn calls(&self) -> Vec<(ResourceKind, String, String, PeerId)> {
            self.subs
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
            kind: ResourceKind,
            resource_id: String,
            uri: String,
            peer: Arc<dyn PeerHandle>,
        ) -> Result<crate::ports::subscriber_registry::SubscribeDisposition, DomainError> {
            if let Ok(mut g) = self.subs.lock() {
                g.push((kind, resource_id, uri, peer.id()));
            }
            Ok(crate::ports::subscriber_registry::SubscribeDisposition::Created)
        }
        async fn unsubscribe(&self, _peer_id: &PeerId, _uri: &str) -> bool {
            false
        }
        async fn drop_peer(&self, _peer_id: &PeerId) {}
    }

    type UseCaseUnderTest = SubscribeResourceUseCase<
        DashMapShellRepo,
        DashMapCommandRepo,
        DashMapTransferRepo,
        DashMapSessionRepo,
        RecordingRegistry,
    >;

    struct Harness {
        uc: UseCaseUnderTest,
        shells: Arc<DashMapShellRepo>,
        commands: Arc<DashMapCommandRepo>,
        transfers: Arc<DashMapTransferRepo>,
        sessions: Arc<DashMapSessionRepo>,
        registry: Arc<RecordingRegistry>,
    }

    fn build_harness() -> Harness {
        let shells = Arc::new(DashMapShellRepo::new());
        let commands = Arc::new(DashMapCommandRepo::new());
        let transfers = Arc::new(DashMapTransferRepo::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        let registry = Arc::new(RecordingRegistry::default());
        let cascade = crate::adapters::lifecycle::cascade::CascadeCoordinator::new();
        let clock = Arc::new(crate::adapters::clock::system::SystemClock);
        let lifecycle: Arc<dyn crate::ports::lifecycle_policy::LifecyclePolicyPort> =
            crate::adapters::lifecycle::refcount::RefcountedLifecycleAdapter::new(cascade, clock);
        let uc = SubscribeResourceUseCase::new(
            Arc::clone(&shells),
            Arc::clone(&commands),
            Arc::clone(&transfers),
            Arc::clone(&sessions),
            Arc::clone(&registry),
            lifecycle,
        );
        Harness {
            uc,
            shells,
            commands,
            transfers,
            sessions,
            registry,
        }
    }

    fn sample_address() -> Address {
        Address::new("h.example.com".to_string(), 22).expect("address")
    }

    async fn seed_shell(repo: &DashMapShellRepo, id: &str) {
        repo.insert(ShellEntity::new(
            ShellId::new(id.to_string()),
            SessionId::new("s".to_string()),
            ShellTerminal::new("xterm".to_string(), 80, 24),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            Duration::from_secs(60),
            10 * 1024 * 1024,
        ))
        .await
        .expect("seed");
    }

    async fn seed_command(repo: &DashMapCommandRepo, id: &str) {
        repo.insert(CommandEntity::new(
            CommandId::new(id.to_string()),
            SessionId::new("s".to_string()),
            "echo".to_string(),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        ))
        .await
        .expect("seed");
    }

    async fn seed_transfer(repo: &DashMapTransferRepo, id: &str) {
        repo.insert(TransferEntity::new(
            TransferId::new(id.to_string()),
            SessionId::new("s".to_string()),
            TransferDirection::Upload,
            "/tmp/x".to_string(),
            "/r/x".to_string(),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            1024,
        ))
        .await
        .expect("seed");
    }

    async fn seed_session(repo: &DashMapSessionRepo, id: &str) {
        repo.insert(SessionEntity {
            id: SessionId::new(id.to_string()),
            name: None,
            agent_id: None,
            address: sample_address(),
            username: "alice".to_string(),
            connected_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            default_timeout: Duration::from_secs(30),
            retry_attempts: 0,
            compression_enabled: true,
            last_health_check: None,
            healthy: None,
        })
        .await
        .expect("seed");
    }

    fn req(uri: &str, peer_id: &str) -> SubscribeResourceRequest {
        SubscribeResourceRequest {
            uri: uri.to_string(),
            peer: StubPeer::new(peer_id) as Arc<dyn PeerHandle>,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_to_known_shell_succeeds_and_records_call() {
        let h = build_harness();
        seed_shell(&h.shells, "sh-1").await;
        let outcome =
            h.uc.execute(req("shell://sh-1/output", "peer-1"))
                .await
                .expect("execute");
        assert_eq!(outcome.uri, "shell://sh-1/output");
        let calls = h.registry.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, ResourceKind::Shell);
        assert_eq!(calls[0].1, "sh-1");
        assert_eq!(calls[0].2, "shell://sh-1/output");
        assert_eq!(calls[0].3.as_str(), "peer-1");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_to_unknown_shell_returns_shell_not_found() {
        let h = build_harness();
        let err =
            h.uc.execute(req("shell://ghost/output", "peer-1"))
                .await
                .expect_err("must fail");
        match err {
            DomainError::ShellNotFound(id) => assert_eq!(id.as_str(), "ghost"),
            other => panic!("unexpected: {other:?}"),
        }
        // Registry was never invoked.
        assert!(h.registry.calls().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_to_known_command_succeeds() {
        let h = build_harness();
        seed_command(&h.commands, "c-1").await;
        let _ =
            h.uc.execute(req("command://c-1/output", "peer-1"))
                .await
                .expect("execute");
        let calls = h.registry.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, ResourceKind::Command);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_to_unknown_command_returns_command_not_found() {
        let h = build_harness();
        let err =
            h.uc.execute(req("command://ghost/output", "peer-1"))
                .await
                .expect_err("must fail");
        match err {
            DomainError::CommandNotFound(id) => assert_eq!(id.as_str(), "ghost"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_to_known_transfer_succeeds() {
        let h = build_harness();
        seed_transfer(&h.transfers, "t-1").await;
        let _ =
            h.uc.execute(req("transfer://t-1/progress", "peer-1"))
                .await
                .expect("execute");
        let calls = h.registry.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, ResourceKind::Transfer);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_to_known_session_succeeds() {
        let h = build_harness();
        seed_session(&h.sessions, "s-1").await;
        let _ =
            h.uc.execute(req("session://s-1/health", "peer-1"))
                .await
                .expect("execute");
        let calls = h.registry.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, ResourceKind::Session);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_to_forward_uri_always_succeeds() {
        // Forwards are permissive in v3 to allow attaching before the
        // producer publishes the first event. The use case mirrors that
        // behaviour for both feature builds.
        let h = build_harness();
        let outcome =
            h.uc.execute(req("forward://f-anything/events", "peer-1"))
                .await
                .expect("execute");
        assert_eq!(outcome.uri, "forward://f-anything/events");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn malformed_uri_returns_invalid_argument() {
        let h = build_harness();
        let err =
            h.uc.execute(req("ftp://x/output", "peer-1"))
                .await
                .expect_err("must fail");
        match err {
            DomainError::InvalidArgument(msg) => assert!(msg.contains("ftp")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cursor_query_in_uri_is_stripped_from_canonical() {
        let h = build_harness();
        seed_shell(&h.shells, "sh-1").await;
        let outcome =
            h.uc.execute(req("shell://sh-1/output?cursor=auto", "peer-1"))
                .await
                .expect("execute");
        // Canonical URI must not carry the query string.
        assert_eq!(outcome.uri, "shell://sh-1/output");
        let calls = h.registry.calls();
        assert_eq!(calls[0].2, "shell://sh-1/output");
    }

    const RESUB_URI: &str = "shell://sh-resub/output";

    #[derive(Debug, Default)]
    struct ResubNotifier {
        events: Mutex<Vec<(PeerId, String)>>,
    }

    impl NotifierPort for ResubNotifier {
        async fn notify_resource_updated(
            &self,
            peer: Arc<dyn PeerHandle>,
            uri: &str,
        ) -> Result<(), DomainError> {
            self.events
                .lock()
                .unwrap()
                .push((peer.id(), uri.to_string()));
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

    type ResubSubscribe = SubscribeResourceUseCase<
        DashMapShellRepo,
        DashMapCommandRepo,
        DashMapTransferRepo,
        DashMapSessionRepo,
        MemoryRegistry<ResubNotifier>,
    >;

    struct ResubHarness {
        subscribe: ResubSubscribe,
        unsubscribe: crate::application::unsubscribe_resource::UnsubscribeResourceUseCase<
            MemoryRegistry<ResubNotifier>,
        >,
        registry: Arc<MemoryRegistry<ResubNotifier>>,
        lanes: Arc<SubscriberLaneAdapter<UuidIds>>,
        lifecycle: Arc<dyn LifecyclePolicyPort>,
        notifier: Arc<ResubNotifier>,
        shells: Arc<DashMapShellRepo>,
    }

    fn resub_harness(max_per_uri: u16) -> ResubHarness {
        let shells = Arc::new(DashMapShellRepo::new());
        let commands = Arc::new(DashMapCommandRepo::new());
        let transfers = Arc::new(DashMapTransferRepo::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        let notifier = Arc::new(ResubNotifier::default());
        let registry = MemoryRegistry::new(Arc::clone(&notifier));
        let cascade = crate::adapters::lifecycle::cascade::CascadeCoordinator::new();
        let clock = Arc::new(crate::adapters::clock::system::SystemClock);
        let lifecycle_concrete =
            crate::adapters::lifecycle::refcount::RefcountedLifecycleAdapter::new(cascade, clock);
        let lifecycle: Arc<dyn LifecyclePolicyPort> =
            Arc::clone(&lifecycle_concrete) as Arc<dyn LifecyclePolicyPort>;
        let lanes = SubscriberLaneAdapter::new(Arc::new(UuidIds), 1, max_per_uri, 64);
        let subscribe = SubscribeResourceUseCase::new(
            Arc::clone(&shells),
            Arc::clone(&commands),
            Arc::clone(&transfers),
            Arc::clone(&sessions),
            Arc::clone(&registry),
            Arc::clone(&lifecycle),
        );
        let unsubscribe = crate::application::unsubscribe_resource::UnsubscribeResourceUseCase::new(
            Arc::clone(&registry),
            Arc::clone(&lifecycle),
        );
        ResubHarness {
            subscribe,
            unsubscribe,
            registry,
            lanes,
            lifecycle,
            notifier,
            shells,
        }
    }

    fn resub_req(peer: &str) -> SubscribeResourceRequest {
        SubscribeResourceRequest {
            uri: RESUB_URI.to_string(),
            peer: StubPeer::new(peer) as Arc<dyn PeerHandle>,
        }
    }

    fn resub_unsub(
        peer: &str,
    ) -> crate::application::unsubscribe_resource::UnsubscribeResourceRequest {
        crate::application::unsubscribe_resource::UnsubscribeResourceRequest {
            uri: RESUB_URI.to_string(),
            peer_id: PeerId::new(peer.to_string()),
        }
    }

    async fn seed_resub_shell(h: &ResubHarness) {
        seed_shell(&h.shells, "sh-resub").await;
        h.lifecycle.track_resource(
            ResourceKind::Shell,
            "sh-resub",
            &SessionId::new("s".to_string()),
            LifecyclePolicy::default(),
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resources_subscribe_records_no_lane_and_delivers_once() {
        let h = resub_harness(16);
        seed_resub_shell(&h).await;
        h.subscribe.execute(resub_req("peer-resub")).await.unwrap();

        assert_eq!(h.registry.subscribers_for_uri(RESUB_URI), 1);
        assert!(h.lanes.list_subs().is_empty(), "no lane may be opened");

        let threshold = crate::adapters::config::internal::resolve_notify_flush_bytes();
        SubscriberRegistryPort::record_bytes_with_tail(
            h.registry.as_ref(),
            ResourceKind::Shell,
            "sh-resub",
            &vec![b'x'; threshold],
        );
        let mut delivered = 0;
        for _ in 0..400 {
            delivered = h.notifier.events.lock().unwrap().len();
            if delivered >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(delivered, 1, "the per-peer fan-out delivers once");
        // Let a duplicate land before counting (the same 100 ms settle the
        // lifecycle test below uses).
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            h.notifier.events.lock().unwrap().len(),
            1,
            "exactly one notification, not a duplicate"
        );
        assert!(h.lanes.list_subs().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn repeated_resources_subscribe_counts_once_in_the_lifecycle() {
        let h = resub_harness(16);
        seed_resub_shell(&h).await;
        h.subscribe.execute(resub_req("peer-rep")).await.unwrap();
        h.subscribe.execute(resub_req("peer-rep")).await.unwrap();

        assert_eq!(h.registry.subscribers_for_uri(RESUB_URI), 1);
        assert_eq!(
            h.lifecycle
                .snapshot(ResourceKind::Shell, "sh-resub")
                .unwrap()
                .sub_count,
            1,
            "a refresh must not increment the lifecycle"
        );
        assert!(h.lanes.list_subs().is_empty());

        h.unsubscribe
            .execute(resub_unsub("peer-rep"))
            .await
            .unwrap();
        assert_eq!(h.registry.subscribers_for_uri(RESUB_URI), 0);
        assert_eq!(
            h.lifecycle
                .snapshot(ResourceKind::Shell, "sh-resub")
                .unwrap()
                .sub_count,
            0
        );
        h.unsubscribe
            .execute(resub_unsub("peer-rep"))
            .await
            .unwrap();
        assert_eq!(
            h.lifecycle
                .snapshot(ResourceKind::Shell, "sh-resub")
                .unwrap()
                .sub_count,
            0
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lifecycle_refusal_leaves_no_registry_entry_and_no_delivery() {
        let h = resub_harness(16);
        seed_resub_shell(&h).await;
        h.lifecycle
            .force_close(ResourceKind::Shell, "sh-resub")
            .unwrap();

        let err = h
            .subscribe
            .execute(resub_req("peer-gone"))
            .await
            .expect_err("closed resource must fail");
        assert!(matches!(err, DomainError::ResourceGone(_)));
        assert!(h.registry.snapshot_subscribers(RESUB_URI).is_empty());
        assert_eq!(
            h.lifecycle
                .snapshot(ResourceKind::Shell, "sh-resub")
                .unwrap()
                .sub_count,
            0
        );

        let threshold = crate::adapters::config::internal::resolve_notify_flush_bytes();
        SubscriberRegistryPort::record_bytes_with_tail(
            h.registry.as_ref(),
            ResourceKind::Shell,
            "sh-resub",
            &vec![b'x'; threshold],
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(h.notifier.events.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seventeenth_resources_subscribe_with_interleaved_unsubscribe_succeeds() {
        let h = resub_harness(16);
        seed_resub_shell(&h).await;
        for round in 0..17 {
            h.subscribe
                .execute(resub_req("peer-17"))
                .await
                .unwrap_or_else(|e| panic!("subscribe round {round} failed: {e}"));
            h.unsubscribe.execute(resub_unsub("peer-17")).await.unwrap();
        }
    }
}
