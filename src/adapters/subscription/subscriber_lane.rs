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
use crate::ports::notifier::PeerHandle;
use crate::ports::subscriber_lane::{
    InlineLaneCounters, LaneAdmin, LaneFuture, LanePolicy, SubSummary, SubscriberLaneAsync,
    SubscriberLanePort,
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
            // ADR 0006 Amendment 1 — populated at the resource fan-out
            // level on `MemoryRegistry`; lane-side bridging lands in a
            // follow-up minor (v5.2) so v5.1 stays scoped to the
            // debouncer pipeline.
            byte_triggered_flushes: 0,
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
    /// ADR 0012 phase 4 -- opt-in gate for inline-push delivery.
    ///
    /// Defaults to `false` so the legacy resources-updated fan-out
    /// stays byte-identical on existing lanes. The `sub_open` use case
    /// flips this to `true` later when the client requested inline
    /// push AND the peer `CapabilityRegistry` entry advertises the
    /// `InlinePush` flag. Loads use `Acquire` so they pair with the
    /// `Release` store on the opt-in path; the gate is a hot-path
    /// atomic, never a lock, per ADR 0012 lock-free invariants.
    pub inline_push: AtomicBool,
    /// ADR 0012 phase 4 -- monotonic per-lane `seq` counter for inline
    /// payloads.
    ///
    /// Allocated by the lane-fanout bridge when a fan-out tick produces
    /// an inline payload. Each successful `compose_inline_payload`
    /// consumes one slot via `fetch_add 1 Release`.
    pub inline_seq: AtomicU64,
    /// ADR 0012 phase 5 D2 fix -- cumulative inline notifications
    /// delivered to this lane.
    ///
    /// Pure observability counter, separate from the legacy
    /// `events_sent` so `sub_stats` can differentiate inline vs
    /// pull-mode delivery. Incremented per fragment inside the bridge
    /// `ship_inline_fragments` path. `Relaxed` ordering -- no
    /// read-side happens-before dependency.
    pub inline_events_sent: AtomicU64,
    /// ADR 0012 phase 5 D2 fix -- cumulative raw inline bytes
    /// delivered, pre-base64.
    ///
    /// Mirrors `bytes_sent` but only for the inline-push leg. Folded
    /// per fragment via `fetch_add Relaxed`.
    pub inline_bytes_sent: AtomicU64,
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
    /// Optional rmcp peer the lane should notify on producer events.
    /// Stored as `Option<Arc<dyn PeerHandle>>` so the NDJSON daemon
    /// transport can keep using lanes without a peer concept.
    peer: Option<Arc<dyn PeerHandle>>,
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
        let filter = Arc::new(FilterPipeline::new(policy.filter.clone()));
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
            inline_push: AtomicBool::new(false),
            inline_seq: AtomicU64::new(0),
            inline_events_sent: AtomicU64::new(0),
            inline_bytes_sent: AtomicU64::new(0),
            atomics: LaneAtomics::default(),
            capacity,
            fallback_rx: AsyncMutex::new(Some(rx)),
            peer: policy.peer,
        }
    }

    /// Borrow the optional peer handle. `None` for transports without
    /// a peer concept (NDJSON daemon).
    pub const fn peer(&self) -> Option<&Arc<dyn PeerHandle>> {
        self.peer.as_ref()
    }

    /// Increment lane atomics after a successful producer notify. The
    /// channel-mux drain path is unchanged; this counter increment
    /// runs from the legacy URI broadcast bridge so stats remain
    /// observable on stdio/HTTP transports.
    pub fn record_notify(&self, bytes_added: usize) {
        self.atomics.record_send(bytes_added);
    }

    /// ADR 0012 phase 4 -- flip the inline-push gate.
    ///
    /// Called by the `sub_open` use case once the capability handshake
    /// confirms the peer advertised `ssh_inline_push`. Uses `Release`
    /// ordering so the bridge's `Acquire` load on `inline_push`
    /// happens-after this store. Idempotent; safe to call repeatedly.
    pub fn set_inline_push(&self, enabled: bool) {
        self.inline_push.store(enabled, Ordering::Release);
    }

    /// Borrow the live policy without cloning the inner `Arc`.
    fn current_policy(&self) -> LagPolicy {
        **self.policy.load()
    }

    /// Build a [`SubSummary`] for `sub_list` rendering.
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
    /// Sink fired on lane close so the composition root can unregister
    /// the lane from the channel mux. Mirrors `rx_sink`; without it the
    /// mux lane table leaks one entry per closed lane (BUG #3).
    close_sink: ArcSwap<Option<CloseSink>>,
    /// Reserve-style live lane count. CAS-incremented against
    /// `max_total` BEFORE a lane is allocated and decremented in
    /// `remove_lane`, so two concurrent opens cannot both slip past the
    /// global cap through a racy `self.lanes.len()` read (BUG #25).
    total_lanes: AtomicUsize,
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

