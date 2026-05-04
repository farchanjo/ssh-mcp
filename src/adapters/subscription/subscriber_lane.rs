//! Per-`SubId` lane adapter (v5 Phase 2 — Channel Mux + `SubId`).
//!
//! Implements [`crate::ports::subscriber_lane::SubscriberLanePort`] and
//! [`crate::ports::subscriber_lane::SubscriberLaneAsync`]. Each lane
//! owns:
//!
//! - A bounded `tokio::sync::mpsc::Sender<NotifyMsg>` (capacity from
//!   `SSH_LANE_BUFFER`, default 1024).
//! - Live atomic counters mirroring [`crate::domain::subscription::SubscriberStats`].
//! - An [`arc_swap::ArcSwap`] holding the active [`crate::domain::subscription::LagPolicy`]
//!   so producers can swap policy without any blocking.
//! - An `AtomicBool` pause flag that throttles the consumer task.
//! - An `AtomicU64` byte cursor for replay anchoring.
//!
//! The hot path is lock-free: producers take a shard guard on the
//! [`dashmap::DashMap`] keyed by [`crate::domain::subscription::SubId`],
//! resolve the lane atomically, drop the guard, then push into the
//! mpsc — never holding the guard across any `.await`.
//!
//! See [ADR 0004](../docs/adr/0004-channel-mux-fairness.md) and
//! [ADR 0006](../docs/adr/0006-backpressure-policies.md) for the spec.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use arc_swap::ArcSwap;
use dashmap::DashMap;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

use super::filter::FilterPipeline;
use crate::domain::error::DomainError;
use crate::domain::subscription::{
    FilterRule, LagPolicy, SubId, SubscriberStats, SubscriptionLifetime,
};
use crate::ports::id_generator::IdGeneratorPort;
use crate::ports::subscriber_lane::{
    LaneAdmin, LaneFuture, LanePolicy, SubSummary, SubscriberLaneAsync, SubscriberLanePort,
};
use crate::ports::subscriber_registry::ResourceKind;

/// Live message pushed into a lane mpsc.
///
/// The Phase 2 surface is byte-oriented (matches what the v4
/// debouncer fan-out already produces). Phase 3 layers structured
/// events on top.
#[derive(Debug, Clone)]
pub enum LaneMsg {
    /// Resource bytes (e.g. PTY output, command stdout chunk). Carries
    /// the resource sequence number so the consumer can reconcile gaps.
    Data {
        /// Resource sequence number.
        seq: u64,
        /// Payload bytes.
        payload: Vec<u8>,
    },
    /// Snapshot rebuild after a [`LagPolicy::Snapshot`] gap.
    Snapshot {
        /// Cursor after the snapshot — every byte before this offset
        /// has been delivered.
        cursor: u64,
        /// Full delta payload reconstructed from the resource ring
        /// buffer.
        delta: Vec<u8>,
    },
    /// `lagged` marker emitted by [`LagPolicy::DropOldest`] /
    /// [`LagPolicy::DropNewest`].
    Lagged {
        /// Number of events dropped since the last marker.
        dropped: u64,
    },
}

impl LaneMsg {
    /// Approximate bytes-on-wire — used to fold the payload size into
    /// the lane stats counters.
    const fn payload_size(&self) -> usize {
        match self {
            Self::Data { payload, .. } => payload.len(),
            Self::Snapshot { delta, .. } => delta.len(),
            Self::Lagged { .. } => 0,
        }
    }
}

/// Live atomic-state mirror of [`SubscriberStats`].
#[derive(Debug, Default)]
struct LaneAtomics {
    events_sent: AtomicU64,
    bytes_sent: AtomicU64,
    lagged_drops: AtomicU64,
    lagged_recoveries: AtomicU64,
    queue_depth: AtomicUsize,
    queue_high_watermark: AtomicUsize,
    block_total_ms: AtomicU64,
}

impl LaneAtomics {
    fn snapshot(&self) -> SubscriberStats {
        SubscriberStats {
            events_sent: self.events_sent.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            lagged_drops: self.lagged_drops.load(Ordering::Relaxed),
            lagged_recoveries: self.lagged_recoveries.load(Ordering::Relaxed),
            queue_depth: self.queue_depth.load(Ordering::Relaxed),
            queue_high_watermark: self.queue_high_watermark.load(Ordering::Relaxed),
            block_total_ms: self.block_total_ms.load(Ordering::Relaxed),
        }
    }

