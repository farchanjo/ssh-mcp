//! Chaos 18 — replay during a concurrent producer.
//!
//! Producer emits at high rate while a parallel task issues
//! `replay_from_cursor` calls. Requirement (ADR 0012 replay-cursor fix):
//! replay clamps to the live byte cursor and never advances it past
//! production, the cursor never regresses under any interleaving, and
//! every replay completes deterministically with no lost events.

use std::sync::Arc;

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneMsg, SubscriberLaneAdapter};
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const URI: &str = "shell://replay-race/output";
const PRODUCED: u64 = 4_096;
const REPLAYS: u32 = 64;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos18_replay_during_producer_keeps_cursor_monotonic() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 8, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::Snapshot,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 8,
        peer: None,
    };
    let sub_id = adapter
        .open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "replay-race".to_string(),
            policy,
        )
        .await
        .unwrap();

    // Producer.
    let producer_adapter = Arc::clone(&adapter);
    let producer = tokio::spawn(async move {
        for seq in 0..PRODUCED {
            let _ = producer_adapter.produce(
                URI,
                &LaneMsg::Data {
                    seq,
                    payload: vec![0_u8; 16],
                },
            );
            if seq.is_multiple_of(32) {
                tokio::task::yield_now().await;
            }
        }
    });

    // Replay churner: requests cursors in a non-monotonic sequence, all
    // beyond the live byte cursor. Each request must clamp to the live
    // cursor (never advancing it) and never regress a prior observation.
    let replay_adapter = Arc::clone(&adapter);
    let replay_id = sub_id.clone();
    let replayer = tokio::spawn(async move {
        let mut max_cursor = 0_u64;
        for i in 0..REPLAYS {
            let cursor = u64::from(i) * 3_u64; // 0, 3, 6, 9, ...
            replay_adapter
                .replay_from_cursor(&replay_id, cursor)
                .await
                .unwrap();
            // Ask again with a smaller cursor — must NOT regress.
            replay_adapter
                .replay_from_cursor(&replay_id, cursor.saturating_sub(2))
                .await
                .unwrap();
            let observed = replay_adapter.current_cursor(&replay_id, URI);
            assert!(
                observed >= max_cursor,
                "cursor regressed: {observed} < {max_cursor} (i={i})",
            );
            max_cursor = observed;
            tokio::task::yield_now().await;
        }
        max_cursor
    });

    producer.await.unwrap();
    let final_cursor = replayer.await.unwrap();
    // Replay no longer advances the cursor via `fetch_max`; it clamps to
    // the live byte cursor. `LaneMsg::Data` delivery does not byte-account
    // the lane cursor, so every out-of-range replay target clamps to 0 —
    // the point of the fix is precisely that a client-supplied cursor can
    // never pin the shared byte-accumulator forward past production.
    assert_eq!(
        final_cursor, 0,
        "replay must clamp to the live byte cursor, not advance past production: {final_cursor}",
    );
    // Lane unaffected by the replay race. Under Snapshot policy, each
    // produced event is either delivered (`events_sent`) or triggers
    // a recovery (`lagged_recoveries`) when the mpsc is full.
    let stats = adapter.stats_snapshot(&sub_id).expect("lane present");
    let total = stats.events_sent + stats.lagged_recoveries;
    assert!(
        total >= PRODUCED,
        "producer events lost: total={total} stats={stats:?}",
    );
}
