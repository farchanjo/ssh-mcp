//! Best-effort `notifications/progress` emitter for v4.7.
//!
//! Long-running tools (`ssh_get_command_output(wait=true)`,
//! `ssh_get_transfer_progress(wait=true)`, `ssh_shell_wait_for`,
//! `ssh_execute(wait=true)`) used to leave the LLM in dead air for tens of
//! seconds. This module wraps each polling loop in a `tokio::select!` that
//! interleaves the use case future with a bounded `tokio::time::interval`
//! tick that fires `Peer::notify_progress` updates against the calling
//! client.
//!
//! ## Contract
//!
//! - **No-op when the request omits `_meta.progressToken`.** Per the MCP
//!   spec, `progressToken` is optional; absent token means the client did
//!   not opt in to progress notifications.
//! - **Best-effort delivery.** Notification errors are swallowed so the
//!   user-visible response is unchanged when the transport hiccups.
//! - **Bounded cadence.** Callers pick the tick interval (5 s for command
//!   / transfer, 1 s for `wait_for`); rate limiting comes from the
//!   `tokio::time::interval`, never from `sleep` busy-waits.
//! - **Lock-free.** `ProgressEmitter` is `Clone` and only carries a
//!   reference-counted `Peer<RoleServer>` plus a small enum tag; no
//!   `Mutex` on the hot path.

use std::time::{Duration, Instant};

use rmcp::Peer;
use rmcp::RoleServer;
use rmcp::model::{ProgressNotificationParam, ProgressToken};
use rmcp::service::RequestContext;

/// Bounded best-effort progress notifier.
///
/// `Disabled` means the originating MCP request omitted
/// `_meta.progressToken`; in that case every `emit` call is a no-op (no
/// syscall, no allocation, no transport traffic).
#[derive(Debug, Clone)]
pub struct ProgressEmitter {
    state: EmitterState,
}

/// Enum-tagged emitter so the disabled path is a single field check
/// against the discriminant — no extra heap or atomic load required.
#[derive(Debug, Clone)]
enum EmitterState {
    /// Caller did not supply `_meta.progressToken`; every emit is a no-op.
    Disabled,
    /// Caller supplied a token; emissions go through the rmcp peer.
    Enabled {
        peer: Peer<RoleServer>,
        token: ProgressToken,
    },
}

impl ProgressEmitter {
    /// Inspect the inbound [`RequestContext`] for a
    /// `_meta.progressToken` and build the appropriate emitter state.
    /// When the token is absent, returns a no-op emitter so every
    /// downstream `emit` call is cheap.
    #[must_use]
    pub fn new(ctx: &RequestContext<RoleServer>) -> Self {
        let state = ctx
            .meta
            .get_progress_token()
            .map_or(EmitterState::Disabled, |token| EmitterState::Enabled {
                peer: ctx.peer.clone(),
                token,
            });
        Self { state }
    }

    /// Build a disabled emitter without an rmcp context. Mostly useful in
    /// tests so callers can exercise helpers that take a `&ProgressEmitter`.
    /// Also gated behind the `test-fixtures` feature so integration
    /// tests that wire the leak-warn bridge can build a no-op emitter
    /// without standing up an rmcp transport.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            state: EmitterState::Disabled,
        }
    }

    /// `true` when the caller supplied `_meta.progressToken` and the
    /// emitter will actually deliver notifications. Useful so callers can
    /// short-circuit expensive snapshot reads when nobody is listening.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        matches!(self.state, EmitterState::Enabled { .. })
    }

    /// Fire one `notifications/progress` notification. Errors are
    /// swallowed (best-effort) so the user-visible response is unchanged
    /// when the transport closes mid-flight.
    ///
    /// `progress` is the cumulative amount of work done (bytes, seconds,
    /// or any monotonic counter). `total` is optional — pass `None` when
    /// the work magnitude is unknown. `message` is a free-form caption.
    pub async fn emit(&self, progress: f64, total: Option<f64>, message: Option<String>) {
        let EmitterState::Enabled { peer, token } = &self.state else {
            return;
        };
        let mut params = ProgressNotificationParam::new(token.clone(), progress);
        if let Some(t) = total {
            params = params.with_total(t);
        }
        if let Some(msg) = message {
            params = params.with_message(msg);
        }
        // best-effort fire-and-forget; never propagate transport errors
        // up to the user-visible response.
        let _ = peer.notify_progress(params).await;
    }
}

/// Default cadence used by the `with_progress_*` helpers for command and
/// transfer pumps (long-running, low-frequency updates).
pub const COMMAND_TICK: Duration = Duration::from_secs(5);

/// Cadence used by the shell `wait_for` pump (one update per second is
/// frequent enough to give the user a "still alive" cue without spamming
/// the channel).
pub const WAIT_FOR_TICK: Duration = Duration::from_secs(1);

/// Convert a non-negative `usize` to `f64` for progress reporting.
///
/// Saturates at `f64::MAX` for values beyond the precise representable
/// range — fine for a progress signal that tolerates rounding.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "progress emissions tolerate rounding; the signal is informational"
)]
pub const fn usize_to_progress(value: usize) -> f64 {
    value as f64
}

/// Convert a `u64` to `f64` for progress reporting. Same caveat as
/// [`usize_to_progress`].
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "progress emissions tolerate rounding; the signal is informational"
)]
pub const fn u64_to_progress(value: u64) -> f64 {
    value as f64
}

/// Convert a `Duration` elapsed since `start` into a non-negative `f64`
/// of seconds.
///
/// The result is finite for any reasonable wall time; it is therefore
/// safe to feed directly to [`ProgressEmitter::emit`].
#[must_use]
pub fn elapsed_seconds(start: Instant) -> f64 {
    start.elapsed().as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::{ProgressEmitter, elapsed_seconds, u64_to_progress, usize_to_progress};
    use futures::executor::block_on;
    use std::time::Instant;

    #[test]
    fn disabled_emitter_is_noop() {
        // No panic, no allocation observable from the outside; just a
        // synchronous early-return on the first `Disabled` match arm.
        // Driven on `futures::executor::block_on` so we do not depend on
        // `#[tokio::test]`, whose macro expansion injects an
        // `#[allow(clippy::expect_used)]` attribute that our crate forbids.
        block_on(async {
            let em = ProgressEmitter::disabled();
            assert!(!em.is_enabled());
            em.emit(1.0, None, None).await;
            em.emit(2.0, Some(10.0), Some("running".to_string())).await;
        });
    }

    #[test]
    fn disabled_emitter_reports_disabled_state() {
        // Cheap pure check — does not depend on any executor so we can
        // exercise the const fn without spinning a reactor.
        let em = ProgressEmitter::disabled();
        assert!(
            !em.is_enabled(),
            "ProgressEmitter::disabled() must report `is_enabled() == false`"
        );
    }

    #[test]
    fn usize_and_u64_progress_helpers_round_trip_small_values() {
        assert!((usize_to_progress(42) - 42.0).abs() < f64::EPSILON);
        assert!((u64_to_progress(1024) - 1024.0).abs() < f64::EPSILON);
    }

    #[test]
    fn elapsed_seconds_tracks_a_real_instant() {
        // Real-clock smoke test (no `test-util` dependency). The
        // converter must be monotone non-decreasing, return 0.0 when
        // start == now, and stay finite for any reasonable wall time.
        let start = Instant::now();
        let s = elapsed_seconds(start);
        assert!(s.is_finite(), "elapsed must stay finite");
        assert!(s >= 0.0, "elapsed must never be negative");
    }
}
