//! Properties: subscribe / unsubscribe accounting.
//!
//! - `prop_subscribe_unsubscribe_balanced` — the lifecycle adapter
//!   tracks subscriber count exactly across any sequence of attaches
//!   and detaches.
//! - `prop_subscribe_after_close_returns_resource_gone` — once the
//!   resource is closed, subsequent attaches surface a typed error.

use std::sync::Arc;

use proptest::prelude::*;
use ssh_mcp::adapters::clock::fake::FakeClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::LifecyclePolicy;
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::subscriber_registry::ResourceKind;

fn build() -> Arc<RefcountedLifecycleAdapter<FakeClock>> {
    let clock = Arc::new(FakeClock::new(1_000_000));
    let cascade = CascadeCoordinator::new();
    let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));
    adapter.track_resource(
        ResourceKind::Shell,
        "rsc",
        &SessionId::new("balance".to_string()),
        LifecyclePolicy::default(),
    );
    adapter
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// `prop_subscribe_unsubscribe_balanced`
    ///
    /// `subscribers - unsubscribers = sub_count` exactly.
    #[test]
    fn prop_subscribe_unsubscribe_balanced(
        attach in 1_u32..32,
        detach in 0_u32..32,
    ) {
        let adapter = build();
        for _ in 0..attach {
            adapter.on_subscribe(ResourceKind::Shell, "rsc").unwrap();
        }
        let detach_actual = detach.min(attach);
        for _ in 0..detach_actual {
            adapter.on_unsubscribe(ResourceKind::Shell, "rsc").unwrap();
        }
        let snap = adapter.snapshot(ResourceKind::Shell, "rsc").unwrap();
        prop_assert_eq!(snap.sub_count as u32, attach - detach_actual);
    }

    /// `prop_subscribe_after_close_returns_resource_gone`
    ///
    /// After force_close, every on_subscribe surfaces ResourceGone.
    #[test]
    fn prop_subscribe_after_close_returns_resource_gone(
        attempts in 1_u32..16,
    ) {
        let adapter = build();
        adapter.force_close(ResourceKind::Shell, "rsc").unwrap();
        for _ in 0..attempts {
            let err = adapter.on_subscribe(ResourceKind::Shell, "rsc").unwrap_err();
            prop_assert!(matches!(err, DomainError::ResourceGone(_)));
        }
    }
}
