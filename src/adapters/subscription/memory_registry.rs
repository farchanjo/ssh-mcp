//! In-process [`SubscriberRegistryPort`] +
//! [`SubscriberRegistryAsync`] implementation.
//!
//! Direct port of the legacy
//! [`crate::adapters::subscription::legacy::SubscriptionRegistry`]
//! semantics with two changes for the v4 hexagonal layout:
//!
//! 1. **No global singleton.** The registry is instantiated by the
//!    composition root and shared as `Arc<MemoryRegistry<N>>`.
//! 2. **Notifier injection.** The debouncer task previously called the
//!    rmcp peer directly through the `SUBSCRIPTION_REGISTRY` global. The
//!    v4 task now drives the injected [`NotifierPort`] adapter, which is
//!    monomorphised at construction time so the registry stays free of
//!    rmcp-specific types.
//!
//! ## Backpressure semantics (preserved verbatim from v3)
//!
//! - **Sequence numbers** — every `(kind, resource_id)` carries an
//!   [`AtomicU64`]; [`MemoryRegistry::next_seq`] allocates monotonically
//!   from `1`, [`MemoryRegistry::current_seq`] reads the latest value.
//! - **Debouncer** — the first subscriber for `(kind, resource_id)`
//!   spawns a task that coalesces wakeups in
//!   `resolve_notify_debounce_ms()` ms windows, force-flushes every
//!   `resolve_notify_force_flush_ms()` ms, and emits a keepalive every
//!   `resolve_notify_keepalive_s()` s.
//! - **Cursors** — [`PeerProgress`] holds `(byte_cursor, last_seq_seen)`
//!   for each `(peer_id, uri)` pair. `compensate_truncation` decrements
//!   every cursor on a URI by the dropped byte count (saturating).
//! - **Peer GC** — [`MemoryRegistry::gc_closed_peers`] walks the
//!   subscribers, asks each [`PeerHandle::is_closed`], and drops the
//!   ones whose transport has gone away.

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::{self, MissedTickBehavior, interval};
use tracing::debug;

use crate::adapters::config::internal::{
    resolve_notify_debounce_ms, resolve_notify_force_flush_ms, resolve_notify_keepalive_s,
};
use crate::domain::error::DomainError;
use crate::domain::ids::PeerId;
use crate::ports::notifier::{NotifierPort, PeerHandle};
use crate::ports::subscriber_registry::{
    ResourceKind, SubscriberRegistryAsync, SubscriberRegistryPort, SubscriberSnapshot,
};

/// Per-`(peer, uri)` cursor state shared with `resources/read`.
#[derive(Debug, Default)]
pub struct PeerProgress {
    /// Byte offset already consumed by the peer (head-pagination cursor).
    pub byte_cursor: AtomicU64,
    /// Highest sequence number the peer has observed. `0` means "no events
    /// seen yet" — sequences allocated by [`MemoryRegistry::next_seq`]
    /// always start at `1`.
    pub last_seq_seen: AtomicU64,
}

/// Internal subscriber record stored per URI.
#[derive(Clone)]
struct Subscriber {
    /// Stable peer identifier (mirrored from `peer.id()`; cached here so
    /// snapshots and removals do not need a virtual call).
    peer_id: PeerId,
    /// Live peer handle. Cloning the registry's snapshot also clones every
    /// `Arc<dyn PeerHandle>` (cheap pointer bump).
    peer: Arc<dyn PeerHandle>,
    /// Resource scheme.
    kind: ResourceKind,
    /// Resource id portion of the URI.
    resource_id: String,
}

impl fmt::Debug for Subscriber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscriber")
            .field("peer_id", &self.peer_id)
            .field("kind", &self.kind)
            .field("resource_id", &self.resource_id)
            .finish_non_exhaustive()
    }
}

type ResourceKey = (ResourceKind, String);
type PeerUriKey = (PeerId, String);

