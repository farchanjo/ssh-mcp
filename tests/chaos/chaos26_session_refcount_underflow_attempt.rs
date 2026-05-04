//! Chaos 26 — session refcount underflow attempt.
//!
//! The refcounted lifecycle adapter detects subscriber-count underflow
//! and surfaces a typed `DomainError::Internal` instead of panicking.
//! This chaos test drives the +1 sub / +2 unsub sequence (the canonical
//! buggy producer pattern) and asserts the recovery branch keeps the
//! adapter consistent.

use std::sync::Arc;

use ssh_mcp::adapters::clock::fake::FakeClock;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::{LifecyclePolicy, LifecycleState};
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::subscriber_registry::ResourceKind;

#[test]
fn chaos26_underflow_attempt_recovers_and_reports_internal_error() {
    let clock = Arc::new(FakeClock::new(1_000_000));
    let cascade = CascadeCoordinator::new();
    let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));

    let session = SessionId::new("underflow".to_string());
    adapter.track_resource(
        ResourceKind::Shell,
        "sh-underflow",
        &session,
        LifecyclePolicy::default(),
    );

    // +1 sub
    adapter
        .on_subscribe(ResourceKind::Shell, "sh-underflow")
        .unwrap();
    // -1 unsub (clean)
    adapter
        .on_unsubscribe(ResourceKind::Shell, "sh-underflow")
        .unwrap();
    // -1 unsub (BUG — would underflow). The adapter MUST detect it and
    // recover the counter to zero rather than wrapping or panicking.
    let err = adapter
        .on_unsubscribe(ResourceKind::Shell, "sh-underflow")
        .unwrap_err();
    match err {
        DomainError::Internal(msg) => assert!(
            msg.contains("underflow"),
            "expected underflow message, got: {msg}"
        ),
        other => panic!("unexpected error: {other:?}"),
    }

    // Snapshot is self-consistent: count is back to zero, state is
    // Observed (a clean unsubscribe didn't transition because policy
    // doesn't release on no-subs by default).
    let snap = adapter
        .snapshot(ResourceKind::Shell, "sh-underflow")
        .expect("snap");
    assert_eq!(snap.sub_count, 0, "counter not recovered: {snap:?}");
    assert_eq!(snap.state, LifecycleState::Observed);

    // Subsequent operations succeed normally — no poison.
    adapter
        .on_subscribe(ResourceKind::Shell, "sh-underflow")
        .unwrap();
    let snap = adapter
        .snapshot(ResourceKind::Shell, "sh-underflow")
        .unwrap();
    assert_eq!(snap.sub_count, 1);

    // Cleanup. Cascade refs converge to zero on close.
    adapter
        .force_close(ResourceKind::Shell, "sh-underflow")
        .unwrap();
    assert_eq!(cascade.session_active_refs(&session), 0);
}
