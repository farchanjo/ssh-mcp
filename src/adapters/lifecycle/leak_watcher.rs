//! v5 Phase 3 `SUB_LEAK_RISK` auto-warning watcher.
//!
//! Periodically scans the [`RefcountedLifecycleAdapter`] for resources
//! that crossed the `SSH_SUB_LEAK_RISK_WARN_S` age threshold (default
//! 2 s) while still in `Owned` state with zero subscribers AND were not
//! opened with `release_when_no_subs=true`. Each match drops a typed
//! [`LeakRiskAlert`] onto a broadcast channel so multiple consumers
//! (the rmcp tool layer, the NDJSON daemon, integration tests) can
//! subscribe without contention.
//!
//! Optional hard-close: when `SSH_SUB_LEAK_RISK_KILL_S` (default 0)
//! is set to a value >= 1, the watcher additionally calls
//! [`LifecyclePolicyPort::force_close`] once a resource crosses that
//! age — a defence-in-depth backstop for agent leaks that survive the
//! warning channel.
//!
//! See ADR 0005 §"Hygiene tail" for the operator-facing semantics.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;

use crate::adapters::clock::system::SystemClock;
use crate::adapters::config::internal::{resolve_sub_leak_risk_kill_s, resolve_sub_leak_risk_warn_s};
use crate::adapters::lifecycle::refcount::{LifecycleScanEntry, RefcountedLifecycleAdapter};
use crate::domain::lifecycle::LifecycleState;
use crate::ports::clock::ClockPort;
use crate::ports::lifecycle_policy::LifecyclePolicyPort;
use crate::ports::subscriber_registry::ResourceKind;

/// Broadcast channel capacity for [`LeakRiskAlert`] subscribers.
///
/// Each alert is small (struct of fixed-size fields), so 256 entries
/// is enough to absorb a sweep burst without dropping. Slow consumers
/// see [`broadcast::error::RecvError::Lagged`] — the watcher itself
/// does not back-pressure on its own emit channel.
pub const LEAK_BROADCAST_CAPACITY: usize = 256;

/// Default scan interval.
///
/// Set to 1 second so a 2 s warn threshold fires within ~1 sweep of
/// the actual leak. Configurable through the watcher constructor —
/// integration tests use shorter intervals so the test harness stays
/// snappy.
pub const DEFAULT_SCAN_INTERVAL: Duration = Duration::from_secs(1);

/// Alert emitted when a resource crosses the warn / kill threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeakRiskAlert {
    /// Resource scheme.
    pub kind: ResourceKind,
    /// Resource id portion of the URI.
    pub resource_id: String,
    /// Age of the resource (milliseconds) at the moment of detection.
    pub age_ms: u64,
    /// Severity tier — `Warn` fires at `SSH_SUB_LEAK_RISK_WARN_S`,
    /// `Kill` fires at `SSH_SUB_LEAK_RISK_KILL_S` (when >= 1).
    pub severity: LeakRiskSeverity,
}

/// Severity tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeakRiskSeverity {
    /// `WARN: SUB_LEAK_RISK` — resource stayed Owned past the warn
    /// threshold.
    Warn,
    /// Hard-close fired through `force_close`.
    Kill,
}

/// Configuration for [`LeakWatcher`].
#[derive(Debug, Clone, Copy)]
pub struct LeakWatcherConfig {
    /// Warn threshold in seconds. `0` disables the watcher entirely.
    pub warn_after_s: u32,
    /// Hard-close threshold in seconds. `0` disables the kill phase.
    pub kill_after_s: u32,
    /// Scan interval. Defaults to [`DEFAULT_SCAN_INTERVAL`].
    pub scan_interval: Duration,
}

impl Default for LeakWatcherConfig {
    fn default() -> Self {
        Self {
            warn_after_s: 2,
            kill_after_s: 0,
            scan_interval: DEFAULT_SCAN_INTERVAL,
        }
    }
}

/// Live watcher.
#[derive(Debug, Clone)]
pub struct LeakWatcher {
    tx: broadcast::Sender<LeakRiskAlert>,
}

impl LeakWatcher {
    /// Build a fresh watcher and spawn the scan task.
    ///
    /// Returns the watcher handle (`subscribe()` to consume alerts), the
    /// task `JoinHandle`, and a cancellation token the caller can flip
    /// to stop the loop.
    #[must_use]
    pub fn spawn<C>(
        adapter: &Arc<RefcountedLifecycleAdapter<C>>,
        config: LeakWatcherConfig,
    ) -> LeakWatcherHandle
    where
        C: ClockPort + Send + Sync + 'static,
    {
        let (tx, _rx) = broadcast::channel::<LeakRiskAlert>(LEAK_BROADCAST_CAPACITY);
        let cancel = CancellationToken::new();
        let watcher = Self { tx: tx.clone() };
        let task = if config.warn_after_s == 0 {
            tokio::spawn(async {})
        } else {
            let cancel_clone = cancel.clone();
            tokio::spawn(scan_loop(Arc::clone(adapter), tx, config, cancel_clone))
        };
        LeakWatcherHandle {
            watcher,
            cancel,
            task,
        }
    }

