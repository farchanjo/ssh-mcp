//! Chaos 06 — concurrent disconnect + subscribe.
//!
//! Thread A drives `force_close`; thread B races a fresh subscribe
//! on the same resource. The CAS in the lifecycle adapter MUST
//! resolve to exactly one outcome — either the close wins (B sees
//! `ResourceGone`) or the subscribe wins (B observes the resource
//! and the close happens after).

use std::sync::Arc;

use ssh_mcp::adapters::clock::fake::FakeClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::{LifecyclePolicy, LifecycleState};
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const ITERATIONS: usize = 32;

#[test]
fn chaos06_concurrent_disconnect_subscribe_resolves_to_single_outcome() {
    // The race is exercised by hand-rolled per-iteration spawns; the
    // outer loop amplifies the chance of the CAS interleaving.
    for iter in 0..ITERATIONS {
        let clock = Arc::new(FakeClock::new(1_000_000));
        let cascade = CascadeCoordinator::new();
        let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));
        let session = SessionId::new(format!("race-{iter}"));
        let resource_id = format!("sh-race-{iter}");
        adapter.track_resource(
            ResourceKind::Shell,
            &resource_id,
            &session,
            LifecyclePolicy::default(),
        );

        let adapter_a = Arc::clone(&adapter);
        let resource_a = resource_id.clone();
        let h_close = std::thread::spawn(move || {
            adapter_a
                .force_close(ResourceKind::Shell, &resource_a)
                .unwrap();
        });

        let adapter_b = Arc::clone(&adapter);
        let resource_b = resource_id.clone();
        let h_sub =
            std::thread::spawn(move || adapter_b.on_subscribe(ResourceKind::Shell, &resource_b));

        h_close.join().unwrap();
        let sub_outcome = h_sub.join().unwrap();
        let final_state = adapter
            .snapshot(ResourceKind::Shell, &resource_id)
            .map(|s| s.state);

        match sub_outcome {
            Ok(()) => {
                // Subscribe won the race — the resource was Observed
                // briefly. The subsequent close still flips it Closed.
                assert_eq!(final_state, Some(LifecycleState::Closed));
            }
            Err(DomainError::ResourceGone(_)) => {
                // Close won — subscribe correctly surfaced the typed
                // error. The state must be Closed.
                assert_eq!(final_state, Some(LifecycleState::Closed));
            }
            Err(err) => {
                panic!("unexpected error on race iteration {iter}: {err:?}");
            }
        }
    }
}