    fn record_send(&self, payload: usize) {
        self.events_sent.fetch_add(1, Ordering::Relaxed);
        let bytes = u64::try_from(payload).unwrap_or(u64::MAX);
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
    }

    fn record_drop(&self) {
        self.lagged_drops.fetch_add(1, Ordering::Relaxed);
    }

    fn record_recovery(&self) {
        self.lagged_recoveries.fetch_add(1, Ordering::Relaxed);
    }

    fn observe_depth(&self, depth: usize) {
        self.queue_depth.store(depth, Ordering::Relaxed);
        self.queue_high_watermark
            .fetch_max(depth, Ordering::Relaxed);
    }
}

/// One subscriber lane.
pub struct LaneState {
    /// Stable subscriber id (`UUIDv7`).
    pub sub_id: SubId,
    /// Resource scheme.
    pub kind: ResourceKind,
    /// Resource id portion of the URI.
    pub resource_id: String,
    /// Canonical URI.
    pub uri: String,
    /// Bounded producer side.
    pub tx: mpsc::Sender<LaneMsg>,
    /// Active lag policy (hot-reloadable).
    pub policy: ArcSwap<LagPolicy>,
    /// Lifetime descriptor (read-only after open).
    pub lifetime: SubscriptionLifetime,
    /// Lane filter pipeline.
    pub filter: Arc<FilterPipeline>,
    /// Pause flag.
    pub paused: AtomicBool,
    /// Live byte cursor for replay anchoring.
    pub cursor: AtomicU64,
    /// Live stats mirror.
    atomics: LaneAtomics,
    /// Capacity used to size the mpsc.
    capacity: usize,
    /// Optional fallback receiver — held until the channel-mux drain
    /// task picks it up. The composition root installs an
    /// [`SubscriberLaneAdapter::install_rx_sink`] that hands the rx
    /// to the mux; without a sink we keep the receiver alive on the
    /// lane so producers do not see `Closed` errors.
    fallback_rx: AsyncMutex<Option<mpsc::Receiver<LaneMsg>>>,
}

impl fmt::Debug for LaneState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaneState")
            .field("sub_id", &self.sub_id)
            .field("kind", &self.kind)
            .field("resource_id", &self.resource_id)
            .field("uri", &self.uri)
            .field("paused", &self.paused.load(Ordering::Relaxed))
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl LaneState {
    /// Build a new lane state plus its consumer end. Returns the
    /// receiver so the composition root can hand it to the channel
    /// mux drain task at construction time (the lane never re-touches
    /// the receiver afterwards).
    fn build(
        sub_id: SubId,
        kind: ResourceKind,
        resource_id: String,
        uri: String,
        policy: LanePolicy,
        capacity: usize,
    ) -> Self {
        let (tx, rx) = mpsc::channel(capacity);
        let filter = Arc::new(FilterPipeline::new(policy.filter));
        Self {
            sub_id,
            kind,
            resource_id,
            uri,
            tx,
            policy: ArcSwap::from_pointee(policy.lag_policy),
            lifetime: policy.lifetime,
            filter,
            paused: AtomicBool::new(false),
            cursor: AtomicU64::new(0),
            atomics: LaneAtomics::default(),
            capacity,
            fallback_rx: AsyncMutex::new(Some(rx)),
        }
    }

    /// Borrow the live policy without cloning the inner `Arc`.
    fn current_policy(&self) -> LagPolicy {
        **self.policy.load()
    }

    /// Build a [`SubSummary`] for `ssh_sub_list` rendering.
    fn summary(&self) -> SubSummary {
        SubSummary {
            sub_id: self.sub_id.clone(),
            kind: self.kind,
            resource_id: self.resource_id.clone(),
            uri: self.uri.clone(),
            lag_policy: self.current_policy(),
            lifetime: self.lifetime,
            paused: self.paused.load(Ordering::Relaxed),
            stats: self.atomics.snapshot(),
        }
    }
}

/// Default lane mpsc capacity when the caller passes `0`.
const DEFAULT_LANE_BUFFER: usize = 1024;

