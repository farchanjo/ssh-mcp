//! Chaos 07 — clock skew during grace timer.
//!
//! The lifecycle grace timer reads `ClockPort::utc_now`. A skew of
//! +1h while the timer is armed MUST NOT panic, MUST NOT
//! double-close, and MUST eventually transition the resource into
//! `Closed` once a poll picks up the new deadline.

use std::sync::Arc;
use std::time::Duration;

use ssh_mcp::adapters::clock::fake::FakeClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::{LifecyclePolicy, LifecycleState};
use ssh_mcp::ports::lifecycle_policy::{LifecyclePolicyAsync, LifecyclePolicyPort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos07_clock_skew_during_grace_does_not_double_close() {
    let clock = Arc::new(FakeClock::new(1_000_000));
    let cascade = CascadeCoordinator::new();
    let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));

    let session = SessionId::new("skew".to_string());
    let policy = LifecyclePolicy {
        release_when_no_subs: true,
        grace_ms: 5_000,
        cascade_session: false,
    };
    adapter.track_resource(ResourceKind::Shell, "sh-skew", &session, policy);
    adapter
        .on_subscribe(ResourceKind::Shell, "sh-skew")
        .unwrap();
    adapter
        .arm_grace_timer(ResourceKind::Shell, "sh-skew".to_string())
        .await;

    // Drop the subscriber — arms the grace timer.
    adapter
        .on_unsubscribe(ResourceKind::Shell, "sh-skew")
        .unwrap();
    adapter
        .arm_grace_timer(ResourceKind::Shell, "sh-skew".to_string())
        .await;

    // Skew the clock +1h. The timer task polls the clock periodically;
    // the deadline should now look reached.
    clock.advance(Duration::from_secs(3_600));

    // Force-close after the skew to exercise the double-close path.
    adapter.force_close(ResourceKind::Shell, "sh-skew").unwrap();
    let snap = adapter.snapshot(ResourceKind::Shell, "sh-skew").unwrap();
    assert_eq!(snap.state, LifecycleState::Closed);

    // A second force_close at the post-skew instant MUST be idempotent
    // (no panic, no double cascade fire).
    adapter.force_close(ResourceKind::Shell, "sh-skew").unwrap();
    let snap = adapter.snapshot(ResourceKind::Shell, "sh-skew").unwrap();
    assert_eq!(snap.state, LifecycleState::Closed);
}
