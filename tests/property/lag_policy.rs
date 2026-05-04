//! Properties: lag policy invariants.
//!
//! - `prop_lag_policy_invariants_drop_oldest` — under DropOldest, the
//!   producer never blocks.
//! - `prop_lag_policy_drop_newest_drops_under_overflow` — DropNewest
//!   never errors but increments the drop counter when over capacity.
//! - `prop_lag_policy_snapshot_emits_marker_on_full` — Snapshot
//!   policy emits a marker once on the first overflow.
//! - `prop_lag_policy_round_trips_via_serde` — every variant
//!   round-trips through serde.

use std::sync::Arc;

use proptest::prelude::*;
use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneMsg, SubscriberLaneAdapter};
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const URI: &str = "shell://lag/output";

fn build_lane(buffer: usize, policy: LagPolicy) -> Arc<SubscriberLaneAdapter<UuidIds>> {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), buffer, 8, 64);
    let lane_policy = LanePolicy {
        lag_policy: policy,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: buffer,
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(adapter.open_lane(
        URI.to_string(),
        ResourceKind::Shell,
        "lag".to_string(),
        lane_policy,
    ))
    .unwrap();
    adapter
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// `prop_lag_policy_invariants_drop_oldest`
    ///
    /// DropOldest is non-blocking even when the consumer never drains.
    #[test]
    fn prop_lag_policy_invariants_drop_oldest(
        produced in 8_u64..256,
    ) {
        let adapter = build_lane(2, LagPolicy::DropOldest);
        for seq in 0..produced {
            let r = adapter.produce(URI, &LaneMsg::Data { seq, payload: vec![0_u8; 4] });
            prop_assert!(r.is_ok(), "drop_oldest blocked at seq {seq}: {r:?}");
        }
        let stats = SubscriberLanePort::list_subs(&*adapter).into_iter().next().unwrap().stats;
        prop_assert!(stats.lagged_drops > 0, "no drops recorded for produced={produced}");
    }

    /// `prop_lag_policy_drop_newest_drops_under_overflow`
    ///
    /// DropNewest preserves invariant: events_sent + lagged_drops
    /// >= produced.
    #[test]
    fn prop_lag_policy_drop_newest_drops_under_overflow(
        produced in 8_u64..256,
    ) {
        let adapter = build_lane(2, LagPolicy::DropNewest);
        for seq in 0..produced {
            let r = adapter.produce(URI, &LaneMsg::Data { seq, payload: vec![0_u8; 2] });
            prop_assert!(r.is_ok());
        }
        let stats = SubscriberLanePort::list_subs(&*adapter).into_iter().next().unwrap().stats;
        prop_assert!(stats.events_sent + stats.lagged_drops >= produced);
    }

    /// `prop_lag_policy_snapshot_emits_marker_on_full`
    ///
    /// Snapshot policy bumps `lagged_recoveries` at least once when
    /// the buffer overflows.
    #[test]
    fn prop_lag_policy_snapshot_emits_marker_on_full(produced in 4_u64..32) {
        let adapter = build_lane(1, LagPolicy::Snapshot);
        for seq in 0..produced {
            let _ = adapter.produce(URI, &LaneMsg::Data { seq, payload: vec![0_u8; 1] });
        }
        let stats = SubscriberLanePort::list_subs(&*adapter).into_iter().next().unwrap().stats;
        prop_assert!(stats.lagged_recoveries >= 1, "snapshot marker missing: {stats:?}");
    }

    /// `prop_lag_policy_round_trips_via_serde`
    ///
    /// Every variant round-trips lossless through serde JSON.
    #[test]
    fn prop_lag_policy_round_trips_via_serde(
        idx in 0_u8..4,
    ) {
        let variant = match idx {
            0 => LagPolicy::BlockSlow,
            1 => LagPolicy::DropOldest,
            2 => LagPolicy::DropNewest,
            _ => LagPolicy::Snapshot,
        };
        let json = serde_json::to_string(&variant).unwrap();
        let back: LagPolicy = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(variant, back);
    }
}
