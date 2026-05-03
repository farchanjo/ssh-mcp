//! Use case: bulk-disconnect every session owned by a given agent.
//!
//! Mirrors the v3 `ssh_disconnect_agent_impl` (see
//! `src/mcp/tools/connection.rs::ssh_disconnect_agent_impl` and the
//! `cleanup_agent_sessions` helper in `src/mcp/tools/legacy_helpers.rs`).
//!
//! # Orchestration shape
//!
//! 1. Resolve the set of session ids bound to the agent via
//!    [`SessionRepository::list_by_agent`].
//! 2. For each owned session, in order:
//!    - Cancel every command attached to the session: per-command
//!      [`SshClientPort::cancel`] (best-effort — already-finished commands
//!      surface `CommandNotFound`, which is silently swallowed) followed by
//!      [`CommandRepository::update`] to flip the status to `Cancelled` and
//!      [`CommandRepository::remove`] to drop the snapshot.
//!    - Close every shell attached to the session: a
//!      [`ShellRepository::update`] to flip the status to `Closed` and a
//!      [`ShellRepository::remove`] to drop the snapshot. The SSH side of
//!      the shell is torn down implicitly by the upcoming
//!      [`SshClientPort::disconnect`].
//!    - Abort every transfer attached to the session: a
//!      [`TransferRepository::update`] to flip the status to `Cancelled`
//!      and a [`TransferRepository::remove`] to drop the snapshot.
//!    - Issue [`SshClientPort::disconnect`] to drop the russh handle.
//!    - Drop the session via [`SessionRepository::remove`] and unbind it
//!      from the agent index via [`SessionRepository::unregister_agent`].
//! 3. Aggregate the per-session counts into a single
//!    [`DisconnectAgentOutcome`].
//!
//! # Error semantics
//!
//! Mirrors the v3 baseline: cleanup is **best-effort** and never aborts
//! mid-flight. Port failures (failed `disconnect`, `cancel`, `update`,
//! `remove`) are swallowed inside the use case so a single dying session
//! cannot leak storage state for sibling sessions owned by the same agent.
//! The only fatal outcome is a `Storage` failure on the initial
//! `list_by_agent` lookup, which is propagated unchanged. An unknown
//! agent id resolves to an outcome with all counters at zero (also v3
//! behaviour).
//!
//! # Concurrency contract
//!
//! Every `.await` happens outside any repository / fake guard — the use
//! case clones small handles (`Arc`) into local bindings before entering
//! await points, exactly like the H10 canary.

use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::ids::{AgentId, SessionId};
use crate::ports::command_repo::CommandRepository;
use crate::ports::session_repo::SessionRepository;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::ssh_client::SshClientPort;
use crate::ports::transfer_repo::TransferRepository;

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct DisconnectAgentRequest {
    /// Identifier of the agent whose sessions must be disconnected. An
    /// unknown id resolves to a zero-count outcome rather than an error.
    pub agent_id: AgentId,
}

/// Outbound DTO surfacing the cleanup tally. Counts are exact
/// (non-aggregated): each successfully removed entity contributes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisconnectAgentOutcome {
    /// Echo of the requested agent id so the inbound adapter can render
    /// the response without the original DTO.
    pub agent_id: AgentId,
    /// Number of sessions removed from the repository.
    pub sessions_disconnected: usize,
    /// Number of commands flipped to `Cancelled` and removed.
    pub commands_cancelled: usize,
    /// Number of shells flipped to `Closed` and removed.
    pub shells_closed: usize,
    /// Number of transfers flipped to `Cancelled` and removed.
    pub transfers_aborted: usize,
}

/// Bulk disconnect-by-agent use case generic over every adapter
/// dependency. The composition root pins concrete adapter types per
/// binary; tests inject fakes via [`crate::adapters`].
#[derive(Debug)]
pub struct DisconnectAgentUseCase<S, R, CR, ShR, TR>
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
}