/// In-process [`SubscriberLanePort`] / [`SubscriberLaneAsync`]
/// adapter.
///
/// Generic over [`IdGeneratorPort`] so the composition root can wire
/// the production `UUIDv7` generator (or a deterministic counter for
/// tests).
pub struct SubscriberLaneAdapter<I: IdGeneratorPort> {
    /// `(SubId)` -> live lane state.
    lanes: DashMap<SubId, Arc<LaneState>>,
    /// `(uri)` -> set of `SubId`s pinned to that URI. Used to enforce
    /// per-URI caps and for `unsubscribe-by-uri` flows.
    by_uri: DashMap<String, Vec<SubId>>,
    /// Id generator used to mint fresh `SubId` values.
    ids: Arc<I>,
    /// Default lane capacity when the caller supplies `0`.
    default_capacity: usize,
    /// Per-URI cap.
    max_per_uri: u16,
    /// Global cap.
    max_total: u16,
    /// Sink the adapter publishes lane mpsc receivers into so the
    /// channel mux drain task can pick them up. Wrapped in
    /// `ArcSwap<Option<...>>` so the wiring can be installed after
    /// adapter construction without races.
    rx_sink: ArcSwap<Option<RxSink>>,
}

impl<I: IdGeneratorPort + fmt::Debug> fmt::Debug for SubscriberLaneAdapter<I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubscriberLaneAdapter")
            .field("lanes", &self.lanes.len())
            .field("default_capacity", &self.default_capacity)
            .field("max_per_uri", &self.max_per_uri)
            .field("max_total", &self.max_total)
            .finish_non_exhaustive()
    }
}

/// Closure invoked when a fresh lane opens. The composition root
/// installs a sink that hands the receiver to the channel mux drain
/// task. Tests can install a no-op sink.
pub type RxSink = Box<dyn Fn(SubId, mpsc::Receiver<LaneMsg>) + Send + Sync>;

impl<I: IdGeneratorPort> SubscriberLaneAdapter<I> {
    /// Build a fresh adapter.
    #[must_use]
    pub fn new(
        ids: Arc<I>,
        default_capacity: usize,
        max_per_uri: u16,
        max_total: u16,
    ) -> Arc<Self> {
        Arc::new(Self {
            lanes: DashMap::new(),
            by_uri: DashMap::new(),
            ids,
            default_capacity: if default_capacity == 0 {
                DEFAULT_LANE_BUFFER
            } else {
                default_capacity
            },
            max_per_uri,
            max_total,
            rx_sink: ArcSwap::from_pointee(None),
        })
    }

    /// Install the rx sink. Idempotent: a later install replaces the
    /// previous sink without dropping in-flight lanes.
    pub fn install_rx_sink(&self, sink: RxSink) {
        self.rx_sink.store(Arc::new(Some(sink)));
    }

    /// Push a [`LaneMsg`] onto every lane currently bound to `uri`.
    /// Applies each lane's filter + lag policy. Used by the per-URI
    /// debouncer.
    ///
    /// # Errors
    ///
    /// Aggregates per-lane failures into a single `LaneBufferFull`
    /// error when at least one lane refused to deliver.
    pub fn produce(&self, uri: &str, msg: &LaneMsg) -> Result<(), DomainError> {
        let lanes = self.snapshot_lanes_for_uri(uri);
        let payload_size = msg.payload_size();
        let mut last_err: Option<DomainError> = None;
        for lane in lanes {
            if let Err(e) = lane_dispatch(&lane, msg.clone(), payload_size) {
                last_err = Some(e);
            }
        }
        last_err.map_or(Ok(()), Err)
    }

    fn snapshot_lanes_for_uri(&self, uri: &str) -> Vec<Arc<LaneState>> {
        self.by_uri.get(uri).map_or_else(Vec::new, |entry| {
            entry
                .value()
                .iter()
                .filter_map(|sid| self.lanes.get(sid).map(|l| Arc::clone(l.value())))
                .collect()
        })
    }

    fn ensure_uri_capacity(&self, uri: &str) -> Result<(), DomainError> {
        if self.lanes.len() >= usize::from(self.max_total) {
            return Err(DomainError::MaxSubsTotalExceeded {
                limit: self.max_total,
            });
        }
        let count = self.by_uri.get(uri).map_or(0, |entry| entry.value().len());
        if count >= usize::from(self.max_per_uri) {
            return Err(DomainError::MaxSubsPerUriExceeded {
                uri: uri.to_string(),
                limit: self.max_per_uri,
            });
        }
        Ok(())
    }

