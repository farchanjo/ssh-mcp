//! Use case: tear down a single SSH session and reclaim every aggregate it
//! owns.
//!
//! Translates the v3 `ssh_disconnect_impl` (from `src/mcp/tools/connection.rs`)
//! into a hexagonal use case orchestrating ports. No rmcp / russh / tokio
//! types leak into the public surface.
//!
//! # Orchestration shape
//!
//! 1. Resolve the session via [`SessionRepository::get`]. A missing id
//!    short-circuits with [`DomainError::SessionNotFound`] before any side
//!    effect runs (mirrors v3's "log + error" branch but typed as a domain
//!    error so the inbound adapter renders the canonical
//!    `SSH_DISCONNECT: ERROR / REASON: [SESSION_NOT_FOUND]` block).
//! 2. Cancel every async command owned by the session
//!    ([`CommandRepository::list_filtered`] -> [`SshClientPort::cancel`] +
//!    [`CommandRepository::remove`]).
//! 3. Close every interactive shell owned by the session
//!    ([`ShellRepository::list_filtered`] -> [`ShellRepository::remove`]).
//!    The russh PTY is reaped automatically when the SSH transport tears
//!    down in step 5, so no port-level "close shell" call is required here.
//! 4. Abort every in-flight transfer owned by the session
//!    ([`TransferRepository::list_filtered`] -> [`TransferRepository::remove`]).
//!    Same reasoning as step 3 for the SFTP channels.
//! 5. Remove the session from [`SessionRepository`] and unregister it from
//!    the agent secondary index when an agent id is bound. This happens
//!    *before* the SSH-side disconnect so a transport failure cannot strand
//!    a half-dead entry in the repository.
//! 6. Tear down the SSH transport via [`SshClientPort::disconnect`]. Errors
//!    are surfaced to the caller — the in-memory state is already consistent
//!    so the inbound adapter can render a `SSH_DISCONNECT: ERROR / REASON:
//!    [TRANSPORT]` block while still being safe to retry (the session is
//!    gone; the second call yields `SessionNotFound`).
//!
//! # Concurrency contract
//!
//! Every `.await` happens outside any repository / fake guard — the use case
//! clones small handles (`Arc`) into local bindings before entering await
//! points. The repository ports return owned values so no shard-spanning
//! borrows escape.

use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::ids::SessionId;
use crate::domain::session::SessionEntity;
use crate::domain::subscription::SubId;
use crate::ports::command_repo::CommandRepository;
use crate::ports::lifecycle_policy::LifecyclePolicyPort;
use crate::ports::session_repo::SessionRepository;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::ssh_client::SshClientPort;
use crate::ports::subscriber_lane::LaneAdmin;
use crate::ports::subscriber_registry::ResourceKind;
use crate::ports::transfer_repo::TransferRepository;

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct DisconnectRequest {
    /// Session to disconnect.
    pub session_id: SessionId,
}

/// Outcome surfaced to the inbound adapter on a successful tear-down.
///
/// Counts are exposed so the rmcp tool wrapper can render
/// `COMMANDS_CANCELLED: N`, `SHELLS_CLOSED: N`, `TRANSFERS_ABORTED: N`
/// lines for operator visibility — the v3 wrapper logs equivalent counts via
/// `tracing::info!`, but emitting them as part of the outcome lets clients
/// reason about the cleanup without scraping logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisconnectOutcome {
    /// Session that was torn down.
    pub session_id: SessionId,
    /// Number of async commands cancelled by the orchestration.
    pub commands_cancelled: usize,
    /// Number of interactive shells closed by the orchestration.
    pub shells_closed: usize,
    /// Number of in-flight transfers aborted by the orchestration.
    pub transfers_aborted: usize,
}

/// Disconnect-session use case generic over every adapter dependency. The
/// composition root pins concrete adapter types per binary; tests inject
/// fakes via [`crate::adapters`].
#[derive(Debug)]
pub struct DisconnectSessionUseCase<S, R, CR, ShR, TR>
where
    S: SshClientPort + Send + Sync,
    R: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
{
    ssh: Arc<S>,
    sessions: Arc<R>,
    commands: Arc<CR>,
    shells: Arc<ShR>,
    transfers: Arc<TR>,
    /// v5.3 lifecycle cascade — closes child resource lifecycle entries
    /// alongside the repo removal so `SUB_LEAK_RISK` warnings clear on
    /// `ssh_disconnect` / `ssh_disconnect_agent`. Optional so test
    /// fixtures can construct without the full lifecycle adapter.
    lifecycle: Option<Arc<dyn LifecyclePolicyPort>>,
    /// v5.3.2 lane cascade — closes orphaned `sub_open` lanes
    /// bound to the resources owned by this session so they don't
    /// outlive their producer. Optional for the same fixture reason.
    lane_admin: Option<Arc<dyn LaneAdmin>>,
}

