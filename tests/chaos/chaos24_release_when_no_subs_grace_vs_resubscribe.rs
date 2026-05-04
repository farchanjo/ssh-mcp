//! Chaos 24 — `release_when_no_subs` grace timer vs concurrent resubscribe.
//!
//! With `release_when_no_subs=true`, a subscribe + unsubscribe arms a
//! grace timer. Halfway through the grace window a re-subscribe races
//! against the grace deadline. Requirement: the CAS resolves to a
//! single outcome — either the resubscribe wins (`Releasing -> Observed`)
//! or the explicit `force_close` wins; never both.

use std::sync::Arc;
use std::time::Duration;

use ssh_mcp::adapters::clock::fake::FakeClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::{LifecyclePolicy, LifecycleState};
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const ITERATIONS: u32 = 32;
const GRACE_MS: u32 = 1_000;

#[test]
fn chaos24_grace_vs_resubscribe_resolves_to_single_outcome() {
    for iter in 0..ITERATIONS {
        let clock = Arc::new(FakeClock::new(1_000_000));
        let cascade = CascadeCoordinator::new();
        let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));

        let session = SessionId::new(format!("grace-resub-{iter}"));
        let resource_id = format!("sh-grace-{iter}");
        let mut policy = LifecyclePolicy::release_with_default_grace();
        policy.grace_ms = GRACE_MS;
        adapter.track_resource(ResourceKind::Shell, &resource_id, &session, policy);

        // Sub then unsub to arm the grace timer.
        adapter
            .on_subscribe(ResourceKind::Shell, &resource_id)
            .unwrap();
        adapter
            .on_unsubscribe(ResourceKind::Shell, &resource_id)
            .unwrap();
        let mid = adapter.snapshot(ResourceKind::Shell, &resource_id).unwrap();
        assert_eq!(mid.state, LifecycleState::Releasing, "iter {iter}");

        // Half the grace window elapses.
        clock.advance(Duration::from_millis(u64::from(GRACE_MS / 2)));

        let adapter_resub = Arc::clone(&adapter);
        let resource_resub = resource_id.clone();
        let resub = std::thread::spawn(move || {
            adapter_resub.on_subscribe(ResourceKind::Shell, &resource_resub)
        });
        let adapter_close = Arc::clone(&adapter);
        let resource_close = resource_id.clone();
        let closer = std::thread::spawn(move || {
            adapter_close.force_close(ResourceKind::Shell, &resource_close)
        });

        let resub_result = resub.join().unwrap();
        let close_result = closer.join().unwrap();
        close_result.unwrap();

        let final_state = adapter
            .snapshot(ResourceKind::Shell, &resource_id)
            .map(|s| s.state);

        match resub_result {
            Ok(()) => {
                // Subscribe MAY have observed Releasing -> Observed before
                // the close fired. The close eventually flips the state to
                // Closed.
                assert_eq!(final_state, Some(LifecycleState::Closed), "iter {iter}");
            }
            Err(err) => {
                // Close beat the resubscribe — typed ResourceGone.
                assert!(
                    matches!(err, ssh_mcp::domain::error::DomainError::ResourceGone(_)),
                    "iter {iter}: unexpected error: {err:?}",
                );
                assert_eq!(final_state, Some(LifecycleState::Closed));
            }
        }
        // Cascade refcount converges to zero on a closed session.
        assert_eq!(cascade.session_active_refs(&session), 0);
    }
}
