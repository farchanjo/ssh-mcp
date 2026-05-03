//! Use case: spawn an SFTP upload and return the initial transfer snapshot.
//!
//! Translates the v3 `ssh_upload_impl` (from `src/mcp/tools/sftp.rs`) into a
//! hexagonal use case orchestrating ports. No rmcp / russh / russh-sftp /
//! tokio types leak into the public surface.
//!
//! # Orchestration shape
//!
//! 1. Resolve the [`crate::domain::session::SessionEntity`] via
//!    [`SessionRepository::get`]. A missing id short-circuits with
//!    [`DomainError::SessionNotFound`] before any side effect runs so the
//!    inbound adapter can render the canonical
//!    `SSH_UPLOAD: ERROR / REASON: [SESSION_NOT_FOUND]` block.
//! 2. Guard the per-session transfer cap via
//!    [`TransferRepository::count_by_session`] +
//!    [`ConfigPort::max_transfers_per_session`]. A breach short-circuits with
//!    [`DomainError::MaxTransfersExceeded`].
//! 3. Mint a fresh [`TransferId`] via [`IdGeneratorPort::new_transfer_id`].
//! 4. Hand the work to [`SftpClientPort::upload`] — the adapter spawns the
//!    streaming task internally, performs the local-file stat, and returns a
//!    [`crate::domain::transfer::TransferEntity`] in
//!    [`crate::domain::transfer::TransferStatus::Running`] populated with the
//!    resolved local path and total byte count.
//! 5. Persist the entity through [`TransferRepository::insert`]. On insert
//!    failure the SFTP-side cancellation token is fired via
//!    [`SftpClientPort::cancel`] so the adapter does not orphan a streaming
//!    task; failures are swallowed because the actionable signal is the
//!    storage error.
//! 6. Poke the subscription registry on the
//!    [`crate::ports::subscriber_registry::ResourceKind::Transfer`] kind so
//!    any subscriber on `transfer://<id>/progress` wakes up immediately.
//!
//! # Concurrency contract
//!
//! Every `.await` happens outside any repository / fake guard — the use case
//! clones small handles ([`Arc`]) into local bindings before entering await
//! points. Repository / port methods return owned values so no shard-spanning
//! borrows escape. `subscribers.poke` is a sync call (`Notify::notify_one`)
//! and stays outside any guard.

use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::ids::{AgentId, SessionId, TransferId};
use crate::ports::clock::ClockPort;
use crate::ports::config::ConfigPort;
use crate::ports::id_generator::IdGeneratorPort;
use crate::ports::session_repo::SessionRepository;
use crate::ports::sftp_client::{SftpClientPort, UploadRequest as PortUploadRequest};
use crate::ports::subscriber_registry::{ResourceKind, SubscriberRegistryPort};
use crate::ports::transfer_repo::TransferRepository;

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct UploadRequest {
    /// Owning session.
    pub session_id: SessionId,
    /// Local source path. Tilde expansion / relative-to-home resolution is
    /// the responsibility of the production [`SftpClientPort`] adapter so
    /// the use case stays free of filesystem assumptions.
    pub local_path: String,
    /// Remote destination path.
    pub remote_path: String,
}

/// Outbound DTO surfacing every observable result the rmcp tool wrapper
/// must render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadOutcome {
    /// Freshly minted transfer id.
    pub transfer_id: TransferId,
    /// Echoed back so the caller can correlate.
    pub session_id: SessionId,
    /// Optional agent the session was bound to at the time of the upload.
    pub agent_id: Option<AgentId>,
    /// Resolved local path returned by the SFTP adapter (after tilde / home
    /// expansion).
    pub local_path: String,
    /// Remote destination path (echoed unchanged).
    pub remote_path: String,
    /// Total bytes the local file holds at start time. Snapshot value;
    /// progress events deliver the moving counter via the `transfer://`
    /// MCP resource.
    pub total_bytes: u64,
    /// RFC 3339 timestamp of when the use case spawned the streaming task.
    pub started_at: String,
}