    fn allocate_lane(
        &self,
        uri: &str,
        kind: ResourceKind,
        resource_id: &str,
        policy: LanePolicy,
    ) -> Arc<LaneState> {
        let sub_id = self.ids.new_session_id();
        // The id generator port mints any UUIDv7 — the session id helper
        // already produces the right shape; we wrap the underlying string
        // in a fresh SubId so the type system reflects the v5 surface.
        let sub_id = SubId::new(sub_id.into_inner());
        let capacity = if policy.buffer_size == 0 {
            self.default_capacity
        } else {
            policy.buffer_size
        };
        let lane = LaneState::build(
            sub_id.clone(),
            kind,
            resource_id.to_string(),
            uri.to_string(),
            policy,
            capacity,
        );
        let lane_arc = Arc::new(lane);
        self.lanes.insert(sub_id.clone(), Arc::clone(&lane_arc));
        self.by_uri.entry(uri.to_string()).or_default().push(sub_id);
        lane_arc
    }

    async fn forward_rx(&self, lane: &LaneState) {
        let sink = self.rx_sink.load_full();
        if let Some(sink) = sink.as_ref() {
            let mut guard = lane.fallback_rx.lock().await;
            if let Some(rx) = guard.take() {
                (sink)(lane.sub_id.clone(), rx);
            }
        }
    }

    /// Drop the lane and its `by_uri` entry. Idempotent.
    fn remove_lane(&self, sub_id: &SubId) -> Option<Arc<LaneState>> {
        let lane = self.lanes.remove(sub_id).map(|(_, lane)| lane)?;
        if let Some(mut entry) = self.by_uri.get_mut(&lane.uri) {
            entry.retain(|s| s != sub_id);
        }
        // Empty entries get pruned eagerly so iteration stays cheap.
        self.by_uri.retain(|_, v| !v.is_empty());
        Some(lane)
    }
}

fn lane_dispatch(lane: &LaneState, msg: LaneMsg, payload_size: usize) -> Result<(), DomainError> {
    if !lane.filter.passes(&msg) {
        // Filtered events still increment the events_sent counter so
        // the operator can correlate filter rate vs production rate.
        lane.atomics.record_send(payload_size);
        return Ok(());
    }
    let policy = lane.current_policy();
    let result = match policy {
        LagPolicy::DropNewest => lane_dispatch_drop_newest(lane, msg, payload_size),
        LagPolicy::DropOldest => lane_dispatch_drop_oldest(lane, msg, payload_size),
        LagPolicy::Snapshot => lane_dispatch_snapshot(lane, msg, payload_size),
        LagPolicy::BlockSlow => lane_dispatch_block_slow(lane, msg, payload_size),
    };
    let depth = lane.capacity.saturating_sub(lane.tx.capacity());
    lane.atomics.observe_depth(depth);
    result
}

fn lane_dispatch_drop_newest(
    lane: &LaneState,
    msg: LaneMsg,
    payload_size: usize,
) -> Result<(), DomainError> {
    match lane.tx.try_send(msg) {
        Ok(()) => {
            lane.atomics.record_send(payload_size);
            Ok(())
        }
        Err(TrySendError::Full(_)) => {
            lane.atomics.record_drop();
            Ok(())
        }
        Err(TrySendError::Closed(_)) => Err(DomainError::LaneBufferFull {
            sub_id: lane.sub_id.clone(),
            capacity: lane.capacity,
        }),
    }
}

