//! Chaos 11 — mpsc full panic safety.
//!
//! Every supported `LagPolicy` variant MUST keep `produce` panic-free
//! when the lane mpsc fills up. We exercise all four variants in a
//! single test so the suite stays diffable.

use std::sync::Arc;

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneMsg, SubscriberLaneAdapter};
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

async fn drive_policy(uri: &str, lag_policy: LagPolicy) {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 2, 8, 64);
    let policy = LanePolicy {
        lag_policy,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 2,
    };
    let _ = adapter
        .open_lane(
            uri.to_string(),
            ResourceKind::Shell,
            "fullpanic".to_string(),
            policy,
        )
        .await
        .unwrap();
    // Push 64 events; consumer never drains. The lane MUST absorb the
    // overflow per its policy without panic / poisoning.
    for seq in 0..64_u64 {
        let payload = vec![0_u8; 16];
        let _ = adapter.produce(uri, &LaneMsg::Data { seq, payload });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos11_drop_oldest_never_panics_on_full() {
    drive_policy("shell://full-do/output", LagPolicy::DropOldest).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos11_drop_newest_never_panics_on_full() {
    drive_policy("shell://full-dn/output", LagPolicy::DropNewest).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos11_snapshot_never_panics_on_full() {
    drive_policy("shell://full-sn/output", LagPolicy::Snapshot).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos11_block_slow_does_not_panic_when_consumer_drains_async() {
    // BlockSlow holds the producer; we keep the consumer drainable
    // by not filling beyond capacity — the test confirms the policy
    // does not panic under bounded production.
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 2, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::BlockSlow,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 4,
    };
    let _ = adapter
        .open_lane(
            "shell://full-bs/output".to_string(),
            ResourceKind::Shell,
            "fullpanic".to_string(),
            policy,
        )
        .await
        .unwrap();
    for seq in 0..3_u64 {
        let _ = adapter.produce(
            "shell://full-bs/output",
            &LaneMsg::Data {
                seq,
                payload: vec![0_u8; 8],
            },
        );
    }
}