    /// Subscribe to alerts.
    ///
    /// Each subscriber receives every alert emitted from the moment of
    /// subscription onwards.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<LeakRiskAlert> {
        self.tx.subscribe()
    }
}

/// Bundled handle — cancelling the token + awaiting the task is the
/// canonical shutdown procedure.
#[derive(Debug)]
pub struct LeakWatcherHandle {
    /// Live watcher (subscribe through it).
    pub watcher: LeakWatcher,
    /// Cancellation token. Flip to stop the scan loop.
    pub cancel: CancellationToken,
    /// `JoinHandle` for the scan task.
    pub task: JoinHandle<()>,
}

/// Convenience constructor for the production wiring.
///
/// Pulls the thresholds from the env via the config resolvers.
#[must_use]
pub fn spawn_default(adapter: &Arc<RefcountedLifecycleAdapter<SystemClock>>) -> LeakWatcherHandle {
    let warn = resolve_sub_leak_risk_warn_s();
    let kill = resolve_sub_leak_risk_kill_s();
    LeakWatcher::spawn(
        adapter,
        LeakWatcherConfig {
            warn_after_s: warn,
            kill_after_s: kill,
            scan_interval: DEFAULT_SCAN_INTERVAL,
        },
    )
}

async fn scan_loop<C>(
    adapter: Arc<RefcountedLifecycleAdapter<C>>,
    tx: broadcast::Sender<LeakRiskAlert>,
    config: LeakWatcherConfig,
    cancel: CancellationToken,
) where
    C: ClockPort + Send + Sync + 'static,
{
    let mut tick = interval(config.scan_interval);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            _ = tick.tick() => {
                run_scan_pass(&adapter, &tx, &config);
            }
        }
    }
}

fn run_scan_pass<C>(
    adapter: &RefcountedLifecycleAdapter<C>,
    tx: &broadcast::Sender<LeakRiskAlert>,
    config: &LeakWatcherConfig,
) where
    C: ClockPort + Send + Sync + 'static,
{
    let warn_ms = u64::from(config.warn_after_s).saturating_mul(1_000);
    let kill_ms = u64::from(config.kill_after_s).saturating_mul(1_000);
    for entry in adapter.scan() {
        if let Some(alert) = classify(&entry, warn_ms, kill_ms) {
            // best-effort send — slow consumers handle Lagged on recv.
            let _ = tx.send(alert.clone());
            if alert.severity == LeakRiskSeverity::Kill {
                let _ = adapter.force_close(alert.kind, &alert.resource_id);
            }
        }
    }
}

/// Pure classification helper. Decoupled from the loop so unit tests
/// can drive every branch without spawning a task.
#[must_use]
pub fn classify(
    entry: &LifecycleScanEntry,
    warn_ms: u64,
    kill_ms: u64,
) -> Option<LeakRiskAlert> {
    // Resources that opted in to refcount-driven release self-clean —
    // the leak watcher never warns on them.
    if entry.policy.release_when_no_subs {
        return None;
    }
    // Only `Owned` (sub_count == 0) resources are at risk; once at
    // least one peer attaches the lane is no longer a leak candidate.
    if entry.state != LifecycleState::Owned || entry.sub_count > 0 {
        return None;
    }
    if kill_ms > 0 && entry.age_ms >= kill_ms {
        return Some(LeakRiskAlert {
            kind: entry.kind,
            resource_id: entry.resource_id.clone(),
            age_ms: entry.age_ms,
            severity: LeakRiskSeverity::Kill,
        });
    }
    if entry.age_ms >= warn_ms {
        return Some(LeakRiskAlert {
            kind: entry.kind,
            resource_id: entry.resource_id.clone(),
            age_ms: entry.age_ms,
            severity: LeakRiskSeverity::Warn,
        });
    }
    None
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests use unwrap for brevity per CLAUDE.md test policy"
)]
mod tests {
    use super::{
        DEFAULT_SCAN_INTERVAL, LEAK_BROADCAST_CAPACITY, LeakRiskSeverity, LeakWatcher,
        LeakWatcherConfig, LifecycleScanEntry, classify,
    };
    use crate::adapters::clock::system::SystemClock;
    use crate::adapters::lifecycle::cascade::CascadeCoordinator;
    use crate::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
    use crate::domain::ids::SessionId;
    use crate::domain::lifecycle::{LifecyclePolicy, LifecycleState};
    use crate::ports::lifecycle_policy::LifecyclePolicyPort;
    use crate::ports::subscriber_registry::ResourceKind;
    use std::sync::Arc;
    use std::time::Duration;

    fn entry(state: LifecycleState, sub_count: usize, age_ms: u64, release: bool) -> LifecycleScanEntry {
        LifecycleScanEntry {
            kind: ResourceKind::Shell,
            resource_id: "x".to_string(),
            state,
            sub_count,
            age_ms,
            policy: LifecyclePolicy {
                release_when_no_subs: release,
                grace_ms: 2_000,
                cascade_session: false,
            },
        }
    }

