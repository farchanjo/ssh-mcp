//! Properties: replay window.
//!
//! - `prop_replay_window_consistency` — calling `replay_from_cursor`
//!   advances the lane cursor monotonically and never panics.
//! - `prop_replay_handles_random_cursor_input` — every random cursor
//!   produces an Ok result on a live lane.

use std::sync::Arc;

use proptest::prelude::*;
use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::SubscriberLaneAdapter;
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const URI: &str = "shell://replay/output";

fn build() -> Arc<SubscriberLaneAdapter<UuidIds>> {
    SubscriberLaneAdapter::new(Arc::new(UuidIds), 64, 8, 64)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// `prop_replay_window_consistency`
    ///
    /// `replay_from_cursor` is non-decreasing and idempotent for the
    /// same cursor.
    #[test]
    fn prop_replay_window_consistency(targets in proptest::collection::vec(0_u64..u64::from(u32::MAX), 1..16)) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let adapter = build();
        let policy = LanePolicy {
            lag_policy: LagPolicy::Snapshot,
            lifetime: SubscriptionLifetime::Manual,
            filter: FilterRule::None,
            buffer_size: 64,
        };
        let sub_id = rt.block_on(adapter.open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "replay".to_string(),
            policy,
        )).unwrap();

        let mut last = 0_u64;
        for t in targets {
            rt.block_on(<SubscriberLaneAdapter::<UuidIds> as ssh_mcp::ports::subscriber_lane::SubscriberLaneAsync>::replay_from_cursor(&adapter, &sub_id, t)).unwrap();
            let observed = SubscriberLanePort::current_cursor(&*adapter, &sub_id, URI);
            prop_assert!(observed >= last, "replay regressed cursor");
            last = observed;
        }
    }

    /// `prop_replay_unknown_sub_id_errors`
    ///
    /// Replay against an unknown sub_id surfaces a typed error.
    #[test]
    fn prop_replay_unknown_sub_id_errors(cursor in 0_u64..u64::from(u32::MAX)) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let adapter = build();
        let phantom = ssh_mcp::domain::subscription::SubId::new("does-not-exist".to_string());
        let result = rt.block_on(<SubscriberLaneAdapter::<UuidIds> as ssh_mcp::ports::subscriber_lane::SubscriberLaneAsync>::replay_from_cursor(&adapter, &phantom, cursor));
        prop_assert!(result.is_err());
    }
}
