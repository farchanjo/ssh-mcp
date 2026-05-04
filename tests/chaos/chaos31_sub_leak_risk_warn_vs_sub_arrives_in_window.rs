//! Chaos 31 — leak warn vs concurrent subscribe inside the window.
//!
//! A resource is opened without a subscriber. The leak watcher fires
//! a `Warn` after the threshold. Concurrently a peer subscribes —
//! the watcher's next sweep observes the resource is now `Observed`
//! and clears the active alert. Requirement: the alert map converges
//! to a deterministic state (either an active warn at the moment of
//! detection or cleared after the subscribe), never both, never
//! corrupted.

use std::sync::Arc;
use std::time::Duration;

use ssh_mcp::adapters::clock::system::SystemClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::leak_watcher::{
    LeakRiskSeverity, LeakWatcher, LeakWatcherConfig, LeakWatcherProbe,
};
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::LifecyclePolicy;
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::subscriber_registry::ResourceKind;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos31_warn_vs_subscribe_inside_window_resolves_deterministically() {
    let cascade = CascadeCoordinator::new();
    let clock = Arc::new(SystemClock);
    let adapter = RefcountedLifecycleAdapter::new(cascade, clock);

    adapter.track_resource(
        ResourceKind::Shell,
        "warn-vs-sub",
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

    // Wait for the first warn alert to fire.
    let mut rx = handle.watcher.subscribe();
    let alert = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("alert in time")
        .expect("alert ok");
    assert_eq!(alert.severity, LeakRiskSeverity::Warn);
    assert_eq!(alert.resource_id, "warn-vs-sub");

    // Now race a subscribe against subsequent warn re-fires. Every
    // sweep after the subscribe MUST clear the alert; before the
    // subscribe lands, the watcher MAY emit additional warns.
    let adapter_sub = Arc::clone(&adapter);
    let sub_task = tokio::spawn(async move {
        // Tiny stagger so the watcher races at least one extra sweep.
        tokio::time::sleep(Duration::from_millis(60)).await;
        adapter_sub
            .on_subscribe(ResourceKind::Shell, "warn-vs-sub")
            .unwrap();
    });
    sub_task.await.unwrap();

    // After the subscribe, the watcher must clear the alert within a
    // bounded number of sweeps (50 ms scan_interval -> 1 s = 20
    // sweeps).
    let watcher = handle.watcher.clone();
    let cleared = tokio::time::timeout(Duration::from_secs(2), async {
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
        cleared,
        "watcher failed to clear alert after subscribe — race produced inconsistent state",
    );
    handle.cancel.cancel();
    let _ = handle.task.await;
}
