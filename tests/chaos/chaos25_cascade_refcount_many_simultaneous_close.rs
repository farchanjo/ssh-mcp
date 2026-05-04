//! Chaos 25 — cascade refcount under many simultaneous closes.
//!
//! A session owns 10 shells; ten parallel threads each close one
//! shell. Requirement: the cascade refcount drops to zero exactly
//! once and the auto-disconnect hook fires exactly once.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ssh_mcp::adapters::clock::fake::FakeClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::LifecyclePolicy;
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const SHELLS: usize = 10;

#[test]
fn chaos25_cascade_simultaneous_close_fires_hook_once() {
    let clock = Arc::new(FakeClock::new(1_000_000));
    let cascade = CascadeCoordinator::new();
    let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));

    // Install hook that counts firings.
    let hook_calls = Arc::new(AtomicUsize::new(0));
    let captured = Arc::clone(&hook_calls);
    cascade.install_auto_disconnect_hook(Arc::new(move |_session_id| {
        captured.fetch_add(1, Ordering::AcqRel);
    }));

    let session = SessionId::new("cascade-many".to_string());
    for i in 0..SHELLS {
        adapter.track_resource(
            ResourceKind::Shell,
            &format!("sh-{i}"),
            &session,
            LifecyclePolicy::default(),
        );
    }
    assert_eq!(cascade.session_active_refs(&session), SHELLS);

    // Ten threads close in parallel.
    let mut handles = Vec::with_capacity(SHELLS);
    for i in 0..SHELLS {
        let adapter_c = Arc::clone(&adapter);
        handles.push(std::thread::spawn(move || {
            adapter_c
                .force_close(ResourceKind::Shell, &format!("sh-{i}"))
                .unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // Refcount drops to zero monotonically; hook fires exactly once.
    assert_eq!(cascade.session_active_refs(&session), 0);
    assert_eq!(
        hook_calls.load(Ordering::Acquire),
        1,
        "auto-disconnect hook fired wrong number of times",
    );
    assert!(cascade.is_session_reaped(&session));
}
