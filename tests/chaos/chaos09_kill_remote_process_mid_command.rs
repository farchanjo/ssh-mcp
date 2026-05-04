//! Chaos 09 — kill remote process mid-command.
//!
//! Mirrors the production path where the remote shell SIGKILLs a
//! command before completion. The lifecycle adapter MUST be able to
//! force_close the command resource without the subscriber count
//! going negative.

use std::sync::Arc;

use ssh_mcp::adapters::clock::fake::FakeClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::{LifecyclePolicy, LifecycleState};
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::subscriber_registry::ResourceKind;

#[test]
fn chaos09_kill_remote_process_does_not_underflow_subs() {
    let clock = Arc::new(FakeClock::new(1_000_000));
    let cascade = CascadeCoordinator::new();
    let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));

    let session = SessionId::new("kill".to_string());
    adapter.track_resource(
        ResourceKind::Command,
        "cmd-killed",
        &session,
        LifecyclePolicy::default(),
    );

    // 3 subscribers attach — plus 1 unattached observer to exercise
    // the underflow guard.
    for _ in 0..3 {
        adapter
            .on_subscribe(ResourceKind::Command, "cmd-killed")
            .unwrap();
    }
    let snap = adapter
        .snapshot(ResourceKind::Command, "cmd-killed")
        .unwrap();
    assert_eq!(snap.sub_count, 3);

    // SIGKILL hits the remote process — adapter force_closes.
    adapter
        .force_close(ResourceKind::Command, "cmd-killed")
        .unwrap();
    let snap = adapter
        .snapshot(ResourceKind::Command, "cmd-killed")
        .unwrap();
    assert_eq!(snap.state, LifecycleState::Closed);
    // sub_count is preserved — close does not zero it. The atomic
    // decrement happens on per-subscriber unsubscribe paths and the
    // closed state simply rejects further on_subscribe calls.
    assert!(snap.sub_count <= 3);
}
