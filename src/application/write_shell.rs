//! Use case: write raw bytes to an interactive PTY shell.
//!
//! Translates the v3 `ssh_shell_write_impl` (from
//! `src/mcp/tools/shell.rs::ssh_shell_write_impl`) into a hexagonal use case
//! orchestrating ports. No rmcp / russh / tokio types leak into the public
//! surface.
//!
//! # Orchestration shape
//!
//! 1. Resolve the shell snapshot via [`ShellRepository::get`]. A missing
//!    id short-circuits with [`DomainError::ShellNotFound`] before any
//!    side effect runs.
//! 2. Forward the byte buffer to [`SshClientPort::write_shell`]. The
//!    russh adapter returns the number of bytes the writer task accepted
//!    (always equal to `bytes.len()` in practice; the use case echoes
//!    whatever the adapter reports).
//! 3. Stamp the activity touchpoint with [`ClockPort::utc_now`] and
//!    re-emit the shell entity through [`ShellRepository::update`]. The
//!    v3 implementation tracked activity through a separate runtime
//!    atomic carried by `ShellInfo`; v4 routes the touch through the
//!    repository so the persistence layer remains the single source of
//!    truth for shell lifecycle bookkeeping. The repository accepts the
//!    re-emitted entity unchanged today; once the domain entity grows a
//!    dedicated `last_activity_at` field the touch will land on the
//!    stored snapshot without any change to this use case.
//! 4. Return [`WriteShellOutcome`] carrying the byte count and the RFC
//!    3339 wall-clock activity stamp the rmcp tool wrapper renders back
//!    to the caller.
//!
//! # Concurrency contract
//!
//! Every `.await` happens outside any repository / fake guard — the use
//! case clones small handles ([`Arc`]) into local bindings before
//! entering await points, exactly like the H10 canary.

use std::sync::Arc;

use bytes::Bytes;

use crate::domain::error::DomainError;
use crate::domain::ids::ShellId;
use crate::ports::clock::ClockPort;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::ssh_client::SshClientPort;

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct WriteShellRequest {
    /// Target shell.
    pub shell_id: ShellId,
    /// Bytes to forward to the PTY writer task.
    pub bytes: Bytes,
}

/// Outbound DTO surfacing every observable result the rmcp tool wrapper
/// must render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteShellOutcome {
    /// Echo of the targeted shell id so the inbound adapter can render
    /// the response without retaining the original DTO.
    pub shell_id: ShellId,
    /// Number of bytes the SSH writer task accepted. The russh adapter
    /// reports `bytes.len()` on success; alternative adapters (e.g. an
    /// in-memory test fake) may report fewer when scripted to do so.
    pub bytes_written: usize,
    /// RFC 3339 wall-clock timestamp at which the activity touch was
    /// recorded. Mirrors the v3 `touch_activity` callback that bumps the
    /// shell's inactivity TTL countdown.
    pub last_activity_at: String,
}

/// Write-shell use case generic over every adapter dependency. The
/// composition root pins concrete adapter types per binary; tests inject
/// fakes via [`crate::adapters`].
#[derive(Debug)]
pub struct WriteShellUseCase<S, ShR, C>
where
    S: SshClientPort + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    C: ClockPort + Send + Sync,
{
    ssh: Arc<S>,
    shells: Arc<ShR>,
    clock: Arc<C>,
}