impl<S, R, CR, ShR, TR> DisconnectSessionUseCase<S, R, CR, ShR, TR>
where
    S: SshClientPort + Send + Sync,
    R: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles. No
    /// lifecycle cascade — used by tests / fixtures that bypass the
    /// full production wiring.
    #[must_use]
    pub const fn new(
        ssh: Arc<S>,
        sessions: Arc<R>,
        commands: Arc<CR>,
        shells: Arc<ShR>,
        transfers: Arc<TR>,
    ) -> Self {
        Self {
            ssh,
            sessions,
            commands,
            shells,
            transfers,
            lifecycle: None,
            lane_admin: None,
        }
    }

    /// Wire the use case with the lifecycle cascade installed —
    /// production composition uses this constructor so child resources
    /// (commands / shells / transfers) transition to `Closed` in the
    /// lifecycle adapter alongside the repo removal.
    #[must_use]
    pub fn with_lifecycle(
        ssh: Arc<S>,
        sessions: Arc<R>,
        commands: Arc<CR>,
        shells: Arc<ShR>,
        transfers: Arc<TR>,
        lifecycle: Arc<dyn LifecyclePolicyPort>,
        lane_admin: Arc<dyn LaneAdmin>,
    ) -> Self {
        Self {
            ssh,
            sessions,
            commands,
            shells,
            transfers,
            lifecycle: Some(lifecycle),
            lane_admin: Some(lane_admin),
        }
    }

    /// Close every `sub_open` lane bound to a resource id.
    /// Best-effort: per-lane close failures are swallowed so one bad
    /// lane does not abort the wider cleanup.
    async fn close_lanes_for_resource(&self, resource_id: &str) {
        let Some(lane_admin) = self.lane_admin.as_ref() else {
            return;
        };
        let stale: Vec<SubId> = lane_admin
            .list()
            .into_iter()
            .filter(|s| s.resource_id == resource_id)
            .map(|s| s.sub_id)
            .collect();
        for sub_id in stale {
            let _ = lane_admin.close(&sub_id).await;
        }
    }

    /// Drive the disconnect orchestration. See module-level docs for the
    /// step-by-step semantics.
    ///
    /// # Errors
    ///
    /// - [`DomainError::SessionNotFound`] when the id is unknown at probe
    ///   time. The use case returns before any side effect.
    /// - Any [`DomainError`] surfaced by the underlying repositories.
    /// - Any [`DomainError`] surfaced by [`SshClientPort::disconnect`]; the
    ///   in-memory aggregates are already cleaned up so the caller may
    ///   safely retry with a new disconnect (the second attempt will yield
    ///   `SessionNotFound`).
    pub async fn execute(&self, req: DisconnectRequest) -> Result<DisconnectOutcome, DomainError> {
        let session = self
            .sessions
            .get(&req.session_id)
            .await?
            .ok_or_else(|| DomainError::SessionNotFound(req.session_id.clone()))?;

        let transfers_aborted = self.abort_session_transfers(&req.session_id).await?;
        let shells_closed = self.close_session_shells(&req.session_id).await?;
        let commands_cancelled = self.cancel_session_commands(&req.session_id).await?;

        self.purge_session_entry(&session).await?;
        self.ssh.disconnect(&req.session_id).await?;

        Ok(DisconnectOutcome {
            session_id: req.session_id,
            commands_cancelled,
            shells_closed,
            transfers_aborted,
        })
    }

    /// Cancel every async command owned by `session_id`. Cancellation
    /// failures on the SSH side are tolerated (the channel may already be
    /// dead) but repository removal failures are surfaced.
    async fn cancel_session_commands(&self, session_id: &SessionId) -> Result<usize, DomainError> {
        let commands = self.commands.list_filtered(Some(session_id), None).await?;
        let count = commands.len();
        for cmd in &commands {
            let _ = self.ssh.cancel(&cmd.id).await;
        }
        for cmd in commands {
            if let Some(lifecycle) = self.lifecycle.as_ref() {
                let _ = lifecycle.force_close(ResourceKind::Command, cmd.id.as_str());
            }
            self.close_lanes_for_resource(cmd.id.as_str()).await;
            self.commands.remove(&cmd.id).await?;
        }
        Ok(count)
    }

    /// Close every interactive shell owned by `session_id`. The russh PTY is
    /// reaped automatically when the SSH transport disconnects, so the
    /// repository removal is the only side effect required here.
    async fn close_session_shells(&self, session_id: &SessionId) -> Result<usize, DomainError> {
        let shells = self.shells.list_filtered(Some(session_id)).await?;
        let count = shells.len();
        for shell in shells {
            if let Some(lifecycle) = self.lifecycle.as_ref() {
                let _ = lifecycle.force_close(ResourceKind::Shell, shell.id.as_str());
            }
            self.close_lanes_for_resource(shell.id.as_str()).await;
            self.shells.remove(&shell.id).await?;
        }
        Ok(count)
    }

    /// Abort every in-flight transfer owned by `session_id`. Same reasoning
    /// as [`Self::close_session_shells`] for the SFTP channel cleanup.
    async fn abort_session_transfers(&self, session_id: &SessionId) -> Result<usize, DomainError> {
        let transfers = self.transfers.list_filtered(Some(session_id)).await?;
        let count = transfers.len();
        for xfer in transfers {
            if let Some(lifecycle) = self.lifecycle.as_ref() {
                let _ = lifecycle.force_close(ResourceKind::Transfer, xfer.id.as_str());
            }
            self.close_lanes_for_resource(xfer.id.as_str()).await;
            self.transfers.remove(&xfer.id).await?;
        }
        Ok(count)
    }

    /// Remove the session from the repository and unregister it from the
    /// agent secondary index when an agent id is bound. Done *before* the
    /// SSH-side disconnect so a transport failure cannot strand a half-dead
    /// entry.
    async fn purge_session_entry(&self, session: &SessionEntity) -> Result<(), DomainError> {
        if let Some(agent_id) = session.agent_id.as_ref() {
            self.sessions
                .unregister_agent(agent_id, &session.id)
                .await?;
        }
        self.sessions.remove(&session.id).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DisconnectRequest, DisconnectSessionUseCase};
    use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
    use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
    use crate::adapters::ssh::fake::{FakeSshCall, FakeSshClient};
    use crate::domain::command::{CommandEntity, CommandStatus};
    use crate::domain::error::DomainError;
    use crate::domain::identity::Address;
    use crate::domain::ids::{AgentId, CommandId, SessionId, ShellId, TransferId};
    use crate::domain::session::SessionEntity;
    use crate::domain::shell::{ShellEntity, ShellTerminal};
    use crate::domain::transfer::{TransferDirection, TransferEntity};
    use crate::ports::command_repo::CommandRepository;
    use crate::ports::session_repo::SessionRepository;
    use crate::ports::shell_repo::ShellRepository;
    use crate::ports::transfer_repo::TransferRepository;
    use chrono::Utc;
    use std::sync::Arc;
    use std::time::Duration;

    type UseCaseUnderTest = DisconnectSessionUseCase<
        FakeSshClient,
        DashMapSessionRepo,
        DashMapCommandRepo,
        DashMapShellRepo,
        DashMapTransferRepo,
    >;

    struct Wired {
        uc: UseCaseUnderTest,
        ssh: Arc<FakeSshClient>,
        sessions: Arc<DashMapSessionRepo>,
        commands: Arc<DashMapCommandRepo>,
        shells: Arc<DashMapShellRepo>,
        transfers: Arc<DashMapTransferRepo>,
    }

    fn build_wired() -> Wired {
        let ssh = Arc::new(FakeSshClient::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        let commands = Arc::new(DashMapCommandRepo::new());
        let shells = Arc::new(DashMapShellRepo::new());
        let transfers = Arc::new(DashMapTransferRepo::new());
        let uc = DisconnectSessionUseCase::new(
            Arc::clone(&ssh),
            Arc::clone(&sessions),
            Arc::clone(&commands),
            Arc::clone(&shells),
            Arc::clone(&transfers),
        );
        Wired {
            uc,
            ssh,
            sessions,
            commands,
            shells,
            transfers,
        }
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

    fn seed_commands(repo: &DashMapCommandRepo, session_id: &SessionId, n: usize) {
        let session = session_id.clone();
        let repo_clone = repo.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                for i in 0..n {
                    let cmd = CommandEntity {
                        id: CommandId::new(format!("c-{i}")),
                        session_id: session.clone(),
                        command: format!("cmd-{i}"),
                        status: CommandStatus::Running,
                        started_at: Utc::now(),
                        exit_code: None,
                        timed_out: false,
                    };
                    repo_clone.insert(cmd).await.expect("cmd insert");
                }
            });
        });
    }

    fn seed_shells(repo: &DashMapShellRepo, session_id: &SessionId, n: usize) {
        let session = session_id.clone();
        let repo_clone = repo.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                for i in 0..n {
                    let shell = ShellEntity::new(
                        ShellId::new(format!("sh-{i}")),
                        session.clone(),
                        ShellTerminal::new("xterm".to_string(), 80, 24),
                        Utc::now(),
                        Duration::from_secs(600),
                        10 * 1024 * 1024,
                    );
                    repo_clone.insert(shell).await.expect("shell insert");
                }
            });
        });
    }

    fn seed_transfers(repo: &DashMapTransferRepo, session_id: &SessionId, n: usize) {
        let session = session_id.clone();
        let repo_clone = repo.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                for i in 0..n {
                    let xfer = TransferEntity::new(
                        TransferId::new(format!("t-{i}")),
                        session.clone(),
                        TransferDirection::Upload,
                        format!("/tmp/x-{i}"),
                        format!("/r/x-{i}"),
                        Utc::now(),
                        1024,
                    );
                    repo_clone.insert(xfer).await.expect("xfer insert");
                }
            });
        });
    }

    // --- Scenario 1 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn happy_path_session_with_no_aggregates_disconnects_cleanly() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-clean", None);
        let outcome = wired
            .uc
            .execute(DisconnectRequest {
                session_id: seeded.id.clone(),
            })
            .await
            .expect("disconnect");
        assert_eq!(outcome.session_id, seeded.id);
        assert_eq!(outcome.commands_cancelled, 0);
        assert_eq!(outcome.shells_closed, 0);
        assert_eq!(outcome.transfers_aborted, 0);
        assert!(
            wired.sessions.get(&seeded.id).await.expect("get").is_none(),
            "session must be removed from repository"
        );
        let calls = wired.ssh.calls();
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0], FakeSshCall::Disconnect { .. }));
    }

    // --- Scenario 2 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn session_with_running_commands_cancels_and_removes_them_all() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-cmds", None);
        seed_commands(&wired.commands, &seeded.id, 3);
        let outcome = wired
            .uc
            .execute(DisconnectRequest {
                session_id: seeded.id.clone(),
            })
            .await
            .expect("disconnect");
        assert_eq!(outcome.commands_cancelled, 3);
        assert_eq!(
            wired
                .commands
                .count_by_session(&seeded.id)
                .await
                .expect("count"),
            0
        );
    }

    // --- Scenario 3 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn session_with_open_shells_closes_them_all() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-shells", None);
        seed_shells(&wired.shells, &seeded.id, 4);
        let outcome = wired
            .uc
            .execute(DisconnectRequest {
                session_id: seeded.id.clone(),
            })
            .await
            .expect("disconnect");
        assert_eq!(outcome.shells_closed, 4);
        assert_eq!(
            wired
                .shells
                .count_by_session(&seeded.id)
                .await
                .expect("count"),
            0
        );
    }

    // --- Scenario 4 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn session_with_in_flight_transfers_aborts_them_all() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-xfers", None);
        seed_transfers(&wired.transfers, &seeded.id, 2);
        let outcome = wired
            .uc
            .execute(DisconnectRequest {
                session_id: seeded.id.clone(),
            })
            .await
            .expect("disconnect");
        assert_eq!(outcome.transfers_aborted, 2);
        assert_eq!(
            wired
                .transfers
                .count_by_session(&seeded.id)
                .await
                .expect("count"),
            0
        );
    }

    // --- Scenario 5 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn session_with_mixed_aggregates_returns_accurate_counts() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-mix", None);
        seed_commands(&wired.commands, &seeded.id, 2);
        seed_shells(&wired.shells, &seeded.id, 3);
        seed_transfers(&wired.transfers, &seeded.id, 4);
        let outcome = wired
            .uc
            .execute(DisconnectRequest {
                session_id: seeded.id.clone(),
            })
            .await
            .expect("disconnect");
        assert_eq!(outcome.commands_cancelled, 2);
        assert_eq!(outcome.shells_closed, 3);
        assert_eq!(outcome.transfers_aborted, 4);
        assert!(wired.sessions.get(&seeded.id).await.expect("get").is_none());
    }

    // --- Scenario 6 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn session_not_found_short_circuits_with_domain_error() {
        let wired = build_wired();
        let unknown = SessionId::new("ghost".to_string());
        let err = wired
            .uc
            .execute(DisconnectRequest {
                session_id: unknown.clone(),
            })
            .await
            .expect_err("must fail");
        assert_eq!(err, DomainError::SessionNotFound(unknown));
        // No side effect on the SSH adapter: the probe short-circuits before
        // any disconnect / cancel call.
        assert_eq!(wired.ssh.call_count(), 0);
    }

    // --- Scenario 7 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn ssh_disconnect_failure_is_surfaced_to_caller() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-broken", None);
        wired
            .ssh
            .queue_disconnect_error(DomainError::Transport("eof".to_string()));
        let err = wired
            .uc
            .execute(DisconnectRequest {
                session_id: seeded.id.clone(),
            })
            .await
            .expect_err("must surface transport failure");
        assert_eq!(err, DomainError::Transport("eof".to_string()));
        // The session is still purged from the repo so a retry surfaces the
        // canonical SessionNotFound rather than looping on the broken
        // transport.
        assert!(wired.sessions.get(&seeded.id).await.expect("get").is_none());
    }

    // --- Scenario 8 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn calling_disconnect_twice_yields_session_not_found_on_second() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-idem", None);
        wired
            .uc
            .execute(DisconnectRequest {
                session_id: seeded.id.clone(),
            })
            .await
            .expect("first disconnect");
        let err = wired
            .uc
            .execute(DisconnectRequest {
                session_id: seeded.id.clone(),
            })
            .await
            .expect_err("second must fail");
        assert_eq!(err, DomainError::SessionNotFound(seeded.id));
    }

    // --- Scenario 9 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_bound_session_is_unregistered_from_secondary_index() {
        let wired = build_wired();
        let agent = AgentId::new("agent-7".to_string());
        let seeded = seed_session(&wired.sessions, "sess-agent", Some(agent.clone()));
        // Sanity: agent index has the binding before disconnect.
        let pre = wired
            .sessions
            .list_by_agent(&agent)
            .await
            .expect("list_by_agent");
        assert_eq!(pre, vec![seeded.id.clone()]);
        wired
            .uc
            .execute(DisconnectRequest {
                session_id: seeded.id.clone(),
            })
            .await
            .expect("disconnect");
        let post = wired
            .sessions
            .list_by_agent(&agent)
            .await
            .expect("list_by_agent");
        assert!(
            post.is_empty(),
            "agent secondary index must be cleaned up on disconnect"
        );
    }

    // --- Scenario 10 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn aggregates_belonging_to_other_sessions_are_left_alone() {
        let wired = build_wired();
        let target = seed_session(&wired.sessions, "sess-target", None);
        let other = seed_session(&wired.sessions, "sess-other", None);
        seed_commands(&wired.commands, &target.id, 2);
        // Other session has its own aggregate identified by an ID not
        // colliding with the target's "c-*" prefix.
        let other_id = other.id.clone();
        let cmds = wired.commands.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let cmd = CommandEntity {
                    id: CommandId::new("survivor".to_string()),
                    session_id: other_id,
                    command: "still-running".to_string(),
                    status: CommandStatus::Running,
                    started_at: Utc::now(),
                    exit_code: None,
                    timed_out: false,
                };
                cmds.insert(cmd).await.expect("survivor insert");
            });
        });
        wired
            .uc
            .execute(DisconnectRequest {
                session_id: target.id.clone(),
            })
            .await
            .expect("disconnect target");
        // Other session untouched.
        assert!(wired.sessions.get(&other.id).await.expect("get").is_some());
        assert_eq!(
            wired
                .commands
                .count_by_session(&other.id)
                .await
                .expect("count"),
            1,
            "other session's commands must survive"
        );
    }

    // --- Scenario 11 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn orchestration_calls_ssh_cancel_for_each_running_command() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-cancel-log", None);
        seed_commands(&wired.commands, &seeded.id, 3);
        wired
            .uc
            .execute(DisconnectRequest {
                session_id: seeded.id.clone(),
            })
            .await
            .expect("disconnect");
        // The fake's `cancel` returns CommandNotFound by default — the use
        // case must tolerate that and continue. The recorded log has only
        // one Disconnect entry (cancels are not recorded by the fake).
        let calls = wired.ssh.calls();
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0], FakeSshCall::Disconnect { .. }));
        // Yet the commands were removed.
        assert_eq!(
            wired
                .commands
                .count_by_session(&seeded.id)
                .await
                .expect("count"),
            0
        );
    }

    // --- Scenario 12 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn outcome_session_id_matches_request_session_id() {
        let wired = build_wired();
        let seeded = seed_session(&wired.sessions, "sess-echo", None);
        let outcome = wired
            .uc
            .execute(DisconnectRequest {
                session_id: seeded.id.clone(),
            })
            .await
            .expect("disconnect");
        assert_eq!(outcome.session_id, seeded.id);
    }
}
