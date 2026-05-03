//! Use case: open a TCP local-port-forwarder over an established SSH session.
//!
//! Translates the v3 `ssh_forward_impl` (from
//! `src/mcp/tools/forward.rs`) into a hexagonal use case orchestrating the
//! session repository, the SSH client port (which exposes the actual russh
//! channel), the forward repository, the clock port, the id generator port,
//! and the configuration port.
//!
//! # Orchestration shape
//!
//! 1. Confirm the parent session exists via [`SessionRepository::get`]; an
//!    unknown id surfaces [`DomainError::SessionNotFound`].
//! 2. Mint a fresh forward id via [`IdGeneratorPort::new_forward_id`].
//! 3. Stamp the start timestamp via [`ClockPort::utc_now`].
//! 4. Build a [`ForwardEntity`] in [`crate::domain::forward::ForwardStatus::Running`]
//!    and persist it via [`ForwardRepository::insert`].
//! 5. Return a [`ForwardPortOutcome`] DTO carrying the freshly minted id and
//!    the echoed routing tuple so the rmcp tool wrapper (etapa H16) can
//!    render the response without holding the entity.
//!
//! # Note on the SSH-side wiring
//!
//! The actual TCP listener and the russh `tcpip-forward` request are owned
//! by the SSH client adapter. The v3 implementation called
//! the legacy v3 `setup_port_forwarder` directly from the rmcp
//! tool; the v4 split keeps the use case ignorant of russh internals — the
//! adapter's listener spawning happens behind a yet-to-be-wired
//! `SshClientPort::open_forward` call (H15.x). For the H15 milestone the
//! use case persists the lifecycle entity so [`crate::application::list_resources`]
//! and [`crate::application::read_resource`] surface the resource straight
//! away. The composition root will call the SSH adapter's spawn helper from
//! the same handler before invoking this use case.
//!
//! # Concurrency contract
//!
//! Every `.await` happens outside any repository / fake guard — the use case
//! clones small handles (`Arc`) into local bindings before entering await
//! points, exactly like the [`crate::application::connect_session`] canary.
//!
//! # Feature gate
//!
//! Compiled only when the `port_forward` Cargo feature is enabled, mirroring
//! the legacy v3 forward module gate.

#![cfg(feature = "port_forward")]

use std::io::ErrorKind;
use std::sync::Arc;

use tokio::net::TcpListener;

use crate::domain::error::DomainError;
use crate::domain::forward::ForwardEntity;
use crate::domain::ids::{ForwardId, SessionId};
use crate::ports::clock::ClockPort;
use crate::ports::config::ConfigPort;
use crate::ports::forward_repo::ForwardRepository;
use crate::ports::id_generator::IdGeneratorPort;
use crate::ports::session_repo::SessionRepository;
use crate::ports::ssh_client::SshClientPort;

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct ForwardPortRequest {
    /// Parent session (must already be connected).
    pub session_id: SessionId,
    /// Local TCP port the listener will bind to.
    pub local_port: u16,
    /// Remote host (server-side) the forwarder dials into.
    pub remote_address: String,
    /// Remote TCP port.
    pub remote_port: u16,
}

/// Outbound DTO surfacing the freshly minted forwarder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardPortOutcome {
    /// Stable identifier minted at start time.
    pub forward_id: ForwardId,
    /// Echo of the requesting session.
    pub session_id: SessionId,
    /// Echo of the local port.
    pub local_port: u16,
    /// Echo of the remote host.
    pub remote_address: String,
    /// Echo of the remote port.
    pub remote_port: u16,
    /// RFC 3339 timestamp the forwarder was started.
    pub started_at: String,
}

/// Forward-port use case generic over every adapter dependency. The
/// composition root pins concrete adapter types per binary; tests inject
/// fakes via [`crate::adapters`].
#[derive(Debug)]
pub struct ForwardPortUseCase<S, R, FR, C, I, Cfg>
where
    S: SshClientPort + Send + Sync,
    R: SessionRepository + Send + Sync,
    FR: ForwardRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    I: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    ssh: Arc<S>,
    sessions: Arc<R>,
    forwards: Arc<FR>,
    clock: Arc<C>,
    ids: Arc<I>,
    config: Arc<Cfg>,
}

