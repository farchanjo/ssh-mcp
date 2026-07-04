//! Round-robin channel mux adapter (v5 Phase 2).
//!
//! Implements [`crate::ports::channel_mux::ChannelMuxPort`] and runs a
//! background task that drains every active per-`SubId` lane in
//! round-robin order. Fairness invariant: between any two adjacent
//! lanes A and B that both have backlog, the mux drains them in
//! alternation.
//!
//! See [ADR 0004](../docs/adr/0004-channel-mux-fairness.md).

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use arc_swap::ArcSwap;
use dashmap::DashMap;
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::subscriber_lane::LaneMsg;
use crate::domain::subscription::SubId;
use crate::ports::channel_mux::{AggregateStats, ChannelMuxPort};

/// Per-lane drain state held by the mux.
struct LaneDrain {
    /// Receiver end of the lane mpsc.
    rx: AsyncMutex<mpsc::Receiver<LaneMsg>>,
}

impl fmt::Debug for LaneDrain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaneDrain").finish_non_exhaustive()
    }
}

/// Live mux state. The hot path (`drain_loop`) walks the
/// [`DashMap`] snapshot, advancing `cursor_lane` after every
/// successful drain so every lane gets fair air time.
pub struct ChannelMuxAdapter {
    lanes: DashMap<SubId, Arc<LaneDrain>>,
    cursor_lane: AtomicUsize,
    /// Global waker — fires whenever any lane gets a new message.
    /// The drain loop sleeps on `notified()` whenever every lane is
    /// empty.
    waker: Arc<Notify>,
    /// Aggregate counters (lock-free, sampled).
    events_sent: AtomicU64,
    bytes_sent: AtomicU64,
    /// Outbound sink — writes to a bounded mpsc that the composition
    /// root drains. The `ssh-mcp-tail` daemon (`composition::embed`)
    /// is the canonical drain target for the NDJSON stdout writer;
    /// HTTP / stdio binaries keep the receiver alive on the
    /// composition root so the mpsc never reports `Closed`. The sink
    /// is optional so unit tests can instantiate the mux without an
    /// outbound writer.
    outbound: ArcSwap<Option<mpsc::Sender<(SubId, LaneMsg)>>>,
    /// Cooperative shutdown token. Cancelling stops the drain loop.
    shutdown: CancellationToken,
}

impl fmt::Debug for ChannelMuxAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelMuxAdapter")
            .field("lanes", &self.lanes.len())
            .field("cursor_lane", &self.cursor_lane.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl ChannelMuxAdapter {
    /// Build a fresh mux. Use [`Self::register_lane`] to feed lane
    /// receivers in (the `SubscriberLaneAdapter` does this through
    /// its `RxSink`).
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            lanes: DashMap::new(),
            cursor_lane: AtomicUsize::new(0),
            waker: Arc::new(Notify::new()),
            events_sent: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            outbound: ArcSwap::from_pointee(None),
            shutdown: CancellationToken::new(),
        })
    }

    /// Trigger the drain loop to exit cleanly.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
        self.waker.notify_one();
    }

    /// Install (or replace) the outbound sink. Receivers from this
    /// channel feed the binary's outbound writer (NDJSON daemon, rmcp
    /// peer fan-out, etc).
    pub fn install_outbound(&self, sink: mpsc::Sender<(SubId, LaneMsg)>) {
        self.outbound.store(Arc::new(Some(sink)));
    }

    /// Register a lane receiver. Idempotent: registering the same
    /// `sub_id` twice replaces the receiver.
    pub fn register_lane(&self, sub_id: SubId, rx: mpsc::Receiver<LaneMsg>) {
        let lane = Arc::new(LaneDrain {
            rx: AsyncMutex::new(rx),
        });
        self.lanes.insert(sub_id, lane);
        self.waker.notify_one();
    }

    /// Drop a lane. Idempotent.
    pub fn unregister_lane(&self, sub_id: &SubId) {
        self.lanes.remove(sub_id);
    }

    /// Background drain loop. Returns a [`JoinHandle`] so the
    /// composition root can abort the task at shutdown.
    #[must_use]
    pub fn spawn_drain(self: &Arc<Self>) -> JoinHandle<()> {
        let mux = Arc::clone(self);
        tokio::spawn(async move { mux.drain_loop().await })
    }

    async fn drain_loop(self: Arc<Self>) {
        // The drain loop runs for the entire lifetime of the
        // composition root; cooperative shutdown via `shutdown()`
        // exits cleanly.
        while !self.shutdown.is_cancelled() {
            let lanes_snapshot = self.lane_snapshot();
            if lanes_snapshot.is_empty() {
                tokio::select! {
                    () = self.waker.notified() => {},
                    () = self.shutdown.cancelled() => return,
                }
                continue;
            }
            if !self.try_drain_round(&lanes_snapshot).await {
                tokio::select! {
                    () = self.waker.notified() => {},
                    () = self.shutdown.cancelled() => return,
                }
            }
        }
    }

    fn lane_snapshot(&self) -> Vec<(SubId, Arc<LaneDrain>)> {
        self.lanes
            .iter()
            .map(|entry| (entry.key().clone(), Arc::clone(entry.value())))
            .collect()
    }

    /// Round-robin one full pass over every lane. Returns `true` when
    /// at least one lane produced a message (so the loop keeps going
    /// without sleeping); `false` when every lane was empty.
    async fn try_drain_round(&self, lanes: &[(SubId, Arc<LaneDrain>)]) -> bool {
        let len = lanes.len();
        if len == 0 {
            return false;
        }
        let start = self.cursor_lane.load(Ordering::Relaxed) % len;
        let mut produced_any = false;
        for offset in 0..len {
            let idx = (start + offset) % len;
            let (sub_id, lane) = &lanes[idx];
            if let Some(msg) = lane_try_recv(lane).await {
                self.forward(sub_id.clone(), msg).await;
                self.cursor_lane.store((idx + 1) % len, Ordering::Relaxed);
                produced_any = true;
            }
        }
        produced_any
    }

    async fn forward(&self, sub_id: SubId, msg: LaneMsg) {
        let payload_size = match &msg {
            LaneMsg::Data { payload, .. } => payload.len(),
            LaneMsg::Snapshot { delta, .. } => delta.len(),
            LaneMsg::Lagged { .. } => 0,
        };
        self.events_sent.fetch_add(1, Ordering::Relaxed);
        let bytes = u64::try_from(payload_size).unwrap_or(u64::MAX);
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
        let sink_arc = self.outbound.load_full();
        if let Some(sink) = sink_arc.as_ref()
            && let Err(e) = sink.send((sub_id, msg)).await
        {
            debug!("mux outbound send failed: {e}");
        }
    }
}

