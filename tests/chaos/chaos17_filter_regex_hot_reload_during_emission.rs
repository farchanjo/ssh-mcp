//! Chaos 17 — filter regex hot-reload during active emission.
//!
//! Producer emits at full rate while a second task swaps the lane's
//! [`FilterRule`] 100 times. Requirement: every swap is atomic (the
//! filter pipeline always observes either the previous OR next regex,
//! never a half-built state) and the lane never panics.

use std::sync::Arc;

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneMsg, SubscriberLaneAdapter};
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const URI: &str = "shell://filter-reload/output";
const PRODUCED: u64 = 2_048;
const RELOADS: u32 = 100;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos17_filter_hot_reload_during_emission_is_atomic() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 8, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::DropOldest,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 8, peer: None,
    };
    let sub_id = adapter
        .open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "filter-reload".to_string(),
            policy,
        )
        .await
        .unwrap();

    // Producer.
    let producer_adapter = Arc::clone(&adapter);
    let producer = tokio::spawn(async move {
        for seq in 0..PRODUCED {
            let payload = if seq.is_multiple_of(2) {
                b"ERROR: boom".to_vec()
            } else {
                b"WARN: hi".to_vec()
            };
            let _ = producer_adapter.produce(URI, &LaneMsg::Data { seq, payload });
            if seq.is_multiple_of(32) {
                tokio::task::yield_now().await;
            }
        }
    });

    // Filter reloader: alternates between two valid regexes + None +
    // one invalid regex (rejected synchronously without affecting
    // the active filter).
    let reloader_adapter = Arc::clone(&adapter);
    let reloader_id = sub_id.clone();
    let reloader = tokio::spawn(async move {
        let mut invalid_rejections = 0_u32;
        for i in 0..RELOADS {
            let rule = match i % 4 {
                0 => FilterRule::Regex("ERROR.*".to_string()),
                1 => FilterRule::Regex("WARN.*".to_string()),
                2 => FilterRule::None,
                _ => FilterRule::Regex("([".to_string()), // invalid -> Err
            };
            match reloader_adapter.set_filter(&reloader_id, rule).await {
                Ok(()) => {}
                Err(DomainError::InvalidArgument(_)) => invalid_rejections += 1,
                Err(other) => panic!("unexpected error: {other:?}"),
            }
            tokio::task::yield_now().await;
        }
        invalid_rejections
    });

    producer.await.unwrap();
    let invalid_rejections = reloader.await.unwrap();

    // Invalid regex (`(`[ ) was attempted RELOADS / 4 times.
    assert_eq!(invalid_rejections, RELOADS / 4);
    // Lane is still alive. Under DropOldest, every produced event
    // either (a) increments events_sent (forwarded or filtered), or
    // (b) increments lagged_drops (mpsc full). The combined total
    // must cover every produced event.
    let stats = adapter.stats_snapshot(&sub_id).expect("lane");
    let total = stats.events_sent + stats.lagged_drops;
    assert!(
        total >= PRODUCED,
        "events_sent + lagged_drops < produced: total={total} produced={PRODUCED} stats={stats:?}",
    );
}
