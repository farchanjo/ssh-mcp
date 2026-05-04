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

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;

use crate::adapters::clock::system::SystemClock;
use crate::adapters::config::internal::{
    resolve_sub_leak_risk_kill_s, resolve_sub_leak_risk_warn_s,
};
use crate::adapters::lifecycle::refcount::{LifecycleScanEntry, RefcountedLifecycleAdapter};
use crate::application::read_resource::{canonical_uri, parse_uri};
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

/// Active alert key — composite of `(kind, resource_id)`. Mirrors the
/// shape used by the lifecycle adapter so a single alert per resource
/// is tracked even if the watcher fires repeatedly.
type AlertKey = (ResourceKind, String);

/// Read-only snapshot of currently-flagged resources.
///
/// Implemented by [`LeakWatcher`] in production and by fakes in tests
/// so list-style render paths can attach `WARN: SUB_LEAK_RISK ...`
/// lines without depending on the concrete adapter.
pub trait LeakWatcherProbe: Send + Sync + 'static {
    /// Return every alert currently in effect. Order is unspecified —
    /// callers that need stable ordering should sort by `(kind, resource_id)`.
    fn current_alerts(&self) -> Vec<LeakRiskAlert>;

    /// Return the alert in effect for `(kind, resource_id)` if any.
    fn alert_for(&self, kind: ResourceKind, resource_id: &str) -> Option<LeakRiskAlert>;

    /// Return the alert in effect for the resource addressed by
    /// `canonical_uri` (e.g. `shell://abc/output`). `None` when the URI
    /// is unparseable or no alert is in effect.
    fn alert_for_uri(&self, uri: &str) -> Option<LeakRiskAlert>;
}

/// Live watcher.
///
/// In addition to broadcasting [`LeakRiskAlert`]s, the watcher keeps a
/// lock-free [`DashMap`] of currently-flagged resources so list-style
/// renderers can attach `WARN: SUB_LEAK_RISK <uri>` lines without
/// re-subscribing to the broadcast channel. Entries are inserted on
/// emit, refreshed on each subsequent emit (severity may upgrade
/// `Warn` -> `Kill`), and cleared on `clear_alert` or once the
/// resource transitions out of `Owned` (e.g. a peer subscribes).
#[derive(Debug, Clone)]
pub struct LeakWatcher {
    tx: broadcast::Sender<LeakRiskAlert>,
    /// Lock-free snapshot of in-effect alerts. Shared with the scan
    /// task so emit + reset operations stay atomic per key.
    active: Arc<DashMap<AlertKey, LeakRiskAlert>>,
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
        let active: Arc<DashMap<AlertKey, LeakRiskAlert>> = Arc::new(DashMap::new());
        let cancel = CancellationToken::new();
        let watcher = Self {
            tx: tx.clone(),
            active: Arc::clone(&active),
        };
        let task = if config.warn_after_s == 0 {
            tokio::spawn(async {})
        } else {
            let cancel_clone = cancel.clone();
            tokio::spawn(scan_loop(
                Arc::clone(adapter),
                tx,
                Arc::clone(&active),
                config,
                cancel_clone,
            ))
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

impl LeakWatcherProbe for LeakWatcher {
    fn current_alerts(&self) -> Vec<LeakRiskAlert> {
        self.active
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    fn alert_for(&self, kind: ResourceKind, resource_id: &str) -> Option<LeakRiskAlert> {
        self.active
            .get(&(kind, resource_id.to_string()))
            .map(|entry| entry.value().clone())
    }

    fn alert_for_uri(&self, uri: &str) -> Option<LeakRiskAlert> {
        let parsed = parse_uri(uri).ok()?;
        self.alert_for(parsed.kind, &parsed.id)
    }
}

/// Build the canonical URI for the resource flagged by `alert`. Helper
/// shared by the leak-warn bridge + list render injection so both
/// surfaces emit the same `shell://<id>/output` form.
#[must_use]
pub fn alert_canonical_uri(alert: &LeakRiskAlert) -> String {
    canonical_uri(alert.kind, &alert.resource_id)
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
    active: Arc<DashMap<AlertKey, LeakRiskAlert>>,
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
                run_scan_pass(&adapter, &tx, &active, &config);
            }
        }
    }
}

