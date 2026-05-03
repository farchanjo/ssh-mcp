//! Use case: encode a semantic keystroke and stream it to an interactive
//! PTY shell.
//!
//! Mirrors the v3 `ssh_shell_send_key_impl`
//! (see the legacy v3 shell-tools module) translated into the hexagonal stack:
//! the request DTO is shaped after the rmcp tool arguments without any
//! `Option<bool>` clutter, and every side-effect is routed through ports.
//!
//! # Orchestration shape
//!
//! 1. Validate `repeat ∈ 1..=64` — falls into
//!    [`DomainError::InvalidArgument`] when the caller sends `0` or a value
//!    larger than the v3 [`MAX_SEND_KEY_REPEAT`] cap.
//! 2. Confirm the shell exists via [`ShellRepository::get`] — this also
//!    surfaces `ShellNotFound` before any encoding work.
//! 3. Encode the key + modifiers via [`ShellKey::encode`]; rejects
//!    incompatible modifier combinations as `DomainError::InvalidArgument`
//!    so the inbound adapter can render the same error block as v3
//!    (`MODIFIER_NOT_ALLOWED`).
//! 4. Materialise the full payload by repeating the encoded byte slice
//!    `repeat` times, then write it as a single
//!    [`SshClientPort::write_shell`] call. Submitting one batch (rather
//!    than `repeat` discrete writes) keeps the russh channel ordering
//!    deterministic and matches the v3 mpsc semantics under contention.
//! 5. Stamp `sent_at` from [`ClockPort::utc_now`] so the inbound adapter
//!    has a wall-clock anchor for the response/log without reaching back
//!    into the SSH adapter.
//!
//! # Concurrency contract
//!
//! No repository or fake guard crosses an `await` point. The shell lookup
//! returns an owned [`ShellEntity`] before the SSH write fires, and the
//! `Arc<C>` clock handle is dereferenced synchronously.

use std::sync::Arc;

use bytes::Bytes;
use chrono::{DateTime, Utc};

use crate::domain::error::DomainError;
use crate::domain::ids::ShellId;
use crate::domain::keys::{EncodeError, KeyModifiers, ShellKey};
use crate::ports::clock::ClockPort;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::ssh_client::SshClientPort;

/// Hard cap on the per-request `repeat` factor. Mirrors the v3
/// `MAX_SEND_KEY_REPEAT` constant in `src/mcp/tools/shell.rs`.
pub const MAX_SEND_KEY_REPEAT: u8 = 64;

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct SendKeyRequest {
    /// Target shell.
    pub shell_id: ShellId,
    /// Named keystroke to send.
    pub key: ShellKey,
    /// Modifier flags applied on top of [`Self::key`].
    pub modifiers: KeyModifiers,
    /// How many times to send the encoded payload back-to-back. Must lie
    /// in `1..=64`.
    pub repeat: u8,
}

/// Outbound DTO surfacing every observable result the rmcp tool wrapper
/// must render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendKeyOutcome {
    /// Echo of the target shell.
    pub shell_id: ShellId,
    /// Stable label of the encoded key (e.g. `"ctrl_c"`, `"arrow_up"`).
    pub key_label: String,
    /// `+`-joined active modifier label (e.g. `"shift+ctrl"`), `None` when
    /// no modifier was set.
    pub modifier_label: Option<String>,
    /// Echo of the requested repeat factor.
    pub repeat: u8,
    /// Total bytes written to the PTY (`encoded.len() * repeat`).
    pub bytes_sent: usize,
    /// Wall-clock instant at which the use case dispatched the write.
    pub sent_at: DateTime<Utc>,
}

/// Send-key use case generic over every adapter dependency. The composition
/// root pins concrete adapter types per binary; tests inject fakes via
/// [`crate::adapters`].
#[derive(Debug)]
pub struct SendKeyUseCase<S, ShR, C>
where
    S: SshClientPort + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    C: ClockPort + Send + Sync,
{
    ssh: Arc<S>,
    shells: Arc<ShR>,
    clock: Arc<C>,
}