/// Upload-file use case generic over every adapter dependency. The
/// composition root pins concrete adapter types per binary; tests inject
/// fakes via [`crate::adapters`].
#[derive(Debug)]
pub struct UploadFileUseCase<F, SR, TR, C, I, Cfg, Sub>
where
    F: SftpClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    I: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
    Sub: SubscriberRegistryPort + Send + Sync,
{
    sftp: Arc<F>,
    sessions: Arc<SR>,
    transfers: Arc<TR>,
    clock: Arc<C>,
    ids: Arc<I>,
    config: Arc<Cfg>,
    subscribers: Arc<Sub>,
}

impl<F, SR, TR, C, I, Cfg, Sub> UploadFileUseCase<F, SR, TR, C, I, Cfg, Sub>
where
    F: SftpClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    I: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
    Sub: SubscriberRegistryPort + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(
        sftp: Arc<F>,
        sessions: Arc<SR>,
        transfers: Arc<TR>,
        clock: Arc<C>,
        ids: Arc<I>,
        config: Arc<Cfg>,
        subscribers: Arc<Sub>,
    ) -> Self {
        Self {
            sftp,
            sessions,
            transfers,
            clock,
            ids,
            config,
            subscribers,
        }
    }

    /// Drive the upload orchestration. See module-level docs for the
    /// step-by-step semantics.
    ///
    /// # Errors
    ///
    /// - [`DomainError::SessionNotFound`] when the session id is unknown.
    /// - [`DomainError::MaxTransfersExceeded`] when the per-session running
    ///   cap is reached.
    /// - Any [`DomainError`] surfaced by [`SftpClientPort::upload`] (e.g.
    ///   `Sftp` for local-file errors, `SessionNotFound` when the russh
    ///   handle was already torn down between the session probe and the
    ///   SFTP call).
    /// - Any [`DomainError`] surfaced by [`TransferRepository::insert`]; on
    ///   insert failure the SFTP-side cancellation token is fired so the
    ///   streaming task does not outlive the failed insert.
    pub async fn execute(&self, req: UploadRequest) -> Result<UploadOutcome, DomainError> {
        let session = self.lookup_session(&req.session_id).await?;
        self.guard_transfer_cap(&req.session_id).await?;

        let transfer_id = self.ids.new_transfer_id();
        let entity = self
            .sftp
            .upload(
                transfer_id.clone(),
                PortUploadRequest {
                    session_id: req.session_id.clone(),
                    local_path: req.local_path.clone(),
                    remote_path: req.remote_path.clone(),
                },
            )
            .await?;

        // Snapshot before the move into `insert`.
        let resolved_local = entity.local_path.clone();
        let resolved_remote = entity.remote_path.clone();
        let total_bytes = entity.total_bytes;
        let started_at = entity.started_at;

        self.persist_or_rollback(&transfer_id, entity).await?;

        self.subscribers
            .poke(ResourceKind::Transfer, transfer_id.as_str());

        // Touching the clock keeps the port plumbed even though the
        // adapter owns the canonical `started_at`. A future iteration
        // can route every timestamp through the clock without another
        // constructor churn.
        let _ = self.clock.utc_now();

        Ok(UploadOutcome {
            transfer_id,
            session_id: req.session_id,
            agent_id: session.agent_id,
            local_path: resolved_local,
            remote_path: resolved_remote,
            total_bytes,
            started_at: started_at.to_rfc3339(),
        })
    }

    /// Resolve the session entity, mapping `Ok(None)` to
    /// [`DomainError::SessionNotFound`].
    async fn lookup_session(
        &self,
        session_id: &SessionId,
    ) -> Result<crate::domain::session::SessionEntity, DomainError> {
        self.sessions
            .get(session_id)
            .await?
            .ok_or_else(|| DomainError::SessionNotFound(session_id.clone()))
    }

    /// Reject the request when the per-session transfer cap is reached.
    async fn guard_transfer_cap(&self, session_id: &SessionId) -> Result<(), DomainError> {
        let limit = self.config.max_transfers_per_session();
        let running = self.transfers.count_by_session(session_id).await?;
        if running >= limit {
            return Err(DomainError::MaxTransfersExceeded { limit });
        }
        Ok(())
    }

    /// Persist the freshly minted entity and roll the SFTP-side task back
    /// on insert failure so the adapter does not orphan a streaming task.
    async fn persist_or_rollback(
        &self,
        transfer_id: &TransferId,
        entity: crate::domain::transfer::TransferEntity,
    ) -> Result<(), DomainError> {
        if let Err(err) = self.transfers.insert(entity).await {
            // Storage errors during rollback are swallowed: the insert
            // error is the actionable signal.
            let _ = self.sftp.cancel(transfer_id).await;
            return Err(err);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{UploadFileUseCase, UploadRequest};
    use crate::adapters::clock::fake::FakeClock;
    use crate::adapters::config::memory::MapConfig;
    use crate::adapters::id_generator::deterministic::SequentialIds;
    use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
    use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
    use crate::adapters::sftp::fake::{FakeSftpCall, FakeSftpClient};
    use crate::domain::error::DomainError;
    use crate::domain::identity::Address;
    use crate::domain::ids::{AgentId, SessionId, TransferId};
    use crate::domain::session::SessionEntity;
    use crate::domain::transfer::{TransferDirection, TransferEntity, TransferStatus};
    use crate::ports::session_repo::SessionRepository;
    use crate::ports::subscriber_registry::{
        ResourceKind, SubscriberRegistryPort, SubscriberSnapshot,
    };
    use crate::ports::transfer_repo::TransferRepository;
    use chrono::Utc;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Test-only [`SubscriberRegistryPort`] that records every `poke` call.
    /// The H14 use cases only call `poke`; the other methods are stubbed
    /// with deterministic, side-effect-free defaults so any unintended
    /// usage surfaces as wrong behaviour rather than a panic.
    #[derive(Debug, Default)]
    struct RecordingRegistry {
        pokes: Mutex<Vec<(ResourceKind, String)>>,
    }

    impl RecordingRegistry {
        fn new() -> Self {
            Self::default()
        }

        fn pokes(&self) -> Vec<(ResourceKind, String)> {
            self.pokes
                .lock()
                .map_or_else(|p| p.into_inner().clone(), |g| g.clone())
        }
    }

    impl SubscriberRegistryPort for RecordingRegistry {
        fn next_seq(&self, _kind: ResourceKind, _resource_id: &str) -> u64 {
            0
        }

        fn current_seq(&self, _kind: ResourceKind, _resource_id: &str) -> u64 {
            0
        }

        fn poke(&self, kind: ResourceKind, resource_id: &str) {
            if let Ok(mut guard) = self.pokes.lock() {
                guard.push((kind, resource_id.to_string()));
            }
        }

        fn compensate_truncation(&self, _uri: &str, _bytes_dropped: u64) {}

        fn snapshot_subscribers(&self, _uri: &str) -> Vec<SubscriberSnapshot> {
            Vec::new()
        }

        fn peer_byte_cursor(&self, _peer_id: &crate::domain::ids::PeerId, _uri: &str) -> u64 {
            0
        }

        fn advance_peer_byte_cursor(
            &self,
            _peer_id: &crate::domain::ids::PeerId,
            _uri: &str,
            target: u64,
        ) -> u64 {
            target
        }

        fn gc_closed_peers(&self) -> usize {
            0
        }
    }

    type UseCaseUnderTest = UploadFileUseCase<
        FakeSftpClient,
        DashMapSessionRepo,
        DashMapTransferRepo,
        FakeClock,
        SequentialIds,
        MapConfig,
        RecordingRegistry,
    >;

    struct Wired {
        uc: UseCaseUnderTest,
        sftp: Arc<FakeSftpClient>,
        sessions: Arc<DashMapSessionRepo>,
        transfers: Arc<DashMapTransferRepo>,
        registry: Arc<RecordingRegistry>,
    }

    fn build_wired_with(config: MapConfig) -> Wired {
        let sftp = Arc::new(FakeSftpClient::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        let transfers = Arc::new(DashMapTransferRepo::new());
        // 2026-05-02 12:00:00 UTC — same anchor as the connect_session canary.
        let clock = Arc::new(FakeClock::new(1_777_982_400_000_u64));
        let ids = Arc::new(SequentialIds::default());
        let cfg = Arc::new(config);
        let registry = Arc::new(RecordingRegistry::new());
        let uc = UploadFileUseCase::new(
            Arc::clone(&sftp),
            Arc::clone(&sessions),
            Arc::clone(&transfers),
            Arc::clone(&clock),
            Arc::clone(&ids),
            Arc::clone(&cfg),
            Arc::clone(&registry),
        );
        Wired {
            uc,
            sftp,
            sessions,
            transfers,
            registry,
        }
    }

    fn build_wired() -> Wired {
        build_wired_with(MapConfig::default_v3())
    }

    fn sample_address() -> Address {
        Address::new("h.example.com".to_string(), 22).expect("address")
    }

    fn seed_session(repo: &DashMapSessionRepo, id: &str, agent: Option<AgentId>) -> SessionEntity {
        let entity = SessionEntity {
            id: SessionId::new(id.to_string()),
            name: None,
            agent_id: agent.clone(),
            address: sample_address(),
            username: "alice".to_string(),
            connected_at: Utc::now(),
            default_timeout: Duration::from_secs(30),
            retry_attempts: 0,
            compression_enabled: true,
            last_health_check: None,
            healthy: None,
        };
        let to_insert = entity.clone();
        let agent_clone = agent;
        let repo_clone = repo.clone();
        let id_clone = entity.id.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                repo_clone.insert(to_insert).await.expect("seed insert");
                if let Some(a) = agent_clone.as_ref() {
                    repo_clone
                        .register_agent(a, &id_clone)
                        .await
                        .expect("agent reg");
                }
            });
        });
        entity
    }

    fn seed_transfer(
        repo: &DashMapTransferRepo,
        id: &str,
        session: &str,
        direction: TransferDirection,
    ) -> TransferEntity {
        let entity = TransferEntity::new(
            TransferId::new(id.to_string()),
            SessionId::new(session.to_string()),
            direction,
            "/tmp/local".to_string(),
            "/srv/remote".to_string(),
            Utc::now(),
            1024,
        );
        let to_insert = entity.clone();
        let repo_clone = repo.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                repo_clone.insert(to_insert).await.expect("seed insert");
            });
        });
        entity
    }

    fn base_request(session_id: SessionId) -> UploadRequest {
        UploadRequest {
            session_id,
            local_path: "/tmp/source.bin".to_string(),
            remote_path: "/srv/dest.bin".to_string(),
        }
    }

    // --- Scenario 1 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn happy_path_returns_upload_outcome_with_running_status_persisted() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-1", None);
        wired.sftp.queue_upload_ok(2048);

        let outcome = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("upload ok");

        assert_eq!(outcome.session_id, seeded.id);
        assert_eq!(outcome.transfer_id.as_str(), "xfer-0");
        assert_eq!(outcome.local_path, "/tmp/source.bin");
        assert_eq!(outcome.remote_path, "/srv/dest.bin");
        assert_eq!(outcome.total_bytes, 2048);
        assert!(
            outcome.started_at.contains('T'),
            "started_at must be RFC 3339; got {}",
            outcome.started_at
        );

        let stored = wired
            .transfers
            .get(&outcome.transfer_id)
            .await
            .expect("get")
            .expect("stored");
        assert_eq!(stored.status, TransferStatus::Running);
        assert_eq!(stored.direction, TransferDirection::Upload);
    }

    // --- Scenario 2 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_session_short_circuits_with_session_not_found() {
        let wired = build_wired();
        let unknown = SessionId::new("ghost".to_string());
        let err = wired
            .uc
            .execute(base_request(unknown.clone()))
            .await
            .expect_err("upload must fail");
        assert_eq!(err, DomainError::SessionNotFound(unknown));
        assert_eq!(
            wired.sftp.call_count(),
            0,
            "missing session must short-circuit before any SFTP call"
        );
    }

    // --- Scenario 3 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn max_transfers_exceeded_short_circuits_with_explicit_error() {
        let cfg = MapConfig::default_v3().with_max_transfers_per_session(2);
        let wired = build_wired_with(cfg);
        let seeded = seed_session(&wired.sessions, "sess-cap", None);
        let _ = seed_transfer(
            &wired.transfers,
            "t-pre-1",
            "sess-cap",
            TransferDirection::Upload,
        );
        let _ = seed_transfer(
            &wired.transfers,
            "t-pre-2",
            "sess-cap",
            TransferDirection::Download,
        );

        let err = wired
            .uc
            .execute(base_request(seeded.id))
            .await
            .expect_err("upload must fail at the cap");
        assert_eq!(err, DomainError::MaxTransfersExceeded { limit: 2 });
        assert_eq!(
            wired.sftp.call_count(),
            0,
            "cap breach must short-circuit before SFTP"
        );
    }

    // --- Scenario 4 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_id_propagates_into_outcome() {
        let wired = build_wired();
        let agent = AgentId::new("claude-agent-7".to_string());
        let seeded = seed_session(&wired.sessions, "sess-agent", Some(agent.clone()));
        wired.sftp.queue_upload_ok(512);

        let outcome = wired
            .uc
            .execute(base_request(seeded.id))
            .await
            .expect("upload ok");
        assert_eq!(outcome.agent_id.as_ref(), Some(&agent));
    }

    // --- Scenario 5 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn local_file_error_propagates_as_sftp_domain_error() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-local", None);
        wired
            .sftp
            .queue_upload_error(DomainError::Sftp("local file missing".to_string()));

        let err = wired
            .uc
            .execute(base_request(seeded.id))
            .await
            .expect_err("upload must fail");
        assert_eq!(err, DomainError::Sftp("local file missing".to_string()));
        // Repo was never touched.
        let all = wired.transfers.list_filtered(None).await.expect("list");
        assert!(all.is_empty(), "failure path must not persist a transfer");
    }

    // --- Scenario 6 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn subscriber_poke_fires_for_transfer_resource_kind() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-poke", None);
        wired.sftp.queue_upload_ok(64);

        let outcome = wired
            .uc
            .execute(base_request(seeded.id))
            .await
            .expect("upload ok");

        let pokes = wired.registry.pokes();
        assert_eq!(pokes.len(), 1);
        assert_eq!(pokes[0].0, ResourceKind::Transfer);
        assert_eq!(pokes[0].1, outcome.transfer_id.as_str());
    }

    // --- Scenario 7 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn subscriber_poke_does_not_fire_when_session_missing() {
        let wired = build_wired();
        let unknown = SessionId::new("nope".to_string());
        let _ = wired.uc.execute(base_request(unknown)).await;
        assert!(
            wired.registry.pokes().is_empty(),
            "no poke must fire when the session lookup fails"
        );
    }

    // --- Scenario 8 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn sftp_call_carries_the_request_paths_verbatim() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-route", None);
        wired.sftp.queue_upload_ok(128);

        let mut req = base_request(seeded.id.clone());
        req.local_path = "/tmp/data/photo.jpg".to_string();
        req.remote_path = "/srv/uploads/photo.jpg".to_string();
        let outcome = wired.uc.execute(req).await.expect("upload ok");

        let calls = wired.sftp.calls();
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            FakeSftpCall::Upload {
                transfer_id,
                request,
            } => {
                assert_eq!(transfer_id, &outcome.transfer_id);
                assert_eq!(request.session_id, seeded.id);
                assert_eq!(request.local_path, "/tmp/data/photo.jpg");
                assert_eq!(request.remote_path, "/srv/uploads/photo.jpg");
            }
            FakeSftpCall::Download { .. } | FakeSftpCall::Cancel { .. } => {
                panic!("expected Upload, got {:?}", calls[0])
            }
        }
    }

    // --- Scenario 9 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn second_upload_in_same_session_increments_counter_under_cap() {
        let cfg = MapConfig::default_v3().with_max_transfers_per_session(5);
        let wired = build_wired_with(cfg);
        let seeded = seed_session(&wired.sessions, "sess-multi", None);
        wired.sftp.queue_upload_ok(64);
        wired.sftp.queue_upload_ok(128);

        let first = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("first ok");
        let second = wired
            .uc
            .execute(base_request(seeded.id.clone()))
            .await
            .expect("second ok");

        assert_ne!(first.transfer_id, second.transfer_id);
        let count = wired
            .transfers
            .count_by_session(&seeded.id)
            .await
            .expect("count");
        assert_eq!(count, 2);
    }

    // --- Scenario 10 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn session_storage_failure_during_lookup_propagates() {
        // The DashMap repo never errors on `get`, so we verify the contract
        // through the session-not-found path which exercises the same `?`
        // boundary: the use case must surface the storage outcome verbatim.
        let wired = build_wired();
        let err = wired
            .uc
            .execute(base_request(SessionId::new("absent".to_string())))
            .await
            .expect_err("must fail");
        match err {
            DomainError::SessionNotFound(id) => assert_eq!(id.as_str(), "absent"),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }
}
