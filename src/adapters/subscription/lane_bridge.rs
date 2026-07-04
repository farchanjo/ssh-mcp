//! Lane-fanout bridge for the legacy broadcast pipeline.
//!
//! Wakes up `sub_open`-created lanes from the URI broadcast path
//! so stdio/HTTP transports get `notifications/resources/updated`
//! push delivery without a dedicated channel-mux drain task.
//! Composition root constructs one [`LaneFanoutBridge`] holding the
//! shared [`SubscriberLaneAdapter`] and the production
//! [`NotifierPort`], then installs it on the [`MemoryRegistry`] via
//! `install_lane_bridge`. Producer-side [`MemoryRegistry::broadcast`]
//! calls the bridge before the legacy peer fanout, fanning each push
//! out to every lane bound to the URI and incrementing per-lane
//! atomics.
//!
//! ADR 0012 phase 4 layers an inline-push branch on top: when a lane
//! has flipped its `inline_push` gate to `true`, the bridge composes
//! an `InlinePayload` from the bytes-added window, splits it on the
//! `inline_max_bytes` cap, and ships one `notifications/ssh/output`
//! per fragment INSTEAD of the legacy `notifications/resources/updated`.
//! Opt-out lanes keep the legacy fan-out byte-identical.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::time::timeout;
use tracing::debug;

use crate::adapters::config::internal::resolve_inline_push_max_bytes_per_notify;
use crate::adapters::subscription::subscriber_lane::{LaneState, SubscriberLaneAdapter};
use crate::domain::inline_payload::InlinePayload;
use crate::ports::id_generator::IdGeneratorPort;
use crate::ports::notifier::{LaneNotifierBridge, NotifierPort, PeerHandle};

/// Default per-notification byte ceiling for inline-push fragments.
///
/// Mirrors the ADR 0012 negotiated default. Phase 9 will plumb the
/// `SSH_INLINE_PUSH_MAX_BYTES_PER_NOTIFY` env var through composition;
/// phase 4 ships the constant only.
pub const DEFAULT_INLINE_MAX_BYTES: usize = 32 * 1024;

/// Bounded capacity of the per-URI inline-dispatch channel.
///
/// ADR 0012 phase 9 (BUG #6 fix): the single ordered consumer drains
/// this channel FIFO. The bound is what turns a fast-producer /
/// slow-peer pairing into backpressure (drop-newest per the lane
/// `LagPolicy`) instead of unbounded in-flight task/memory growth.
const INLINE_DISPATCH_CHANNEL_CAP: usize = 1024;

/// Idle interval after which an inline consumer re-checks whether its
/// URI still has live lanes. A consumer parked on an empty channel with
/// no remaining lanes reaps itself and exits, bounding teardown latency
/// so short-lived resource URIs do not accumulate parked tasks.
const INLINE_CONSUMER_IDLE_CHECK: Duration = Duration::from_secs(30);

/// Per-URI ordered send side, keyed by canonical URI. Each entry is
/// drained by exactly one long-lived consumer task spawned on first
/// inline dispatch for that URI.
type InlineChannels = DashMap<String, mpsc::Sender<InlineDispatch>>;

/// A pre-composed inline-push work item.
///
/// ADR 0012 phase 9 (B2 ordering fix): the `seq`/`cursor_after` inside
/// every fragment are minted synchronously on the producer thread by
/// [`LaneFanoutBridge::prepare_inline_dispatch`]; the only work left is
/// the async notifier send. BUG #6 fix: rather than one detached task
/// per producer write (whose sends race on the multi-thread runtime and
/// can reverse seq order), the pre-minted item is pushed into a bounded
/// per-URI channel and shipped by a single ordered consumer, so items
/// are also SENT in the producer-determined order.
struct InlineDispatch {
    lane: Arc<LaneState>,
    peer: Arc<dyn PeerHandle>,
    /// Pre-split, pre-numbered inline fragments. BUG #4 fix: the split
    /// and the seq reservation happen together at compose time so a
    /// multi-fragment write reserves EXACTLY as many seq slots as it
    /// emits — a single `fetch_add(1)` would let a trailing fragment
    /// reuse the next write's seq.
    fragments: Vec<InlinePayload>,
}

/// Concrete [`LaneNotifierBridge`] implementation.
///
/// Generic over the id generator and the notifier adapter so the
/// production wiring stays free of `Box<dyn Trait>` for the hot path.
pub struct LaneFanoutBridge<I, N>
where
    I: IdGeneratorPort,
    N: NotifierPort + Send + Sync + 'static,
{
    lanes: Arc<SubscriberLaneAdapter<I>>,
    notifier: Arc<N>,
    /// Per-notification byte ceiling enforced by `InlinePayload::split`
    /// when an opt-in lane composes an inline payload. Plumbed in from
    /// composition; phase 9 wires the env var.
    inline_max_bytes: usize,
    /// BUG #6 fix: per-URI ordered inline-dispatch channels. Producers
    /// push pre-minted [`InlineDispatch`] items here; one consumer per
    /// URI drains FIFO so delivery order matches producer order and the
    /// bounded channel applies backpressure instead of spawning an
    /// unbounded number of racing send tasks. `Arc` so the spawned
    /// consumer can hold a handle to reap its own entry on teardown.
    inline_channels: Arc<InlineChannels>,
}

