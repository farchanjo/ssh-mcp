//! Use case: enumerate every active ssh-mcp resource.
//!
//! Translates the legacy v3 `list_resources_impl` into a
//! hexagonal use case orchestrating the five repositories.
//!
//! # Orchestration shape
//!
//! 1. Fetch every aggregate from the four always-on repositories (sessions,
//!    commands, shells, transfers) plus, when the `port_forward` Cargo
//!    feature is enabled, the forward repository.
//! 2. Yield one [`ResourceListing`] per entity, with its canonical URI
//!    pre-built (`shell://<id>/output`, `command://<id>/output`,
//!    `transfer://<id>/progress`, `session://<id>/health`,
//!    `forward://<id>/events`).
//! 3. The result is unpaginated — the v3 surface returned a flat
//!    [`rmcp::model::ListResourcesResult`] without `next_cursor`, and the
//!    hexagonal use case mirrors that shape. The inbound adapter (etapa
//!    H16) decides whether to paginate or chunk responses.
//!
//! # Concurrency contract
//!
//! Each repository call is a single `.await` against an owned vector
//! return type, so the use case never holds a shard guard across an
//! await point. Repository errors are propagated unchanged so callers can
//! distinguish a backend fault from an empty universe.

use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::ids::{CommandId, SessionId, ShellId, TransferId};
use crate::domain::transfer::TransferDirection;
use crate::ports::command_repo::CommandRepository;
use crate::ports::session_repo::SessionRepository;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::transfer_repo::TransferRepository;

use crate::domain::ids::ForwardId;
use crate::ports::forward_repo::ForwardRepository;

/// Inbound DTO. Currently a unit struct — the v3 surface does not
/// paginate; H16 may extend this with an opaque cursor parameter.
#[derive(Debug, Default, Clone, Copy)]
pub struct ListResourcesRequest;

/// Single resource entry surfaced to the inbound adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceListing {
    /// `shell://<shell_id>/output` — interactive PTY output buffer.
    Shell {
        /// Canonical URI.
        uri: String,
        /// Shell identifier.
        shell_id: ShellId,
        /// Owning session.
        session_id: SessionId,
        /// Negotiated `TERM` value.
        term_type: String,
    },
    /// `command://<command_id>/output` — async command output stream.
    Command {
        /// Canonical URI.
        uri: String,
        /// Command identifier.
        command_id: CommandId,
        /// Verbatim shell command line.
        command: String,
        /// Owning session.
        session_id: SessionId,
    },
    /// `transfer://<transfer_id>/progress` — point-in-time SFTP progress.
    Transfer {
        /// Canonical URI.
        uri: String,
        /// Transfer identifier.
        transfer_id: TransferId,
        /// Direction of the transfer.
        direction: TransferDirection,
        /// Owning session.
        session_id: SessionId,
    },
    /// `session://<session_id>/health` — SSH session health snapshot.
    Session {
        /// Canonical URI.
        uri: String,
        /// Session identifier.
        session_id: SessionId,
        /// Resolved `host:port`.
        host: String,
        /// Last known health flag (`None` when never probed).
        healthy: Option<bool>,
    },
    /// `forward://<forward_id>/events` — port-forward event log.
    ///
    /// Always present so the resource enumerator has a single shape. When
    /// the `port_forward` feature is disabled the forward repository stays
    /// empty, so this variant is simply never produced.
    Forward {
        /// Canonical URI.
        uri: String,
        /// Forwarder identifier.
        forward_id: ForwardId,
        /// Local listener port.
        local_port: u16,
        /// Remote endpoint string `<host>:<port>`.
        remote: String,
        /// Owning session.
        session_id: SessionId,
    },
}

/// Outbound DTO carrying every resource entry the inbound adapter must
/// render.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ListResourcesOutcome {
    /// Aggregated entries; ordering is unspecified to match the
    /// underlying [`dashmap::DashMap`] iteration semantics. Inbound
    /// adapters that need a stable ordering must sort by `uri`.
    pub resources: Vec<ResourceListing>,
}

/// List-resources use case generic over every repository port.
///
/// The forward repository is always present as a type parameter so the
/// [`crate::composition::UseCases`] container has a single shape across
/// both `port_forward` feature configurations. When the feature is
/// disabled the forward repository stays empty, so
/// [`collect_forward_resources`] simply yields nothing.
///
/// The composition root pins concrete adapter types per binary; tests
/// inject fakes via [`crate::adapters`].
#[derive(Debug)]
pub struct ListResourcesUseCase<SR, CR, ShR, TR, FR>
where
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    FR: ForwardRepository + Send + Sync,
{
    sessions: Arc<SR>,
    commands: Arc<CR>,
    shells: Arc<ShR>,
    transfers: Arc<TR>,
    forwards: Arc<FR>,
}