/// Closure invoked when a lane closes.
///
/// The composition root installs a sink that unregisters the lane from
/// the channel mux. Without it the mux lane table grows unbounded for the
/// process lifetime because `close_lane` only drops the adapter-local
/// maps.
pub type CloseSink = Box<dyn Fn(&SubId) + Send + Sync>;

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
            close_sink: ArcSwap::from_pointee(None),
            total_lanes: AtomicUsize::new(0),
        })
    }

    /// Install the rx sink. Idempotent: a later install replaces the
    /// previous sink without dropping in-flight lanes.
    pub fn install_rx_sink(&self, sink: RxSink) {
        self.rx_sink.store(Arc::new(Some(sink)));
    }

    /// Install the close sink. Idempotent: a later install replaces the
    /// previous sink. Fired from `close_lane` after the lane is removed
    /// so the composition root can drop the channel-mux registration
    /// and bound the mux lane table (BUG #3).
    pub fn install_close_sink(&self, sink: CloseSink) {
        self.close_sink.store(Arc::new(Some(sink)));
    }

    /// Invoke the installed close sink (if any) for `sub_id`.
    fn fire_close_sink(&self, sub_id: &SubId) {
        let sink = self.close_sink.load_full();
        if let Some(sink) = sink.as_ref() {
            (sink)(sub_id);
        }
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

    /// Public lane snapshot for the legacy broadcast bridge. Returns
    /// every lane bound to `uri`. Used by the `MemoryRegistry`
    /// notifier integration to fan `notifications/resources/updated`
    /// out to lane peers and increment lane stats on stdio/HTTP
    /// transports (the `ssh-mcp-tail` NDJSON daemon keeps using the
    /// channel-mux outbound sink instead).
    pub fn lanes_for_uri_public(&self, uri: &str) -> Vec<Arc<LaneState>> {
        self.snapshot_lanes_for_uri(uri)
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

    /// Reserve one global lane slot, CAS-incrementing `total_lanes`
    /// against `max_total`. Two concurrent opens can no longer both pass
    /// a racy `self.lanes.len()` read (BUG #25). Release via
    /// [`Self::release_total`] on any early return.
    fn reserve_total(&self) -> Result<(), DomainError> {
        let max = usize::from(self.max_total);
        let updated =
            self.total_lanes
                .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |current| {
                    (current < max).then_some(current + 1)
                });
        if updated.is_ok() {
            Ok(())
        } else {
            Err(DomainError::MaxSubsTotalExceeded {
                limit: self.max_total,
            })
        }
    }

    /// Release a previously reserved global lane slot.
    fn release_total(&self) {
        self.total_lanes.fetch_sub(1, Ordering::AcqRel);
    }

    /// Build a fresh lane `Arc` without touching the registry maps.
    fn build_lane_arc(
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
        Arc::new(LaneState::build(
            sub_id,
            kind,
            resource_id.to_string(),
            uri.to_string(),
            policy,
            capacity,
        ))
    }

    /// Enforce the per-URI cap and insert the lane under ONE `by_uri`
    /// shard write guard so the length check and the push are atomic on
    /// that shard (BUG #25). The global slot is reserved separately via
    /// [`Self::reserve_total`] before this call.
    fn allocate_lane_checked(
        &self,
        uri: &str,
        kind: ResourceKind,
        resource_id: &str,
        policy: LanePolicy,
    ) -> Result<Arc<LaneState>, DomainError> {
        let lane_arc = self.build_lane_arc(uri, kind, resource_id, policy);
        let sub_id = lane_arc.sub_id.clone();
        // One shard guard spans the cap check and the push. `self.lanes`
        // is a distinct DashMap, so inserting into it under this guard
        // cannot deadlock the `by_uri` shard; the lane is inserted only
        // after the check passes, so a rejected open leaves no orphan.
        // The guard is scoped to this block so it drops before the return
        // (keeps the critical section tight).
        {
            let mut slot = self.by_uri.entry(uri.to_string()).or_default();
            if slot.len() >= usize::from(self.max_per_uri) {
                return Err(DomainError::MaxSubsPerUriExceeded {
                    uri: uri.to_string(),
                    limit: self.max_per_uri,
                });
            }
            self.lanes.insert(sub_id.clone(), Arc::clone(&lane_arc));
            slot.push(sub_id);
        }
        Ok(lane_arc)
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
        // Release the reserved global slot now the lane is gone (BUG #25).
        self.total_lanes.fetch_sub(1, Ordering::AcqRel);
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
    // BUG #19: DropOldest cannot evict the HEAD from here. The lane
    // handed its mpsc Receiver to the channel-mux drain task at open
    // time, so the producer side only has `try_send` and cannot pop the
    // oldest buffered event. Under sustained overflow this path
    // therefore degrades to newest-drop — the same observable effect as
    // DropNewest — plus a `Lagged` marker. True head-eviction needs a
    // lane-owned ring buffer (tracked as a known limitation). We emit
    // the marker, bump the drop counter, and retry once in case a racing
    // consumer drained a slot between attempts.
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

    fn set_inline_push(&self, sub_id: &SubId, enabled: bool) -> bool {
        self.lanes.get(sub_id).is_some_and(|entry| {
            entry.value().set_inline_push(enabled);
            true
        })
    }

    fn inline_stats(&self, sub_id: &SubId) -> Option<InlineLaneCounters> {
        self.lanes.get(sub_id).map(|entry| {
            let lane = entry.value();
            InlineLaneCounters {
                inline_push: lane.inline_push.load(Ordering::Acquire),
                inline_events_sent: lane.inline_events_sent.load(Ordering::Relaxed),
                inline_bytes_sent: lane.inline_bytes_sent.load(Ordering::Relaxed),
            }
        })
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
        // Validate the filter regex before reserving any capacity so a
        // bad pattern never consumes a global slot.
        if let FilterRule::Regex(pattern) = &policy.filter {
            FilterPipeline::compile_regex(pattern)?;
        }
        // Reserve the global slot first (CAS), then enforce the per-URI
        // cap atomically under one shard guard; release the reservation
        // if the per-URI insert is rejected (BUG #25).
        self.reserve_total()?;
        match self.allocate_lane_checked(&uri, kind, &resource_id, policy) {
            Ok(lane) => {
                self.forward_rx(&lane).await;
                Ok(lane.sub_id.clone())
            }
            Err(e) => {
                self.release_total();
                Err(e)
            }
        }
    }

    async fn close_lane(&self, sub_id: &SubId) -> Result<(), DomainError> {
        if self.remove_lane(sub_id).is_some() {
            // BUG #3: drop the channel-mux registration too, else the
            // mux lane table grows unbounded for the process lifetime.
            self.fire_close_sink(sub_id);
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
                // BUG #5: `cursor` is client-controlled. The lane's
                // `cursor` atomic doubles as the inline-push byte
                // accumulator (`compose_inline_payload` fetch_adds it),
                // so an out-of-range replay cursor must NOT pin it
                // forward. Clamp the request to the bytes actually
                // produced and emit the Snapshot from the clamped
                // target; the live cursor is never advanced past
                // production (no `fetch_max` here on purpose).
                let produced = lane.cursor.load(Ordering::Relaxed);
                let target = cursor.min(produced);
                // Phase 2 emits a Snapshot marker so the consumer can
                // refresh from the resource ring buffer. The actual
                // bytes flow through the standard `produce` path on
                // the next push.
                let _ = lane.tx.try_send(LaneMsg::Snapshot {
                    cursor: target,
                    delta: Vec::new(),
                });
                lane.atomics.record_recovery();
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
            peer: None,
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
    async fn replay_clamps_to_produced_cursor() {
        // BUG #5: replay must not advance the shared byte cursor past the
        // bytes actually produced, so a client-supplied `from_cursor`
        // cannot pin the inline-push accumulator forward.
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
        // Simulate 10 produced bytes so the live cursor sits at 10.
        a.advance_cursor(&sid, "shell://x/output", 10);
        // Replay within the produced window leaves the cursor untouched.
        a.replay_from_cursor(&sid, 5).await.unwrap();
        assert_eq!(a.current_cursor(&sid, "shell://x/output"), 10);
        // An out-of-range replay cursor is clamped, never pinned forward.
        a.replay_from_cursor(&sid, 100_000_000).await.unwrap();
        assert_eq!(a.current_cursor(&sid, "shell://x/output"), 10);
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
                    peer: None,
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
                    peer: None,
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