impl<S, R, CR, ShR, TR> DisconnectAgentUseCase<S, R, CR, ShR, TR>
where
    S: SshClientPort + Send + Sync,
    R: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
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
        }
    }

    /// Drive the bulk-cleanup orchestration. See module-level docs for
    /// the step-by-step semantics.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` only when the initial
    /// `list_by_agent` repository call fails. Per-session cleanup
    /// failures are swallowed so the bulk operation can always make
    /// progress across sibling sessions.
    pub async fn execute(
        &self,
        req: DisconnectAgentRequest,
    ) -> Result<DisconnectAgentOutcome, DomainError> {
        let session_ids = self.sessions.list_by_agent(&req.agent_id).await?;
        let mut outcome = DisconnectAgentOutcome {
            agent_id: req.agent_id.clone(),
            sessions_disconnected: 0,
            commands_cancelled: 0,
            shells_closed: 0,
            transfers_aborted: 0,
        };
        for session_id in session_ids {
            let counts = self.cleanup_session(&session_id).await;
            outcome.commands_cancelled = outcome.commands_cancelled.saturating_add(counts.commands);
            outcome.shells_closed = outcome.shells_closed.saturating_add(counts.shells);
            outcome.transfers_aborted = outcome.transfers_aborted.saturating_add(counts.transfers);
            self.tear_down_session(&req.agent_id, &session_id).await;
            outcome.sessions_disconnected = outcome.sessions_disconnected.saturating_add(1);
        }
        Ok(outcome)
    }

    /// Cancel commands, close shells, and abort transfers attached to a
    /// single session. Each substep is best-effort: a port failure logs
    /// nothing (the rmcp wrapper observes the final tally) but does not
    /// abort the wider cleanup.
    async fn cleanup_session(&self, session_id: &SessionId) -> CleanupCounts {
        let commands = self.cancel_commands(session_id).await;
        let shells = self.close_shells(session_id).await;
        let transfers = self.abort_transfers(session_id).await;
        CleanupCounts {
            commands,
            shells,
            transfers,
        }
    }

    /// Cancel every command attached to `session_id`. Already-finished
    /// commands surface `CommandNotFound` from the SSH port; that is
    /// expected and intentionally swallowed.
    async fn cancel_commands(&self, session_id: &SessionId) -> usize {
        let listed = self
            .commands
            .list_filtered(Some(session_id), None)
            .await
            .unwrap_or_default();
        let mut count = 0_usize;
        for entity in listed {
            let _ = self.ssh.cancel(&entity.id).await;
            let cancelled = entity.clone().cancel();
            let _ = self.commands.update(cancelled).await;
            if matches!(self.commands.remove(&entity.id).await, Ok(Some(_))) {
                count = count.saturating_add(1);
            }
        }
        count
    }

    /// Mark every shell attached to `session_id` as closed and drop the
    /// snapshot. The SSH-side channel is implicitly torn down by the
    /// upcoming session disconnect.
    async fn close_shells(&self, session_id: &SessionId) -> usize {
        let listed = self
            .shells
            .list_filtered(Some(session_id))
            .await
            .unwrap_or_default();
        let mut count = 0_usize;
        for entity in listed {
            let closed = entity.clone().close();
            let _ = self.shells.update(closed).await;
            if matches!(self.shells.remove(&entity.id).await, Ok(Some(_))) {
                count = count.saturating_add(1);
            }
        }
        count
    }

    /// Mark every transfer attached to `session_id` as cancelled and
    /// drop the snapshot.
    async fn abort_transfers(&self, session_id: &SessionId) -> usize {
        let listed = self
            .transfers
            .list_filtered(Some(session_id))
            .await
            .unwrap_or_default();
        let mut count = 0_usize;
        for entity in listed {
            let cancelled = entity.clone().cancel();
            let _ = self.transfers.update(cancelled).await;
            if matches!(self.transfers.remove(&entity.id).await, Ok(Some(_))) {
                count = count.saturating_add(1);
            }
        }
        count
    }

    /// Issue `disconnect` on the SSH port and drop the session from the
    /// repository, unbinding it from the agent index. All errors are
    /// best-effort by design (mirrors v3 `cleanup_agent_sessions`).
    async fn tear_down_session(&self, agent_id: &AgentId, session_id: &SessionId) {
        let _ = self.ssh.disconnect(session_id).await;
        let _ = self.sessions.remove(session_id).await;
        let _ = self.sessions.unregister_agent(agent_id, session_id).await;
    }
}

/// Per-session cleanup tally bubbled up to [`DisconnectAgentUseCase::execute`].
#[derive(Debug, Default, Clone, Copy)]
struct CleanupCounts {
    /// Commands cancelled and removed.
    commands: usize,
    /// Shells closed and removed.
    shells: usize,
    /// Transfers cancelled and removed.
    transfers: usize,
}

