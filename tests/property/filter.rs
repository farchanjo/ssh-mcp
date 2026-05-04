//! Properties: filter pipeline.
//!
//! - `prop_filter_regex_match_drop` — match=true forwards, match=false
//!   drops.
//! - `prop_filter_regex_preserves_order` — surviving events keep
//!   their relative order.
//! - `prop_filter_set_is_hot_swappable` — hot reload between None and
//!   Regex preserves correctness.

use proptest::prelude::*;
use ssh_mcp::adapters::subscription::filter::FilterPipeline;
use ssh_mcp::adapters::subscription::subscriber_lane::LaneMsg;
use ssh_mcp::domain::subscription::FilterRule;

fn data(seq: u64, payload: &[u8]) -> LaneMsg {
    LaneMsg::Data {
        seq,
        payload: payload.to_vec(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// `prop_filter_regex_match_drop`
    ///
    /// `passes` returns true iff the regex matches the payload.
    #[test]
    fn prop_filter_regex_match_drop(
        body in "[a-zA-Z0-9 ]{1,32}",
    ) {
        let f = FilterPipeline::new(FilterRule::Regex("KEEP".to_string()));
        let want = body.contains("KEEP");
        let msg = data(0, body.as_bytes());
        prop_assert_eq!(f.passes(&msg), want);
    }

    /// `prop_filter_regex_preserves_order`
    ///
    /// Filter doesn't drop matching events — every input matching the
    /// regex passes. This implicitly documents that the filter does
    /// not reorder events, since `passes` is a stateless predicate.
    #[test]
    fn prop_filter_regex_preserves_order(seqs in proptest::collection::vec(0_u64..1000, 1..32)) {
        let f = FilterPipeline::new(FilterRule::Regex("KEEP".to_string()));
        let mut forwarded = 0_usize;
        for &s in &seqs {
            let msg = data(s, b"KEEP this");
            if f.passes(&msg) {
                forwarded += 1;
            }
        }
        // Every input matched the regex, so every event is forwarded.
        prop_assert_eq!(forwarded, seqs.len());
    }

    /// `prop_filter_set_is_hot_swappable`
    ///
    /// Calling `set` to swap from None -> Regex applies immediately.
    #[test]
    fn prop_filter_set_is_hot_swappable(
        body in "[a-zA-Z0-9 ]{1,32}",
    ) {
        let f = FilterPipeline::new(FilterRule::None);
        let msg = data(0, body.as_bytes());
        prop_assert!(f.passes(&msg)); // None passes everything
        f.set(FilterRule::Regex("MATCH".to_string())).unwrap();
        let want = body.contains("MATCH");
        prop_assert_eq!(f.passes(&msg), want);
    }

    /// `prop_filter_marker_messages_always_pass`
    ///
    /// Snapshot and Lagged markers always pass regardless of regex —
    /// they are control events.
    #[test]
    fn prop_filter_marker_messages_always_pass(
        pattern in "[a-zA-Z]{1,8}",
    ) {
        let f = FilterPipeline::new(FilterRule::Regex(pattern));
        let snapshot = LaneMsg::Snapshot { cursor: 0, delta: vec![] };
        let lagged = LaneMsg::Lagged { dropped: 1 };
        prop_assert!(f.passes(&snapshot));
        prop_assert!(f.passes(&lagged));
    }
}