/// In-process [`SubscriberRegistryPort`] +
/// [`SubscriberRegistryAsync`] adapter.
///
/// `N` is the concrete [`NotifierPort`] adapter the debouncer fans out
/// over. Pinned at construction; the registry is monomorphised once per
/// process so it stays free of dyn-async overhead.
pub struct MemoryRegistry<N> {
    /// `uri` -> list of subscribers.
    subscribers: DashMap<String, Vec<Subscriber>>,
    /// `(peer_id, uri)` -> shared cursor state.
    peer_progress: DashMap<PeerUriKey, Arc<PeerProgress>>,
    /// `(kind, resource_id)` -> running debouncer task.
    debounce_tasks: DashMap<ResourceKey, JoinHandle<()>>,
    /// `(kind, resource_id)` -> sequence counter.
    sequence_counters: DashMap<ResourceKey, Arc<AtomicU64>>,
    /// `(kind, resource_id)` -> wakeup notify shared with the debouncer.
    wakers: DashMap<ResourceKey, Arc<Notify>>,
    /// Notifier the debouncer fans out over.
    notifier: Arc<N>,
    /// Self-reference used by the debouncer task so the async port impl
    /// can still spawn it from `&self`. Set once at construction via
    /// [`Arc::new_cyclic`] and never overwritten.
    self_ref: Weak<Self>,
}

impl<N> fmt::Debug for MemoryRegistry<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryRegistry")
            .field("subscribers_uris", &self.subscribers.len())
            .field("debounce_tasks", &self.debounce_tasks.len())
            .field("sequence_counters", &self.sequence_counters.len())
            .finish_non_exhaustive()
    }
}

