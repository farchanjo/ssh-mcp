//! Use case: tear down a single interactive shell.
//!
//! Translates the v3 `ssh_shell_close_impl` (from `src/mcp/tools/shell.rs`)
//! into a hexagonal use case orchestrating two ports:
//!
//! 1. [`ShellRepository::get`] resolves the target shell. A missing id
//!    short-circuits with [`DomainError::ShellNotFound`] before any side
//!    effect runs (mirrors v3's `SHELL_NOT_FOUND` early return so the inbound
//!    adapter can render the canonical
//!    `SSH_SHELL_CLOSE: ERROR / REASON: [SHELL_NOT_FOUND]` block).
//! 2. [`SshClientPort::close_shell`] tears down the russh PTY channel and
//!    cancels the reader / writer tasks owned by the SSH adapter. Errors are
//!    propagated to the caller without touching the repository so a transient
//!    transport failure leaves the entry in place for a retry.
//! 3. [`ShellRepository::remove`] purges the entity once the SSH-side
//!    teardown succeeded.
//!
//! Calling the use case twice on the same id is therefore safe: the second
//! call resolves the now-absent entry as [`DomainError::ShellNotFound`].
//!
//! # Concurrency contract
//!
//! Every `.await` happens outside any repository / fake guard — the use case
//! clones small handles ([`Arc`]) into local bindings before entering await
//! points. The repository port returns owned values so no shard-spanning
//! borrows escape.

use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::ids::ShellId;
use crate::ports::lifecycle_policy::LifecyclePolicyPort;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::ssh_client::SshClientPort;
use crate::ports::subscriber_registry::ResourceKind;

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct CloseShellRequest {
    /// Shell to tear down.
    pub shell_id: ShellId,
}

/// Outcome surfaced to the inbound adapter on a successful tear-down.
///
/// The id echoes the request so the rmcp tool wrapper can render
/// `SHELL_ID: <id>` without threading the request through the response
/// builder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseShellOutcome {
    /// Shell that was torn down.
    pub shell_id: ShellId,
}

/// Close-shell use case generic over its two adapter dependencies. The
/// composition root pins concrete adapter types per binary; tests inject
/// fakes via [`crate::adapters`].
#[derive(Debug)]
pub struct CloseShellUseCase<S, ShR>
where
    S: SshClientPort + Send + Sync,
    ShR: ShellRepository + Send + Sync,
{
    ssh: Arc<S>,
    shells: Arc<ShR>,
    /// v5 lifecycle: force-close the lifecycle entry so the cascade
    /// coordinator decrements the owning session's active-resource
    /// count.
    lifecycle: Arc<dyn LifecyclePolicyPort>,
}

impl<S, ShR> CloseShellUseCase<S, ShR>
where
    S: SshClientPort + Send + Sync,
    ShR: ShellRepository + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(
        ssh: Arc<S>,
        shells: Arc<ShR>,
        lifecycle: Arc<dyn LifecyclePolicyPort>,
    ) -> Self {
        Self {
            ssh,
            shells,
            lifecycle,
        }
    }

    /// Drive the close orchestration. See module-level docs for the
    /// step-by-step semantics.
    ///
    /// # Errors
    ///
    /// - [`DomainError::ShellNotFound`] when the shell id is unknown to the
    ///   repository at probe time; no side effect runs.
    /// - Any [`DomainError`] surfaced by [`SshClientPort::close_shell`]; the
    ///   repository entry is left untouched so the caller may safely retry
    ///   once the underlying transport recovers.
    /// - Any [`DomainError`] surfaced by [`ShellRepository::remove`].
    pub async fn execute(&self, req: CloseShellRequest) -> Result<CloseShellOutcome, DomainError> {
        let shell_id = req.shell_id;
        if self.shells.get(&shell_id).await?.is_none() {
            return Err(DomainError::ShellNotFound(shell_id));
        }
        self.ssh.close_shell(&shell_id).await?;
        self.shells.remove(&shell_id).await?;
        // v5 lifecycle: drive the resource into the Closed state so
        // the cascade coordinator can debit the session refcount.
        self.lifecycle
            .force_close(ResourceKind::Shell, shell_id.as_str())?;
        Ok(CloseShellOutcome { shell_id })
    }
}

