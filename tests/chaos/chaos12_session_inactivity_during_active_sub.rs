//! Chaos 12 — session inactivity during active subscription.
//!
//! With `release_when_no_subs=false` and at least one subscriber
//! attached, the resource MUST stay in `Observed` regardless of
//! clock advance. This is the v5 contract: active subscribers pin
//! the resource alive against inactivity reapers.

use std::sync::Arc;
use std::time::Duration;

use ssh_mcp::adapters::clock::fake::FakeClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::{LifecyclePolicy, LifecycleState};
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::subscriber_registry::ResourceKind;

#[test]
fn chaos12_active_subs_keep_resource_alive_past_inactivity() {
    let clock = Arc::new(FakeClock::new(1_000_000));
    let cascade = CascadeCoordinator::new();
    let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));

    let session = SessionId::new("inactivity".to_string());
    adapter.track_resource(
        ResourceKind::Shell,
        "sh-active",
        &session,
        LifecyclePolicy::default(),
    );
    adapter
        .on_subscribe(ResourceKind::Shell, "sh-active")
        .unwrap();
    let pre = adapter.snapshot(ResourceKind::Shell, "sh-active").unwrap();
    assert_eq!(pre.state, LifecycleState::Observed);
    assert_eq!(pre.sub_count, 1);

    // Skew clock 24h forward. Active subs pin the resource alive.
    clock.advance(Duration::from_secs(24 * 60 * 60));

    let post = adapter.snapshot(ResourceKind::Shell, "sh-active").unwrap();
    assert_eq!(post.state, LifecycleState::Observed);
    assert_eq!(post.sub_count, 1);
}