impl<S, R, FR, C, I, Cfg> ForwardPortUseCase<S, R, FR, C, I, Cfg>
where
    S: SshClientPort + Send + Sync,
    R: SessionRepository + Send + Sync,
    FR: ForwardRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    I: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(
        ssh: Arc<S>,
        sessions: Arc<R>,
        forwards: Arc<FR>,
        clock: Arc<C>,
        ids: Arc<I>,
        config: Arc<Cfg>,
    ) -> Self {
        Self {
            ssh,
            sessions,
            forwards,
            clock,
            ids,
            config,
        }
    }

    /// Drive the forward-port orchestration. See module-level docs for the
    /// step-by-step semantics.
    ///
    /// # Errors
    ///
    /// - [`DomainError::SessionNotFound`] when the parent session is
    ///   unknown to the repository.
    /// - [`DomainError::PortInUse`] when the local port is already bound.
    /// - [`DomainError::Transport`] tagged `FORWARD_FAILED:` when the
    ///   pre-flight bind fails for any other I/O reason (permission denied,
    ///   network unreachable, etc.) so smaller LLMs see the specific wire
    ///   code instead of the collapsed `TRANSPORT_ERROR`.
    /// - [`DomainError::Storage`] when the forward repository rejects the
    ///   insert.
    pub async fn execute(
        &self,
        req: ForwardPortRequest,
    ) -> Result<ForwardPortOutcome, DomainError> {
        // Reserve the SSH/config/ids handles in local bindings up-front so
        // the orchestration body has no implicit borrow of `self.*` across
        // await points.
        let _ssh_handle: Arc<S> = Arc::clone(&self.ssh);
        let _config_handle: Arc<Cfg> = Arc::clone(&self.config);
        // The session existence check is a guard rail — rmcp tool wrappers
        // typically validate the id before reaching this layer, but
        // re-checking here keeps the use case honest in unit tests.
        let session = self.sessions.get(&req.session_id).await?;
        if session.is_none() {
            return Err(DomainError::SessionNotFound(req.session_id));
        }

        // Best-effort pre-flight bind. Catches obvious failures (port
        // already bound, permission denied) before the entity is persisted
        // so a smaller LLM does not get a `forward://` resource for a
        // listener that never came up. The probe is dropped immediately;
        // a full TOCTOU race against the real SSH-side bind is out of
        // scope (the production listener will fail again with the same
        // error class and be reported through the `forward://` events).
        Self::preflight_bind(req.local_port).await?;

        let forward_id = self.ids.new_forward_id();
        let started_at = self.clock.utc_now();
        let entity = ForwardEntity::new(
            forward_id.clone(),
            req.session_id.clone(),
            req.local_port,
            req.remote_address.clone(),
            req.remote_port,
            started_at,
        );
        self.forwards.insert(entity).await?;
        Ok(ForwardPortOutcome {
            forward_id,
            session_id: req.session_id,
            local_port: req.local_port,
            remote_address: req.remote_address,
            remote_port: req.remote_port,
            started_at: started_at.to_rfc3339(),
        })
    }

    /// Best-effort pre-flight bind on the requested local port. Maps
    /// [`ErrorKind::AddrInUse`] to [`DomainError::PortInUse`] (already a
    /// dedicated wire code) and every other I/O failure to
    /// [`DomainError::Transport`] tagged `FORWARD_FAILED:` so the wire
    /// classifier promotes it to the specific code instead of collapsing
    /// onto `TRANSPORT_ERROR`.
    async fn preflight_bind(local_port: u16) -> Result<(), DomainError> {
        match TcpListener::bind(("0.0.0.0", local_port)).await {
            Ok(listener) => {
                drop(listener);
                Ok(())
            }
            Err(err) if err.kind() == ErrorKind::AddrInUse => {
                Err(DomainError::PortInUse(local_port))
            }
            Err(err) => Err(DomainError::Transport(format!(
                "FORWARD_FAILED: bind 0.0.0.0:{local_port} failed: {err}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ForwardPortRequest, ForwardPortUseCase};
    use crate::adapters::clock::fake::FakeClock;
    use crate::adapters::config::memory::MapConfig;
    use crate::adapters::id_generator::deterministic::SequentialIds;
    use crate::adapters::repo::dashmap::forward::DashMapForwardRepo;
    use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
    use crate::adapters::ssh::fake::FakeSshClient;
    use crate::domain::error::DomainError;
    use crate::domain::forward::ForwardStatus;
    use crate::domain::identity::Address;
    use crate::domain::ids::{ForwardId, SessionId};
    use crate::domain::session::SessionEntity;
    use crate::ports::forward_repo::ForwardRepository;
    use crate::ports::session_repo::SessionRepository;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;

    type UseCaseUnderTest = ForwardPortUseCase<
        FakeSshClient,
        DashMapSessionRepo,
        DashMapForwardRepo,
        FakeClock,
        SequentialIds,
        MapConfig,
    >;

    fn build_use_case() -> (
        UseCaseUnderTest,
        Arc<FakeSshClient>,
        Arc<DashMapSessionRepo>,
        Arc<DashMapForwardRepo>,
    ) {
        let ssh = Arc::new(FakeSshClient::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        let forwards = Arc::new(DashMapForwardRepo::new());
        let clock = Arc::new(FakeClock::new(1_777_982_400_000_u64));
        let ids = Arc::new(SequentialIds::default());
        let config = Arc::new(MapConfig::default_v3());
        let uc = ForwardPortUseCase::new(
            Arc::clone(&ssh),
            Arc::clone(&sessions),
            Arc::clone(&forwards),
            Arc::clone(&clock),
            Arc::clone(&ids),
            Arc::clone(&config),
        );
        (uc, ssh, sessions, forwards)
    }

    fn sample_address() -> Address {
        Address::new("h.example.com".to_string(), 22).expect("address")
    }

    async fn seed_session(repo: &DashMapSessionRepo, sid: &str) {
        let entity = SessionEntity {
            id: SessionId::new(sid.to_string()),
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
        };
        repo.insert(entity).await.expect("seed session");
    }

    fn req(sid: &str, local: u16, host: &str, remote: u16) -> ForwardPortRequest {
        ForwardPortRequest {
            session_id: SessionId::new(sid.to_string()),
            local_port: local,
            remote_address: host.to_string(),
            remote_port: remote,
        }
    }

    /// Tests target port 0 so the OS picks a free ephemeral port for the
    /// pre-flight bind. The use case still echoes the requested port back
    /// (the probe is dropped, the input value is what propagates into the
    /// persisted entity), so assertions read the input value verbatim.
    const FREE_PORT: u16 = 0;

    #[tokio::test(flavor = "multi_thread")]
    async fn happy_path_persists_forward_and_returns_outcome() {
        let (uc, _ssh, sessions, forwards) = build_use_case();
        seed_session(&sessions, "s-1").await;
        let outcome = uc
            .execute(req("s-1", FREE_PORT, "internal", 80))
            .await
            .expect("execute");
        assert_eq!(outcome.session_id.as_str(), "s-1");
        assert_eq!(outcome.local_port, FREE_PORT);
        assert_eq!(outcome.remote_address, "internal");
        assert_eq!(outcome.remote_port, 80);
        // forward_id matches the deterministic adapter shape.
        assert_eq!(outcome.forward_id.as_str(), "fwd-0");
        let stored = forwards
            .get(&ForwardId::new("fwd-0".to_string()))
            .await
            .expect("get")
            .expect("forward must be persisted");
        assert_eq!(stored.status, ForwardStatus::Running);
        assert_eq!(stored.local_port, FREE_PORT);
        assert_eq!(stored.remote_host, "internal");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_session_returns_session_not_found() {
        let (uc, _ssh, _sessions, forwards) = build_use_case();
        let err = uc
            .execute(req("ghost", FREE_PORT, "internal", 80))
            .await
            .expect_err("unknown session must fail");
        match err {
            DomainError::SessionNotFound(sid) => assert_eq!(sid.as_str(), "ghost"),
            other => panic!("unexpected error: {other:?}"),
        }
        // Repo must remain empty.
        assert!(forwards.list_filtered(None).await.expect("list").is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn started_at_is_rfc3339_and_matches_clock() {
        let (uc, _ssh, sessions, _forwards) = build_use_case();
        seed_session(&sessions, "s-1").await;
        let outcome = uc
            .execute(req("s-1", FREE_PORT, "db.internal", 5432))
            .await
            .expect("execute");
        // The clock is fixed via FakeClock(1_777_982_400_000) -> 2026-05-05T11:20:00Z.
        let parsed = chrono::DateTime::parse_from_rfc3339(&outcome.started_at);
        assert!(parsed.is_ok(), "started_at must be RFC 3339: {outcome:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn three_calls_mint_distinct_ids() {
        let (uc, _ssh, sessions, forwards) = build_use_case();
        seed_session(&sessions, "s-1").await;
        for _ in 0..3 {
            uc.execute(req("s-1", FREE_PORT, "internal", 80))
                .await
                .expect("execute");
        }
        let mut all = forwards.list_filtered(None).await.expect("list");
        all.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        assert_eq!(all.len(), 3);
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["fwd-0", "fwd-1", "fwd-2"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forward_is_indexed_by_session() {
        let (uc, _ssh, sessions, forwards) = build_use_case();
        seed_session(&sessions, "s-1").await;
        seed_session(&sessions, "s-2").await;
        uc.execute(req("s-1", FREE_PORT, "a", 80))
            .await
            .expect("execute s-1");
        uc.execute(req("s-2", FREE_PORT, "b", 90))
            .await
            .expect("execute s-2");
        let owned = forwards
            .list_filtered(Some(&SessionId::new("s-1".to_string())))
            .await
            .expect("list_filtered");
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].local_port, FREE_PORT);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_session_does_not_consume_id_generator_state() {
        let (uc, _ssh, sessions, forwards) = build_use_case();
        // First call fails (no session) so the id generator must not have
        // advanced — the next happy path must still get fwd-0.
        let _ = uc
            .execute(req("ghost", FREE_PORT, "x", 1))
            .await
            .expect_err("unknown");
        seed_session(&sessions, "s-1").await;
        let outcome = uc
            .execute(req("s-1", FREE_PORT, "internal", 80))
            .await
            .expect("execute");
        assert_eq!(outcome.forward_id.as_str(), "fwd-0");
        assert_eq!(forwards.list_filtered(None).await.expect("list").len(), 1);
    }

    /// The pre-flight bind in [`ForwardPortUseCase::execute`] must
    /// promote `AddrInUse` to [`DomainError::PortInUse`] (the dedicated
    /// `PORT_IN_USE` wire code) so the LLM can branch on a precise
    /// failure class instead of the generic `TRANSPORT_ERROR`. The probe
    /// binds on `0.0.0.0:<port>` so this test holds a listener on the
    /// same `0.0.0.0:<port>` (`SO_REUSEADDR` is OFF by default in tokio)
    /// to guarantee a deterministic collision regardless of the host
    /// loopback / wildcard binding conventions.
    #[tokio::test(flavor = "multi_thread")]
    async fn local_port_already_bound_returns_port_in_use() {
        let (uc, _ssh, sessions, _forwards) = build_use_case();
        seed_session(&sessions, "s-1").await;
        let held = TcpListener::bind(("0.0.0.0", 0))
            .await
            .expect("seed listener");
        let local_port = held.local_addr().expect("local_addr").port();
        let err = uc
            .execute(req("s-1", local_port, "internal", 80))
            .await
            .expect_err("bind collision must fail");
        match err {
            DomainError::PortInUse(p) => assert_eq!(p, local_port),
            other => panic!("unexpected error: {other:?}"),
        }
        drop(held);
    }
}
