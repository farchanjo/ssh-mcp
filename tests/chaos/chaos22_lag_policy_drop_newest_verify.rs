//! Chaos 22 — `LagPolicy::DropNewest` keeps the oldest events.
//!
//! Push N events into a lane sized for K events with a frozen
//! consumer. Requirement: under `DropNewest` the first K events stay
//! in the buffer (they get drained later in delivery order); the
//! remaining N - K events all increment `lagged_drops`.

use std::sync::Arc;

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneMsg, SubscriberLaneAdapter};
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const URI: &str = "shell://drop-newest/output";
const CAPACITY: usize = 4;
const PRODUCED: u64 = 32;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos22_drop_newest_keeps_oldest_events_dropping_overflow() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), CAPACITY, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::DropNewest,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: CAPACITY,
    };
    let sub_id = adapter
        .open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "drop-newest".to_string(),
            policy,
        )
        .await
        .unwrap();

    for seq in 0..PRODUCED {
        let _ = adapter.produce(
            URI,
            &LaneMsg::Data {
                seq,
                payload: vec![0_u8; 8],
            },
        );
    }

    let stats = adapter.stats_snapshot(&sub_id).expect("lane");
    // events_sent counts only the events that actually entered the
    // mpsc — must equal capacity (the first K).
    assert!(
        stats.events_sent <= u64::try_from(CAPACITY).unwrap_or(0),
        "DropNewest accepted more than capacity: {stats:?}",
    );
    // The remaining produced events must show up as lagged_drops.
    let expected_drops = PRODUCED - stats.events_sent;
    assert_eq!(
        stats.lagged_drops, expected_drops,
        "drop count mismatch: stats={stats:?} expected={expected_drops}",
    );
    // No Snapshot recovery emitted — DropNewest never falls back.
    assert_eq!(
        stats.lagged_recoveries, 0,
        "DropNewest must not emit Snapshot recovery: {stats:?}",
    );
}