    #[test]
    fn classify_returns_none_when_release_when_no_subs_set() {
        // release_when_no_subs=true means the resource self-cleans.
        // Even if it stayed Owned past warn_ms, the watcher must not
        // warn.
        let e = entry(LifecycleState::Owned, 0, 5_000, true);
        assert!(classify(&e, 2_000, 0).is_none());
    }

    #[test]
    fn classify_returns_warn_when_owned_past_threshold() {
        let e = entry(LifecycleState::Owned, 0, 3_000, false);
        let alert = classify(&e, 2_000, 0).expect("warn");
        assert_eq!(alert.severity, LeakRiskSeverity::Warn);
        assert_eq!(alert.age_ms, 3_000);
    }

    #[test]
    fn classify_returns_kill_when_owned_past_kill_threshold() {
        let e = entry(LifecycleState::Owned, 0, 30_000, false);
        let alert = classify(&e, 2_000, 10_000).expect("kill");
        assert_eq!(alert.severity, LeakRiskSeverity::Kill);
    }

    #[test]
    fn classify_skips_observed_resources() {
        let e = entry(LifecycleState::Observed, 1, 30_000, false);
        assert!(classify(&e, 2_000, 0).is_none());
    }

    #[test]
    fn classify_skips_releasing_resources() {
        let e = entry(LifecycleState::Releasing, 0, 30_000, false);
        assert!(classify(&e, 2_000, 0).is_none());
    }

    #[test]
    fn classify_skips_below_warn_threshold() {
        let e = entry(LifecycleState::Owned, 0, 1_000, false);
        assert!(classify(&e, 2_000, 0).is_none());
    }

    #[test]
    fn classify_kill_threshold_zero_disables_kill_phase() {
        let e = entry(LifecycleState::Owned, 0, 30_000, false);
        let alert = classify(&e, 2_000, 0).expect("warn (kill disabled)");
        assert_eq!(alert.severity, LeakRiskSeverity::Warn);
    }

    #[test]
    fn leak_watcher_config_default_values() {
        let c = LeakWatcherConfig::default();
        assert_eq!(c.warn_after_s, 2);
        assert_eq!(c.kill_after_s, 0);
        assert_eq!(c.scan_interval, DEFAULT_SCAN_INTERVAL);
    }

    fn build_adapter() -> Arc<RefcountedLifecycleAdapter<SystemClock>> {
        let cascade = CascadeCoordinator::new();
        let clock = Arc::new(SystemClock);
        RefcountedLifecycleAdapter::new(cascade, clock)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn watcher_emits_warn_when_resource_stays_owned() {
        let adapter = build_adapter();
        adapter.track_resource(
            ResourceKind::Shell,
            "leaky",
            &SessionId::new("s".to_string()),
            LifecyclePolicy::default(),
        );
        let handle = LeakWatcher::spawn(
            &adapter,
            LeakWatcherConfig {
                warn_after_s: 1,
                kill_after_s: 0,
                scan_interval: Duration::from_millis(100),
            },
        );
        let mut rx = handle.watcher.subscribe();
        // Wait up to 5 seconds for the alert.
        let alert = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("alert in time")
            .expect("alert ok");
        assert_eq!(alert.severity, LeakRiskSeverity::Warn);
        assert_eq!(alert.kind, ResourceKind::Shell);
        assert_eq!(alert.resource_id, "leaky");
        handle.cancel.cancel();
        let _ = handle.task.await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn watcher_does_not_emit_when_release_when_no_subs_set() {
        let adapter = build_adapter();
        adapter.track_resource(
            ResourceKind::Shell,
            "self-cleaning",
            &SessionId::new("s".to_string()),
            LifecyclePolicy {
                release_when_no_subs: true,
                grace_ms: 2_000,
                cascade_session: false,
            },
        );
        let handle = LeakWatcher::spawn(
            &adapter,
            LeakWatcherConfig {
                warn_after_s: 1,
                kill_after_s: 0,
                scan_interval: Duration::from_millis(100),
            },
        );
        let mut rx = handle.watcher.subscribe();
        // No alert expected within 1.5 seconds (warn_after = 1s).
        let outcome = tokio::time::timeout(Duration::from_millis(1_500), rx.recv()).await;
        assert!(
            outcome.is_err(),
            "expected timeout — release_when_no_subs lanes must not fire SUB_LEAK_RISK"
        );
        handle.cancel.cancel();
        let _ = handle.task.await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn watcher_zero_warn_disables_loop() {
        let adapter = build_adapter();
        adapter.track_resource(
            ResourceKind::Shell,
            "x",
            &SessionId::new("s".to_string()),
            LifecyclePolicy::default(),
        );
        let handle = LeakWatcher::spawn(
            &adapter,
            LeakWatcherConfig {
                warn_after_s: 0,
                kill_after_s: 0,
                scan_interval: Duration::from_millis(100),
            },
        );
        let mut rx = handle.watcher.subscribe();
        let outcome = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
        assert!(outcome.is_err(), "warn_after_s=0 must disable the scanner");
    }

    #[test]
    fn leak_broadcast_capacity_is_documented() {
        assert_eq!(LEAK_BROADCAST_CAPACITY, 256);
    }
}