impl<SR, CR, ShR, TR, FR> ListResourcesUseCase<SR, CR, ShR, TR, FR>
where
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    FR: ForwardRepository + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(
        sessions: Arc<SR>,
        commands: Arc<CR>,
        shells: Arc<ShR>,
        transfers: Arc<TR>,
        forwards: Arc<FR>,
    ) -> Self {
        Self {
            sessions,
            commands,
            shells,
            transfers,
            forwards,
        }
    }

    /// Drive the list-resources orchestration. See module-level docs.
    ///
    /// # Errors
    ///
    /// Propagates the first repository error encountered; partial results
    /// are not surfaced to callers.
    pub async fn execute(
        &self,
        _req: ListResourcesRequest,
    ) -> Result<ListResourcesOutcome, DomainError> {
        let mut resources = Vec::new();
        collect_shell_resources(&*self.shells, &mut resources).await?;
        collect_command_resources(&*self.commands, &mut resources).await?;
        collect_transfer_resources(&*self.transfers, &mut resources).await?;
        collect_session_resources(&*self.sessions, &mut resources).await?;
        collect_forward_resources(&*self.forwards, &mut resources).await?;
        Ok(ListResourcesOutcome { resources })
    }
}

async fn collect_shell_resources<ShR: ShellRepository + Send + Sync>(
    repo: &ShR,
    out: &mut Vec<ResourceListing>,
) -> Result<(), DomainError> {
    let listed = repo.list_filtered(None).await?;
    for entity in listed {
        out.push(ResourceListing::Shell {
            uri: format!("shell://{}/output", entity.id),
            shell_id: entity.id,
            session_id: entity.session_id,
            term_type: entity.terminal.term_type,
        });
    }
    Ok(())
}

async fn collect_command_resources<CR: CommandRepository + Send + Sync>(
    repo: &CR,
    out: &mut Vec<ResourceListing>,
) -> Result<(), DomainError> {
    let listed = repo.list_filtered(None, None).await?;
    for entity in listed {
        out.push(ResourceListing::Command {
            uri: format!("command://{}/output", entity.id),
            command_id: entity.id,
            command: entity.command,
            session_id: entity.session_id,
        });
    }
    Ok(())
}

async fn collect_transfer_resources<TR: TransferRepository + Send + Sync>(
    repo: &TR,
    out: &mut Vec<ResourceListing>,
) -> Result<(), DomainError> {
    let listed = repo.list_filtered(None).await?;
    for entity in listed {
        out.push(ResourceListing::Transfer {
            uri: format!("transfer://{}/progress", entity.id),
            transfer_id: entity.id,
            direction: entity.direction,
            session_id: entity.session_id,
        });
    }
    Ok(())
}

async fn collect_session_resources<SR: SessionRepository + Send + Sync>(
    repo: &SR,
    out: &mut Vec<ResourceListing>,
) -> Result<(), DomainError> {
    let listed = repo.list().await?;
    for entity in listed {
        out.push(ResourceListing::Session {
            uri: format!("session://{}/health", entity.id),
            session_id: entity.id.clone(),
            host: format!("{}:{}", entity.address.host(), entity.address.port()),
            healthy: entity.healthy,
        });
    }
    Ok(())
}