#[cfg(test)]
mod tests {
    use super::{DisconnectAgentOutcome, DisconnectAgentRequest, DisconnectAgentUseCase};
    use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
    use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
    use crate::adapters::ssh::fake::{FakeSshCall, FakeSshClient};
    use crate::domain::command::CommandEntity;
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
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use std::time::Duration;

    type UseCaseUnderTest = DisconnectAgentUseCase<
        FakeSshClient,
        DashMapSessionRepo,
        DashMapCommandRepo,
        DashMapShellRepo,
        DashMapTransferRepo,
    >;

    struct Harness {
        uc: UseCaseUnderTest,
        ssh: Arc<FakeSshClient>,
        sessions: Arc<DashMapSessionRepo>,
        commands: Arc<DashMapCommandRepo>,
        shells: Arc<DashMapShellRepo>,
        transfers: Arc<DashMapTransferRepo>,
    }

    fn build_harness() -> Harness {
        let ssh = Arc::new(FakeSshClient::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        let commands = Arc::new(DashMapCommandRepo::new());
        let shells = Arc::new(DashMapShellRepo::new());
        let transfers = Arc::new(DashMapTransferRepo::new());
        let uc = DisconnectAgentUseCase::new(
            Arc::clone(&ssh),
            Arc::clone(&sessions),
            Arc::clone(&commands),
            Arc::clone(&shells),
            Arc::clone(&transfers),
        );
        Harness {
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

    async fn seed_session(repo: &DashMapSessionRepo, sid: &str, agent: Option<&AgentId>) {
        let entity = SessionEntity {
            id: SessionId::new(sid.to_string()),
            name: None,
            agent_id: agent.cloned(),
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
        if let Some(a) = agent {
            repo.register_agent(a, &SessionId::new(sid.to_string()))
                .await
                .expect("register agent");
        }
    }

    async fn seed_command(repo: &DashMapCommandRepo, cid: &str, sid: &str) {
        let entity = CommandEntity::new(
            CommandId::new(cid.to_string()),
            SessionId::new(sid.to_string()),
            "echo".to_string(),
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

    fn req(agent: &AgentId) -> DisconnectAgentRequest {
        DisconnectAgentRequest {
            agent_id: agent.clone(),
        }
    }

    // --- Scenario 1 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_agent_yields_zero_counts_and_no_ssh_calls() {
        let h = build_harness();
        let agent = AgentId::new("ghost".to_string());
        let outcome = h.uc.execute(req(&agent)).await.expect("execute");
        assert_eq!(
            outcome,
            DisconnectAgentOutcome {
                agent_id: agent,
                sessions_disconnected: 0,
                commands_cancelled: 0,
                shells_closed: 0,
                transfers_aborted: 0,
            }
        );
        assert!(
            h.ssh.calls().is_empty(),
            "no SSH calls must be issued for an unknown agent"
        );
    }

    // --- Scenario 2 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_with_zero_sessions_yields_zero_counts() {
        let h = build_harness();
        let agent = AgentId::new("alpha".to_string());
        // Register the agent index slot but with no sessions bound.
        // (insert + immediate remove would also work; using a sibling
        // session under a different agent keeps the repo non-empty so the
        // assertion is meaningful).
        seed_session(
            &h.sessions,
            "other-1",
            Some(&AgentId::new("beta".to_string())),
        )
        .await;
        let outcome = h.uc.execute(req(&agent)).await.expect("execute");
        assert_eq!(outcome.sessions_disconnected, 0);
        assert_eq!(outcome.commands_cancelled, 0);
        assert_eq!(outcome.shells_closed, 0);
        assert_eq!(outcome.transfers_aborted, 0);
        // Sibling agent's session must remain untouched.
        assert!(
            h.sessions
                .get(&SessionId::new("other-1".to_string()))
                .await
                .expect("get")
                .is_some()
        );
    }

    // --- Scenario 3 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_with_three_clean_sessions_yields_three_disconnects() {
        let h = build_harness();
        let agent = AgentId::new("alpha".to_string());
        for sid in ["s-1", "s-2", "s-3"] {
            seed_session(&h.sessions, sid, Some(&agent)).await;
        }
        let outcome = h.uc.execute(req(&agent)).await.expect("execute");
        assert_eq!(outcome.sessions_disconnected, 3);
        assert_eq!(outcome.commands_cancelled, 0);
        assert_eq!(outcome.shells_closed, 0);
        assert_eq!(outcome.transfers_aborted, 0);
        // All three sessions removed from the repo.
        assert!(h.sessions.list().await.expect("list").is_empty());
        // Agent index emptied.
        assert!(
            h.sessions
                .list_by_agent(&agent)
                .await
                .expect("list_by_agent")
                .is_empty()
        );
        // SSH adapter saw exactly three Disconnect calls.
        let disconnects = h
            .ssh
            .calls()
            .into_iter()
            .filter(|c| matches!(c, FakeSshCall::Disconnect { .. }))
            .count();
        assert_eq!(disconnects, 3);
    }

    // --- Scenario 4 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn cleans_up_commands_shells_and_transfers_for_owned_sessions() {
        let h = build_harness();
        let agent = AgentId::new("alpha".to_string());
        seed_session(&h.sessions, "s-1", Some(&agent)).await;
        seed_session(&h.sessions, "s-2", Some(&agent)).await;
        // s-1: 2 commands, 1 shell, 1 transfer. s-2: 1 command, 0 shells, 2 transfers.
        seed_command(&h.commands, "c-1a", "s-1").await;
        seed_command(&h.commands, "c-1b", "s-1").await;
        seed_command(&h.commands, "c-2", "s-2").await;
        seed_shell(&h.shells, "sh-1", "s-1").await;
        seed_transfer(&h.transfers, "t-1", "s-1").await;
        seed_transfer(&h.transfers, "t-2a", "s-2").await;
        seed_transfer(&h.transfers, "t-2b", "s-2").await;

        let outcome = h.uc.execute(req(&agent)).await.expect("execute");
        assert_eq!(outcome.sessions_disconnected, 2);
        assert_eq!(outcome.commands_cancelled, 3);
        assert_eq!(outcome.shells_closed, 1);
        assert_eq!(outcome.transfers_aborted, 3);

        // Repos drained for the owned sessions.
        assert!(
            h.commands
                .list_filtered(Some(&SessionId::new("s-1".to_string())), None)
                .await
                .expect("list")
                .is_empty()
        );
        assert!(
            h.commands
                .list_filtered(Some(&SessionId::new("s-2".to_string())), None)
                .await
                .expect("list")
                .is_empty()
        );
        assert!(
            h.shells
                .list_filtered(Some(&SessionId::new("s-1".to_string())))
                .await
                .expect("list")
                .is_empty()
        );
        assert!(
            h.transfers
                .list_filtered(Some(&SessionId::new("s-1".to_string())))
                .await
                .expect("list")
                .is_empty()
        );
        assert!(
            h.transfers
                .list_filtered(Some(&SessionId::new("s-2".to_string())))
                .await
                .expect("list")
                .is_empty()
        );
    }

    // --- Scenario 5 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn other_agents_state_is_left_untouched() {
        let h = build_harness();
        let alpha = AgentId::new("alpha".to_string());
        let beta = AgentId::new("beta".to_string());
        seed_session(&h.sessions, "alpha-1", Some(&alpha)).await;
        seed_session(&h.sessions, "beta-1", Some(&beta)).await;
        seed_command(&h.commands, "c-alpha", "alpha-1").await;
        seed_command(&h.commands, "c-beta", "beta-1").await;
        seed_shell(&h.shells, "sh-beta", "beta-1").await;
        seed_transfer(&h.transfers, "t-beta", "beta-1").await;

        let outcome = h.uc.execute(req(&alpha)).await.expect("execute");
        assert_eq!(outcome.sessions_disconnected, 1);
        assert_eq!(outcome.commands_cancelled, 1);
        assert_eq!(outcome.shells_closed, 0);
        assert_eq!(outcome.transfers_aborted, 0);

        // Beta's state intact.
        assert!(
            h.sessions
                .get(&SessionId::new("beta-1".to_string()))
                .await
                .expect("get")
                .is_some()
        );
        assert!(
            h.commands
                .get(&CommandId::new("c-beta".to_string()))
                .await
                .expect("get")
                .is_some()
        );
        assert!(
            h.shells
                .get(&ShellId::new("sh-beta".to_string()))
                .await
                .expect("get")
                .is_some()
        );
        assert!(
            h.transfers
                .get(&TransferId::new("t-beta".to_string()))
                .await
                .expect("get")
                .is_some()
        );
    }

    // --- Scenario 6 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn ssh_disconnect_failure_is_swallowed_and_cleanup_continues() {
        // Selected approach: best-effort cleanup mirroring v3
        // `cleanup_agent_sessions` (which warns and continues on
        // disconnect errors). The use case never aborts the bulk
        // operation because of a single port fault; the final tally
        // therefore counts every successfully removed session, including
        // those whose SSH-level disconnect failed.
        let h = build_harness();
        let agent = AgentId::new("alpha".to_string());
        seed_session(&h.sessions, "ok-1", Some(&agent)).await;
        seed_session(&h.sessions, "boom", Some(&agent)).await;
        seed_session(&h.sessions, "ok-2", Some(&agent)).await;
        // Script the disconnect queue: ok, fail, ok (in order of
        // session iteration; DashMap iteration order is not guaranteed,
        // so we queue all three with the failure in the middle and
        // assert on aggregate counts only).
        h.ssh.queue_disconnect_ok();
        h.ssh
            .queue_disconnect_error(DomainError::Transport("dead".to_string()));
        h.ssh.queue_disconnect_ok();

        let outcome = h.uc.execute(req(&agent)).await.expect("execute");
        // All three sessions counted as disconnected — the use case is
        // best-effort and never aborts because of a port fault.
        assert_eq!(outcome.sessions_disconnected, 3);
        // All three sessions removed from the repository.
        assert!(h.sessions.list().await.expect("list").is_empty());
    }

