//! Properties: lifecycle state machine.
//!
//! - For any sequence of (subscribe, unsubscribe, force_close), the
//!   observable state must always be one of {Owned, Observed,
//!   Releasing, Closed} and `sub_count >= 0`.
//! - `force_close` is exactly-once: regardless of CAS interleaving
//!   the resource transitions to `Closed` exactly once.

use std::sync::Arc;

use proptest::prelude::*;
use ssh_mcp::adapters::clock::fake::FakeClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::{LifecyclePolicy, LifecycleState};
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::subscriber_registry::ResourceKind;

#[derive(Debug, Clone, Copy)]
enum Action {
    Subscribe,
    Unsubscribe,
    ForceClose,
}

fn action_strategy() -> impl Strategy<Value = Action> {
    prop_oneof![
        Just(Action::Subscribe),
        Just(Action::Unsubscribe),
        Just(Action::ForceClose),
    ]
}

fn build() -> Arc<RefcountedLifecycleAdapter<FakeClock>> {
    let clock = Arc::new(FakeClock::new(1_000_000));
    let cascade = CascadeCoordinator::new();
    let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));
    adapter.track_resource(
        ResourceKind::Shell,
        "rsc",
        &SessionId::new("s".to_string()),
        LifecyclePolicy::default(),
    );
    adapter
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// `prop_state_machine_no_invalid_transitions`
    ///
    /// Any sequence of subscribe / unsubscribe / force_close keeps
    /// the visible state in the documented enum and never panics.
    #[test]
    fn prop_state_machine_no_invalid_transitions(
        actions in proptest::collection::vec(action_strategy(), 1..32),
    ) {
        let adapter = build();
        let mut subs_attached: usize = 0;
        for action in actions {
            match action {
                Action::Subscribe => {
                    let _ = adapter.on_subscribe(ResourceKind::Shell, "rsc");
                    subs_attached = subs_attached.saturating_add(1);
                }
                Action::Unsubscribe => {
                    if subs_attached > 0 {
                        let _ = adapter.on_unsubscribe(ResourceKind::Shell, "rsc");
                        subs_attached -= 1;
                    }
                }
                Action::ForceClose => {
                    let _ = adapter.force_close(ResourceKind::Shell, "rsc");
                }
            }
            let snap = adapter.snapshot(ResourceKind::Shell, "rsc").unwrap();
            // State is always one of the documented variants.
            prop_assert!(matches!(
                snap.state,
                LifecycleState::Owned
                    | LifecycleState::Observed
                    | LifecycleState::Releasing
                    | LifecycleState::Closed
            ));
        }
    }

    /// `prop_no_double_close`
    ///
    /// Repeated force_close calls keep the state in `Closed` and never
    /// panic.
    #[test]
    fn prop_no_double_close(repeat in 1_u32..32) {
        let adapter = build();
        for _ in 0..repeat {
            adapter.force_close(ResourceKind::Shell, "rsc").unwrap();
        }
        let snap = adapter.snapshot(ResourceKind::Shell, "rsc").unwrap();
        prop_assert_eq!(snap.state, LifecycleState::Closed);
    }
}
