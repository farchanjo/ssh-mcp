//! Chaos 05 — burst subscribe / unsubscribe.
//!
//! Hammers the lane registry with rapid open / close cycles on the
//! same URI. Requirement: no leak (the by_uri index keeps shrinking
//! to empty between cycles), no panic, every error is typed.

use std::sync::Arc;

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::SubscriberLaneAdapter;
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const URI: &str = "shell://burst/output";
// 256 cycles is enough to exhibit the leak in a buggy registry while
// staying within ~ms of CI test budget.
const CYCLES: u32 = 256;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos05_burst_subscribe_unsubscribe_no_leak() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 8, 16, 1024);
    let policy = LanePolicy {
        lag_policy: LagPolicy::Snapshot,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 4,
    };

    for _ in 0..CYCLES {
        let sub_id = adapter
            .open_lane(
                URI.to_string(),
                ResourceKind::Shell,
                "burst".to_string(),
                policy.clone(),
            )
            .await
            .unwrap();
        adapter.close_lane(&sub_id).await.unwrap();
    }

    // After the burst, the registry must be empty — no leaked lanes.
    assert!(
        SubscriberLanePort::list_subs(&*adapter).is_empty(),
        "lanes leaked after burst",
    );

    // A subsequent open MUST still succeed — the URI cap was not
    // permanently consumed by the leaked entries.
    let final_id = adapter
        .open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "burst".to_string(),
            policy,
        )
        .await
        .unwrap();
    assert_eq!(SubscriberLanePort::list_subs(&*adapter).len(), 1);
    adapter.close_lane(&final_id).await.unwrap();
}