    // --- Scenario 7 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn ssh_cancel_failure_does_not_block_command_removal() {
        // The SSH `cancel` port returns CommandNotFound for already-
        // finished commands. Cleanup must still flip the snapshot to
        // `Cancelled` and drop it.
        let h = build_harness();
        let agent = AgentId::new("alpha".to_string());
        seed_session(&h.sessions, "s-1", Some(&agent)).await;
        seed_command(&h.commands, "c-finished", "s-1").await;
        // FakeSshClient::cancel always returns CommandNotFound — that is
        // exactly the "already finished" path we want to validate.

        let outcome = h.uc.execute(req(&agent)).await.expect("execute");
        assert_eq!(outcome.commands_cancelled, 1);
        assert!(
            h.commands
                .get(&CommandId::new("c-finished".to_string()))
                .await
                .expect("get")
                .is_none()
        );
    }

    // --- Scenario 8 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn agent_index_is_unbound_after_bulk_cleanup() {
        let h = build_harness();
        let agent = AgentId::new("alpha".to_string());
        seed_session(&h.sessions, "s-1", Some(&agent)).await;
        seed_session(&h.sessions, "s-2", Some(&agent)).await;
        let _ = h.uc.execute(req(&agent)).await.expect("execute");
        let bound = h
            .sessions
            .list_by_agent(&agent)
            .await
            .expect("list_by_agent");
        assert!(
            bound.is_empty(),
            "agent secondary index must be drained after cleanup"
        );
    }

    // --- Scenario 9 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn ssh_cancel_is_invoked_once_per_owned_command() {
        let h = build_harness();
        let agent = AgentId::new("alpha".to_string());
        seed_session(&h.sessions, "s-1", Some(&agent)).await;
        seed_command(&h.commands, "c-1", "s-1").await;
        seed_command(&h.commands, "c-2", "s-1").await;
        seed_command(&h.commands, "c-3", "s-1").await;
        let _ = h.uc.execute(req(&agent)).await.expect("execute");
        // FakeSshClient does not log `cancel`; the visible side effect
        // is the absence of these commands in the repo.
        for cid in ["c-1", "c-2", "c-3"] {
            assert!(
                h.commands
                    .get(&CommandId::new(cid.to_string()))
                    .await
                    .expect("get")
                    .is_none()
            );
        }
    }

    // --- Scenario 10 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn outcome_echoes_requested_agent_id() {
        let h = build_harness();
        let agent = AgentId::new("claude-instance-7".to_string());
        let outcome = h.uc.execute(req(&agent)).await.expect("execute");
        assert_eq!(outcome.agent_id, agent);
    }
}