impl<I, N> fmt::Debug for LaneFanoutBridge<I, N>
where
    I: IdGeneratorPort,
    N: NotifierPort + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaneFanoutBridge")
            .field("inline_max_bytes", &self.inline_max_bytes)
            .field("inline_channels", &self.inline_channels.len())
            .finish_non_exhaustive()
    }
}

impl<I, N> LaneFanoutBridge<I, N>
where
    I: IdGeneratorPort,
    N: NotifierPort + Send + Sync + 'static,
{
    /// Construct a bridge with the default inline cap
    /// [`DEFAULT_INLINE_MAX_BYTES`]. Kept as a convenience for fakes
    /// and tests that do not want to touch env vars; the production
    /// composition root calls [`Self::from_env`] instead.
    #[must_use]
    pub fn new(lanes: Arc<SubscriberLaneAdapter<I>>, notifier: Arc<N>) -> Arc<Self> {
        Self::with_inline_max(lanes, notifier, DEFAULT_INLINE_MAX_BYTES)
    }

    /// Construct a bridge resolving the inline cap from the env var
    /// `SSH_INLINE_PUSH_MAX_BYTES_PER_NOTIFY`. ADR 0012 phase 9
    /// production entrypoint.
    #[must_use]
    pub fn from_env(lanes: Arc<SubscriberLaneAdapter<I>>, notifier: Arc<N>) -> Arc<Self> {
        Self::with_inline_max(lanes, notifier, resolve_inline_push_max_bytes_per_notify())
    }

    /// Construct a bridge with an explicit inline-payload byte cap.
    /// `0` is treated as "no split" by `InlinePayload::split`.
    #[must_use]
    pub fn with_inline_max(
        lanes: Arc<SubscriberLaneAdapter<I>>,
        notifier: Arc<N>,
        inline_max_bytes: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            lanes,
            notifier,
            inline_max_bytes,
            inline_channels: Arc::new(DashMap::new()),
        })
    }

    /// Compose an inline payload for `lane` from `bytes_added`.
    ///
    /// Returns `None` when the lane has not opted in or when the
    /// window is empty. Pure data composition: no `.await`, no locks,
    /// no shard guards held. The `Acquire` load on `inline_push`
    /// pairs with the `Release` store in `LaneState::set_inline_push`.
    ///
    /// ADR 0012 phase 5 (D1 fix) -- cursor source is the lane's
    /// existing `cursor` byte-anchor atomic, advanced atomically here
    /// via `fetch_add(len, Release)`. Both the legacy
    /// `notifications/resources/updated` path and the inline path read
    /// the post-add value, so pull-mode and inline-mode hosts converge
    /// on the same byte cursor. The bumped cursor value is also
    /// available to `record_notify` via the existing atomic state.
    fn compose_inline_payload(
        lane: &LaneState,
        uri: &str,
        bytes_added: &[u8],
        inline_max_bytes: usize,
    ) -> Vec<InlinePayload> {
        if !lane.inline_push.load(Ordering::Acquire) {
            return Vec::new();
        }
        if bytes_added.is_empty() {
            return Vec::new();
        }
        let len = u64::try_from(bytes_added.len()).unwrap_or(u64::MAX);
        let prev = lane.cursor.fetch_add(len, Ordering::Release);
        let cursor_after = prev.saturating_add(len);
        // BUG #4 fix -- split FIRST (with a placeholder base seq), then
        // reserve EXACTLY `fragments.len()` seq slots and renumber. The
        // UTF-8 back-walk can yield more fragments than a
        // `div_ceil(len, max)` estimate, so the count must come from the
        // real split; a single `fetch_add(1)` would let a trailing
        // fragment reuse the next write's seq.
        let placeholder = InlinePayload::new(
            lane.sub_id.clone(),
            uri.to_string(),
            0,
            cursor_after,
            bytes_added.to_vec(),
            false,
        );
        let mut fragments = placeholder.split(inline_max_bytes, is_text_uri(uri));
        let reserve = u64::try_from(fragments.len()).unwrap_or(u64::MAX);
        let base = lane.inline_seq.fetch_add(reserve, Ordering::Release);
        for (offset, fragment) in fragments.iter_mut().enumerate() {
            fragment.seq = base.saturating_add(u64::try_from(offset).unwrap_or(u64::MAX));
        }
        fragments
    }

    /// Inline-aware fan-out. Phase 4 production caller is the trait
    /// method `notify_lanes`, which passes `&[]` for `bytes_added`
    /// because the legacy `MemoryRegistry::broadcast` pipeline only
    /// carries byte counters. Tests drive this method directly with
    /// real byte slices.
    ///
    /// ADR 0012 phase 5 (D3 fix) -- legacy fan-out always fires on
    /// the existing debouncer cadence. When the lane opted in AND
    /// composed a payload, inline notifications fire IN ADDITION,
    /// never as a replacement.
    pub async fn notify_lanes_with_bytes(&self, uri: &str, bytes_added: &[u8]) {
        let lanes = self.lanes.lanes_for_uri_public(uri);
        for lane in lanes {
            self.notify_one_lane(&lane, uri, bytes_added).await;
        }
    }

    /// ADR 0012 phase 9 — inline-only producer hook. Fans out the
    /// real byte tail to every opt-in inline lane bound to `uri`
    /// WITHOUT firing the legacy `resources/updated` notification.
    /// The legacy fan-out still rides the debouncer-driven
    /// [`Self::notify_lanes_with_bytes`] (called with `&[]`), so
    /// opt-out subscribers continue to see byte-identical traffic.
    pub async fn notify_lanes_inline_bytes(&self, uri: &str, bytes_added: &[u8]) {
        if bytes_added.is_empty() {
            return;
        }
        let lanes = self.lanes.lanes_for_uri_public(uri);
        for lane in lanes {
            self.notify_one_lane_inline_only(&lane, uri, bytes_added)
                .await;
        }
    }

    /// ADR 0012 phase 9 (B2 ordering fix) — mint the per-lane
    /// `seq`/`cursor_after` for every opt-in lane bound to `uri`
    /// SYNCHRONOUSLY, returning ready-to-ship [`InlineDispatch`] work
    /// items. The `fetch_add`s inside [`Self::compose_inline_payload`]
    /// run on the caller thread in producer-call order, so the ordering
    /// of the resulting seq/cursor is producer-determined even when the
    /// actual `.await` send is deferred to a spawned task. No `.await`,
    /// no shard guard held.
    fn prepare_inline_dispatch(&self, uri: &str, bytes_added: &[u8]) -> Vec<InlineDispatch> {
        if bytes_added.is_empty() {
            return Vec::new();
        }
        let lanes = self.lanes.lanes_for_uri_public(uri);
        let mut dispatches: Vec<InlineDispatch> = Vec::with_capacity(lanes.len());
        for lane in lanes {
            let Some(peer) = lane.peer().map(Arc::clone) else {
                continue;
            };
            let fragments =
                Self::compose_inline_payload(&lane, uri, bytes_added, self.inline_max_bytes);
            if fragments.is_empty() {
                continue;
            }
            dispatches.push(InlineDispatch {
                lane,
                peer,
                fragments,
            });
        }
        dispatches
    }

    async fn notify_one_lane(&self, lane: &LaneState, uri: &str, bytes_added: &[u8]) {
        let Some(peer) = lane.peer().map(Arc::clone) else {
            return;
        };
        let fragments = Self::compose_inline_payload(lane, uri, bytes_added, self.inline_max_bytes);
        if !fragments.is_empty() {
            Self::ship_inline_fragments(&self.notifier, lane, Arc::clone(&peer), fragments).await;
        }
        self.ship_legacy_notify(lane, peer, uri, bytes_added).await;
    }

    async fn notify_one_lane_inline_only(&self, lane: &LaneState, uri: &str, bytes_added: &[u8]) {
        let Some(peer) = lane.peer().map(Arc::clone) else {
            return;
        };
        let fragments = Self::compose_inline_payload(lane, uri, bytes_added, self.inline_max_bytes);
        if fragments.is_empty() {
            return;
        }
        Self::ship_inline_fragments(&self.notifier, lane, peer, fragments).await;
    }

    async fn ship_inline_fragments(
        notifier: &Arc<N>,
        lane: &LaneState,
        peer: Arc<dyn PeerHandle>,
        fragments: Vec<InlinePayload>,
    ) {
        let peer_id = peer.id();
        for fragment in fragments {
            let frag_len = fragment.bytes.len();
            let peer_for_frag = Arc::clone(&peer);
            if let Err(err) = notifier.notify_ssh_output(peer_for_frag, fragment).await {
                debug!("lane inline notify failed for peer {peer_id}: {err}");
                return;
            }
            // ADR 0012 phase 5 D2 fix -- separate counters from the
            // legacy `events_sent` / `bytes_sent` so `sub_stats` can
            // differentiate the two delivery legs. Relaxed: no
            // happens-before with any reader.
            lane.inline_events_sent.fetch_add(1, Ordering::Relaxed);
            let frag_bytes = u64::try_from(frag_len).unwrap_or(u64::MAX);
            lane.inline_bytes_sent
                .fetch_add(frag_bytes, Ordering::Relaxed);
        }
    }

    async fn ship_legacy_notify(
        &self,
        lane: &LaneState,
        peer: Arc<dyn PeerHandle>,
        uri: &str,
        bytes_added: &[u8],
    ) {
        let peer_id = peer.id();
        if let Err(err) = self.notifier.notify_resource_updated(peer, uri).await {
            debug!("lane peer notify failed for peer {peer_id}: {err}");
            return;
        }
        lane.record_notify(bytes_added.len());
    }
}