impl<N> MemoryRegistry<N>
where
    N: NotifierPort + Send + Sync + 'static,
{
    /// Build a fresh empty registry that fans out via `notifier`.
    ///
    /// The returned [`Arc`] is the only valid handle — the registry
    /// captures a [`Weak`] back-reference to itself so the async port
    /// impl can spawn the debouncer task from `&self`.
    #[must_use]
    pub fn new(notifier: Arc<N>) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            subscribers: DashMap::new(),
            peer_progress: DashMap::new(),
            debounce_tasks: DashMap::new(),
            sequence_counters: DashMap::new(),
            wakers: DashMap::new(),
            notifier,
            self_ref: Weak::clone(weak),
        })
    }

    /// Get-or-create the cursor entry for `(peer_id, uri)`.
    #[must_use]
    pub fn peer_progress(&self, peer_id: &PeerId, uri: &str) -> Arc<PeerProgress> {
        let key = (peer_id.clone(), uri.to_string());
        if let Some(entry) = self.peer_progress.get(&key) {
            return Arc::clone(entry.value());
        }
        let progress = Arc::new(PeerProgress::default());
        self.peer_progress
            .entry(key)
            .or_insert_with(|| Arc::clone(&progress));
        progress
    }

    fn sequence_counter(&self, kind: ResourceKind, id: &str) -> Arc<AtomicU64> {
        let key = (kind, id.to_string());
        if let Some(entry) = self.sequence_counters.get(&key) {
            return Arc::clone(entry.value());
        }
        let counter = Arc::new(AtomicU64::new(0));
        let inserted = self
            .sequence_counters
            .entry(key)
            .or_insert_with(|| Arc::clone(&counter));
        Arc::clone(inserted.value())
    }

    fn ensure_debouncer(&self, kind: ResourceKind, resource_id: &str) {
        let key = (kind, resource_id.to_string());
        if self.debounce_tasks.contains_key(&key) {
            return;
        }
        let Some(registry) = self.self_ref.upgrade() else {
            // Composition root has dropped the only `Arc` — nothing to do.
            debug!("ensure_debouncer: registry already dropped");
            return;
        };
        let waker = Arc::new(Notify::new());
        self.wakers.insert(key.clone(), Arc::clone(&waker));
        let uri = format_uri(kind, resource_id);
        let cfg = DebouncerConfig {
            debounce_ms: resolve_notify_debounce_ms(),
            force_flush_ms: resolve_notify_force_flush_ms(),
            keepalive_s: resolve_notify_keepalive_s(),
        };
        let task = tokio::spawn(debouncer_task(registry, uri, waker, cfg));
        self.debounce_tasks.insert(key, task);
    }

    fn stop_debouncer(&self, kind: ResourceKind, resource_id: &str) {
        let key = (kind, resource_id.to_string());
        if let Some((_, handle)) = self.debounce_tasks.remove(&key) {
            handle.abort();
        }
        self.wakers.remove(&key);
    }

    /// Sync flavour of `drop_peer` used by `gc_closed_peers` (which is
    /// itself sync). The async [`SubscriberRegistryAsync::drop_peer`]
    /// just delegates here.
    fn drop_peer_sync(&self, peer_id: &PeerId) {
        let uris = collect_uris_for_peer(&self.subscribers, peer_id);
        for uri in uris {
            self.unsubscribe_sync(peer_id, &uri);
        }
    }

    /// Sync flavour of `unsubscribe` shared by both the async port impl
    /// and `drop_peer_sync`.
    fn unsubscribe_sync(&self, peer_id: &PeerId, uri: &str) {
        let became_empty = self.subscribers.get_mut(uri).is_some_and(|mut entry| {
            entry.retain(|s| &s.peer_id != peer_id);
            entry.is_empty()
        });
        if became_empty {
            self.subscribers.remove(uri);
            if let Some((kind, resource_id)) = parse_uri(uri) {
                self.stop_debouncer(kind, &resource_id);
            }
        }
        self.peer_progress
            .remove(&(peer_id.clone(), uri.to_string()));
    }

    /// Sync flavour of `subscribe`. Returns `true` when this is the
    /// first subscriber for `(kind, resource_id)` (i.e. the caller must
    /// spawn the debouncer).
    fn insert_subscriber(
        &self,
        kind: ResourceKind,
        resource_id: &str,
        uri: &str,
        peer: Arc<dyn PeerHandle>,
    ) -> bool {
        let peer_id = peer.id();
        let sub = Subscriber {
            peer_id: peer_id.clone(),
            peer,
            kind,
            resource_id: resource_id.to_string(),
        };
        let mut entry = self.subscribers.entry(uri.to_string()).or_default();
        // Replace any prior subscription from the same peer on this URI.
        entry.retain(|s| s.peer_id != peer_id);
        entry.push(sub);
        let first = entry.len() == 1;
        drop(entry);

        // Make sure the (peer, uri) cursor exists before any read happens.
        self.peer_progress
            .entry((peer_id, uri.to_string()))
            .or_insert_with(|| Arc::new(PeerProgress::default()));
        first
    }
}

