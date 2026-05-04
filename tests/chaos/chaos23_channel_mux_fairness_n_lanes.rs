//! Chaos 23 — channel mux fairness with N lanes and uneven load.
//!
//! Twenty lanes are registered with the [`ChannelMuxAdapter`]. One
//! lane (#5) gets a 2x burst; the other nineteen get the baseline
//! load. Requirement: every lane drains and the heaviest lane does
//! not starve the others (no lane sees zero deliveries while another
//! has > N/2 deliveries).

use std::time::Duration;

use ssh_mcp::adapters::subscription::channel_mux::ChannelMuxAdapter;
use ssh_mcp::adapters::subscription::subscriber_lane::LaneMsg;
use ssh_mcp::domain::subscription::SubId;
use ssh_mcp::ports::channel_mux::ChannelMuxPort;
use tokio::sync::mpsc;

const LANES: usize = 20;
const BASELINE_PER_LANE: u32 = 8;
const BURST_LANE_INDEX: usize = 5;
const BURST_MULTIPLIER: u32 = 2;
const LANE_CAPACITY: usize = 32;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos23_channel_mux_fairness_n_lanes_no_starvation() {
    let mux = ChannelMuxAdapter::new();
    let (out_tx, mut out_rx) = mpsc::channel::<(SubId, LaneMsg)>(LANES * LANE_CAPACITY * 4);
    mux.install_outbound(out_tx);

    // Register N lanes and pre-fill each with its quota.
    let mut tx_handles = Vec::with_capacity(LANES);
    for i in 0..LANES {
        let (tx, rx) = mpsc::channel::<LaneMsg>(LANE_CAPACITY);
        let sid = SubId::new(format!("lane-{i}"));
        mux.register_lane(sid.clone(), rx);
        let quota = if i == BURST_LANE_INDEX {
            BASELINE_PER_LANE * BURST_MULTIPLIER
        } else {
            BASELINE_PER_LANE
        };
        for seq in 0..quota {
            tx.send(LaneMsg::Data {
                seq: u64::from(seq),
                payload: vec![u8::try_from(i).unwrap_or(0)],
            })
            .await
            .unwrap();
        }
        tx_handles.push(tx);
    }

    let _drain = mux.spawn_drain();

    // Collect everything within a bounded window.
    let mut counts = vec![0_u32; LANES];
    let total_expected =
        (LANES as u32 - 1) * BASELINE_PER_LANE + BASELINE_PER_LANE * BURST_MULTIPLIER;
    let collected = tokio::time::timeout(Duration::from_secs(5), async {
        let mut total = 0_u32;
        while let Some((sid, _msg)) = out_rx.recv().await {
            let s = sid.as_str();
            if let Some(suffix) = s.strip_prefix("lane-") {
                if let Ok(idx) = suffix.parse::<usize>() {
                    if let Some(slot) = counts.get_mut(idx) {
                        *slot += 1;
                    }
                }
            }
            total += 1;
            if total >= total_expected {
                break;
            }
        }
        total
    })
    .await
    .expect("drain finished in time");

    assert_eq!(collected, total_expected, "missed messages: {counts:?}");

    // Every lane must have made progress.
    for (i, c) in counts.iter().enumerate() {
        assert!(*c > 0, "lane {i} starved: counts={counts:?}");
    }
    // The burst lane drains its 2x quota; baseline lanes drain BASELINE_PER_LANE each.
    assert_eq!(
        counts[BURST_LANE_INDEX],
        BASELINE_PER_LANE * BURST_MULTIPLIER,
        "burst lane under-drained: counts={counts:?}",
    );

    // Aggregate counters reflect the total drained.
    let stats = mux.aggregate_stats();
    assert!(
        stats.events_sent >= u64::from(total_expected),
        "aggregate_stats lagging: stats={stats:?}",
    );

    drop(tx_handles);
    mux.shutdown();
}
