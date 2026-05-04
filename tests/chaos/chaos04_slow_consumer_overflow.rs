//! Chaos 04 — slow consumer overflow.
//!
//! `LagPolicy::DropOldest` MUST keep the producer running even when
//! the consumer never drains. This scales the v5 spec target down to
//! a tractable in-process scenario (capacity 4, producer 1024,
//! consumer 0).

use std::sync::Arc;

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneMsg, SubscriberLaneAdapter};
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const PRODUCED: u64 = 1024;
const URI: &str = "shell://overflow/output";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos04_slow_consumer_drop_oldest_keeps_producer_running() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 4, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::DropOldest,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 4,
        peer: None,
    };
    let sub_id = adapter
        .open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "overflow".to_string(),
            policy,
        )
        .await
        .unwrap();

    // Producer pushes a lot more than the buffer can hold; the consumer
    // never drains. Backpressure MUST be absorbed by the lag policy and
    // every produce call returns Ok.
    for seq in 0..PRODUCED {
        let result = adapter.produce(
            URI,
            &LaneMsg::Data {
                seq,
                payload: vec![0_u8; 4],
            },
        );
        assert!(result.is_ok(), "produce {seq} blocked: {result:?}");
    }

    // Stats counters reflect the drops without the producer ever
    // panicking or returning an error.
    let stats = SubscriberLanePort::stats_snapshot(&*adapter, &sub_id).expect("stats");
    assert!(
        stats.events_sent + stats.lagged_drops >= PRODUCED,
        "producer accounting lost events: {stats:?}",
    );
    assert!(
        stats.lagged_drops > 0,
        "drop_oldest never engaged despite full buffer: {stats:?}",
    );
}