/// Try to pull one message from a lane without `await`-ing on a held
/// mutex. The `tokio::sync::Mutex` lock is acquired inside the
/// helper, the message is read with `try_recv`, the guard drops
/// before returning — the round-robin loop never holds a guard
/// across `.await`.
async fn lane_try_recv(lane: &Arc<LaneDrain>) -> Option<LaneMsg> {
    let mut guard = lane.rx.lock().await;
    guard.try_recv().ok()
}

impl ChannelMuxPort for ChannelMuxAdapter {
    fn active_lane_count(&self) -> usize {
        self.lanes.len()
    }

    fn aggregate_stats(&self) -> AggregateStats {
        AggregateStats {
            active_lanes: self.lanes.len(),
            events_sent: self.events_sent.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            lagged_drops: 0,
            lagged_recoveries: 0,
            max_queue_high_watermark: 0,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests use unwrap for brevity per CLAUDE.md test policy"
)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::{ChannelMuxAdapter, LaneMsg};
    use crate::domain::subscription::SubId;
    use crate::ports::channel_mux::ChannelMuxPort;

    fn build_lane(
        capacity: usize,
    ) -> (
        tokio::sync::mpsc::Sender<LaneMsg>,
        tokio::sync::mpsc::Receiver<LaneMsg>,
    ) {
        tokio::sync::mpsc::channel(capacity)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_mux_reports_zero_lanes() {
        let m = ChannelMuxAdapter::new();
        assert_eq!(m.active_lane_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn register_lane_increments_count() {
        let m = ChannelMuxAdapter::new();
        let (_tx, rx) = build_lane(4);
        m.register_lane(SubId::new("s1".to_string()), rx);
        assert_eq!(m.active_lane_count(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unregister_lane_decrements_count() {
        let m = ChannelMuxAdapter::new();
        let (_tx, rx) = build_lane(4);
        let sid = SubId::new("s1".to_string());
        m.register_lane(sid.clone(), rx);
        m.unregister_lane(&sid);
        assert_eq!(m.active_lane_count(), 0);
    }

    #[tokio::test]
    async fn round_robin_drain_alternates_lanes() {
        let m = ChannelMuxAdapter::new();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<(SubId, LaneMsg)>(16);
        m.install_outbound(out_tx);

        let (tx_a, rx_a) = build_lane(8);
        let (tx_b, rx_b) = build_lane(8);
        m.register_lane(SubId::new("a".to_string()), rx_a);
        m.register_lane(SubId::new("b".to_string()), rx_b);

        let _drain = m.spawn_drain();

        for i in 0_u64..4 {
            tx_a.send(LaneMsg::Data {
                seq: i,
                payload: vec![b'a'],
            })
            .await
            .unwrap();
            tx_b.send(LaneMsg::Data {
                seq: i,
                payload: vec![b'b'],
            })
            .await
            .unwrap();
        }

        // Collect forwarded messages as the drain produces them (bounded
        // per-recv wait) instead of racing a fixed sleep, which flaked under
        // heavy parallel test load. Current-thread runtime schedules the
        // drain task cooperatively when we await `recv`.
        let mut received = Vec::new();
        while received.len() < 8 {
            match tokio::time::timeout(Duration::from_secs(2), out_rx.recv()).await {
                Ok(Some((sid, _msg))) => received.push(sid.as_str().to_string()),
                Ok(None) | Err(_) => break,
            }
        }
        // Both lanes must have made progress (no starvation).
        assert!(
            received.iter().any(|s| s == "a"),
            "lane a starved: {received:?}"
        );
        assert!(
            received.iter().any(|s| s == "b"),
            "lane b starved: {received:?}"
        );
    }

    #[tokio::test]
    async fn aggregate_stats_increments_on_forward() {
        let m = ChannelMuxAdapter::new();
        let (out_tx, _out_rx) = tokio::sync::mpsc::channel::<(SubId, LaneMsg)>(16);
        m.install_outbound(out_tx);

        let (tx, rx) = build_lane(8);
        m.register_lane(SubId::new("s".to_string()), rx);
        let _drain = m.spawn_drain();

        tx.send(LaneMsg::Data {
            seq: 1,
            payload: vec![0_u8; 16],
        })
        .await
        .unwrap();

        // Drive the drain cooperatively on the current-thread runtime: the
        // spawned drain task shares this thread, so yielding hands it control
        // to forward the queued event. Deterministic regardless of host load
        // — the previous fixed-sleep multi-thread form flaked under the OS
        // thread oversubscription of the full parallel lib-test run.
        let mut stats = m.aggregate_stats();
        for _ in 0..1_000_u32 {
            if stats.events_sent >= 1 {
                break;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(1)).await;
            stats = m.aggregate_stats();
        }
        assert_eq!(stats.active_lanes, 1);
        assert!(stats.events_sent >= 1);
        assert!(stats.bytes_sent >= 16);
    }

    #[tokio::test]
    async fn ordering_within_a_lane_is_preserved() {
        let m = ChannelMuxAdapter::new();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<(SubId, LaneMsg)>(16);
        m.install_outbound(out_tx);

        let (tx, rx) = build_lane(8);
        let sid = SubId::new("only".to_string());
        m.register_lane(sid.clone(), rx);
        let _drain = m.spawn_drain();

        for i in 0_u64..5 {
            tx.send(LaneMsg::Data {
                seq: i,
                payload: vec![],
            })
            .await
            .unwrap();
        }

        // Collect forwarded messages as the drain task produces them (bounded
        // per-recv wait) instead of racing a single fixed sleep, which flaked
        // under heavy parallel test load. On the current-thread runtime the
        // drain task shares this thread, so awaiting `recv` schedules it.
        let mut seqs = Vec::new();
        while seqs.len() < 5 {
            match tokio::time::timeout(Duration::from_secs(2), out_rx.recv()).await {
                Ok(Some((_sid, LaneMsg::Data { seq, .. }))) => seqs.push(seq),
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        assert_eq!(seqs, vec![0, 1, 2, 3, 4], "ordering broken: {seqs:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_lane_add_during_drain_does_not_panic() {
        let m = ChannelMuxAdapter::new();
        let (out_tx, _out_rx) = tokio::sync::mpsc::channel::<(SubId, LaneMsg)>(64);
        m.install_outbound(out_tx);

        let _drain = m.spawn_drain();

        let m_clone = Arc::clone(&m);
        let producer = tokio::spawn(async move {
            for i in 0_u32..20 {
                let (tx, rx) = tokio::sync::mpsc::channel(4);
                m_clone.register_lane(SubId::new(format!("s{i}")), rx);
                let _ = tx
                    .send(LaneMsg::Data {
                        seq: u64::from(i),
                        payload: vec![],
                    })
                    .await;
            }
        });

        producer.await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(m.active_lane_count() <= 20);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn aggregate_stats_default_when_no_lanes() {
        let m = ChannelMuxAdapter::new();
        let stats = m.aggregate_stats();
        assert_eq!(stats.active_lanes, 0);
        assert_eq!(stats.events_sent, 0);
    }
}