impl<N> SubscriberRegistryPort for MemoryRegistry<N>
where
    N: NotifierPort + Send + Sync + 'static,
{
    fn next_seq(&self, kind: ResourceKind, resource_id: &str) -> u64 {
        let counter = self.sequence_counter(kind, resource_id);
        counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn current_seq(&self, kind: ResourceKind, resource_id: &str) -> u64 {
        self.sequence_counters
            .get(&(kind, resource_id.to_string()))
            .map_or(0, |entry| entry.load(Ordering::Relaxed))
    }

    fn poke(&self, kind: ResourceKind, resource_id: &str) {
        if let Some(entry) = self.wakers.get(&(kind, resource_id.to_string())) {
            entry.notify_one();
        }
    }

    fn compensate_truncation(&self, uri: &str, bytes_dropped: u64) {
        if bytes_dropped == 0 {
            return;
        }
        for entry in &self.peer_progress {
            if entry.key().1 == uri {
                let progress = entry.value();
                let current = progress.byte_cursor.load(Ordering::Relaxed);
                let next = current.saturating_sub(bytes_dropped);
                progress.byte_cursor.store(next, Ordering::Relaxed);
            }
        }
    }

    fn snapshot_subscribers(&self, uri: &str) -> Vec<SubscriberSnapshot> {
        self.subscribers.get(uri).map_or_else(Vec::new, |entry| {
            entry
                .value()
                .iter()
                .map(|sub| SubscriberSnapshot {
                    peer_id: sub.peer_id.clone(),
                    uri: uri.to_string(),
                    kind: sub.kind,
                    resource_id: sub.resource_id.clone(),
                })
                .collect()
        })
    }

    fn peer_byte_cursor(&self, peer_id: &PeerId, uri: &str) -> u64 {
        self.peer_progress
            .get(&(peer_id.clone(), uri.to_string()))
            .map_or(0, |entry| entry.byte_cursor.load(Ordering::Relaxed))
    }

    fn advance_peer_byte_cursor(&self, peer_id: &PeerId, uri: &str, target: u64) -> u64 {
        let progress = self.peer_progress(peer_id, uri);
        progress.byte_cursor.fetch_max(target, Ordering::Relaxed);
        progress.byte_cursor.load(Ordering::Relaxed)
    }

    fn gc_closed_peers(&self) -> usize {
        let mut closed: Vec<PeerId> = Vec::new();
        let mut seen: HashSet<PeerId> = HashSet::new();
        for entry in &self.subscribers {
            for sub in entry.value() {
                if seen.insert(sub.peer_id.clone()) && sub.peer.is_closed() {
                    closed.push(sub.peer_id.clone());
                }
            }
        }
        let dropped = closed.len();
        for peer_id in closed {
            self.drop_peer_sync(&peer_id);
        }
        dropped
    }
}

impl<N> SubscriberRegistryAsync for MemoryRegistry<N>
where
    N: NotifierPort + Send + Sync + 'static,
{
    async fn subscribe(
        &self,
        kind: ResourceKind,
        resource_id: String,
        uri: String,
        peer: Arc<dyn PeerHandle>,
    ) -> Result<(), DomainError> {
        let first = self.insert_subscriber(kind, &resource_id, &uri, peer);
        if first {
            // First subscriber for this resource — spawn the debouncer.
            // The task itself runs against an `Arc<Self>` upgraded from
            // the cyclic back-reference, so `&self` is enough here.
            self.ensure_debouncer(kind, &resource_id);
        }
        Ok(())
    }

    async fn unsubscribe(&self, peer_id: &PeerId, uri: &str) {
        self.unsubscribe_sync(peer_id, uri);
    }

    async fn drop_peer(&self, peer_id: &PeerId) {
        self.drop_peer_sync(peer_id);
    }
}

/// Format a URI for `(kind, id)`. Mirrors the v3 helper of the same name.
#[must_use]
pub fn format_uri(kind: ResourceKind, id: &str) -> String {
    let scheme = match kind {
        ResourceKind::Shell => "shell",
        ResourceKind::Command => "command",
        ResourceKind::Transfer => "transfer",
        ResourceKind::Session => "session",
        ResourceKind::Forward => "forward",
    };
    let suffix = match kind {
        ResourceKind::Shell | ResourceKind::Command => "output",
        ResourceKind::Transfer => "progress",
        ResourceKind::Session => "health",
        ResourceKind::Forward => "events",
    };
    format!("{scheme}://{id}/{suffix}")
}

/// Parse `scheme://id/suffix` back into `(kind, id)`. Returns `None`
/// for unknown schemes.
#[must_use]
pub fn parse_uri(uri: &str) -> Option<(ResourceKind, String)> {
    let (scheme, rest) = uri.split_once("://")?;
    let kind = match scheme {
        "shell" => ResourceKind::Shell,
        "command" => ResourceKind::Command,
        "transfer" => ResourceKind::Transfer,
        "session" => ResourceKind::Session,
        "forward" => ResourceKind::Forward,
        unknown => {
            debug!("parse_uri: unknown scheme {unknown}");
            return None;
        }
    };
    let id = rest.split_once('/').map_or(rest, |(id, _)| id);
    Some((kind, id.to_string()))
}

/// Configuration consumed by the per-resource debouncer task.
#[derive(Debug, Clone, Copy)]
struct DebouncerConfig {
    debounce_ms: u64,
    force_flush_ms: u64,
    keepalive_s: u64,
}

fn collect_uris_for_peer(subs: &DashMap<String, Vec<Subscriber>>, peer_id: &PeerId) -> Vec<String> {
    subs.iter()
        .filter_map(|entry| {
            entry
                .value()
                .iter()
                .any(|s| &s.peer_id == peer_id)
                .then(|| entry.key().clone())
        })
        .collect()
}

async fn debouncer_task<N>(
    registry: Arc<MemoryRegistry<N>>,
    uri: String,
    waker: Arc<Notify>,
    cfg: DebouncerConfig,
) where
    N: NotifierPort + Send + Sync + 'static,
{
    let mut keepalive_tick = interval(Duration::from_secs(cfg.keepalive_s));
    keepalive_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    keepalive_tick.tick().await; // drain immediate first tick

    let mut force_flush_tick = interval(Duration::from_millis(cfg.force_flush_ms));
    force_flush_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    force_flush_tick.tick().await;

    let debounce = Duration::from_millis(cfg.debounce_ms);

    loop {
        tokio::select! {
            biased;
            () = waker.notified() => {
                time::sleep(debounce).await;
                broadcast(&registry, &uri).await;
            }
            _ = force_flush_tick.tick() => {
                broadcast(&registry, &uri).await;
            }
            _ = keepalive_tick.tick() => {
                broadcast(&registry, &uri).await;
            }
        }
    }
}

async fn broadcast<N>(registry: &Arc<MemoryRegistry<N>>, uri: &str)
where
    N: NotifierPort + Send + Sync + 'static,
{
    // Snapshot subscribers BEFORE awaiting so we never hold a DashMap
    // shard guard across `.await`.
    let subs: Vec<Arc<dyn PeerHandle>> = registry
        .subscribers
        .get(uri)
        .map(|entry| entry.value().iter().map(|s| Arc::clone(&s.peer)).collect())
        .unwrap_or_default();
    if subs.is_empty() {
        return;
    }
    for peer in subs {
        let peer_id = peer.id();
        if let Err(err) = registry.notifier.notify_resource_updated(peer, uri).await {
            debug!("notify_resource_updated failed for peer {peer_id}: {err}");
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
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    use super::{MemoryRegistry, format_uri, parse_uri};
    use crate::domain::error::DomainError;
    use crate::domain::ids::PeerId;
    use crate::ports::notifier::{NotifierPort, PeerHandle};
    use crate::ports::subscriber_registry::{
        ResourceKind, SubscriberRegistryAsync, SubscriberRegistryPort,
    };

    /// Test notifier that records every `(peer_id, uri)` pair it sees.
    #[derive(Debug, Default)]
    struct RecordingNotifier {
        events: Mutex<Vec<(PeerId, String)>>,
    }

    impl NotifierPort for RecordingNotifier {
        async fn notify_resource_updated(
            &self,
            peer: Arc<dyn PeerHandle>,
            uri: &str,
        ) -> Result<(), DomainError> {
            self.events
                .lock()
                .unwrap()
                .push((peer.id(), uri.to_string()));
            Ok(())
        }
    }

    fn _assert_notifier<T: NotifierPort + Send + Sync + 'static>() {}

    /// Stub peer with toggleable `is_closed` state.
    #[derive(Debug)]
    struct StubPeer {
        id: PeerId,
        closed: std::sync::atomic::AtomicBool,
    }

    impl StubPeer {
        fn new(id: &str) -> Arc<Self> {
            Arc::new(Self {
                id: PeerId::new(id.to_string()),
                closed: std::sync::atomic::AtomicBool::new(false),
            })
        }
        fn close(&self) {
            self.closed.store(true, Ordering::Relaxed);
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

    fn registry() -> Arc<MemoryRegistry<RecordingNotifier>> {
        let notifier = Arc::new(RecordingNotifier::default());
        MemoryRegistry::new(notifier)
    }

    #[test]
    fn next_seq_increments_monotonically() {
        let reg = registry();
        let a = reg.next_seq(ResourceKind::Shell, "shell-1");
        let b = reg.next_seq(ResourceKind::Shell, "shell-1");
        let c = reg.next_seq(ResourceKind::Shell, "shell-1");
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert_eq!(c, 3);
        assert_eq!(reg.current_seq(ResourceKind::Shell, "shell-1"), 3);
    }

    #[test]
    fn next_seq_per_kind_independent() {
        let reg = registry();
        assert_eq!(reg.next_seq(ResourceKind::Shell, "id"), 1);
        assert_eq!(reg.next_seq(ResourceKind::Command, "id"), 1);
        assert_eq!(reg.next_seq(ResourceKind::Transfer, "id"), 1);
        assert_eq!(reg.next_seq(ResourceKind::Session, "id"), 1);
        assert_eq!(reg.next_seq(ResourceKind::Forward, "id"), 1);
    }

    #[test]
    fn current_seq_zero_when_never_allocated() {
        let reg = registry();
        assert_eq!(reg.current_seq(ResourceKind::Transfer, "x"), 0);
    }

    #[test]
    fn poke_without_subscribers_is_noop() {
        let reg = registry();
        reg.poke(ResourceKind::Shell, "missing");
    }

    #[test]
    fn compensate_truncation_decrements_matching_uri_only() {
        let reg = registry();
        let p1 = reg.peer_progress(&PeerId::new("peer-1".to_string()), "shell://a/output");
        let p2 = reg.peer_progress(&PeerId::new("peer-2".to_string()), "shell://a/output");
        let other = reg.peer_progress(&PeerId::new("peer-1".to_string()), "shell://b/output");
        p1.byte_cursor.store(100, Ordering::Relaxed);
        p2.byte_cursor.store(50, Ordering::Relaxed);
        other.byte_cursor.store(77, Ordering::Relaxed);

        reg.compensate_truncation("shell://a/output", 30);

        assert_eq!(p1.byte_cursor.load(Ordering::Relaxed), 70);
        assert_eq!(p2.byte_cursor.load(Ordering::Relaxed), 20);
        assert_eq!(other.byte_cursor.load(Ordering::Relaxed), 77);
    }

    #[test]
    fn compensate_truncation_saturates_at_zero() {
        let reg = registry();
        let p = reg.peer_progress(&PeerId::new("p".to_string()), "shell://a/output");
        p.byte_cursor.store(10, Ordering::Relaxed);
        reg.compensate_truncation("shell://a/output", 100);
        assert_eq!(p.byte_cursor.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn compensate_truncation_zero_dropped_is_noop() {
        let reg = registry();
        let p = reg.peer_progress(&PeerId::new("p".to_string()), "shell://a/output");
        p.byte_cursor.store(10, Ordering::Relaxed);
        reg.compensate_truncation("shell://a/output", 0);
        assert_eq!(p.byte_cursor.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn snapshot_subscribers_empty_for_unknown_uri() {
        let reg = registry();
        assert!(reg.snapshot_subscribers("shell://nope/output").is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn subscribe_then_snapshot_returns_record() {
        let reg = registry();
        let peer: Arc<dyn PeerHandle> = StubPeer::new("peer-A");
        SubscriberRegistryAsync::subscribe(
            reg.as_ref(),
            ResourceKind::Shell,
            "abc".to_string(),
            "shell://abc/output".to_string(),
            peer,
        )
        .await
        .unwrap();

        let snap = reg.snapshot_subscribers("shell://abc/output");
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].peer_id.as_str(), "peer-A");
        assert_eq!(snap[0].kind, ResourceKind::Shell);
        assert_eq!(snap[0].resource_id, "abc");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unsubscribe_clears_snapshot_and_progress() {
        let reg = registry();
        let peer: Arc<dyn PeerHandle> = StubPeer::new("peer-A");
        let peer_id = PeerId::new("peer-A".to_string());
        SubscriberRegistryAsync::subscribe(
            reg.as_ref(),
            ResourceKind::Shell,
            "abc".to_string(),
            "shell://abc/output".to_string(),
            peer,
        )
        .await
        .unwrap();
        let _progress = reg.peer_progress(&peer_id, "shell://abc/output");
        SubscriberRegistryAsync::unsubscribe(reg.as_ref(), &peer_id, "shell://abc/output").await;
        assert!(reg.snapshot_subscribers("shell://abc/output").is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drop_peer_clears_all_uris() {
        let reg = registry();
        let peer: Arc<dyn PeerHandle> = StubPeer::new("peer-A");
        let peer_id = PeerId::new("peer-A".to_string());
        SubscriberRegistryAsync::subscribe(
            reg.as_ref(),
            ResourceKind::Shell,
            "a".to_string(),
            "shell://a/output".to_string(),
            Arc::clone(&peer),
        )
        .await
        .unwrap();
        SubscriberRegistryAsync::subscribe(
            reg.as_ref(),
            ResourceKind::Command,
            "b".to_string(),
            "command://b/output".to_string(),
            peer,
        )
        .await
        .unwrap();

        SubscriberRegistryAsync::drop_peer(reg.as_ref(), &peer_id).await;

        assert!(reg.snapshot_subscribers("shell://a/output").is_empty());
        assert!(reg.snapshot_subscribers("command://b/output").is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gc_closed_peers_drops_only_closed_handles() {
        let reg = registry();
        let alive = StubPeer::new("alive");
        let dead = StubPeer::new("dead");
        let alive_handle: Arc<dyn PeerHandle> = Arc::clone(&alive) as Arc<dyn PeerHandle>;
        let dead_handle: Arc<dyn PeerHandle> = Arc::clone(&dead) as Arc<dyn PeerHandle>;
        SubscriberRegistryAsync::subscribe(
            reg.as_ref(),
            ResourceKind::Shell,
            "a".to_string(),
            "shell://a/output".to_string(),
            alive_handle,
        )
        .await
        .unwrap();
        SubscriberRegistryAsync::subscribe(
            reg.as_ref(),
            ResourceKind::Shell,
            "a".to_string(),
            "shell://a/output".to_string(),
            dead_handle,
        )
        .await
        .unwrap();

        dead.close();
        let dropped = reg.gc_closed_peers();
        assert_eq!(dropped, 1);
        let snap = reg.snapshot_subscribers("shell://a/output");
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].peer_id.as_str(), "alive");
    }

    #[test]
    fn format_uri_round_trip() {
        for kind in [
            ResourceKind::Shell,
            ResourceKind::Command,
            ResourceKind::Transfer,
            ResourceKind::Session,
            ResourceKind::Forward,
        ] {
            let uri = format_uri(kind, "abc");
            let (parsed_kind, parsed_id) = parse_uri(&uri).unwrap();
            assert_eq!(parsed_kind, kind);
            assert_eq!(parsed_id, "abc");
        }
    }

    #[test]
    fn parse_uri_rejects_unknown_scheme() {
        assert!(parse_uri("foo://x/output").is_none());
        assert!(parse_uri("shell-no-scheme").is_none());
    }
}