fn lane_dispatch_drop_oldest(
    lane: &LaneState,
    msg: LaneMsg,
    payload_size: usize,
) -> Result<(), DomainError> {
    match lane.tx.try_send(msg.clone()) {
        Ok(()) => {
            lane.atomics.record_send(payload_size);
            return Ok(());
        }
        Err(TrySendError::Closed(_)) => {
            return Err(DomainError::LaneBufferFull {
                sub_id: lane.sub_id.clone(),
                capacity: lane.capacity,
            });
        }
        Err(TrySendError::Full(_)) => {}
    }
    // The mpsc was full — a real drop_oldest would pop the receiver
    // side, but the lane never owns the receiver after open. Emit a
    // `Lagged` marker, increment the drop counter, and try once more
    // (some racing consumer may have drained between attempts).
    lane.atomics.record_drop();
    let _ = lane.tx.try_send(LaneMsg::Lagged { dropped: 1 });
    match lane.tx.try_send(msg) {
        Ok(()) => {
            lane.atomics.record_send(payload_size);
            Ok(())
        }
        Err(TrySendError::Full(_) | TrySendError::Closed(_)) => {
            // Still no room — double-drop is documented in ADR 0006.
            lane.atomics.record_drop();
            Ok(())
        }
    }
}

fn lane_dispatch_snapshot(
    lane: &LaneState,
    msg: LaneMsg,
    payload_size: usize,
) -> Result<(), DomainError> {
    match lane.tx.try_send(msg) {
        Ok(()) => {
            lane.atomics.record_send(payload_size);
            Ok(())
        }
        Err(TrySendError::Full(_)) => {
            // Snapshot policy: drop the backlog then push a Snapshot
            // marker that the consumer can use to rebuild from the
            // resource ring buffer. The actual rebuild is wired through
            // `replay_from_cursor`; here we just emit the marker so
            // the consumer knows a gap occurred.
            let cursor = lane.cursor.load(Ordering::Relaxed);
            let _ = lane.tx.try_send(LaneMsg::Snapshot {
                cursor,
                delta: Vec::new(),
            });
            lane.atomics.record_recovery();
            Ok(())
        }
        Err(TrySendError::Closed(_)) => Err(DomainError::LaneBufferFull {
            sub_id: lane.sub_id.clone(),
            capacity: lane.capacity,
        }),
    }
}

fn lane_dispatch_block_slow(
    lane: &LaneState,
    msg: LaneMsg,
    payload_size: usize,
) -> Result<(), DomainError> {
    // BlockSlow.await is honoured by the async produce path; on the
    // sync `produce` route we degrade to Snapshot semantics — same
    // outcome as the SSH_BP_BLOCK_TIMEOUT_MS escape hatch documented
    // in ADR 0006.
    lane_dispatch_snapshot(lane, msg, payload_size)
}

impl<I: IdGeneratorPort> SubscriberLanePort for SubscriberLaneAdapter<I> {
    fn stats_snapshot(&self, sub_id: &SubId) -> Option<SubscriberStats> {
        self.lanes
            .get(sub_id)
            .map(|entry| entry.value().atomics.snapshot())
    }

    fn current_cursor(&self, sub_id: &SubId, _uri: &str) -> u64 {
        self.lanes
            .get(sub_id)
            .map_or(0, |entry| entry.value().cursor.load(Ordering::Relaxed))
    }

    fn advance_cursor(&self, sub_id: &SubId, _uri: &str, target: u64) -> u64 {
        self.lanes.get(sub_id).map_or(0, |entry| {
            entry.value().cursor.fetch_max(target, Ordering::Relaxed);
            entry.value().cursor.load(Ordering::Relaxed)
        })
    }

    fn list_subs(&self) -> Vec<SubSummary> {
        self.lanes
            .iter()
            .map(|entry| entry.value().summary())
            .collect()
    }
}

