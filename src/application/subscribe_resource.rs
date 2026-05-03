//! Use case: register a peer for `resources/updated` notifications on a
//! ssh-mcp resource URI.
//!
//! Translates the v3 [`crate::mcp::resources::subscribe_impl`] into a
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
use crate::ports::notifier::PeerHandle;
use crate::ports::session_repo::SessionRepository;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::subscriber_registry::{ResourceKind, SubscriberRegistryAsync};
use crate::ports::transfer_repo::TransferRepository;

#[cfg(feature = "port_forward")]
use crate::ports::forward_repo::ForwardRepository;

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
/// echo back to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribeResourceOutcome {
    /// Canonical URI (no `?cursor=` — the registry keys against this form).
    pub uri: String,
}

/// Subscribe-resource use case (no `port_forward` feature).
#[cfg(not(feature = "port_forward"))]
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
}

/// Subscribe-resource use case with the `port_forward` feature enabled.
#[cfg(feature = "port_forward")]
#[derive(Debug)]
pub struct SubscribeResourceUseCase<ShR, CR, TR, SR, FR, Sub>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    FR: ForwardRepository + Send + Sync,
    Sub: SubscriberRegistryAsync + Send + Sync,
{
    shells: Arc<ShR>,
    commands: Arc<CR>,
    transfers: Arc<TR>,
    sessions: Arc<SR>,
    forwards: Arc<FR>,
    subscribers: Arc<Sub>,
}

#[cfg(not(feature = "port_forward"))]
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
    ) -> Self {
        Self {
            shells,
            commands,
            transfers,
            sessions,
            subscribers,
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
        self.subscribers
            .subscribe(parsed.kind, parsed.id, canonical.clone(), req.peer)
            .await?;
        Ok(SubscribeResourceOutcome { uri: canonical })
    }
}

#[cfg(feature = "port_forward")]
impl<ShR, CR, TR, SR, FR, Sub> SubscribeResourceUseCase<ShR, CR, TR, SR, FR, Sub>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    FR: ForwardRepository + Send + Sync,
    Sub: SubscriberRegistryAsync + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(
        shells: Arc<ShR>,
        commands: Arc<CR>,
        transfers: Arc<TR>,
        sessions: Arc<SR>,
        forwards: Arc<FR>,
        subscribers: Arc<Sub>,
    ) -> Self {
        Self {
            shells,
            commands,
            transfers,
            sessions,
            forwards,
            subscribers,
        }
    }

    /// Drive the subscribe orchestration.
    ///
    /// # Errors
    ///
    /// Same as the non-feature build, plus
    /// [`DomainError::ForwardNotFound`] is surfaced only if the inbound
    /// adapter has a stricter policy — the v3 default behaviour accepts
    /// every forward URI so subscribers can attach to a freshly minted
    /// forwarder before the producer publishes the first event.
    pub async fn execute(
        &self,
        req: SubscribeResourceRequest,
    ) -> Result<SubscribeResourceOutcome, DomainError> {
        let parsed =
            parse_uri(&req.uri).map_err(|e| DomainError::InvalidArgument(e.to_string()))?;
        ensure_exists_with_forward(
            parsed.kind,
            &parsed.id,
            &*self.shells,
            &*self.commands,
            &*self.transfers,
            &*self.sessions,
            &*self.forwards,
        )
        .await?;
        let canonical = canonical_uri(parsed.kind, &parsed.id);
        self.subscribers
            .subscribe(parsed.kind, parsed.id, canonical.clone(), req.peer)
            .await?;
        Ok(SubscribeResourceOutcome { uri: canonical })
    }
}

async fn ensure_exists<ShR, CR, TR, SR>(
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
        ResourceKind::Forward => Ok(()),
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

#[cfg(feature = "port_forward")]
async fn ensure_exists_with_forward<ShR, CR, TR, SR, FR>(
    kind: ResourceKind,
    id: &str,
    shells: &ShR,
    commands: &CR,
    transfers: &TR,
    sessions: &SR,
    forwards: &FR,
) -> Result<(), DomainError>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    FR: ForwardRepository + Send + Sync,
{
    // Forward arm: see `ensure_exists` for the permissive rationale.
    // The `forwards` reference is intentionally unused so the use case
    // stays generic over `FR`; flip to `forwards.get(...)?` here when
    // a stricter existence check is desired.
    let forwards_unused: &FR = forwards;
    let _ = forwards_unused;
    ensure_exists(kind, id, shells, commands, transfers, sessions).await
}

#[cfg(test)]
mod tests {
    use super::{SubscribeResourceRequest, SubscribeResourceUseCase};
    use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
    use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
    use crate::domain::command::CommandEntity;
    use crate::domain::error::DomainError;
    use crate::domain::identity::Address;
    use crate::domain::ids::{CommandId, PeerId, SessionId, ShellId, TransferId};
    use crate::domain::session::SessionEntity;
    use crate::domain::shell::{ShellEntity, ShellTerminal};
    use crate::domain::transfer::{TransferDirection, TransferEntity};
    use crate::ports::command_repo::CommandRepository;
    use crate::ports::notifier::PeerHandle;
    use crate::ports::session_repo::SessionRepository;
    use crate::ports::shell_repo::ShellRepository;
    use crate::ports::subscriber_registry::{
        ResourceKind, SubscriberRegistryAsync, SubscriberRegistryPort, SubscriberSnapshot,
    };
    use crate::ports::transfer_repo::TransferRepository;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    #[cfg(feature = "port_forward")]
    use crate::adapters::repo::dashmap::forward::DashMapForwardRepo;

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
        ) -> Result<(), DomainError> {
            if let Ok(mut g) = self.subs.lock() {
                g.push((kind, resource_id, uri, peer.id()));
            }
            Ok(())
        }
        async fn unsubscribe(&self, _peer_id: &PeerId, _uri: &str) {}
        async fn drop_peer(&self, _peer_id: &PeerId) {}
    }

    #[cfg(feature = "port_forward")]
    type UseCaseUnderTest = SubscribeResourceUseCase<
        DashMapShellRepo,
        DashMapCommandRepo,
        DashMapTransferRepo,
        DashMapSessionRepo,
        DashMapForwardRepo,
        RecordingRegistry,
    >;

    #[cfg(not(feature = "port_forward"))]
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
        #[cfg(feature = "port_forward")]
        let forwards = Arc::new(DashMapForwardRepo::new());
        let registry = Arc::new(RecordingRegistry::default());
        #[cfg(feature = "port_forward")]
        let uc = SubscribeResourceUseCase::new(
            Arc::clone(&shells),
            Arc::clone(&commands),
            Arc::clone(&transfers),
            Arc::clone(&sessions),
            Arc::clone(&forwards),
            Arc::clone(&registry),
        );
        #[cfg(not(feature = "port_forward"))]
        let uc = SubscribeResourceUseCase::new(
            Arc::clone(&shells),
            Arc::clone(&commands),
            Arc::clone(&transfers),
            Arc::clone(&sessions),
            Arc::clone(&registry),
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
}
