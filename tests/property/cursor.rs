//! Properties: per-lane byte cursor.
//!
//! - `prop_cursor_monotonic` — `advance_cursor(target)` is fetch_max,
//!   so the visible cursor is non-decreasing across reads.
//! - `prop_cursor_handles_random_targets` — any sequence of
//!   target advances always converges on the maximum.

use std::sync::Arc;

use proptest::prelude::*;
use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::SubscriberLaneAdapter;
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const URI: &str = "shell://cursor/output";

fn build_lane() -> Arc<SubscriberLaneAdapter<UuidIds>> {
    SubscriberLaneAdapter::new(Arc::new(UuidIds), 8, 8, 64)
}

fn build_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// `prop_cursor_monotonic`
    ///
    /// Any sequence of `advance_cursor` calls produces a
    /// non-decreasing visible cursor.
    #[test]
    fn prop_cursor_monotonic(targets in proptest::collection::vec(0_u64..1024_u64, 1..32)) {
        let rt = build_runtime();
        let adapter = build_lane();
        let policy = LanePolicy {
            lag_policy: LagPolicy::Snapshot,
            lifetime: SubscriptionLifetime::Manual,
            filter: FilterRule::None,
            buffer_size: 8,
        };
        let sub_id = rt.block_on(adapter.open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "cursor".to_string(),
            policy,
        )).unwrap();

        let mut last = 0_u64;
        for target in targets {
            let observed = SubscriberLanePort::advance_cursor(&*adapter, &sub_id, URI, target);
            prop_assert!(observed >= last, "cursor regressed: {observed} < {last}");
            last = observed;
        }
    }

    /// `prop_cursor_converges_on_max`
    ///
    /// After applying every target, the final cursor equals the max
    /// of the input set.
    #[test]
    fn prop_cursor_converges_on_max(targets in proptest::collection::vec(0_u64..1024_u64, 1..32)) {
        let rt = build_runtime();
        let adapter = build_lane();
        let policy = LanePolicy {
            lag_policy: LagPolicy::Snapshot,
            lifetime: SubscriptionLifetime::Manual,
            filter: FilterRule::None,
            buffer_size: 8,
        };
        let sub_id = rt.block_on(adapter.open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "cursor2".to_string(),
            policy,
        )).unwrap();
        let max = *targets.iter().max().unwrap();
        for t in &targets {
            let _ = SubscriberLanePort::advance_cursor(&*adapter, &sub_id, URI, *t);
        }
        let final_cursor = SubscriberLanePort::current_cursor(&*adapter, &sub_id, URI);
        prop_assert_eq!(final_cursor, max);
    }
}
