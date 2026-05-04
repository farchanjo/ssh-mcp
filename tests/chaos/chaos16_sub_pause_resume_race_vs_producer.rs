//! Chaos 16 — pause / resume race vs producer.
//!
//! A high-rate producer pushes events while a second task races
//! pause / resume cycles on the same lane. Requirement: the lane
//! never panics, never poisons the registry, and the dispatch counters
//! stay self-consistent regardless of the interleaving.

use std::sync::Arc;

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneMsg, SubscriberLaneAdapter};
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const URI: &str = "shell://pause-race/output";
const PRODUCED: u64 = 4_096;
const PAUSE_RESUME_CYCLES: u32 = 256;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos16_pause_resume_race_keeps_counters_consistent() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 8, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::DropOldest,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 8,
    };
    let sub_id = adapter
        .open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "pause-race".to_string(),
            policy,
        )
        .await
        .unwrap();

    // Producer loops at full speed.
    let producer_adapter = Arc::clone(&adapter);
    let producer = tokio::spawn(async move {
        for seq in 0..PRODUCED {
            let _ = producer_adapter.produce(
                URI,
                &LaneMsg::Data {
                    seq,
                    payload: vec![0_u8; 8],
                },
            );
            if seq.is_multiple_of(64) {
                tokio::task::yield_now().await;
            }
        }
    });

    // Pause / resume churner.
    let churner_adapter = Arc::clone(&adapter);
    let churner_id = sub_id.clone();
    let churner = tokio::spawn(async move {
        for _ in 0..PAUSE_RESUME_CYCLES {
            let _ = churner_adapter.pause_lane(&churner_id).await;
            tokio::task::yield_now().await;
            let _ = churner_adapter.resume_lane(&churner_id).await;
            tokio::task::yield_now().await;
        }
    });

    producer.await.unwrap();
    churner.await.unwrap();

    // Lane MUST still exist and stats are self-consistent.
    let stats = adapter.stats_snapshot(&sub_id).expect("lane present");
    let total = stats.events_sent + stats.lagged_drops;
    assert!(
        total >= PRODUCED,
        "events_sent + lagged_drops < produced: total={total} produced={PRODUCED} stats={stats:?}",
    );
    // Final state is consistent: a clean resume leaves the lane unpaused.
    adapter.resume_lane(&sub_id).await.unwrap();
    let summary = adapter
        .list_subs()
        .into_iter()
        .find(|s| s.sub_id == sub_id)
        .expect("summary");
    assert!(!summary.paused, "lane must end resumed: {summary:?}");
}