/// BUG #6 fix — ordered inline-dispatch delivery.
///
/// The producer hook mints `seq`/`cursor` synchronously (see
/// [`LaneFanoutBridge::prepare_inline_dispatch`]) then hands each item
/// to a single per-URI FIFO consumer. Because same-URI producer writes
/// arrive sequentially from one resource reader task, pushing into the
/// per-URI channel in that order and draining it with one consumer makes
/// delivery order equal producer order. The bounded channel turns a
/// slow peer into backpressure rather than unbounded in-flight tasks.
///
/// These methods spawn a task that captures `Arc<SubscriberLaneAdapter>`,
/// so they require the stronger `I: Send + Sync + 'static` bound that the
/// [`LaneNotifierBridge`] impl already carries.
impl<I, N> LaneFanoutBridge<I, N>
where
    I: IdGeneratorPort + Send + Sync + 'static,
    N: NotifierPort + Send + Sync + 'static,
{
    /// Synchronous producer entry for the inline path. Mints each opt-in
    /// lane's `seq`/`cursor` in producer-call order via
    /// [`Self::prepare_inline_dispatch`] (no `.await` before the
    /// `fetch_add`), then enqueues the pre-minted items onto the per-URI
    /// ordered channel. The single consumer sends them strictly in
    /// receive order, so two back-to-back writes to the same `uri` are
    /// both MINTED and SENT in order.
    fn dispatch_inline_now(&self, uri: &str, bytes_added: &[u8]) {
        let dispatches = self.prepare_inline_dispatch(uri, bytes_added);
        if dispatches.is_empty() {
            return;
        }
        let tx = self.inline_channel_for(uri);
        for dispatch in dispatches {
            Self::enqueue_dispatch(&tx, dispatch);
        }
    }

    /// Resolve (or lazily create) the ordered channel for `uri`. On a
    /// cold URI a fresh bounded channel is created and its consumer task
    /// spawned; on a warm URI the existing sender is cloned. A lost race
    /// on first creation self-heals: the loser's receiver has no live
    /// sender left, so its consumer exits immediately.
    fn inline_channel_for(&self, uri: &str) -> mpsc::Sender<InlineDispatch> {
        if let Some(tx) = self.inline_channels.get(uri).map(|r| r.value().clone()) {
            return tx;
        }
        let (tx, rx) = mpsc::channel(INLINE_DISPATCH_CHANNEL_CAP);
        self.spawn_inline_consumer(uri.to_string(), rx);
        let entry = self.inline_channels.entry(uri.to_string()).or_insert(tx);
        let stored = entry.value().clone();
        drop(entry);
        stored
    }

    /// Push one pre-minted dispatch onto the ordered channel. `try_send`
    /// keeps the synchronous producer non-blocking; a full channel is
    /// backpressure (bounded memory) — the newest fragment is dropped and
    /// logged with the lane's `LagPolicy`, never silently. A closed
    /// channel means the consumer already reaped a lane-less URI, so the
    /// target subscriber is gone and dropping is correct.
    fn enqueue_dispatch(tx: &mpsc::Sender<InlineDispatch>, dispatch: InlineDispatch) {
        match tx.try_send(dispatch) {
            Err(TrySendError::Full(dropped)) => {
                let policy = **dropped.lane.policy.load();
                let base_seq = dropped.fragments.first().map_or(0, |f| f.seq);
                debug!(
                    "inline backpressure: lane {} channel full (policy {policy:?}, base seq {base_seq}); dropping newest fragments to bound memory",
                    dropped.lane.sub_id.as_str(),
                );
            }
            Ok(()) | Err(TrySendError::Closed(_)) => {}
        }
    }

    /// Spawn the long-lived per-URI consumer. Captures only `Arc` clones
    /// so the task stays `'static` without borrowing the bridge.
    fn spawn_inline_consumer(&self, uri: String, rx: mpsc::Receiver<InlineDispatch>) {
        let notifier = Arc::clone(&self.notifier);
        let lanes = Arc::clone(&self.lanes);
        let channels = Arc::clone(&self.inline_channels);
        tokio::spawn(async move {
            Self::run_inline_consumer(&notifier, &lanes, &channels, uri, rx).await;
        });
    }

    /// Drain the per-URI channel FIFO, shipping each dispatch in order.
    ///
    /// Ordering: producers for a given URI enqueue sequentially, so
    /// receive order equals producer order; awaiting each send in
    /// `recv()` order preserves it end-to-end.
    ///
    /// Teardown: `recv()` returns `None` once every sender is dropped;
    /// additionally, after [`INLINE_CONSUMER_IDLE_CHECK`] of idle the
    /// consumer reaps itself iff the URI has no live lanes. The
    /// `remove_if` predicate runs under the channels-shard write lock, so
    /// a concurrent producer's `inline_channel_for` is serialized against
    /// the removal: a URI with a live lane is never torn down under a
    /// racing producer.
    async fn run_inline_consumer(
        notifier: &Arc<N>,
        lanes: &Arc<SubscriberLaneAdapter<I>>,
        channels: &Arc<InlineChannels>,
        uri: String,
        mut rx: mpsc::Receiver<InlineDispatch>,
    ) {
        loop {
            match timeout(INLINE_CONSUMER_IDLE_CHECK, rx.recv()).await {
                Ok(Some(dispatch)) => {
                    Self::ship_one(notifier, dispatch).await;
                }
                Ok(None) => break,
                Err(_elapsed) => {
                    let removed = channels
                        .remove_if(&uri, |_, _| lanes.lanes_for_uri_public(&uri).is_empty());
                    if removed.is_some() {
                        // The URI had no lanes; drain any last-moment
                        // enqueue for safety, then exit.
                        while let Ok(dispatch) = rx.try_recv() {
                            Self::ship_one(notifier, dispatch).await;
                        }
                        break;
                    }
                }
            }
        }
    }

    /// Ship a single pre-composed dispatch's fragments in order.
    async fn ship_one(notifier: &Arc<N>, dispatch: InlineDispatch) {
        let InlineDispatch {
            lane,
            peer,
            fragments,
        } = dispatch;
        Self::ship_inline_fragments(notifier, &lane, peer, fragments).await;
    }
}

