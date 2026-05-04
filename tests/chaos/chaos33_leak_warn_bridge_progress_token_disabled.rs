//! Chaos 33 — leak warn bridge: progress notifications absent does
//! not stop the list-render WARN line.
//!
//! The leak warn bridge can fire `notifications/progress` when the
//! caller supplies `_meta.progressToken`. Independently, the
//! `LeakWatcherProbe` surface backs the `WARN: SUB_LEAK_RISK ...`
//! line that list-style renderers attach. Requirement: even when no
//! progress token has been registered, a leak alert is still visible
//! through the probe so list renders do NOT degrade silently.

use std::sync::Arc;
use std::time::Duration;

use ssh_mcp::adapters::clock::system::SystemClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::leak_watcher::{
    LeakWatcher, LeakWatcherConfig, LeakWatcherProbe, alert_canonical_uri,
};
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::LifecyclePolicy;
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::subscriber_registry::ResourceKind;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos33_probe_surface_independent_of_progress_token() {
    let cascade = CascadeCoordinator::new();
    let clock = Arc::new(SystemClock);
    let adapter = RefcountedLifecycleAdapter::new(cascade, clock);

    adapter.track_resource(
        ResourceKind::Shell,
        "leaky-no-token",
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

    // Drain the broadcast channel so the watcher's first sweep also
    // populates the active-alert map. (Subscribe BEFORE the alert
    // fires.)
    let mut rx = handle.watcher.subscribe();
    let alert = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("alert in time")
        .expect("alert ok");
    assert_eq!(alert.resource_id, "leaky-no-token");

    // We deliberately did NOT register any progressToken: the bridge
    // would have nothing to notify. The probe surface MUST still
    // expose the alert for list-style render injection.
    let alerts = handle.watcher.current_alerts();
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].resource_id, "leaky-no-token");
    let by_uri = handle
        .watcher
        .alert_for_uri("shell://leaky-no-token/output")
        .expect("probe by URI");
    assert_eq!(
        alert_canonical_uri(&by_uri),
        "shell://leaky-no-token/output",
    );
    let by_kind = handle
        .watcher
        .alert_for(ResourceKind::Shell, "leaky-no-token")
        .expect("probe by kind");
    assert_eq!(by_kind.resource_id, "leaky-no-token");

    // Drain the rx to drop subsequent warns — the probe MUST keep
    // returning the alert until the resource transitions out of Owned.
    let _ = tokio::time::timeout(Duration::from_millis(150), async {
        loop {
            let _ = rx.recv().await;
        }
    })
    .await;
    assert!(!handle.watcher.current_alerts().is_empty());

    handle.cancel.cancel();
    let _ = handle.task.await;
}