#[cfg(test)]
mod tests {
    use super::{CloseShellRequest, CloseShellUseCase};
    use crate::adapters::clock::system::SystemClock;
    use crate::adapters::lifecycle::cascade::CascadeCoordinator;
    use crate::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::adapters::ssh::fake::{FakeSshCall, FakeSshClient};
    use crate::domain::error::DomainError;
    use crate::domain::ids::{SessionId, ShellId};
    use crate::domain::shell::{ShellEntity, ShellTerminal};
    use crate::ports::lifecycle_policy::LifecyclePolicyPort;
    use crate::ports::shell_repo::ShellRepository;
    use chrono::Utc;
    use std::sync::Arc;
    use std::time::Duration;

    type UseCaseUnderTest = CloseShellUseCase<FakeSshClient, DashMapShellRepo>;

    struct Wired {
        uc: UseCaseUnderTest,
        ssh: Arc<FakeSshClient>,
        shells: Arc<DashMapShellRepo>,
    }

    fn build_wired() -> Wired {
        let ssh = Arc::new(FakeSshClient::new());
        let shells = Arc::new(DashMapShellRepo::new());
        let cascade = CascadeCoordinator::new();
        let clock = Arc::new(SystemClock);
        let lifecycle: Arc<dyn LifecyclePolicyPort> =
            RefcountedLifecycleAdapter::new(cascade, clock);
        let uc = CloseShellUseCase::new(Arc::clone(&ssh), Arc::clone(&shells), lifecycle);
        Wired { uc, ssh, shells }
    }

    fn sample_shell(id: &str, session: &str) -> ShellEntity {
        ShellEntity::new(
            ShellId::new(id.to_string()),
            SessionId::new(session.to_string()),
            ShellTerminal::new("xterm-256color".to_string(), 80, 24),
            Utc::now(),
            Duration::from_secs(600),
            10 * 1024 * 1024,
        )
    }