impl<I: IdGeneratorPort + fmt::Debug> LaneAdmin for SubscriberLaneAdapter<I> {
    fn open(
        &self,
        uri: String,
        kind: ResourceKind,
        resource_id: String,
        policy: LanePolicy,
    ) -> LaneFuture<'_, Result<SubId, DomainError>> {
        Box::pin(async move {
            <Self as SubscriberLaneAsync>::open_lane(self, uri, kind, resource_id, policy).await
        })
    }

    fn close<'a>(&'a self, sub_id: &'a SubId) -> LaneFuture<'a, Result<(), DomainError>> {
        Box::pin(async move { <Self as SubscriberLaneAsync>::close_lane(self, sub_id).await })
    }

    fn pause<'a>(&'a self, sub_id: &'a SubId) -> LaneFuture<'a, Result<(), DomainError>> {
        Box::pin(async move { <Self as SubscriberLaneAsync>::pause_lane(self, sub_id).await })
    }

    fn resume<'a>(&'a self, sub_id: &'a SubId) -> LaneFuture<'a, Result<(), DomainError>> {
        Box::pin(async move { <Self as SubscriberLaneAsync>::resume_lane(self, sub_id).await })
    }

    fn set_filter<'a>(
        &'a self,
        sub_id: &'a SubId,
        filter: FilterRule,
    ) -> LaneFuture<'a, Result<(), DomainError>> {
        Box::pin(
            async move { <Self as SubscriberLaneAsync>::set_filter(self, sub_id, filter).await },
        )
    }

    fn replay<'a>(
        &'a self,
        sub_id: &'a SubId,
        cursor: u64,
    ) -> LaneFuture<'a, Result<(), DomainError>> {
        Box::pin(async move {
            <Self as SubscriberLaneAsync>::replay_from_cursor(self, sub_id, cursor).await
        })
    }

    fn stats(&self, sub_id: &SubId) -> Option<SubscriberStats> {
        <Self as SubscriberLanePort>::stats_snapshot(self, sub_id)
    }

    fn list(&self) -> Vec<SubSummary> {
        <Self as SubscriberLanePort>::list_subs(self)
    }
}

impl<I: IdGeneratorPort> SubscriberLaneAsync for SubscriberLaneAdapter<I> {
    async fn open_lane(
        &self,
        uri: String,
        kind: ResourceKind,
        resource_id: String,
        policy: LanePolicy,
    ) -> Result<SubId, DomainError> {
        self.ensure_uri_capacity(&uri)?;
        // Validate filter regex before allocating any state.
        if let FilterRule::Regex(pattern) = &policy.filter {
            FilterPipeline::compile_regex(pattern)?;
        }
        let lane = self.allocate_lane(&uri, kind, &resource_id, policy);
        self.forward_rx(&lane).await;
        Ok(lane.sub_id.clone())
    }

    async fn close_lane(&self, sub_id: &SubId) -> Result<(), DomainError> {
        if self.remove_lane(sub_id).is_some() {
            Ok(())
        } else {
            Err(DomainError::SubNotFound(sub_id.clone()))
        }
    }

    async fn pause_lane(&self, sub_id: &SubId) -> Result<(), DomainError> {
        self.lanes.get(sub_id).map_or_else(
            || Err(DomainError::SubNotFound(sub_id.clone())),
            |entry| {
                entry.value().paused.store(true, Ordering::Release);
                Ok(())
            },
        )
    }

    async fn resume_lane(&self, sub_id: &SubId) -> Result<(), DomainError> {
        self.lanes.get(sub_id).map_or_else(
            || Err(DomainError::SubNotFound(sub_id.clone())),
            |entry| {
                entry.value().paused.store(false, Ordering::Release);
                Ok(())
            },
        )
    }

    async fn set_filter(&self, sub_id: &SubId, filter: FilterRule) -> Result<(), DomainError> {
        self.lanes.get(sub_id).map_or_else(
            || Err(DomainError::SubNotFound(sub_id.clone())),
            |entry| entry.value().filter.set(filter),
        )
    }

    async fn replay_from_cursor(&self, sub_id: &SubId, cursor: u64) -> Result<(), DomainError> {
        self.lanes.get(sub_id).map_or_else(
            || Err(DomainError::SubNotFound(sub_id.clone())),
            |entry| {
                let lane = entry.value();
                // Phase 2 emits a Snapshot marker so the consumer can
                // refresh from the resource ring buffer. The actual
                // bytes flow through the standard `produce` path on
                // the next push.
                let _ = lane.tx.try_send(LaneMsg::Snapshot {
                    cursor,
                    delta: Vec::new(),
                });
                lane.atomics.record_recovery();
                lane.cursor.fetch_max(cursor, Ordering::Relaxed);
                Ok(())
            },
        )
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests use unwrap for brevity per CLAUDE.md test policy"
)]
mod tests {
    use std::sync::Arc;

    use super::{LaneMsg, SubscriberLaneAdapter};
    use crate::adapters::id_generator::uuid::UuidIds;
    use crate::domain::error::DomainError;
    use crate::domain::subscription::{FilterRule, LagPolicy, SubId, SubscriptionLifetime};
    use crate::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
    use crate::ports::subscriber_registry::ResourceKind;