impl<S, ShR, C> SendKeyUseCase<S, ShR, C>
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

    /// Drive the send-key orchestration. See module-level docs for the
    /// step-by-step semantics.
    ///
    /// # Errors
    ///
    /// - [`DomainError::InvalidArgument`] when `repeat` is outside `1..=64`
    ///   or when the modifier combination is rejected by [`ShellKey::encode`].
    /// - [`DomainError::ShellNotFound`] when the shell id is unknown.
    /// - Any [`DomainError`] propagated by [`SshClientPort::write_shell`]
    ///   (most commonly `WriteFailed` or `Transport`).
    pub async fn execute(&self, req: SendKeyRequest) -> Result<SendKeyOutcome, DomainError> {
        let SendKeyRequest {
            shell_id,
            key,
            modifiers,
            repeat,
        } = req;

        validate_repeat(repeat)?;

        let _entity = self
            .shells
            .get(&shell_id)
            .await?
            .ok_or_else(|| DomainError::ShellNotFound(shell_id.clone()))?;

        let encoded = encode_key(key, modifiers)?;
        let payload = build_payload(&encoded, repeat);
        let total_bytes = payload.len();

        let _written = self.ssh.write_shell(&shell_id, payload).await?;

        Ok(SendKeyOutcome {
            shell_id,
            key_label: key.label().to_string(),
            modifier_label: format_modifiers_label(modifiers),
            repeat,
            bytes_sent: total_bytes,
            sent_at: self.clock.utc_now(),
        })
    }
}

/// Reject `repeat` values outside the `1..=64` window. Tags the message
/// with `INVALID_REPEAT:` so the rmcp tool router promotes it to the
/// specific wire code (v4.5).
fn validate_repeat(repeat: u8) -> Result<(), DomainError> {
    if repeat == 0 || repeat > MAX_SEND_KEY_REPEAT {
        return Err(DomainError::InvalidArgument(format!(
            "INVALID_REPEAT: repeat must be between 1 and {MAX_SEND_KEY_REPEAT} inclusive (requested={repeat})"
        )));
    }
    Ok(())
}

/// Encode the key+modifier pair into an owned byte buffer, translating the
/// keys-layer error into a domain-typed one.
fn encode_key(key: ShellKey, modifiers: KeyModifiers) -> Result<Vec<u8>, DomainError> {
    match key.encode(modifiers) {
        Ok(cow) => Ok(cow.into_owned()),
        Err(EncodeError::ModifierNotAllowed {
            key_label,
            requested,
        }) => {
            let detail = format_modifiers_label(requested).unwrap_or_else(|| "(none)".to_string());
            Err(DomainError::InvalidArgument(format!(
                "MODIFIER_NOT_ALLOWED: key '{key_label}' rejects the requested modifier combination (requested={detail})"
            )))
        }
    }
}

/// Materialise a `repeat * encoded.len()` payload buffer.
fn build_payload(encoded: &[u8], repeat: u8) -> Bytes {
    let chunk_len = encoded.len();
    let total = chunk_len.saturating_mul(usize::from(repeat));
    let mut buf = Vec::with_capacity(total);
    for _ in 0..repeat {
        buf.extend_from_slice(encoded);
    }
    Bytes::from(buf)
}

