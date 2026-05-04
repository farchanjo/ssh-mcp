//! Properties: grace timer cancel.
//!
//! - `prop_grace_timer_cancel_correct` — re-subscribing inside the
//!   grace window flips the resource back to Observed.
//! - `prop_grace_remaining_ms_within_bound` — the snapshot's grace
//!   remaining never exceeds the configured grace_ms.

use std::sync::Arc;
use std::time::Duration;

use proptest::prelude::*;
use ssh_mcp::adapters::clock::fake::FakeClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::{LifecyclePolicy, LifecycleState};
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::subscriber_registry::ResourceKind;

fn build_with_release() -> (Arc<RefcountedLifecycleAdapter<FakeClock>>, Arc<FakeClock>) {
    let clock = Arc::new(FakeClock::new(1_000_000));
    let cascade = CascadeCoordinator::new();
    let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));
    adapter.track_resource(
        ResourceKind::Shell,
        "rsc",
        &SessionId::new("grace".to_string()),
        LifecyclePolicy {
            release_when_no_subs: true,
            grace_ms: 5_000,
            cascade_session: false,
        },
    );
    (adapter, clock)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// `prop_grace_timer_cancel_correct`
    ///
    /// Re-subscribing within the grace window flips the resource
    /// back to Observed.
    #[test]
    fn prop_grace_timer_cancel_correct(advance_ms in 0_u32..4_000) {
        let (adapter, clock) = build_with_release();
        adapter.on_subscribe(ResourceKind::Shell, "rsc").unwrap();
        adapter.on_unsubscribe(ResourceKind::Shell, "rsc").unwrap();

        let snap = adapter.snapshot(ResourceKind::Shell, "rsc").unwrap();
        prop_assert_eq!(snap.state, LifecycleState::Releasing);

        clock.advance(Duration::from_millis(u64::from(advance_ms)));
        adapter.on_subscribe(ResourceKind::Shell, "rsc").unwrap();
        let snap = adapter.snapshot(ResourceKind::Shell, "rsc").unwrap();
        prop_assert_eq!(snap.state, LifecycleState::Observed);
    }

    /// `prop_grace_remaining_ms_within_bound`
    ///
    /// `grace_remaining_ms` is bounded by the policy `grace_ms`.
    #[test]
    fn prop_grace_remaining_ms_within_bound(advance_ms in 0_u32..1_000) {
        let (adapter, clock) = build_with_release();
        adapter.on_subscribe(ResourceKind::Shell, "rsc").unwrap();
        adapter.on_unsubscribe(ResourceKind::Shell, "rsc").unwrap();
        clock.advance(Duration::from_millis(u64::from(advance_ms)));
        let snap = adapter.snapshot(ResourceKind::Shell, "rsc").unwrap();
        if let Some(remaining) = snap.grace_remaining_ms {
            prop_assert!(remaining <= 5_000);
        }
    }
}
