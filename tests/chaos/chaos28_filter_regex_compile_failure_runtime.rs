//! Chaos 28 — runtime filter compile failure leaves lane unaffected.
//!
//! `set_filter` with an invalid regex MUST return `InvalidArgument`
//! synchronously and leave the lane's previous filter intact —
//! producers continue to drain through the prior compiled regex.

use std::sync::Arc;

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneMsg, SubscriberLaneAdapter};
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const URI: &str = "shell://filter-fail/output";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos28_invalid_regex_keeps_previous_filter_intact() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 8, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::Snapshot,
        lifetime: SubscriptionLifetime::Manual,
        // Initial filter forwards only ERROR lines.
        filter: FilterRule::Regex("ERROR.*".to_string()),
        buffer_size: 4,
        peer: None,
    };
    let sub_id = adapter
        .open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "filter-fail".to_string(),
            policy,
        )
        .await
        .unwrap();

    // Pre-existing filter is honoured: an ERROR line forwards, a WARN
    // line does not (events_sent counts both filtered and forwarded).
    adapter
        .produce(
            URI,
            &LaneMsg::Data {
                seq: 1,
                payload: b"ERROR: ok".to_vec(),
            },
        )
        .unwrap();
    let baseline = adapter.stats_snapshot(&sub_id).unwrap();
    assert_eq!(baseline.events_sent, 1);
    assert!(baseline.bytes_sent > 0);

    // Now attempt to install an invalid regex. MUST return
    // InvalidArgument synchronously.
    let err = adapter
        .set_filter(&sub_id, FilterRule::Regex("([".to_string()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::InvalidArgument(_)),
        "expected InvalidArgument, got {err:?}",
    );

    // Lane is still alive — produce another ERROR. The prior filter
    // (forwarded ERROR matches) MUST still be active.
    adapter
        .produce(
            URI,
            &LaneMsg::Data {
                seq: 2,
                payload: b"ERROR: still works".to_vec(),
            },
        )
        .unwrap();
    let after = adapter.stats_snapshot(&sub_id).unwrap();
    assert!(
        after.events_sent > baseline.events_sent,
        "lane stalled after invalid regex attempt: before={baseline:?} after={after:?}",
    );
    assert!(after.bytes_sent > baseline.bytes_sent);
}
