//! Chaos 27 — grace timer fire vs explicit `force_close` race.
//!
//! Sub + unsub arms the grace timer; the clock advances past the
//! deadline; a parallel thread issues `force_close`. Requirement:
//! the resource closes exactly once (the cascade refcount drops to
//! zero exactly once) regardless of whether the timer or the explicit
//! close win the CAS.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ssh_mcp::adapters::clock::fake::FakeClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::{LifecyclePolicy, LifecycleState};
use ssh_mcp::ports::lifecycle_policy::{LifecyclePolicyAsync, LifecyclePolicyPort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const ITERATIONS: u32 = 16;
const GRACE_MS: u32 = 100;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos27_grace_vs_close_closes_exactly_once() {
    for iter in 0..ITERATIONS {
        let clock = Arc::new(FakeClock::new(1_000_000));
        let cascade = CascadeCoordinator::new();
        let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));

        let hook_calls = Arc::new(AtomicUsize::new(0));
        let captured = Arc::clone(&hook_calls);
        cascade.install_auto_disconnect_hook(Arc::new(move |_| {
            captured.fetch_add(1, Ordering::AcqRel);
        }));

        let session = SessionId::new(format!("grace-close-{iter}"));
        let resource_id = format!("sh-grace-close-{iter}");
        let mut policy = LifecyclePolicy::release_with_default_grace();
        policy.grace_ms = GRACE_MS;
        adapter.track_resource(ResourceKind::Shell, &resource_id, &session, policy);
        adapter
            .on_subscribe(ResourceKind::Shell, &resource_id)
            .unwrap();
        adapter
            .on_unsubscribe(ResourceKind::Shell, &resource_id)
            .unwrap();

        // Arm the grace timer task.
        adapter
            .arm_grace_timer(ResourceKind::Shell, resource_id.clone())
            .await;
        // Push the clock past the grace deadline.
        clock.advance(Duration::from_millis(u64::from(GRACE_MS) * 2));

        // Race: grace timer fires (background tokio task) vs explicit
        // close.
        let adapter_close = Arc::clone(&adapter);
        let resource_close = resource_id.clone();
        let close_task =
            tokio::spawn(
                async move { adapter_close.force_close(ResourceKind::Shell, &resource_close) },
            );

        // Give the grace timer a few ticks to also try to fire.
        tokio::time::sleep(Duration::from_millis(120)).await;
        close_task.await.unwrap().unwrap();

        // Final state is Closed exactly once.
        let snap = adapter.snapshot(ResourceKind::Shell, &resource_id).unwrap();
        assert_eq!(snap.state, LifecycleState::Closed, "iter {iter}");
        // Cascade refcount is exactly zero — never went negative
        // (under the strict lint baseline an underflow would be a
        // silent recovery, but the hook fires exactly once).
        assert_eq!(
            cascade.session_active_refs(&session),
            0,
            "iter {iter}: refs={}",
            cascade.session_active_refs(&session),
        );
        assert_eq!(
            hook_calls.load(Ordering::Acquire),
            1,
            "iter {iter}: hook fired wrong number of times",
        );
    }
}