    fn seed_shell(repo: &DashMapShellRepo, id: &str, session: &str) -> ShellEntity {
        let entity = sample_shell(id, session);
        let to_insert = entity.clone();
        let repo_clone = repo.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                repo_clone.insert(to_insert).await.expect("seed insert");
            });
        });
        entity
    }

    // --- Scenario 1 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn happy_path_closes_shell_and_removes_repository_entry() {
        let wired = build_wired();
        let seeded = seed_shell(&wired.shells, "sh-1", "sess-1");
        let outcome = wired
            .uc
            .execute(CloseShellRequest {
                shell_id: seeded.id.clone(),
            })
            .await
            .expect("close ok");
        assert_eq!(outcome.shell_id, seeded.id);
        assert!(
            wired.shells.get(&seeded.id).await.expect("get").is_none(),
            "shell entry must be removed after close"
        );
        let calls = wired.ssh.calls();
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            FakeSshCall::CloseShell { shell_id } => assert_eq!(shell_id, &seeded.id),
            FakeSshCall::Connect { .. }
            | FakeSshCall::Disconnect { .. }
            | FakeSshCall::Execute { .. }
            | FakeSshCall::ExecuteAsync { .. }
            | FakeSshCall::HealthCheck { .. }
            | FakeSshCall::OpenShell { .. }
            | FakeSshCall::WriteShell { .. } => {
                panic!("expected CloseShell, got {:?}", calls[0])
            }
        }
    }

    // --- Scenario 2 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn shell_not_found_short_circuits_without_side_effect() {
        let wired = build_wired();
        let unknown = ShellId::new("ghost".to_string());
        let err = wired
            .uc
            .execute(CloseShellRequest {
                shell_id: unknown.clone(),
            })
            .await
            .expect_err("close must fail");
        assert_eq!(err, DomainError::ShellNotFound(unknown));
        assert_eq!(
            wired.ssh.call_count(),
            0,
            "missing-shell branch must not invoke the SSH port"
        );
    }

    // --- Scenario 3 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn ssh_close_failure_propagates_and_leaves_repository_intact() {
        let wired = build_wired();
        let seeded = seed_shell(&wired.shells, "sh-1", "sess-1");
        wired
            .ssh
            .queue_close_shell_error(DomainError::Transport("eof".to_string()));
        let err = wired
            .uc
            .execute(CloseShellRequest {
                shell_id: seeded.id.clone(),
            })
            .await
            .expect_err("close must surface transport error");
        assert_eq!(err, DomainError::Transport("eof".to_string()));
        // Repository entry survives so the caller can safely retry.
        assert!(
            wired.shells.get(&seeded.id).await.expect("get").is_some(),
            "transport failure must not orphan the repository entry"
        );
    }

    // --- Scenario 4 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn idempotent_second_call_yields_shell_not_found() {
        let wired = build_wired();
        let seeded = seed_shell(&wired.shells, "sh-1", "sess-1");
        wired
            .uc
            .execute(CloseShellRequest {
                shell_id: seeded.id.clone(),
            })
            .await
            .expect("first close");
        let err = wired
            .uc
            .execute(CloseShellRequest {
                shell_id: seeded.id.clone(),
            })
            .await
            .expect_err("second call must fail");
        assert_eq!(err, DomainError::ShellNotFound(seeded.id));
    }

    // --- Scenario 5 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn outcome_shell_id_matches_request_shell_id() {
        let wired = build_wired();
        let seeded = seed_shell(&wired.shells, "sh-echo", "sess-1");
        let outcome = wired
            .uc
            .execute(CloseShellRequest {
                shell_id: seeded.id.clone(),
            })
            .await
            .expect("close ok");
        assert_eq!(outcome.shell_id, seeded.id);
    }

    // --- Scenario 6 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn other_shells_are_left_untouched_on_close() {
        let wired = build_wired();
        let target = seed_shell(&wired.shells, "sh-target", "sess-1");
        let other = seed_shell(&wired.shells, "sh-other", "sess-1");
        wired
            .uc
            .execute(CloseShellRequest {
                shell_id: target.id.clone(),
            })
            .await
            .expect("close target");
        assert!(
            wired.shells.get(&target.id).await.expect("get").is_none(),
            "target shell must be removed"
        );
        let survivor = wired
            .shells
            .get(&other.id)
            .await
            .expect("get other")
            .expect("other shell must survive");
        assert_eq!(survivor.id, other.id);
    }

    // --- Scenario 7 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn ssh_port_receives_exactly_the_requested_shell_id() {
        let wired = build_wired();
        let seeded = seed_shell(&wired.shells, "sh-route", "sess-1");
        wired
            .uc
            .execute(CloseShellRequest {
                shell_id: seeded.id.clone(),
            })
            .await
            .expect("close ok");
        let calls = wired.ssh.calls();
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            FakeSshCall::CloseShell { shell_id } => assert_eq!(shell_id, &seeded.id),
            FakeSshCall::Connect { .. }
            | FakeSshCall::Disconnect { .. }
            | FakeSshCall::Execute { .. }
            | FakeSshCall::ExecuteAsync { .. }
            | FakeSshCall::HealthCheck { .. }
            | FakeSshCall::OpenShell { .. }
            | FakeSshCall::WriteShell { .. } => {
                panic!("expected CloseShell, got {:?}", calls[0])
            }
        }
    }

    // --- Scenario 8 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn ssh_shell_not_found_failure_is_surfaced_to_caller() {
        let wired = build_wired();
        let seeded = seed_shell(&wired.shells, "sh-1", "sess-1");
        wired
            .ssh
            .queue_close_shell_error(DomainError::ShellNotFound(seeded.id.clone()));
        let err = wired
            .uc
            .execute(CloseShellRequest {
                shell_id: seeded.id.clone(),
            })
            .await
            .expect_err("close must propagate ShellNotFound from the SSH port");
        assert_eq!(err, DomainError::ShellNotFound(seeded.id.clone()));
        // Repository entry remains so the inbound adapter can decide whether
        // to purge it via a follow-up disconnect.
        assert!(wired.shells.get(&seeded.id).await.expect("get").is_some());
    }
}