/// Format the active modifiers as a `+`-joined label, or `None` when no
/// modifier is set. Matches the v3 `format_modifiers_label` helper in
/// `src/mcp/tools/shell.rs`.
fn format_modifiers_label(mods: KeyModifiers) -> Option<String> {
    if mods.is_empty() {
        return None;
    }
    let mut parts: Vec<&'static str> = Vec::with_capacity(3);
    if mods.shift {
        parts.push("shift");
    }
    if mods.alt {
        parts.push("alt");
    }
    if mods.ctrl {
        parts.push("ctrl");
    }
    Some(parts.join("+"))
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_SEND_KEY_REPEAT, SendKeyOutcome, SendKeyRequest, SendKeyUseCase, format_modifiers_label,
    };
    use crate::adapters::clock::fake::FakeClock;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::adapters::ssh::fake::{FakeSshCall, FakeSshClient};
    use crate::domain::error::DomainError;
    use crate::domain::ids::{SessionId, ShellId};
    use crate::domain::keys::{KeyModifiers, ShellKey};
    use crate::domain::shell::{ShellEntity, ShellTerminal};
    use crate::ports::clock::ClockPort;
    use crate::ports::shell_repo::ShellRepository;
    use chrono::Utc;
    use std::sync::Arc;
    use std::time::Duration;

    type UseCaseUnderTest = SendKeyUseCase<FakeSshClient, DashMapShellRepo, FakeClock>;

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
        let uc = SendKeyUseCase::new(Arc::clone(&ssh), Arc::clone(&shells), Arc::clone(&clock));
        (uc, ssh, shells, clock)
    }

    fn seed_shell(repo: &DashMapShellRepo, shell_id: &str, session_id: &str) -> ShellEntity {
        let entity = ShellEntity::new(
            ShellId::new(shell_id.to_string()),
            SessionId::new(session_id.to_string()),
            ShellTerminal::new("xterm-256color".to_string(), 80, 24),
            Utc::now(),
            Duration::from_secs(300),
            10_u64.saturating_mul(1024).saturating_mul(1024),
        );
        let blocking = entity.clone();
        let repo_clone = repo.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                repo_clone.insert(blocking).await.expect("seed insert");
            });
        });
        entity
    }

    fn no_mods() -> KeyModifiers {
        KeyModifiers::default()
    }

    fn shift() -> KeyModifiers {
        KeyModifiers {
            shift: true,
            alt: false,
            ctrl: false,
        }
    }

    fn alt() -> KeyModifiers {
        KeyModifiers {
            shift: false,
            alt: true,
            ctrl: false,
        }
    }

    fn shift_ctrl() -> KeyModifiers {
        KeyModifiers {
            shift: true,
            alt: false,
            ctrl: true,
        }
    }

    fn build_req(
        shell_id: &str,
        key: ShellKey,
        modifiers: KeyModifiers,
        repeat: u8,
    ) -> SendKeyRequest {
        SendKeyRequest {
            shell_id: ShellId::new(shell_id.to_string()),
            key,
            modifiers,
            repeat,
        }
    }

    fn outcome_or_panic(result: Result<SendKeyOutcome, DomainError>) -> SendKeyOutcome {
        match result {
            Ok(out) => out,
            Err(err) => panic!("expected SendKeyOutcome, got {err:?}"),
        }
    }

    fn write_shell_call(call: &FakeSshCall) -> (&ShellId, &bytes::Bytes) {
        match call {
            FakeSshCall::WriteShell { shell_id, bytes } => (shell_id, bytes),
            other => panic!("expected WriteShell, got {other:?}"),
        }
    }

    // --- Scenario 1 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn plain_key_ctrl_c_writes_single_byte_payload() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed_shell(&shells, "sh-1", "sess-1");

        let outcome = outcome_or_panic(
            uc.execute(build_req("sh-1", ShellKey::CtrlC, no_mods(), 1))
                .await,
        );

        assert_eq!(outcome.shell_id.as_str(), "sh-1");
        assert_eq!(outcome.key_label, "ctrl_c");
        assert_eq!(outcome.modifier_label, None);
        assert_eq!(outcome.repeat, 1);
        assert_eq!(outcome.bytes_sent, 1);

        let calls = ssh.calls();
        assert_eq!(calls.len(), 1);
        let (shell_id, bytes) = write_shell_call(&calls[0]);
        assert_eq!(shell_id.as_str(), "sh-1");
        assert_eq!(bytes.as_ref(), b"\x03");
    }

    // --- Scenario 2 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn shift_arrow_up_emits_csi_with_modifier_code_two() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed_shell(&shells, "sh-1", "sess-1");

        let outcome = outcome_or_panic(
            uc.execute(build_req("sh-1", ShellKey::ArrowUp, shift(), 1))
                .await,
        );

        assert_eq!(outcome.modifier_label.as_deref(), Some("shift"));
        assert_eq!(outcome.bytes_sent, b"\x1b[1;2A".len());

        let calls = ssh.calls();
        let (_shell_id, bytes) = write_shell_call(&calls[0]);
        assert_eq!(bytes.as_ref(), b"\x1b[1;2A");
    }

    // --- Scenario 3 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn repeat_five_concatenates_payload_in_single_write() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed_shell(&shells, "sh-1", "sess-1");

        let outcome = outcome_or_panic(
            uc.execute(build_req("sh-1", ShellKey::CtrlC, no_mods(), 5))
                .await,
        );

        assert_eq!(outcome.repeat, 5);
        assert_eq!(outcome.bytes_sent, 5);

        // Single batched write — chosen over five discrete writes so the
        // russh channel ordering stays deterministic under contention.
        let calls = ssh.calls();
        assert_eq!(calls.len(), 1);
        let (_shell_id, bytes) = write_shell_call(&calls[0]);
        assert_eq!(bytes.as_ref(), b"\x03\x03\x03\x03\x03");
    }

    // --- Scenario 4 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn repeat_zero_is_rejected_as_invalid_argument() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed_shell(&shells, "sh-1", "sess-1");

        let err = uc
            .execute(build_req("sh-1", ShellKey::CtrlC, no_mods(), 0))
            .await
            .expect_err("repeat=0 must be rejected");
        match err {
            DomainError::InvalidArgument(msg) => {
                assert!(
                    msg.starts_with("INVALID_REPEAT:"),
                    "expected INVALID_REPEAT prefix, got {msg}"
                );
                assert!(msg.contains("repeat"), "got message {msg}");
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
        // Validation must happen before any port call.
        assert!(ssh.calls().is_empty());
    }

    // --- Scenario 5 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn repeat_above_cap_is_rejected_as_invalid_argument() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed_shell(&shells, "sh-1", "sess-1");

        let err = uc
            .execute(build_req(
                "sh-1",
                ShellKey::CtrlC,
                no_mods(),
                MAX_SEND_KEY_REPEAT + 1,
            ))
            .await
            .expect_err("repeat > 64 must be rejected");
        match err {
            DomainError::InvalidArgument(msg) => {
                assert!(
                    msg.starts_with("INVALID_REPEAT:"),
                    "expected INVALID_REPEAT prefix, got {msg}"
                );
                assert!(msg.contains("64"), "message must echo the cap, got {msg}");
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
        assert!(ssh.calls().is_empty());
    }

    // --- Scenario 6 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn modifier_on_ctrl_key_is_rejected_with_invalid_argument() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed_shell(&shells, "sh-1", "sess-1");

        let err = uc
            .execute(build_req("sh-1", ShellKey::CtrlC, shift(), 1))
            .await
            .expect_err("CtrlC + shift must be rejected by the keys encoder");
        match err {
            DomainError::InvalidArgument(msg) => {
                assert!(
                    msg.starts_with("MODIFIER_NOT_ALLOWED:"),
                    "expected MODIFIER_NOT_ALLOWED prefix, got {msg}"
                );
                assert!(msg.contains("ctrl_c"), "got {msg}");
                assert!(msg.contains("shift"), "got {msg}");
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
        // No write must reach the SSH adapter when encoding fails.
        assert!(
            !ssh.calls()
                .iter()
                .any(|c| matches!(c, FakeSshCall::WriteShell { .. }))
        );
    }

    // --- Scenario 7 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_shell_id_returns_shell_not_found() {
        let (uc, ssh, _shells, _clock) = build_use_case();

        let err = uc
            .execute(build_req("ghost", ShellKey::CtrlC, no_mods(), 1))
            .await
            .expect_err("unknown shell must be rejected");
        match err {
            DomainError::ShellNotFound(id) => assert_eq!(id.as_str(), "ghost"),
            other => panic!("expected ShellNotFound, got {other:?}"),
        }
        // Repository miss must short-circuit before any write.
        assert!(ssh.calls().is_empty());
    }

    // --- Scenario 8 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn tab_with_shift_emits_back_tab_sequence() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed_shell(&shells, "sh-1", "sess-1");

        let outcome = outcome_or_panic(
            uc.execute(build_req("sh-1", ShellKey::Tab, shift(), 1))
                .await,
        );

        assert_eq!(outcome.key_label, "tab");
        assert_eq!(outcome.modifier_label.as_deref(), Some("shift"));
        assert_eq!(outcome.bytes_sent, b"\x1b[Z".len());

        let calls = ssh.calls();
        let (_shell_id, bytes) = write_shell_call(&calls[0]);
        assert_eq!(bytes.as_ref(), b"\x1b[Z");
    }

    // --- Scenario 9 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn write_failure_propagates_as_domain_error() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed_shell(&shells, "sh-1", "sess-1");
        let bad = ShellId::new("sh-1".to_string());
        ssh.queue_write_shell_error(DomainError::ShellNotFound(bad));

        let err = uc
            .execute(build_req("sh-1", ShellKey::CtrlC, no_mods(), 1))
            .await
            .expect_err("write failure must propagate");
        match err {
            DomainError::ShellNotFound(id) => assert_eq!(id.as_str(), "sh-1"),
            other => panic!("expected propagated ShellNotFound, got {other:?}"),
        }
    }

    // --- Scenario 10 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn modifier_label_aggregates_active_flags_in_canonical_order() {
        let (uc, ssh, shells, _clock) = build_use_case();
        let _ = seed_shell(&shells, "sh-1", "sess-1");

        let outcome = outcome_or_panic(
            uc.execute(build_req("sh-1", ShellKey::ArrowUp, shift_ctrl(), 1))
                .await,
        );

        assert_eq!(outcome.modifier_label.as_deref(), Some("shift+ctrl"));
        // xterm code = 1 + 1 (shift) + 4 (ctrl) = 6.
        let calls = ssh.calls();
        let (_shell_id, bytes) = write_shell_call(&calls[0]);
        assert_eq!(bytes.as_ref(), b"\x1b[1;6A");
    }

    // --- Scenario 11 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn sent_at_uses_clock_port_not_wall_clock() {
        let (uc, _ssh, shells, clock) = build_use_case();
        let _ = seed_shell(&shells, "sh-1", "sess-1");

        let outcome = outcome_or_panic(
            uc.execute(build_req("sh-1", ShellKey::CtrlC, no_mods(), 1))
                .await,
        );

        // FakeClock seeded at 2026-05-02 12:00:00 UTC.
        let expected = clock.utc_now();
        assert_eq!(outcome.sent_at, expected);
    }

    // --- Scenario 12 ----------------------------------------------------

    #[test]
    fn format_modifiers_label_returns_none_when_empty() {
        assert_eq!(format_modifiers_label(KeyModifiers::default()), None);
    }

    #[test]
    fn format_modifiers_label_orders_shift_alt_ctrl_for_all_active() {
        let all_active = KeyModifiers {
            shift: true,
            alt: true,
            ctrl: true,
        };
        assert_eq!(
            format_modifiers_label(all_active).as_deref(),
            Some("shift+alt+ctrl")
        );
    }

    // --- Scenario 13 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn alt_only_modifier_label_round_trips() {
        let (uc, _ssh, shells, _clock) = build_use_case();
        let _ = seed_shell(&shells, "sh-1", "sess-1");

        let outcome = outcome_or_panic(
            uc.execute(build_req("sh-1", ShellKey::ArrowDown, alt(), 1))
                .await,
        );
        assert_eq!(outcome.modifier_label.as_deref(), Some("alt"));
    }
}