impl<S, ShR, C> WriteShellUseCase<S, ShR, C>
where
    S: SshClientPort + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    C: ClockPort + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(ssh: Arc<S>, shells: Arc<ShR>, clock: Arc<C>) -> Self {
        Self { ssh, shells, clock }
    }

    /// Drive the write orchestration. See module-level docs for the
    /// step-by-step semantics.
    ///
    /// # Errors
    ///
    /// - [`DomainError::ShellNotFound`] when the shell id is unknown to
    ///   the repository.
    /// - Any [`DomainError`] surfaced by [`SshClientPort::write_shell`]
    ///   (most commonly [`DomainError::Transport`] when the writer task
    ///   has shut down).
    /// - Any [`DomainError`] surfaced by [`ShellRepository::update`]
    ///   when the activity touch fails to persist; the SSH-side write
    ///   has already succeeded at that point, so callers should treat
    ///   this branch as a partial success.
    pub async fn execute(&self, req: WriteShellRequest) -> Result<WriteShellOutcome, DomainError> {
        let snapshot = self
            .shells
            .get(&req.shell_id)
            .await?
            .ok_or_else(|| DomainError::ShellNotFound(req.shell_id.clone()))?;

        let bytes_written = self
            .ssh
            .write_shell(&req.shell_id, req.bytes.clone())
            .await?;

        let now = self.clock.utc_now();
        self.shells.update(snapshot).await?;

        Ok(WriteShellOutcome {
            shell_id: req.shell_id,
            bytes_written,
            last_activity_at: now.to_rfc3339(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{WriteShellOutcome, WriteShellRequest, WriteShellUseCase};
    use crate::adapters::clock::fake::FakeClock;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::adapters::ssh::fake::{FakeSshCall, FakeSshClient};
    use crate::domain::error::DomainError;
    use crate::domain::ids::{SessionId, ShellId};
    use crate::domain::shell::{ShellEntity, ShellTerminal};
    use crate::ports::clock::ClockPort;
    use crate::ports::shell_repo::ShellRepository;
    use bytes::Bytes;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use std::time::Duration;

    type UseCaseUnderTest = WriteShellUseCase<FakeSshClient, DashMapShellRepo, FakeClock>;

    fn build_use_case() -> (
        UseCaseUnderTest,
        Arc<FakeSshClient>,
        Arc<DashMapShellRepo>,
        Arc<FakeClock>,
    ) {
        let ssh = Arc::new(FakeSshClient::new());
        let shells = Arc::new(DashMapShellRepo::new());
        // 2026-05-02 12:00:00 UTC ≈ 1_777_982_400_000 ms since epoch.
        let clock = Arc::new(FakeClock::new(1_777_982_400_000_u64));
        let uc = WriteShellUseCase::new(Arc::clone(&ssh), Arc::clone(&shells), Arc::clone(&clock));
        (uc, ssh, shells, clock)
    }

    fn sample_terminal() -> ShellTerminal {
        ShellTerminal::new("xterm-256color".to_string(), 80, 24)
    }

    fn sample_entity(shell: &str, session: &str) -> ShellEntity {
        ShellEntity::new(
            ShellId::new(shell.to_string()),
            SessionId::new(session.to_string()),
            sample_terminal(),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            Duration::from_secs(600),
            10_u64.saturating_mul(1024).saturating_mul(1024),
        )
    }

    async fn seed(repo: &DashMapShellRepo, shell: &str, session: &str) -> ShellEntity {
        let entity = sample_entity(shell, session);
        repo.insert(entity.clone()).await.expect("seed insert");
        entity
    }

    fn req(shell: &str, payload: &'static [u8]) -> WriteShellRequest {
        WriteShellRequest {
            shell_id: ShellId::new(shell.to_string()),
            bytes: Bytes::from_static(payload),
        }
    }

    // --- Scenario 1 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn happy_path_returns_bytes_written_and_records_write_call() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed(&shells, "sh-1", "s-1").await;
        let outcome = uc
            .execute(req("sh-1", b"ls -la\n"))
            .await
            .expect("write_shell");
        let WriteShellOutcome {
            shell_id,
            bytes_written,
            last_activity_at: _,
        } = outcome;
        assert_eq!(shell_id.as_str(), "sh-1");
        assert_eq!(bytes_written, b"ls -la\n".len());
        let calls = ssh.calls();
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            FakeSshCall::WriteShell {
                shell_id: recorded_id,
                bytes,
            } => {
                assert_eq!(recorded_id.as_str(), "sh-1");
                assert_eq!(bytes.as_ref(), b"ls -la\n");
            }
            FakeSshCall::Connect { .. }
            | FakeSshCall::Disconnect { .. }
            | FakeSshCall::Execute { .. }
            | FakeSshCall::ExecuteAsync { .. }
            | FakeSshCall::HealthCheck { .. }
            | FakeSshCall::OpenShell { .. }
            | FakeSshCall::CloseShell { .. } => {
                panic!("expected WriteShell, got {:?}", calls[0])
            }
        }
    }

    // --- Scenario 2 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_shell_id_returns_shell_not_found_without_ssh_call() {
        let (uc, ssh, _shells, _clock) = build_use_case();
        let err = uc
            .execute(req("ghost", b"x"))
            .await
            .expect_err("expected ShellNotFound");
        assert_eq!(
            err,
            DomainError::ShellNotFound(ShellId::new("ghost".to_string()))
        );
        assert_eq!(
            ssh.call_count(),
            0,
            "no SSH write must be issued for an unknown shell id"
        );
    }

    // --- Scenario 3 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_bytes_payload_is_accepted_and_yields_zero_bytes_written() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed(&shells, "sh-1", "s-1").await;
        let request = WriteShellRequest {
            shell_id: ShellId::new("sh-1".to_string()),
            bytes: Bytes::new(),
        };
        let outcome = uc.execute(request).await.expect("empty write_shell");
        assert_eq!(outcome.bytes_written, 0);
        assert_eq!(outcome.shell_id.as_str(), "sh-1");
        let calls = ssh.calls();
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            FakeSshCall::WriteShell { bytes, .. } => assert!(bytes.is_empty()),
            FakeSshCall::Connect { .. }
            | FakeSshCall::Disconnect { .. }
            | FakeSshCall::Execute { .. }
            | FakeSshCall::ExecuteAsync { .. }
            | FakeSshCall::HealthCheck { .. }
            | FakeSshCall::OpenShell { .. }
            | FakeSshCall::CloseShell { .. } => {
                panic!("expected WriteShell, got {:?}", calls[0])
            }
        }
    }

    // --- Scenario 4 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn ssh_write_failure_is_surfaced_to_the_caller() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed(&shells, "sh-1", "s-1").await;
        ssh.queue_write_shell_error(DomainError::Transport("writer task closed".to_string()));
        let err = uc
            .execute(req("sh-1", b"x"))
            .await
            .expect_err("queued error must propagate");
        assert_eq!(
            err,
            DomainError::Transport("writer task closed".to_string())
        );
    }

    // --- Scenario 5 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn activity_timestamp_is_taken_from_clock_and_rendered_as_rfc3339() {
        let (uc, _ssh, shells, clock) = build_use_case();
        let _ = seed(&shells, "sh-1", "s-1").await;
        let outcome = uc
            .execute(req("sh-1", b"echo hi\n"))
            .await
            .expect("write_shell");
        let expected = clock.utc_now().to_rfc3339();
        assert_eq!(
            outcome.last_activity_at, expected,
            "last_activity_at must mirror ClockPort::utc_now (frozen FakeClock)"
        );
        assert!(
            outcome.last_activity_at.contains('T'),
            "RFC 3339 stamp must contain 'T'; got {}",
            outcome.last_activity_at
        );
    }

    // --- Scenario 6 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn shell_remains_present_in_repo_after_activity_touch() {
        let (uc, _ssh, shells, _clock) = build_use_case();
        let seeded = seed(&shells, "sh-1", "s-1").await;
        uc.execute(req("sh-1", b"pwd\n"))
            .await
            .expect("write_shell");
        let after = shells
            .get(&ShellId::new("sh-1".to_string()))
            .await
            .expect("get")
            .expect("entity must still be present after the activity touch");
        assert_eq!(after, seeded, "activity touch must preserve every field");
    }

    // --- Scenario 7 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn write_call_is_issued_after_repo_lookup() {
        // Ensures the use case validates the shell exists before
        // forwarding any bytes to the SSH adapter — the inverse order
        // would leak writes for stale ids.
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed(&shells, "sh-1", "s-1").await;
        // First call: unknown id -> no SSH write recorded.
        let _ = uc.execute(req("ghost", b"x")).await;
        assert_eq!(ssh.call_count(), 0);
        // Second call: known id -> exactly one SSH write recorded.
        uc.execute(req("sh-1", b"hi"))
            .await
            .expect("known id must succeed");
        assert_eq!(ssh.call_count(), 1);
    }

    // --- Scenario 8 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn bytes_payload_is_forwarded_verbatim_without_modification() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed(&shells, "sh-1", "s-1").await;
        // Mix of printable ASCII, a control character (Ctrl+C), and a
        // multi-byte UTF-8 sequence to confirm the use case never
        // re-encodes the buffer.
        let payload = Bytes::from_static(b"echo \xC3\xA9\x03\n");
        let request = WriteShellRequest {
            shell_id: ShellId::new("sh-1".to_string()),
            bytes: payload.clone(),
        };
        let outcome = uc.execute(request).await.expect("write_shell");
        assert_eq!(outcome.bytes_written, payload.len());
        match &ssh.calls()[0] {
            FakeSshCall::WriteShell { bytes, .. } => {
                assert_eq!(
                    bytes.as_ref(),
                    payload.as_ref(),
                    "payload must be forwarded byte-for-byte"
                );
            }
            FakeSshCall::Connect { .. }
            | FakeSshCall::Disconnect { .. }
            | FakeSshCall::Execute { .. }
            | FakeSshCall::ExecuteAsync { .. }
            | FakeSshCall::HealthCheck { .. }
            | FakeSshCall::OpenShell { .. }
            | FakeSshCall::CloseShell { .. } => {
                panic!("expected WriteShell, got {:?}", &ssh.calls()[0])
            }
        }
    }

    // --- Scenario 9 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn successive_writes_each_record_their_own_ssh_call() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed(&shells, "sh-1", "s-1").await;
        for payload in [&b"one\n"[..], &b"two\n"[..], &b"three\n"[..]] {
            let request = WriteShellRequest {
                shell_id: ShellId::new("sh-1".to_string()),
                bytes: Bytes::copy_from_slice(payload),
            };
            uc.execute(request).await.expect("write_shell");
        }
        assert_eq!(
            ssh.call_count(),
            3,
            "each successive write must record its own SSH call"
        );
        let writes = ssh
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                FakeSshCall::WriteShell { bytes, .. } => Some(bytes),
                FakeSshCall::Connect { .. }
                | FakeSshCall::Disconnect { .. }
                | FakeSshCall::Execute { .. }
                | FakeSshCall::ExecuteAsync { .. }
                | FakeSshCall::HealthCheck { .. }
                | FakeSshCall::OpenShell { .. }
                | FakeSshCall::CloseShell { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(writes.len(), 3);
        assert_eq!(writes[0].as_ref(), b"one\n");
        assert_eq!(writes[1].as_ref(), b"two\n");
        assert_eq!(writes[2].as_ref(), b"three\n");
    }
}
