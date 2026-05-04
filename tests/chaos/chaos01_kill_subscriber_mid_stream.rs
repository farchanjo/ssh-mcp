//! Chaos 01 — kill subscriber mid-stream.
//!
//! Models the v5 lane `LaneAdmin::close` path that fires when a
//! subscriber transport drops while the producer is still pushing
//! events. Requirement: closing the lane mid-stream MUST drop the
//! receiver eagerly without panicking the producer.

use std::sync::Arc;

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneMsg, SubscriberLaneAdapter};
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos01_close_lane_mid_stream_drops_state_cleanly() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 8, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::Snapshot,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 4,
    };
    let sub_id = adapter
        .open_lane(
            "shell://sh-chaos01/output".to_string(),
            ResourceKind::Shell,
            "sh-chaos01".to_string(),
            policy,
        )
        .await
        .unwrap();

    // Produce 5 events while a "subscriber drop" interleaves at event 3.
    for seq in 0..3_u64 {
        let _ = adapter.produce(
            "shell://sh-chaos01/output",
            &LaneMsg::Data {
                seq,
                payload: vec![b'A' + (seq as u8)],
            },
        );
    }

    // Subscriber transport closes — close_lane simulates the GC pruning
    // a dead peer.
    adapter.close_lane(&sub_id).await.unwrap();

    // Subsequent produces MUST not panic. The lane is gone — produce
    // returns Ok(()) because there are no lanes left for the URI.
    for seq in 3..5_u64 {
        let result = adapter.produce(
            "shell://sh-chaos01/output",
            &LaneMsg::Data {
                seq,
                payload: vec![b'A' + (seq as u8)],
            },
        );
        assert!(result.is_ok(), "produce after close must be Ok: {result:?}");
    }

    // The lane is no longer in the registry — list_subs returns empty.
    assert!(SubscriberLanePort::list_subs(&*adapter).is_empty());

    // A second close on the same id MUST surface SubNotFound.
    let err = adapter.close_lane(&sub_id).await.unwrap_err();
    assert!(matches!(err, DomainError::SubNotFound(_)));
}