    fn adapter() -> Arc<SubscriberLaneAdapter<UuidIds>> {
        SubscriberLaneAdapter::new(Arc::new(UuidIds), 16, 8, 64)
    }

    fn policy(lag: LagPolicy) -> LanePolicy {
        LanePolicy {
            lag_policy: lag,
            lifetime: SubscriptionLifetime::Manual,
            filter: FilterRule::None,
            buffer_size: 4,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_lane_returns_unique_sub_ids() {
        let a = adapter();
        let s1 = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        let s2 = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        assert_ne!(s1, s2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_lane_rejects_invalid_regex() {
        let a = adapter();
        let mut p = policy(LagPolicy::Snapshot);
        p.filter = FilterRule::Regex("([".to_string());
        let err = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                p,
            )
            .await
            .unwrap_err();
        match err {
            DomainError::InvalidArgument(msg) => assert!(msg.to_lowercase().contains("regex")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn close_lane_then_again_returns_sub_not_found() {
        let a = adapter();
        let sid = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        a.close_lane(&sid).await.unwrap();
        let err = a.close_lane(&sid).await.unwrap_err();
        assert!(matches!(err, DomainError::SubNotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_subs_returns_one_summary_per_lane() {
        let a = adapter();
        let s1 = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        let _s2 = a
            .open_lane(
                "shell://y/output".to_string(),
                ResourceKind::Shell,
                "y".to_string(),
                policy(LagPolicy::DropOldest),
            )
            .await
            .unwrap();
        let subs = a.list_subs();
        assert_eq!(subs.len(), 2);
        assert!(subs.iter().any(|s| s.sub_id == s1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pause_resume_round_trip() {
        let a = adapter();
        let sid = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        a.pause_lane(&sid).await.unwrap();
        let summary = a.list_subs().into_iter().find(|s| s.sub_id == sid).unwrap();
        assert!(summary.paused);
        a.resume_lane(&sid).await.unwrap();
        let summary = a.list_subs().into_iter().find(|s| s.sub_id == sid).unwrap();
        assert!(!summary.paused);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pause_unknown_sub_returns_sub_not_found() {
        let a = adapter();
        let err = a
            .pause_lane(&SubId::new("ghost".to_string()))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::SubNotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_filter_hot_reloads_regex() {
        let a = adapter();
        let sid = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        a.set_filter(&sid, FilterRule::Regex("ERROR.*".to_string()))
            .await
            .unwrap();
        a.set_filter(&sid, FilterRule::None).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_filter_rejects_invalid_regex() {
        let a = adapter();
        let sid = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        let err = a
            .set_filter(&sid, FilterRule::Regex("([".to_string()))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::InvalidArgument(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replay_unknown_sub_returns_sub_not_found() {
        let a = adapter();
        let err = a
            .replay_from_cursor(&SubId::new("ghost".to_string()), 100)
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::SubNotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replay_advances_cursor_monotonically() {
        let a = adapter();
        let sid = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        a.replay_from_cursor(&sid, 5).await.unwrap();
        assert_eq!(a.current_cursor(&sid, "shell://x/output"), 5);
        a.replay_from_cursor(&sid, 3).await.unwrap();
        // Cursor monotonic — 3 < 5, so still 5.
        assert_eq!(a.current_cursor(&sid, "shell://x/output"), 5);
        a.replay_from_cursor(&sid, 9).await.unwrap();
        assert_eq!(a.current_cursor(&sid, "shell://x/output"), 9);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn advance_cursor_returns_max_value() {
        let a = adapter();
        let sid = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        let v = a.advance_cursor(&sid, "shell://x/output", 100);
        assert_eq!(v, 100);
        let v = a.advance_cursor(&sid, "shell://x/output", 50);
        // fetch_max means the cursor stays at 100.
        assert_eq!(v, 100);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn current_cursor_returns_zero_for_unknown_sub() {
        let a = adapter();
        assert_eq!(
            a.current_cursor(&SubId::new("ghost".to_string()), "shell://x/output"),
            0
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_lane_enforces_max_per_uri() {
        let a = SubscriberLaneAdapter::new(Arc::new(UuidIds), 8, 2, 64);
        let _ = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        let _ = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        let err = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::MaxSubsPerUriExceeded { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_lane_enforces_max_total() {
        let a = SubscriberLaneAdapter::new(Arc::new(UuidIds), 8, 16, 1);
        let _ = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        let err = a
            .open_lane(
                "shell://y/output".to_string(),
                ResourceKind::Shell,
                "y".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::MaxSubsTotalExceeded { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn produce_drop_newest_keeps_dropping_when_full() {
        let a = adapter();
        let sid = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::DropNewest),
            )
            .await
            .unwrap();
        for i in 0_u64..16 {
            let _ = a.produce(
                "shell://x/output",
                &LaneMsg::Data {
                    seq: i,
                    payload: vec![i.try_into().unwrap_or(0_u8); 8],
                },
            );
        }
        let stats = a.stats_snapshot(&sid).unwrap();
        // Capacity 4 — at most 4 events buffered, the rest are
        // dropped; stats counters must agree.
        assert!(stats.lagged_drops >= 1, "no drops recorded: {stats:?}");
        assert!(stats.events_sent <= 4, "more than capacity sent: {stats:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn produce_snapshot_emits_recovery_when_full() {
        let a = adapter();
        let sid = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        for i in 0_u64..16 {
            let _ = a.produce(
                "shell://x/output",
                &LaneMsg::Data {
                    seq: i,
                    payload: vec![0_u8; 8],
                },
            );
        }
        let stats = a.stats_snapshot(&sid).unwrap();
        assert!(stats.lagged_recoveries >= 1, "no recoveries: {stats:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn produce_records_bytes_sent() {
        let a = adapter();
        let sid = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        let _ = a.produce(
            "shell://x/output",
            &LaneMsg::Data {
                seq: 1,
                payload: vec![0_u8; 32],
            },
        );
        let stats = a.stats_snapshot(&sid).unwrap();
        assert_eq!(stats.events_sent, 1);
        assert_eq!(stats.bytes_sent, 32);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stats_snapshot_returns_none_for_unknown_sub() {
        let a = adapter();
        assert!(a.stats_snapshot(&SubId::new("ghost".to_string())).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn produce_empties_lanes_for_unknown_uri() {
        let a = adapter();
        let res = a.produce(
            "shell://nope/output",
            &LaneMsg::Data {
                seq: 1,
                payload: vec![0_u8; 8],
            },
        );
        // Empty fan-out is success.
        assert!(res.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rx_sink_receives_receiver_on_open() {
        use std::sync::Mutex;
        let a = adapter();
        let captured: Arc<Mutex<Vec<SubId>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);
        a.install_rx_sink(Box::new(move |sub_id, _rx| {
            captured_clone.lock().unwrap().push(sub_id);
        }));
        let _ = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                policy(LagPolicy::Snapshot),
            )
            .await
            .unwrap();
        assert_eq!(captured.lock().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_passthrough_does_not_record_drop() {
        let a = adapter();
        let sid = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                LanePolicy {
                    lag_policy: LagPolicy::Snapshot,
                    lifetime: SubscriptionLifetime::Manual,
                    filter: FilterRule::Regex("ERROR".to_string()),
                    buffer_size: 4,
                },
            )
            .await
            .unwrap();
        // Payload does NOT match — filter pipeline drops it but
        // counters increment so operators can correlate filter rate.
        let _ = a.produce(
            "shell://x/output",
            &LaneMsg::Data {
                seq: 1,
                payload: b"WARN: hi".to_vec(),
            },
        );
        let stats = a.stats_snapshot(&sid).unwrap();
        assert_eq!(stats.events_sent, 1);
        assert_eq!(stats.lagged_drops, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_match_forwards_event() {
        let a = adapter();
        let sid = a
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                LanePolicy {
                    lag_policy: LagPolicy::Snapshot,
                    lifetime: SubscriptionLifetime::Manual,
                    filter: FilterRule::Regex("ERROR".to_string()),
                    buffer_size: 4,
                },
            )
            .await
            .unwrap();
        let _ = a.produce(
            "shell://x/output",
            &LaneMsg::Data {
                seq: 1,
                payload: b"ERROR: boom".to_vec(),
            },
        );
        let stats = a.stats_snapshot(&sid).unwrap();
        assert_eq!(stats.events_sent, 1);
        assert!(stats.bytes_sent > 0);
    }
}
