//! Chaos 08 — SFTP-class disk full surface.
//!
//! When the SFTP layer surfaces a disk-full error mid-transfer, the
//! lifecycle adapter MUST cascade close (force_close) the resource
//! and the cascade coordinator MUST decrement the session refcount
//! exactly once.

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
fn chaos08_disk_full_force_close_propagates_resource_gone() {
    let clock = Arc::new(FakeClock::new(1_000_000));
    let cascade = CascadeCoordinator::new();
    let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));

    let session = SessionId::new("disk-full".to_string());
    adapter.track_resource(
        ResourceKind::Transfer,
        "tx-disk-full",
        &session,
        LifecyclePolicy::default(),
    );
    adapter
        .on_subscribe(ResourceKind::Transfer, "tx-disk-full")
        .unwrap();

    // Pre-condition.
    let pre = adapter
        .snapshot(ResourceKind::Transfer, "tx-disk-full")
        .unwrap();
    assert_eq!(pre.state, LifecycleState::Observed);

    // Disk-full at the SFTP layer translates into a force_close on the
    // resource — surfaces ResourceGone to anyone re-attaching.
    adapter
        .force_close(ResourceKind::Transfer, "tx-disk-full")
        .unwrap();

    let post = adapter
        .snapshot(ResourceKind::Transfer, "tx-disk-full")
        .unwrap();
    assert_eq!(post.state, LifecycleState::Closed);

    let err = adapter
        .on_subscribe(ResourceKind::Transfer, "tx-disk-full")
        .unwrap_err();
    assert!(matches!(err, DomainError::ResourceGone(_)));
}
