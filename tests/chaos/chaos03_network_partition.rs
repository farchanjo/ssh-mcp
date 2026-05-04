//! Chaos 03 — network partition.
//!
//! Models a network partition by dropping an SSH session through
//! the lifecycle adapter while subscribers are still attached. The
//! `force_close` path MUST flip the resource into the terminal
//! `Closed` state and surface `ResourceGone` on subsequent
//! subscribe attempts — the canonical wire-error for a partition.

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
fn chaos03_partition_force_close_marks_resource_gone() {
    let clock = Arc::new(FakeClock::new(1_000_000));
    let cascade = CascadeCoordinator::new();
    let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));

    let session = SessionId::new("partition-1".to_string());
    adapter.track_resource(
        ResourceKind::Shell,
        "sh-partition",
        &session,
        LifecyclePolicy::default(),
    );
    adapter
        .on_subscribe(ResourceKind::Shell, "sh-partition")
        .unwrap();

    // Pre-condition: at least one subscriber is observing.
    let snap = adapter
        .snapshot(ResourceKind::Shell, "sh-partition")
        .unwrap();
    assert_eq!(snap.state, LifecycleState::Observed);
    assert_eq!(snap.sub_count, 1);

    // Partition: force-close from the runtime side (e.g. SSH transport
    // reset).
    adapter
        .force_close(ResourceKind::Shell, "sh-partition")
        .unwrap();

    // Post-condition: snapshot reports Closed; resubscribe surfaces
    // ResourceGone (NOT NotFound — the resource existed once).
    let snap = adapter
        .snapshot(ResourceKind::Shell, "sh-partition")
        .unwrap();
    assert_eq!(snap.state, LifecycleState::Closed);

    let err = adapter
        .on_subscribe(ResourceKind::Shell, "sh-partition")
        .unwrap_err();
    assert!(matches!(err, DomainError::ResourceGone(_)));

    // A second force_close on a closed resource MUST be idempotent.
    adapter
        .force_close(ResourceKind::Shell, "sh-partition")
        .unwrap();
}
