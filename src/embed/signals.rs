//! Graceful shutdown sequence for `ssh-mcp-tail`.
//!
//! On SIGTERM / SIGINT / SIGHUP the daemon cancels its cooperative
//! [`tokio_util::sync::CancellationToken`] which in turn:
//!
//! 1. Stops the dispatcher reading new ops from stdin.
//! 2. Cancels the heartbeat / stats tasks.
//! 3. Lets in-flight tool calls complete (subject to
//!    `SSH_GRACE_HARD_TIMEOUT_S`).
//! 4. Drops the rmcp client so the server task observes the duplex
//!    half close.
//! 5. Awaits the formatter drain so every queued event reaches stdout.
//!
//! If the grace window fires before step 3 finishes we abort the
//! remaining tasks; the operator sees a `WARN: hard_grace_timeout`
//! line on stderr.

use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::time::Duration;

#[cfg(unix)]
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// Spawn the signal listener task. Cancels `shutdown` on the first
/// SIGTERM / SIGINT / SIGHUP. Returns the join handle so callers can
/// optionally `.await` it on graceful exit.
///
/// On Windows there is no portable SIGTERM/SIGHUP surface in tokio, so
/// the listener simply parks on the cancellation token: the daemon still
/// drains cooperatively when shutdown fires from another path (stdin
/// EOF, explicit cancel).
#[must_use]
pub fn spawn_signal_listener(shutdown: CancellationToken) -> JoinHandle<()> {
    #[cfg(unix)]
    {
        tokio::spawn(signal_listener_loop(shutdown))
    }
    #[cfg(not(unix))]
    {
        tokio::spawn(async move { shutdown.cancelled().await })
    }
}

#[cfg(unix)]
async fn signal_listener_loop(shutdown: CancellationToken) {
    let Some(signals) = open_signals() else {
        // The handler init failed: park on the cancellation token
        // so the daemon still drains cooperatively when shutdown
        // fires from another path.
        shutdown.cancelled().await;
        return;
    };
    wait_for_signal(signals, &shutdown).await;
    shutdown.cancel();
}

#[cfg(unix)]
struct OpenedSignals {
    term: Signal,
    int: Signal,
    hup: Signal,
}

#[cfg(unix)]
fn open_signals() -> Option<OpenedSignals> {
    if let (Ok(term), Ok(int), Ok(hup)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
        signal(SignalKind::hangup()),
    ) {
        Some(OpenedSignals { term, int, hup })
    } else {
        tracing::warn!("signal handler init failed");
        None
    }
}

#[cfg(unix)]
async fn wait_for_signal(mut signals: OpenedSignals, shutdown: &CancellationToken) {
    tokio::select! {
        _ = signals.term.recv() => tracing::info!("SIGTERM received"),
        _ = signals.int.recv() => tracing::info!("SIGINT received"),
        _ = signals.hup.recv() => tracing::info!("SIGHUP received"),
        () = shutdown.cancelled() => {},
    }
}

/// Wait for the supplied shutdown token to cancel, then return either
/// `Ok(())` once `done` resolves or `Err(GraceTimeout)` if the
/// `hard_timeout` fires first.
///
/// Used by the daemon main loop to bound the drain so a stuck task
/// can't keep the daemon alive forever.
///
/// # Errors
/// Returns [`GraceTimeout`] when `hard_timeout` elapses before `done`
/// resolves.
pub async fn wait_for_drain<F>(done: F, hard_timeout: Duration) -> Result<(), GraceTimeout>
where
    F: Future<Output = ()>,
{
    match timeout(hard_timeout, done).await {
        Ok(()) => Ok(()),
        Err(_) => Err(GraceTimeout),
    }
}

/// Marker error returned when `wait_for_drain` exhausts its grace
/// window. The caller logs a warn-level message and proceeds with
/// task aborts.
#[derive(Debug, Clone, Copy)]
pub struct GraceTimeout;

impl fmt::Display for GraceTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("hard grace timeout elapsed before drain finished")
    }
}

impl StdError for GraceTimeout {}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test-only assertions are deliberately direct"
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_for_drain_resolves_when_done_first() {
        let result = wait_for_drain(async {}, Duration::from_millis(200)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn wait_for_drain_times_out_on_stuck_future() {
        let stuck = std::future::pending::<()>();
        let result = wait_for_drain(stuck, Duration::from_millis(50)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn signal_listener_cancels_on_external_token_cancel() {
        let token = CancellationToken::new();
        let handle = spawn_signal_listener(token.clone());
        token.cancel();
        // Listener must terminate when the token is externally cancelled.
        handle.await.unwrap();
    }

    #[test]
    fn grace_timeout_displays_message() {
        let timeout_err = GraceTimeout;
        let s = format!("{timeout_err}");
        assert!(s.contains("hard grace timeout"));
    }
}
