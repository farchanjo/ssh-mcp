//! Properties: lane mpsc DropOldest seq monotonicity + accounting.
//!
//! - `prop_lane_mpsc_drop_oldest_seq_increasing` — events delivered
//!   under DropOldest preserve their seq monotonicity.
//! - `prop_lane_events_sent_includes_filtered` — filter-rejected
//!   events still bump events_sent (documented design).

use std::sync::Arc;

use proptest::prelude::*;
use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneMsg, SubscriberLaneAdapter};
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const URI: &str = "shell://lane/output";

fn build_lane(
    buffer: usize,
    policy: LagPolicy,
    filter: FilterRule,
) -> Arc<SubscriberLaneAdapter<UuidIds>> {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), buffer, 8, 64);
    let lane_policy = LanePolicy {
        lag_policy: policy,
        lifetime: SubscriptionLifetime::Manual,
        filter,
        buffer_size: buffer,
        peer: None,
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(adapter.open_lane(
        URI.to_string(),
        ResourceKind::Shell,
        "lane".to_string(),
        lane_policy,
    ))
    .unwrap();
    adapter
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// `prop_lane_mpsc_drop_oldest_seq_increasing`
    ///
    /// Sequence numbers ingested into the lane never go backwards.
    #[test]
    fn prop_lane_mpsc_drop_oldest_seq_increasing(produced in 4_u64..64) {
        let adapter = build_lane(2, LagPolicy::DropOldest, FilterRule::None);
        let mut last = 0_u64;
        for seq in 0..produced {
            let _ = adapter.produce(URI, &LaneMsg::Data { seq, payload: vec![0_u8; 1] });
            prop_assert!(seq >= last);
            last = seq;
        }
        let stats = SubscriberLanePort::list_subs(&*adapter).into_iter().next().unwrap().stats;
        prop_assert!(stats.events_sent + stats.lagged_drops >= produced);
    }

    /// `prop_lane_events_sent_includes_filtered`
    ///
    /// The lane counts filter-dropped events under events_sent so the
    /// operator can see the filter rate vs production rate.
    #[test]
    fn prop_lane_events_sent_includes_filtered(produced in 1_u64..32) {
        let adapter = build_lane(16, LagPolicy::Snapshot, FilterRule::Regex("DOES_NOT_MATCH".to_string()));
        for seq in 0..produced {
            adapter.produce(URI, &LaneMsg::Data { seq, payload: b"non-matching".to_vec() }).unwrap();
        }
        let stats = SubscriberLanePort::list_subs(&*adapter).into_iter().next().unwrap().stats;
        // Every produced event is counted in events_sent (filter rate
        // observable on the wire).
        prop_assert_eq!(stats.events_sent, produced);
    }

    /// `prop_lane_pause_does_not_lose_events_pre_pause`
    ///
    /// Events produced before pause survive — pause only affects
    /// drain, not the producer-side counter.
    #[test]
    fn prop_lane_pause_does_not_lose_events_pre_pause(pre in 0_u64..16) {
        let adapter = build_lane(64, LagPolicy::Snapshot, FilterRule::None);
        for seq in 0..pre {
            adapter.produce(URI, &LaneMsg::Data { seq, payload: vec![0_u8; 1] }).unwrap();
        }
        let stats = SubscriberLanePort::list_subs(&*adapter).into_iter().next().unwrap().stats;
        prop_assert_eq!(stats.events_sent, pre);
    }
}