/// Whether a resource URI carries UTF-8 text. ADR 0012 phase 4 limits
/// inline push to `shell://`, `command://`, and `serial://` URIs; the
/// binary schemes skip the UTF-8 back-walk so the splitter falls back
/// to the raw byte boundary.
fn is_text_uri(uri: &str) -> bool {
    uri.starts_with("shell://") || uri.starts_with("command://") || uri.starts_with("serial://")
}

impl<I, N> LaneNotifierBridge for LaneFanoutBridge<I, N>
where
    I: IdGeneratorPort + Send + Sync + 'static,
    N: NotifierPort + Send + Sync + 'static,
{
    fn notify_lanes<'a>(
        &'a self,
        uri: &'a str,
        _bytes_added: usize,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        // ADR 0012 phase 9 -- the legacy broadcast pipeline now uses
        // this method only for the legacy `resources/updated`
        // delivery on the debouncer cadence. Producers that have the
        // real byte tail in scope drive the inline branch via
        // [`Self::notify_lanes_inline_bytes`] (trait method
        // `notify_lanes_inline`) synchronously, BEFORE the byte
        // counter feeds the debouncer. Calling
        // `notify_lanes_with_bytes(uri, &[])` here keeps the
        // legacy-notify code path unchanged while skipping the inline
        // branch (`compose_inline_payload` returns `None` on empty
        // slice).
        Box::pin(async move { self.notify_lanes_with_bytes(uri, &[]).await })
    }

    fn notify_lanes_inline(&self, uri: &str, bytes_added: &[u8]) {
        // ADR 0012 phase 9 (B2 ordering fix) -- mint seq/cursor
        // synchronously here (producer-call order), spawn only the send.
        self.dispatch_inline_now(uri, bytes_added);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests use unwrap/expect for brevity per CLAUDE.md test policy"
)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::{DEFAULT_INLINE_MAX_BYTES, LaneFanoutBridge};
    use crate::adapters::id_generator::uuid::UuidIds;
    use crate::adapters::subscription::subscriber_lane::{LaneState, SubscriberLaneAdapter};
    use crate::domain::error::DomainError;
    use crate::domain::ids::PeerId;
    use crate::domain::inline_payload::InlinePayload;
    use crate::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
    use crate::ports::notifier::{LaneNotifierBridge, NotifierPort, PeerHandle};
    use crate::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync};
    use crate::ports::subscriber_registry::ResourceKind;

    #[derive(Debug, Default)]
    struct RecordingNotifier {
        resource_events: Mutex<Vec<(PeerId, String)>>,
        inline_events: Mutex<Vec<(PeerId, InlinePayload)>>,
    }

    impl NotifierPort for RecordingNotifier {
        async fn notify_resource_updated(
            &self,
            peer: Arc<dyn PeerHandle>,
            uri: &str,
        ) -> Result<(), DomainError> {
            self.resource_events
                .lock()
                .unwrap()
                .push((peer.id(), uri.to_string()));
            Ok(())
        }

        async fn notify_ssh_output(
            &self,
            peer: Arc<dyn PeerHandle>,
            payload: InlinePayload,
        ) -> Result<(), DomainError> {
            self.inline_events
                .lock()
                .unwrap()
                .push((peer.id(), payload));
            Ok(())
        }
    }

    #[derive(Debug)]
    struct StubPeer {
        id: PeerId,
        closed: AtomicBool,
    }

    impl StubPeer {
        fn new(id: &str) -> Arc<Self> {
            Arc::new(Self {
                id: PeerId::new(id.to_string()),
                closed: AtomicBool::new(false),
            })
        }
    }

    impl PeerHandle for StubPeer {
        fn id(&self) -> PeerId {
            self.id.clone()
        }

        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::Relaxed)
        }
    }

    fn ids() -> Arc<UuidIds> {
        Arc::new(UuidIds)
    }

    fn adapter() -> Arc<SubscriberLaneAdapter<UuidIds>> {
        SubscriberLaneAdapter::new(ids(), 16, 8, 64)
    }

    fn policy(peer: Arc<dyn PeerHandle>) -> LanePolicy {
        LanePolicy {
            lag_policy: LagPolicy::Snapshot,
            lifetime: SubscriptionLifetime::Manual,
            filter: FilterRule::None,
            buffer_size: 4,
            peer: Some(peer),
        }
    }

    async fn open_lane(
        a: &SubscriberLaneAdapter<UuidIds>,
        peer: Arc<dyn PeerHandle>,
        uri: &str,
    ) -> Arc<LaneState> {
        let sid = a
            .open_lane(
                uri.to_string(),
                ResourceKind::Shell,
                "test".to_string(),
                policy(peer),
            )
            .await
            .unwrap();
        a.lanes_for_uri_public(uri)
            .into_iter()
            .find(|l| l.sub_id == sid)
            .expect("lane present after open")
    }

    fn bridge(
        a: Arc<SubscriberLaneAdapter<UuidIds>>,
        n: Arc<RecordingNotifier>,
        inline_max: usize,
    ) -> Arc<LaneFanoutBridge<UuidIds, RecordingNotifier>> {
        LaneFanoutBridge::with_inline_max(a, n, inline_max)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn compose_returns_none_when_inline_push_off() {
        let a = adapter();
        let lane = open_lane(&a, StubPeer::new("p1"), "shell://x/output").await;
        let out = LaneFanoutBridge::<UuidIds, RecordingNotifier>::compose_inline_payload(
            &lane,
            "shell://x/output",
            &[1, 2, 3],
            DEFAULT_INLINE_MAX_BYTES,
        );
        assert!(
            out.is_empty(),
            "expected no fragments when inline_push is off"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn compose_returns_payload_when_inline_push_on() {
        let a = adapter();
        let lane = open_lane(&a, StubPeer::new("p1"), "shell://x/output").await;
        lane.set_inline_push(true);
        let fragments = LaneFanoutBridge::<UuidIds, RecordingNotifier>::compose_inline_payload(
            &lane,
            "shell://x/output",
            b"hello",
            DEFAULT_INLINE_MAX_BYTES,
        );
        assert_eq!(fragments.len(), 1);
        let payload = &fragments[0];
        assert_eq!(payload.bytes, b"hello".to_vec());
        assert_eq!(payload.uri, "shell://x/output");
        assert_eq!(payload.seq, 0);
        assert_eq!(payload.cursor_after, 5);
        assert!(!payload.truncated);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seq_monotonic_per_lane() {
        let a = adapter();
        let lane = open_lane(&a, StubPeer::new("p1"), "shell://x/output").await;
        lane.set_inline_push(true);
        let p0 = LaneFanoutBridge::<UuidIds, RecordingNotifier>::compose_inline_payload(
            &lane,
            "shell://x/output",
            b"a",
            DEFAULT_INLINE_MAX_BYTES,
        );
        let p1 = LaneFanoutBridge::<UuidIds, RecordingNotifier>::compose_inline_payload(
            &lane,
            "shell://x/output",
            b"b",
            DEFAULT_INLINE_MAX_BYTES,
        );
        let p2 = LaneFanoutBridge::<UuidIds, RecordingNotifier>::compose_inline_payload(
            &lane,
            "shell://x/output",
            b"c",
            DEFAULT_INLINE_MAX_BYTES,
        );
        assert_eq!(p0[0].seq, 0);
        assert_eq!(p1[0].seq, 1);
        assert_eq!(p2[0].seq, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cursor_monotonic_per_lane() {
        let a = adapter();
        let lane = open_lane(&a, StubPeer::new("p1"), "shell://x/output").await;
        lane.set_inline_push(true);
        let p0 = LaneFanoutBridge::<UuidIds, RecordingNotifier>::compose_inline_payload(
            &lane,
            "shell://x/output",
            &vec![0_u8; 10],
            DEFAULT_INLINE_MAX_BYTES,
        );
        let p1 = LaneFanoutBridge::<UuidIds, RecordingNotifier>::compose_inline_payload(
            &lane,
            "shell://x/output",
            &vec![0_u8; 20],
            DEFAULT_INLINE_MAX_BYTES,
        );
        let p2 = LaneFanoutBridge::<UuidIds, RecordingNotifier>::compose_inline_payload(
            &lane,
            "shell://x/output",
            &vec![0_u8; 5],
            DEFAULT_INLINE_MAX_BYTES,
        );
        assert_eq!(p0[0].cursor_after, 10);
        assert_eq!(p1[0].cursor_after, 30);
        assert_eq!(p2[0].cursor_after, 35);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn multi_fragment_write_reserves_contiguous_non_overlapping_seq() {
        // BUG #4 regression: a write that splits into N fragments must
        // reserve N seq slots, so the NEXT write starts at base + N and
        // never reuses a seq already assigned to a trailing fragment.
        let a = adapter();
        let lane = open_lane(&a, StubPeer::new("peer-bug4"), "shell://x/output").await;
        lane.set_inline_push(true);
        // 64 KiB at a 32 KiB cap -> 2 fragments, seq 0 and 1.
        let first = LaneFanoutBridge::<UuidIds, RecordingNotifier>::compose_inline_payload(
            &lane,
            "shell://x/output",
            &vec![b'a'; 64 * 1024],
            32 * 1024,
        );
        // The very next write must start at seq 2, not reuse seq 1.
        let second = LaneFanoutBridge::<UuidIds, RecordingNotifier>::compose_inline_payload(
            &lane,
            "shell://x/output",
            b"tail",
            32 * 1024,
        );
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].seq, 0);
        assert_eq!(first[1].seq, 1);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].seq, 2, "next write must not reuse seq 1");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prepare_inline_dispatch_mints_seq_and_cursor_in_producer_order() {
        // ADR 0012 phase 9 (B2 ordering fix) -- two back-to-back
        // producer writes to the SAME uri must have their seq/cursor
        // minted synchronously in call order by `prepare_inline_dispatch`
        // (the fetch_add runs on the caller thread, NOT inside the
        // spawned send task). This guards against the regression where
        // the assignment happened at task-execution time and the
        // multi-thread runtime could reverse the two fragments.
        let a = adapter();
        let n = Arc::new(RecordingNotifier::default());
        let b = bridge(Arc::clone(&a), Arc::clone(&n), DEFAULT_INLINE_MAX_BYTES);
        let lane = open_lane(&a, StubPeer::new("peer-order"), "shell://x/output").await;
        lane.set_inline_push(true);

        let first = b.prepare_inline_dispatch("shell://x/output", b"AAAA");
        let second = b.prepare_inline_dispatch("shell://x/output", b"BBBBBB");

        assert_eq!(first.len(), 1, "one opt-in lane -> one dispatch");
        assert_eq!(second.len(), 1, "one opt-in lane -> one dispatch");
        // Small writes -> exactly one fragment each.
        assert_eq!(first[0].fragments.len(), 1);
        assert_eq!(second[0].fragments.len(), 1);
        // seq is producer-ordered: first write gets 0, second gets 1.
        assert_eq!(first[0].fragments[0].seq, 0);
        assert_eq!(second[0].fragments[0].seq, 1);
        // cursor accumulates in producer order: 4 then 4 + 6 = 10.
        assert_eq!(first[0].fragments[0].cursor_after, 4);
        assert_eq!(second[0].fragments[0].cursor_after, 10);
        // Payload bytes are preserved verbatim on each write.
        assert_eq!(first[0].fragments[0].bytes, b"AAAA".to_vec());
        assert_eq!(second[0].fragments[0].bytes, b"BBBBBB".to_vec());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn back_to_back_inline_writes_delivered_in_seq_order() {
        // BUG #6 fix -- two back-to-back synchronous producer writes to
        // the SAME uri must be DELIVERED (not just minted) in seq order.
        // The old code spawned one detached send task per write, whose
        // notifier awaits raced on the multi-thread runtime and could put
        // seq=1 on the wire before seq=0. The single per-URI FIFO consumer
        // guarantees receive order == producer order == send order.
        let a = adapter();
        let n = Arc::new(RecordingNotifier::default());
        let b = bridge(Arc::clone(&a), Arc::clone(&n), DEFAULT_INLINE_MAX_BYTES);
        let lane = open_lane(&a, StubPeer::new("peer-fifo"), "shell://x/output").await;
        lane.set_inline_push(true);

        // Drive the synchronous producer hook twice in a row on this
        // thread (mirrors one resource reader task's sequential writes).
        b.notify_lanes_inline("shell://x/output", b"AAAA");
        b.notify_lanes_inline("shell://x/output", b"BBBBBB");

        // Delivery is via the spawned consumer; wait for both to land.
        let mut delivered = 0;
        for _ in 0..400 {
            delivered = n.inline_events.lock().unwrap().len();
            if delivered >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            delivered >= 2,
            "consumer did not deliver both inline events (got {delivered})"
        );

        let events = n.inline_events.lock().unwrap().clone();
        assert_eq!(events.len(), 2, "exactly two inline events delivered");
        // Delivered in seq order, not just minted in seq order.
        assert_eq!(events[0].1.seq, 0, "first delivery must be seq 0");
        assert_eq!(events[0].1.bytes, b"AAAA".to_vec());
        assert_eq!(events[0].1.cursor_after, 4);
        assert_eq!(events[1].1.seq, 1, "second delivery must be seq 1");
        assert_eq!(events[1].1.bytes, b"BBBBBB".to_vec());
        assert_eq!(events[1].1.cursor_after, 10);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn notify_lanes_inline_path_calls_notify_ssh_output_alongside_legacy() {
        // ADR 0012 phase 5 D3 fix -- legacy notify_resource_updated
        // continues to fire on the debouncer cadence; inline payloads
        // ship IN ADDITION to the legacy event for opted-in lanes.
        let a = adapter();
        let n = Arc::new(RecordingNotifier::default());
        let b = bridge(Arc::clone(&a), Arc::clone(&n), DEFAULT_INLINE_MAX_BYTES);
        let lane = open_lane(&a, StubPeer::new("peer-inline"), "shell://x/output").await;
        lane.set_inline_push(true);
        let bytes: Vec<u8> = (0_u8..10).collect();
        b.notify_lanes_with_bytes("shell://x/output", &bytes).await;
        let inline = n.inline_events.lock().unwrap().clone();
        let legacy = n.resource_events.lock().unwrap().clone();
        assert_eq!(inline.len(), 1, "exactly one inline notify");
        assert_eq!(inline[0].0.as_str(), "peer-inline");
        assert_eq!(inline[0].1.bytes, bytes);
        assert_eq!(inline[0].1.seq, 0);
        assert_eq!(inline[0].1.cursor_after, 10);
        assert_eq!(
            legacy.len(),
            1,
            "legacy notify_resource_updated must still fire alongside inline"
        );
        assert_eq!(legacy[0].1, "shell://x/output");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn notify_lanes_legacy_path_unchanged_when_inline_off() {
        let a = adapter();
        let n = Arc::new(RecordingNotifier::default());
        let b = bridge(Arc::clone(&a), Arc::clone(&n), DEFAULT_INLINE_MAX_BYTES);
        let _lane = open_lane(&a, StubPeer::new("peer-legacy"), "shell://x/output").await;
        b.notify_lanes_with_bytes("shell://x/output", b"abc").await;
        let inline = n.inline_events.lock().unwrap().clone();
        let legacy = n.resource_events.lock().unwrap().clone();
        assert!(
            inline.is_empty(),
            "inline notifications must not fire on opt-out lanes"
        );
        assert_eq!(legacy.len(), 1, "legacy notify fires exactly once");
        assert_eq!(legacy[0].1, "shell://x/output");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn notify_lanes_inline_split_fragments() {
        let a = adapter();
        let n = Arc::new(RecordingNotifier::default());
        let b = bridge(Arc::clone(&a), Arc::clone(&n), 32 * 1024);
        let lane = open_lane(&a, StubPeer::new("peer-split"), "shell://x/output").await;
        lane.set_inline_push(true);
        let bytes = vec![b'a'; 64 * 1024];
        b.notify_lanes_with_bytes("shell://x/output", &bytes).await;
        let inline = n.inline_events.lock().unwrap().clone();
        assert_eq!(inline.len(), 2, "expected 2 fragments for 64 KiB / 32 KiB");
        assert_eq!(inline[0].1.seq, 0);
        assert_eq!(inline[1].1.seq, 1);
        assert_eq!(inline[0].1.bytes.len(), 32 * 1024);
        assert_eq!(inline[1].1.bytes.len(), 32 * 1024);
        let total_len: usize = inline.iter().map(|e| e.1.bytes.len()).sum();
        assert_eq!(total_len, 64 * 1024);
        assert_eq!(inline[1].1.cursor_after, 64 * 1024);
        assert!(inline[0].1.truncated, "first fragment must be truncated");
        assert!(
            !inline[1].1.truncated,
            "final fragment must not be truncated"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn notify_lanes_text_uri_utf8_safe_split() {
        let a = adapter();
        let n = Arc::new(RecordingNotifier::default());
        let b = bridge(Arc::clone(&a), Arc::clone(&n), 32 * 1024);
        let lane = open_lane(&a, StubPeer::new("peer-utf8"), "shell://x/output").await;
        lane.set_inline_push(true);
        let mut bytes = vec![b'a'; (32 * 1024) - 1];
        bytes.extend_from_slice(&[0xC3, 0xA9]);
        bytes.extend_from_slice(b"trailing");
        let total = bytes.len();
        b.notify_lanes_with_bytes("shell://x/output", &bytes).await;
        let inline = n.inline_events.lock().unwrap().clone();
        assert_eq!(inline.len(), 2);
        assert_eq!(inline[0].1.bytes.len(), (32 * 1024) - 1);
        assert_eq!(*inline[0].1.bytes.last().unwrap(), b'a');
        let second = &inline[1].1.bytes;
        assert_eq!(second[0], 0xC3);
        assert_eq!(second[1], 0xA9);
        assert_eq!(&second[2..], b"trailing");
        assert_eq!(inline[1].1.cursor_after, total as u64);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn inline_events_and_bytes_counters_increment() {
        // ADR 0012 phase 5 D2 fix -- per-lane inline observability
        // counters must climb monotonically per fragment shipped.
        let a = adapter();
        let n = Arc::new(RecordingNotifier::default());
        let b = bridge(Arc::clone(&a), Arc::clone(&n), 32 * 1024);
        let lane = open_lane(&a, StubPeer::new("peer-counter"), "shell://x/output").await;
        lane.set_inline_push(true);
        let total = 96_usize * 1024;
        let bytes = vec![b'a'; total];
        b.notify_lanes_with_bytes("shell://x/output", &bytes).await;
        let inline_events = lane.inline_events_sent.load(Ordering::Relaxed);
        let inline_bytes = lane.inline_bytes_sent.load(Ordering::Relaxed);
        let recorded = n.inline_events.lock().unwrap().len();
        // 96 KiB at 32 KiB cap = 3 fragments.
        assert_eq!(recorded, 3, "expected 3 fragments at 32 KiB cap");
        assert_eq!(inline_events, 3, "events counter must climb per fragment");
        assert_eq!(
            inline_bytes,
            u64::try_from(total).unwrap_or(u64::MAX),
            "bytes counter must equal total bytes shipped"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn notify_lanes_fires_both_legacy_and_inline_on_opt_in() {
        // ADR 0012 phase 5 D3 fix -- BOTH notify_resource_updated
        // and notify_ssh_output fire when the lane has opted in and
        // bytes are non-empty.
        let a = adapter();
        let n = Arc::new(RecordingNotifier::default());
        let b = bridge(Arc::clone(&a), Arc::clone(&n), 32 * 1024);
        let lane = open_lane(&a, StubPeer::new("peer-both"), "shell://x/output").await;
        lane.set_inline_push(true);
        b.notify_lanes_with_bytes("shell://x/output", b"hello")
            .await;
        let inline = n.inline_events.lock().unwrap().clone();
        let legacy = n.resource_events.lock().unwrap().clone();
        assert_eq!(inline.len(), 1, "exactly one inline event for hello");
        assert_eq!(
            legacy.len(),
            1,
            "exactly one legacy notify_resource_updated event"
        );
        assert_eq!(legacy[0].1, "shell://x/output");
        assert_eq!(inline[0].1.uri, "shell://x/output");
    }
}