fn run_scan_pass<C>(
    adapter: &RefcountedLifecycleAdapter<C>,
    tx: &broadcast::Sender<LeakRiskAlert>,
    active: &DashMap<AlertKey, LeakRiskAlert>,
    config: &LeakWatcherConfig,
) where
    C: ClockPort + Send + Sync + 'static,
{
    let warn_ms = u64::from(config.warn_after_s).saturating_mul(1_000);
    let kill_ms = u64::from(config.kill_after_s).saturating_mul(1_000);
    let mut still_active: HashSet<AlertKey> = HashSet::new();
    for entry in adapter.scan() {
        if let Some(alert) = classify(&entry, warn_ms, kill_ms) {
            // best-effort send — slow consumers handle Lagged on recv.
            let _ = tx.send(alert.clone());
            still_active.insert((alert.kind, alert.resource_id.clone()));
            active.insert((alert.kind, alert.resource_id.clone()), alert.clone());
            if alert.severity == LeakRiskSeverity::Kill {
                let _ = adapter.force_close(alert.kind, &alert.resource_id);
            }
        }
    }
    // Drop alerts that are no longer firing — e.g. a peer subscribed
    // (resource transitioned to `Observed`), or the resource closed.
    active.retain(|key, _| still_active.contains(key));
}

/// Pure classification helper. Decoupled from the loop so unit tests
/// can drive every branch without spawning a task.
#[must_use]
pub fn classify(entry: &LifecycleScanEntry, warn_ms: u64, kill_ms: u64) -> Option<LeakRiskAlert> {
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

    fn entry(
        state: LifecycleState,
        sub_count: usize,
        age_ms: u64,
        release: bool,
    ) -> LifecycleScanEntry {
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

    #[test]
    fn classify_kill_takes_priority_over_warn() {
        // Kill has priority when both thresholds are crossed.
        let e = entry(LifecycleState::Owned, 0, 30_000, false);
        let alert = classify(&e, 2_000, 10_000).expect("kill");
        assert_eq!(alert.severity, LeakRiskSeverity::Kill);
    }

    #[test]
    fn classify_age_at_exact_warn_threshold_fires() {
        // age_ms == warn_ms must trigger (>= comparison).
        let e = entry(LifecycleState::Owned, 0, 2_000, false);
        let alert = classify(&e, 2_000, 0).expect("warn");
        assert_eq!(alert.severity, LeakRiskSeverity::Warn);
    }

    #[test]
    fn classify_age_at_exact_kill_threshold_fires() {
        let e = entry(LifecycleState::Owned, 0, 10_000, false);
        let alert = classify(&e, 2_000, 10_000).expect("kill");
        assert_eq!(alert.severity, LeakRiskSeverity::Kill);
    }

    #[test]
    fn leak_risk_alert_eq_is_field_wise() {
        let a = super::LeakRiskAlert {
            kind: ResourceKind::Shell,
            resource_id: "x".to_string(),
            age_ms: 1_000,
            severity: LeakRiskSeverity::Warn,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn classify_returns_none_when_resource_has_one_subscriber() {
        // sub_count > 0 means the resource is observed; no leak risk.
        let e = entry(LifecycleState::Observed, 1, 30_000, false);
        assert!(classify(&e, 2_000, 0).is_none());
    }

    // -- v5 Phase 3 — LeakWatcherProbe surface ---------------------------

    use super::{LeakWatcherProbe, alert_canonical_uri};

    #[tokio::test(flavor = "multi_thread")]
    async fn probe_current_alerts_reflects_active_warning() {
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
                scan_interval: Duration::from_millis(50),
            },
        );
        // Wait until the broadcaster fires the first alert so the probe
        // map is populated.
        let mut rx = handle.watcher.subscribe();
        let _ = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("alert in time")
            .expect("alert ok");
        // Active alerts must surface the same resource.
        let alerts = handle.watcher.current_alerts();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].resource_id, "leaky");
        assert_eq!(alerts[0].kind, ResourceKind::Shell);
        // alert_for + alert_for_uri agree.
        let by_kind = handle
            .watcher
            .alert_for(ResourceKind::Shell, "leaky")
            .expect("alert by kind");
        assert_eq!(by_kind.resource_id, "leaky");
        let by_uri = handle
            .watcher
            .alert_for_uri("shell://leaky/output")
            .expect("alert by uri");
        assert_eq!(by_uri.resource_id, "leaky");
        handle.cancel.cancel();
        let _ = handle.task.await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn probe_alert_for_unknown_uri_returns_none() {
        let adapter = build_adapter();
        let handle = LeakWatcher::spawn(
            &adapter,
            LeakWatcherConfig {
                warn_after_s: 1,
                kill_after_s: 0,
                scan_interval: Duration::from_millis(50),
            },
        );
        // Empty adapter -> empty probe state.
        assert!(handle.watcher.current_alerts().is_empty());
        assert!(
            handle
                .watcher
                .alert_for(ResourceKind::Shell, "nonexistent")
                .is_none()
        );
        // Bad URI -> None (parse error).
        assert!(handle.watcher.alert_for_uri("gibberish").is_none());
        // Unknown URI -> None (parses but no alert).
        assert!(
            handle
                .watcher
                .alert_for_uri("shell://no-such-shell/output")
                .is_none()
        );
        handle.cancel.cancel();
        let _ = handle.task.await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn probe_alert_drops_when_resource_subscribed() {
        // Once a peer subscribes, the resource transitions to
        // `Observed` and the watcher must clear the active-alert entry
        // on the next sweep so the list render stops emitting WARN lines.
        let adapter = build_adapter();
        adapter.track_resource(
            ResourceKind::Shell,
            "transient",
            &SessionId::new("s".to_string()),
            LifecyclePolicy::default(),
        );
        let handle = LeakWatcher::spawn(
            &adapter,
            LeakWatcherConfig {
                warn_after_s: 1,
                kill_after_s: 0,
                scan_interval: Duration::from_millis(50),
            },
        );
        // Wait for first alert.
        let mut rx = handle.watcher.subscribe();
        let _ = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("alert in time")
            .expect("alert ok");
        assert_eq!(handle.watcher.current_alerts().len(), 1);
        // Subscribing transitions to Observed -> watcher should clear
        // the active alert on the next sweep.
        adapter
            .on_subscribe(ResourceKind::Shell, "transient")
            .expect("subscribe");
        // Wait long enough for several sweeps so the cleanup pass runs.
        let watcher = handle.watcher.clone();
        let cleared_in_time = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if watcher.current_alerts().is_empty() {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or(false);
        assert!(
            cleared_in_time,
            "subscribed resource must be cleared from the active-alerts map"
        );
        handle.cancel.cancel();
        let _ = handle.task.await;
    }

    #[test]
    fn alert_canonical_uri_emits_canonical_form() {
        let alert = super::LeakRiskAlert {
            kind: ResourceKind::Command,
            resource_id: "abc".to_string(),
            age_ms: 1_000,
            severity: super::LeakRiskSeverity::Warn,
        };
        assert_eq!(alert_canonical_uri(&alert), "command://abc/output");
    }

    #[test]
    fn watcher_clone_and_probe_dyn_safety_compile_checks() {
        // The watcher's `active` map is held in an Arc so cheap clones
        // see the same state. Important: the bridge clones the watcher
        // rather than re-subscribing the underlying broadcast channel.
        // Verified at the type level: LeakWatcher: Clone + Sync.
        fn _assert_clone<T: Clone + Send + Sync>() {}
        _assert_clone::<LeakWatcher>();
        // Compile-time check that the probe trait is dyn-safe.
        fn _assert_probe(_: Arc<dyn LeakWatcherProbe>) {}
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn watcher_fires_kill_after_kill_threshold_and_force_closes() {
        let adapter = build_adapter();
        adapter.track_resource(
            ResourceKind::Shell,
            "kill-me",
            &SessionId::new("s".to_string()),
            LifecyclePolicy::default(),
        );
        let handle = LeakWatcher::spawn(
            &adapter,
            LeakWatcherConfig {
                warn_after_s: 1,
                kill_after_s: 1,
                scan_interval: Duration::from_millis(50),
            },
        );
        let mut rx = handle.watcher.subscribe();
        // First alert is Warn or Kill — both acceptable; we only assert
        // that *eventually* a Kill alert lands.
        let alert = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let next = rx.recv().await.expect("alert ok");
                if matches!(next.severity, LeakRiskSeverity::Kill) {
                    return next;
                }
            }
        })
        .await
        .expect("kill in time");
        assert_eq!(alert.severity, LeakRiskSeverity::Kill);
        handle.cancel.cancel();
        let _ = handle.task.await;
    }
}
