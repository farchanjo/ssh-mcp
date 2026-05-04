//! Chaos 20 — `stats_snapshot` reads during active producer writes.
//!
//! Producer pushes events at full rate while a parallel reader polls
//! `stats_snapshot` thousands of times. Requirement: counters increase
//! monotonically (never go backwards), `queue_depth` stays bounded by
//! the configured capacity, and no read ever observes a torn struct.

use std::sync::Arc;

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneMsg, SubscriberLaneAdapter};
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const URI: &str = "shell://stats-race/output";
const PRODUCED: u64 = 8_192;
const READS: u32 = 4_096;
const CAPACITY: usize = 8;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos20_stats_read_during_writes_is_monotonic() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), CAPACITY, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::DropOldest,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: CAPACITY,
    };
    let sub_id = adapter
        .open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "stats-race".to_string(),
            policy,
        )
        .await
        .unwrap();

    // Producer.
    let producer_adapter = Arc::clone(&adapter);
    let producer = tokio::spawn(async move {
        for seq in 0..PRODUCED {
            let _ = producer_adapter.produce(
                URI,
                &LaneMsg::Data {
                    seq,
                    payload: vec![0_u8; 4],
                },
            );
            if seq.is_multiple_of(64) {
                tokio::task::yield_now().await;
            }
        }
    });

    // Reader.
    let reader_adapter = Arc::clone(&adapter);
    let reader_id = sub_id.clone();
    let reader = tokio::spawn(async move {
        let mut last_events = 0_u64;
        let mut last_bytes = 0_u64;
        let mut last_drops = 0_u64;
        let mut max_depth = 0_usize;
        for _ in 0..READS {
            let stats = reader_adapter.stats_snapshot(&reader_id).expect("stats");
            assert!(
                stats.events_sent >= last_events,
                "events_sent regressed {last_events} -> {}",
                stats.events_sent,
            );
            assert!(
                stats.bytes_sent >= last_bytes,
                "bytes_sent regressed {last_bytes} -> {}",
                stats.bytes_sent,
            );
            assert!(
                stats.lagged_drops >= last_drops,
                "lagged_drops regressed {last_drops} -> {}",
                stats.lagged_drops,
            );
            assert!(
                stats.queue_depth <= CAPACITY,
                "queue_depth {} > capacity {CAPACITY}",
                stats.queue_depth,
            );
            last_events = stats.events_sent;
            last_bytes = stats.bytes_sent;
            last_drops = stats.lagged_drops;
            if stats.queue_depth > max_depth {
                max_depth = stats.queue_depth;
            }
            tokio::task::yield_now().await;
        }
        max_depth
    });

    producer.await.unwrap();
    let _ = reader.await.unwrap();

    // Final stats are coherent.
    let final_stats = adapter.stats_snapshot(&sub_id).expect("stats");
    let total = final_stats.events_sent + final_stats.lagged_drops;
    assert!(
        total >= PRODUCED,
        "missing events: total={total} stats={final_stats:?}",
    );
}
