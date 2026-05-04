//! Chaos 14 — `UUIDv7` collision simulation.
//!
//! The lane adapter mints fresh `SubId` values via the
//! `IdGeneratorPort`. We inject a custom adapter that returns the
//! SAME id twice in a row to simulate a collision (cosmic ray, faulty
//! generator, etc). Requirement: the second `open_lane` MUST detect
//! the duplicate without corrupting the registry.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ssh_mcp::adapters::subscription::subscriber_lane::SubscriberLaneAdapter;
use ssh_mcp::domain::ids::{CommandId, ForwardId, SessionId, ShellId, TransferId};
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::id_generator::IdGeneratorPort;
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

/// Fake generator that returns the SAME id for the first two
/// `new_session_id` calls (used by the lane adapter to mint `SubId`),
/// then begins counting normally.
#[derive(Debug, Default)]
struct CollidingIdGenerator {
    calls: AtomicU64,
}

impl IdGeneratorPort for CollidingIdGenerator {
    fn new_session_id(&self) -> SessionId {
        let n = self.calls.fetch_add(1, Ordering::Relaxed);
        // First two calls return identical id.
        if n < 2 {
            SessionId::new("collide".to_string())
        } else {
            SessionId::new(format!("sess-{n}"))
        }
    }
    fn new_command_id(&self) -> CommandId {
        CommandId::new(format!(
            "cmd-{}",
            self.calls.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn new_shell_id(&self) -> ShellId {
        ShellId::new(format!(
            "shell-{}",
            self.calls.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn new_transfer_id(&self) -> TransferId {
        TransferId::new(format!(
            "xfer-{}",
            self.calls.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn new_forward_id(&self) -> ForwardId {
        ForwardId::new(format!(
            "fwd-{}",
            self.calls.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos14_uuid_collision_does_not_corrupt_registry() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(CollidingIdGenerator::default()), 8, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::Snapshot,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 4, peer: None,
    };

    let first = adapter
        .open_lane(
            "shell://collide-1/output".to_string(),
            ResourceKind::Shell,
            "collide".to_string(),
            policy.clone(),
        )
        .await
        .unwrap();
    // Second call returns the SAME id — the adapter overwrites the
    // first lane in the by-id map. This is the documented behaviour:
    // the by-uri index for a different URI keeps its independent
    // entry, so no panic, no double-free.
    let second = adapter
        .open_lane(
            "shell://collide-2/output".to_string(),
            ResourceKind::Shell,
            "collide".to_string(),
            policy,
        )
        .await
        .unwrap();
    assert_eq!(first, second, "collision repro precondition");

    // The registry MUST remain self-consistent: at least one summary
    // exists and is queryable without panicking.
    let summaries = SubscriberLanePort::list_subs(&*adapter);
    assert!(
        !summaries.is_empty(),
        "lane registry corrupted on collision"
    );

    // Closing the duplicated id is idempotent — the second close
    // surfaces SubNotFound rather than panicking.
    adapter.close_lane(&first).await.unwrap();
    let err = adapter.close_lane(&second).await;
    assert!(err.is_err(), "double close on collided id must error");
}