async fn collect_forward_resources<FR: ForwardRepository + Send + Sync>(
    repo: &FR,
    out: &mut Vec<ResourceListing>,
) -> Result<(), DomainError> {
    let listed = repo.list_filtered(None).await?;
    for entity in listed {
        out.push(ResourceListing::Forward {
            uri: format!("forward://{}/events", entity.id),
            forward_id: entity.id,
            local_port: entity.local_port,
            remote: format!("{}:{}", entity.remote_host, entity.remote_port),
            session_id: entity.session_id,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ListResourcesRequest, ListResourcesUseCase, ResourceListing};
    use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
    use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
    use crate::domain::command::CommandEntity;
    use crate::domain::identity::Address;
    use crate::domain::ids::{CommandId, SessionId, ShellId, TransferId};
    use crate::domain::session::SessionEntity;
    use crate::domain::shell::{ShellEntity, ShellTerminal};
    use crate::domain::transfer::{TransferDirection, TransferEntity};
    use crate::ports::command_repo::CommandRepository;
    use crate::ports::session_repo::SessionRepository;
    use crate::ports::shell_repo::ShellRepository;
    use crate::ports::transfer_repo::TransferRepository;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use std::time::Duration;

    use crate::adapters::repo::dashmap::forward::DashMapForwardRepo;
    #[cfg(feature = "port_forward")]
    use crate::domain::forward::ForwardEntity;
    #[cfg(feature = "port_forward")]
    use crate::domain::ids::ForwardId;
    #[cfg(feature = "port_forward")]
    use crate::ports::forward_repo::ForwardRepository;

    type UseCaseUnderTest = ListResourcesUseCase<
        DashMapSessionRepo,
        DashMapCommandRepo,
        DashMapShellRepo,
        DashMapTransferRepo,
        DashMapForwardRepo,
    >;

    struct Harness {
        uc: UseCaseUnderTest,
        sessions: Arc<DashMapSessionRepo>,
        commands: Arc<DashMapCommandRepo>,
        shells: Arc<DashMapShellRepo>,
        transfers: Arc<DashMapTransferRepo>,
        #[cfg(feature = "port_forward")]
        forwards: Arc<DashMapForwardRepo>,
    }

    fn build_harness() -> Harness {
        let sessions = Arc::new(DashMapSessionRepo::new());
        let commands = Arc::new(DashMapCommandRepo::new());
        let shells = Arc::new(DashMapShellRepo::new());
        let transfers = Arc::new(DashMapTransferRepo::new());
        let forwards = Arc::new(DashMapForwardRepo::new());
        let uc = ListResourcesUseCase::new(
            Arc::clone(&sessions),
            Arc::clone(&commands),
            Arc::clone(&shells),
            Arc::clone(&transfers),
            Arc::clone(&forwards),
        );
        Harness {
            uc,
            sessions,
            commands,
            shells,
            transfers,
            #[cfg(feature = "port_forward")]
            forwards,
        }
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
            healthy: Some(true),
        };
        repo.insert(entity).await.expect("seed session");
    }

    async fn seed_command(repo: &DashMapCommandRepo, cid: &str, sid: &str) {
        let entity = CommandEntity::new(
            CommandId::new(cid.to_string()),
            SessionId::new(sid.to_string()),
            "echo hi".to_string(),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        );
        repo.insert(entity).await.expect("seed command");
    }

    async fn seed_shell(repo: &DashMapShellRepo, shell_id: &str, session_id: &str) {
        let entity = ShellEntity::new(
            ShellId::new(shell_id.to_string()),
            SessionId::new(session_id.to_string()),
            ShellTerminal::new("xterm-256color".to_string(), 80, 24),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            Duration::from_secs(600),
            10 * 1024 * 1024,
        );
        repo.insert(entity).await.expect("seed shell");
    }

    async fn seed_transfer(repo: &DashMapTransferRepo, tid: &str, sid: &str) {
        let entity = TransferEntity::new(
            TransferId::new(tid.to_string()),
            SessionId::new(sid.to_string()),
            TransferDirection::Upload,
            "/tmp/x".to_string(),
            "/r/x".to_string(),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            1024,
        );
        repo.insert(entity).await.expect("seed transfer");
    }

    #[cfg(feature = "port_forward")]
    async fn seed_forward(repo: &DashMapForwardRepo, fid: &str, sid: &str) {
        let entity = ForwardEntity::new(
            ForwardId::new(fid.to_string()),
            SessionId::new(sid.to_string()),
            8080,
            "internal".to_string(),
            80,
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        );
        repo.insert(entity).await.expect("seed forward");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_universe_yields_no_resources() {
        let h = build_harness();
        let outcome =
            h.uc.execute(ListResourcesRequest::default())
                .await
                .expect("execute");
        assert!(outcome.resources.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lists_one_session_with_canonical_uri() {
        let h = build_harness();
        seed_session(&h.sessions, "s-1").await;
        let outcome =
            h.uc.execute(ListResourcesRequest::default())
                .await
                .expect("execute");
        assert_eq!(outcome.resources.len(), 1);
        match &outcome.resources[0] {
            ResourceListing::Session {
                uri,
                session_id,
                host,
                healthy,
            } => {
                assert_eq!(uri, "session://s-1/health");
                assert_eq!(session_id.as_str(), "s-1");
                assert_eq!(host, "h.example.com:22");
                assert_eq!(*healthy, Some(true));
            }
            other => panic!("expected Session, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lists_one_command_with_canonical_uri() {
        let h = build_harness();
        seed_command(&h.commands, "c-1", "s-1").await;
        let outcome =
            h.uc.execute(ListResourcesRequest::default())
                .await
                .expect("execute");
        assert_eq!(outcome.resources.len(), 1);
        match &outcome.resources[0] {
            ResourceListing::Command {
                uri,
                command_id,
                command,
                session_id,
            } => {
                assert_eq!(uri, "command://c-1/output");
                assert_eq!(command_id.as_str(), "c-1");
                assert_eq!(command, "echo hi");
                assert_eq!(session_id.as_str(), "s-1");
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lists_one_shell_with_canonical_uri() {
        let h = build_harness();
        seed_shell(&h.shells, "sh-1", "s-1").await;
        let outcome =
            h.uc.execute(ListResourcesRequest::default())
                .await
                .expect("execute");
        assert_eq!(outcome.resources.len(), 1);
        match &outcome.resources[0] {
            ResourceListing::Shell {
                uri,
                shell_id,
                session_id,
                term_type,
            } => {
                assert_eq!(uri, "shell://sh-1/output");
                assert_eq!(shell_id.as_str(), "sh-1");
                assert_eq!(session_id.as_str(), "s-1");
                assert_eq!(term_type, "xterm-256color");
            }
            other => panic!("expected Shell, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lists_one_transfer_with_canonical_uri() {
        let h = build_harness();
        seed_transfer(&h.transfers, "t-1", "s-1").await;
        let outcome =
            h.uc.execute(ListResourcesRequest::default())
                .await
                .expect("execute");
        assert_eq!(outcome.resources.len(), 1);
        match &outcome.resources[0] {
            ResourceListing::Transfer {
                uri,
                transfer_id,
                direction,
                session_id,
            } => {
                assert_eq!(uri, "transfer://t-1/progress");
                assert_eq!(transfer_id.as_str(), "t-1");
                assert_eq!(*direction, TransferDirection::Upload);
                assert_eq!(session_id.as_str(), "s-1");
            }
            other => panic!("expected Transfer, got {other:?}"),
        }
    }

    #[cfg(feature = "port_forward")]
    #[tokio::test(flavor = "multi_thread")]
    async fn lists_one_forward_with_canonical_uri() {
        let h = build_harness();
        seed_forward(&h.forwards, "f-1", "s-1").await;
        let outcome =
            h.uc.execute(ListResourcesRequest::default())
                .await
                .expect("execute");
        assert_eq!(outcome.resources.len(), 1);
        match &outcome.resources[0] {
            ResourceListing::Forward {
                uri,
                forward_id,
                local_port,
                remote,
                session_id,
            } => {
                assert_eq!(uri, "forward://f-1/events");
                assert_eq!(forward_id.as_str(), "f-1");
                assert_eq!(*local_port, 8080);
                assert_eq!(remote, "internal:80");
                assert_eq!(session_id.as_str(), "s-1");
            }
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn aggregates_all_kinds_in_a_single_outcome() {
        let h = build_harness();
        seed_session(&h.sessions, "s-1").await;
        seed_command(&h.commands, "c-1", "s-1").await;
        seed_shell(&h.shells, "sh-1", "s-1").await;
        seed_transfer(&h.transfers, "t-1", "s-1").await;
        #[cfg(feature = "port_forward")]
        seed_forward(&h.forwards, "f-1", "s-1").await;

        let outcome =
            h.uc.execute(ListResourcesRequest::default())
                .await
                .expect("execute");
        #[cfg(feature = "port_forward")]
        let expected = 5;
        #[cfg(not(feature = "port_forward"))]
        let expected = 4;
        assert_eq!(outcome.resources.len(), expected);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unhealthy_session_surfaces_healthy_false() {
        let h = build_harness();
        let entity = SessionEntity {
            id: SessionId::new("dead".to_string()),
            name: None,
            agent_id: None,
            address: sample_address(),
            username: "alice".to_string(),
            connected_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            default_timeout: Duration::from_secs(30),
            retry_attempts: 0,
            compression_enabled: true,
            last_health_check: None,
            healthy: Some(false),
        };
        h.sessions.insert(entity).await.expect("seed");
        let outcome =
            h.uc.execute(ListResourcesRequest::default())
                .await
                .expect("execute");
        match &outcome.resources[0] {
            ResourceListing::Session { healthy, .. } => {
                assert_eq!(*healthy, Some(false));
            }
            other => panic!("expected Session, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn never_probed_session_surfaces_healthy_none() {
        let h = build_harness();
        let entity = SessionEntity {
            id: SessionId::new("fresh".to_string()),
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
        h.sessions.insert(entity).await.expect("seed");
        let outcome =
            h.uc.execute(ListResourcesRequest::default())
                .await
                .expect("execute");
        match &outcome.resources[0] {
            ResourceListing::Session { healthy, .. } => assert!(healthy.is_none()),
            other => panic!("expected Session, got {other:?}"),
        }
    }
}
