//! Chaos 21 — `LagPolicy::BlockSlow` falls back to Snapshot when the
//! lane mpsc fills.
//!
//! ADR 0006 documents that the sync `produce` path under
//! [`LagPolicy::BlockSlow`] degrades to Snapshot semantics so the
//! producer never deadlocks. This chaos run drives that degraded path
//! by saturating the lane and verifying the recovery counter fires.

use std::sync::Arc;

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneMsg, SubscriberLaneAdapter};
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const URI: &str = "shell://block-slow/output";
const PRODUCED: u64 = 64;
const CAPACITY: usize = 2;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos21_block_slow_falls_back_to_snapshot_on_full() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), CAPACITY, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::BlockSlow,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: CAPACITY, peer: None,
    };
    let sub_id = adapter
        .open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "block-slow".to_string(),
            policy,
        )
        .await
        .unwrap();

    // Push more than capacity allows; consumer never drains. Under
    // BlockSlow's sync degradation the lane MUST fall back to
    // Snapshot semantics — emit a Snapshot marker, increment
    // lagged_recoveries, and return Ok.
    for seq in 0..PRODUCED {
        adapter
            .produce(
                URI,
                &LaneMsg::Data {
                    seq,
                    payload: vec![0_u8; 8],
                },
            )
            .unwrap();
    }

    let stats = adapter.stats_snapshot(&sub_id).expect("lane");
    assert!(
        stats.lagged_recoveries >= 1,
        "BlockSlow must fall back to Snapshot recovery: {stats:?}",
    );
    // BlockSlow degraded mode never records drops (Snapshot recovers
    // by emitting a marker, not by dropping the new event).
    assert_eq!(
        stats.lagged_drops, 0,
        "BlockSlow degraded mode dropped: {stats:?}"
    );
    // Lane survives — no panic, no poison.
    adapter.close_lane(&sub_id).await.unwrap();
}
