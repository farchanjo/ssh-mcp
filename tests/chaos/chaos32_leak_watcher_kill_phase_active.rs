//! Chaos 32 — leak watcher kill phase fires force_close.
//!
//! With `kill_after_s` set to a small positive value, a resource that
//! stays Owned past the threshold MUST be hard-closed by the watcher
//! (`force_close`). Requirement: a Kill alert lands, the resource
//! transitions to `Closed`, and subsequent subscribes surface
//! `ResourceGone`.

use std::sync::Arc;
use std::time::Duration;

use ssh_mcp::adapters::clock::system::SystemClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::leak_watcher::{
    LeakRiskSeverity, LeakWatcher, LeakWatcherConfig,
};
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::{LifecyclePolicy, LifecycleState};
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::subscriber_registry::ResourceKind;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos32_kill_phase_force_closes_leaky_resource() {
    let cascade = CascadeCoordinator::new();
    let clock = Arc::new(SystemClock);
    let adapter = RefcountedLifecycleAdapter::new(cascade, clock);

    adapter.track_resource(
        ResourceKind::Shell,
        "kill-target",
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

    // Wait until a Kill alert fires.
    let mut rx = handle.watcher.subscribe();
    let alert = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let next = rx.recv().await.expect("recv alert");
            if matches!(next.severity, LeakRiskSeverity::Kill) {
                return next;
            }
        }
    })
    .await
    .expect("kill in time");
    assert_eq!(alert.severity, LeakRiskSeverity::Kill);
    assert_eq!(alert.resource_id, "kill-target");

    // Watcher MUST have force_closed the resource.
    let snap = adapter
        .snapshot(ResourceKind::Shell, "kill-target")
        .expect("snap");
    assert_eq!(
        snap.state,
        LifecycleState::Closed,
        "leak kill did not flip state: snap={snap:?}",
    );

    // Subsequent subscribe surfaces ResourceGone.
    let err = adapter
        .on_subscribe(ResourceKind::Shell, "kill-target")
        .unwrap_err();
    assert!(
        matches!(err, DomainError::ResourceGone(_)),
        "expected ResourceGone after kill, got {err:?}",
    );

    handle.cancel.cancel();
    let _ = handle.task.await;
}
