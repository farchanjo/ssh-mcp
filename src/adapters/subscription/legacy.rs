//! Subscription registry powering MCP `resources/subscribe` for the five
//! ssh-mcp resource schemes.
//!
//! ## Architecture
//!
//! - One [`SubscriptionRegistry`] global instance ([`SUBSCRIPTION_REGISTRY`]).
//! - Per URI (e.g. `shell://abc/output`), a `Vec<SubscriberHandle>` keyed in
//!   a `DashMap` for shard-locked O(1) insert/remove.
//! - Per `(peer_id, uri)` tuple, an [`Arc<PeerProgress>`] cursor tracking
//!   how many bytes the peer has consumed plus the last sequence it has
//!   acknowledged (delta semantics for `resources/read?cursor=auto`).
//! - Per resource (`(ResourceKind, ResourceId)`), one debouncer task that
//!   coalesces wakeups in a configurable window and emits a single
//!   `notifications/resources/updated` per window. A force-flush ticker and
//!   a keepalive ticker guarantee progress even when no fresh chunks
//!   arrive.
//!
//! ## Backpressure features (A + B + D)
//!
//! - **A. Sequence numbers** — every `OutputChunk`, `ProgressEvent`,
//!   `HealthEvent`, `ForwardEvent` carries a `seq: u64` allocated from a
//!   per-resource [`AtomicU64`]. The registry exposes `current_seq` per
//!   resource so `resources/read._meta.last_seq` can be reported and
//!   subscribers can detect gaps after a `Lagged` recovery.
//! - **B. Keepalive** — per-resource ticker emits a notification every
//!   `SSH_NOTIFY_KEEPALIVE_S` even when no new data arrives.
//! - **D. Cumulative chunks** — the debouncer collapses N producer chunks
//!   into a single `notifications/resources/updated` per debounce window.
//!   Subscribers see one notification per window regardless of producer
//!   chatter; `resources/read` does the actual coalescing of bytes.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use rmcp::Peer;
use rmcp::RoleServer;
use rmcp::model::ResourceUpdatedNotificationParam;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::{self, MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::adapters::config::internal::{
    resolve_notify_debounce_ms, resolve_notify_flush_bytes, resolve_notify_force_flush_ms,
    resolve_notify_keepalive_s,
};
use crate::adapters::notifier::rmcp_peer::PeerTable;

/// Resource scheme handled by the subscription registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// `shell://<id>/output`
    Shell,
    /// `command://<id>/output`
    Command,
    /// `transfer://<id>/progress`
    Transfer,
    /// `session://<id>/health`
    Session,
    /// `forward://<id>/events`
    Forward,
    /// `serial://<id>/output` — UART / TTY / COM (v5.2; ADR 0009).
    Serial,
}

/// A single MCP peer's subscription to one resource URI.
#[derive(Clone)]
pub struct SubscriberHandle {
    /// Stable identifier we use to address the peer in our registry.
    /// `Peer<RoleServer>` exposes no public id, so we generate a UUID at
    /// subscribe time and store both sides.
    pub peer_id: String,
    /// Live peer handle. Cloning the registry's snapshot also clones every
    /// `Peer<RoleServer>` (cheap — internally `mpsc::Sender` + `Arc`).
    pub peer: Peer<RoleServer>,
    /// Subscribed URI (e.g. `shell://abc/output`).
    pub uri: String,
    /// Resource scheme.
    pub kind: ResourceKind,
    /// Resource id portion of the URI (e.g. `abc`).
    pub resource_id: String,
}

impl fmt::Debug for SubscriberHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubscriberHandle")
            .field("peer_id", &self.peer_id)
            .field("uri", &self.uri)
            .field("kind", &self.kind)
            .field("resource_id", &self.resource_id)
            .finish_non_exhaustive()
    }
}

/// Per-`(peer, uri)` cursor state shared with `resources/read`.
#[derive(Debug, Default)]
pub struct PeerProgress {
    /// Byte offset already consumed by the peer (head-pagination cursor).
    pub byte_cursor: AtomicU64,
    /// Highest sequence number the peer has observed. `0` means "no events
    /// seen yet" — sequences allocated by [`SubscriptionRegistry::next_seq`]
    /// always start at `1`.
    pub last_seq_seen: AtomicU64,
}

type ResourceKey = (ResourceKind, String);
type PeerUriKey = (String, String);

/// Lock-free subscription registry. Singleton via [`SUBSCRIPTION_REGISTRY`].
pub struct SubscriptionRegistry {
    /// `uri` -> list of subscriber handles.
    subscribers: DashMap<String, Vec<SubscriberHandle>>,
    /// `(peer_id, uri)` -> shared cursor state.
    peer_progress: DashMap<PeerUriKey, Arc<PeerProgress>>,
    /// `(kind, resource_id)` -> running debouncer task.
    debounce_tasks: DashMap<ResourceKey, JoinHandle<()>>,
    /// `(kind, resource_id)` -> sequence counter (allocate via
    /// [`Self::next_seq`]).
    sequence_counters: DashMap<ResourceKey, Arc<AtomicU64>>,
    /// `(kind, resource_id)` -> wakeup notify shared with the debouncer.
    wakers: DashMap<ResourceKey, Arc<Notify>>,
    /// ADR 0006 Amendment 1 — `(kind, resource_id)` -> immediate-flush
    /// notify. Fired by [`Self::record_bytes`] when the per-resource
    /// byte counter crosses `SSH_NOTIFY_FLUSH_BYTES`.
    flush_now: DashMap<ResourceKey, Arc<Notify>>,
    /// ADR 0006 Amendment 1 — `(kind, resource_id)` ->
    /// bytes-since-last-broadcast counter (`Relaxed` ordering — this
    /// is a coalescing hint).
    bytes_since_flush: DashMap<ResourceKey, Arc<AtomicUsize>>,
    /// Cached byte-threshold (`SSH_NOTIFY_FLUSH_BYTES`). Resolved
    /// once at construction. `0` disables byte-threshold entirely.
    flush_bytes_threshold: usize,
    /// ADR 0006 Amendment 1 — process-wide count of byte-triggered
    /// broadcasts.
    byte_triggered_flushes: AtomicU64,
}

impl SubscriptionRegistry {
    /// Build a fresh empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            subscribers: DashMap::new(),
            peer_progress: DashMap::new(),
            debounce_tasks: DashMap::new(),
            sequence_counters: DashMap::new(),
            wakers: DashMap::new(),
            flush_now: DashMap::new(),
            bytes_since_flush: DashMap::new(),
            flush_bytes_threshold: resolve_notify_flush_bytes(),
            byte_triggered_flushes: AtomicU64::new(0),
        }
    }

    /// Allocate the next sequence number for `(kind, id)`. Sequences start
    /// at `1` so that a peer's `last_seq_seen == 0` reads as "no events
    /// observed yet".
    #[must_use]
    pub fn next_seq(&self, kind: ResourceKind, id: &str) -> u64 {
        let counter = self.sequence_counter(kind, id);
        counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Read the latest allocated sequence number for `(kind, id)` (returns
    /// `0` if [`Self::next_seq`] has never been called for it).
    #[must_use]
    pub fn current_seq(&self, kind: ResourceKind, id: &str) -> u64 {
        self.sequence_counters
            .get(&(kind, id.to_string()))
            .map_or(0, |entry| entry.load(Ordering::Relaxed))
    }

    /// Wake the debouncer for `(kind, id)`. No-op when there is no active
    /// debouncer (i.e. no subscribers). Idempotent: multiple `poke`s inside
    /// the debounce window collapse into a single notification.
    pub fn poke(&self, kind: ResourceKind, id: &str) {
        if let Some(entry) = self.wakers.get(&(kind, id.to_string())) {
            entry.notify_one();
        }
    }

    /// Subscribe `peer` (identified by `peer_id`) to `uri`. The first
    /// subscriber for `(kind, resource_id)` spawns the debouncer task.
    /// Returns `Ok(true)` when this is the first subscriber, `Ok(false)`
    /// otherwise.
    ///
    /// # Errors
    ///
    /// Currently infallible. The `Result` is preserved so future
    /// rate-limit / capacity checks can fail without breaking the API.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "owned strings are stored in the registry; taking by value avoids one clone at each call site"
    )]
    pub fn subscribe(
        &self,
        kind: ResourceKind,
        resource_id: String,
        uri: String,
        peer_id: String,
        peer: Peer<RoleServer>,
    ) -> Result<bool, String> {
        let handle = SubscriberHandle {
            peer_id: peer_id.clone(),
            peer,
            uri: uri.clone(),
            kind,
            resource_id: resource_id.clone(),
        };

        let mut entry = self.subscribers.entry(uri.clone()).or_default();
        // Replace any prior subscription from the same peer on this URI so
        // re-subscribes refresh the live `Peer` handle without duplicates.
        entry.retain(|s| s.peer_id != peer_id);
        entry.push(handle);
        let first_after_drop = entry.len() == 1;
        drop(entry);

        // Ensure the (peer, uri) cursor exists so future `resources/read`
        // calls have somewhere to read/write progress.
        self.peer_progress
            .entry((peer_id, uri))
            .or_insert_with(|| Arc::new(PeerProgress::default()));

        if first_after_drop {
            self.spawn_debouncer(kind, &resource_id);
        }
        Ok(first_after_drop)
    }

    /// Unsubscribe `peer_id` from `uri`. Drops the debouncer task when the
    /// last subscriber leaves.
    pub fn unsubscribe(&self, peer_id: &str, uri: &str) {
        let became_empty = self.subscribers.get_mut(uri).is_some_and(|mut entry| {
            entry.retain(|s| s.peer_id != peer_id);
            entry.is_empty()
        });
        if became_empty {
            self.subscribers.remove(uri);
            if let Some((kind, resource_id)) = parse_uri(uri) {
                self.stop_debouncer(kind, &resource_id);
            }
        }
        self.peer_progress
            .remove(&(peer_id.to_string(), uri.to_string()));
    }

    /// Walk every subscriber and drop the ones whose rmcp transport has
    /// closed. Returns the number of peers dropped. Used by the binary
    /// entry points (`ssh-mcp` and `ssh-mcp-stdio`) as a periodic GC pass
    /// since rmcp 1.6 does not surface a peer-disconnect callback.
    #[must_use]
    pub fn gc_closed_peers(&self) -> usize {
        // Snapshot a unique set of (peer_id, peer) pairs before mutating —
        // `drop_peer` re-acquires the same shards, so we must release the
        // outer guards first.
        let mut closed_peer_ids: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for entry in &self.subscribers {
            for handle in entry.value() {
                if seen.insert(handle.peer_id.clone()) && handle.peer.is_transport_closed() {
                    closed_peer_ids.push(handle.peer_id.clone());
                }
            }
        }
        let dropped = closed_peer_ids.len();
        for peer_id in closed_peer_ids {
            self.drop_peer(&peer_id);
        }
        dropped
    }

    /// Drop every subscription owned by `peer_id` across all URIs. Stops
    /// debouncer tasks that are now empty.
    pub fn drop_peer(&self, peer_id: &str) {
        // Collect URIs the peer subscribes to (clone keys, drop guards
        // before iterating to avoid `await_holding_lock`).
        let uris: Vec<String> = self
            .subscribers
            .iter()
            .filter_map(|entry| {
                entry
                    .value()
                    .iter()
                    .any(|s| s.peer_id == peer_id)
                    .then(|| entry.key().clone())
            })
            .collect();

        for uri in uris {
            self.unsubscribe(peer_id, &uri);
        }
    }

    /// Get-or-create the cursor entry for `(peer_id, uri)`.
    #[must_use]
    pub fn peer_progress(&self, peer_id: &str, uri: &str) -> Arc<PeerProgress> {
        let key = (peer_id.to_string(), uri.to_string());
        if let Some(entry) = self.peer_progress.get(&key) {
            return Arc::clone(entry.value());
        }
        let progress = Arc::new(PeerProgress::default());
        self.peer_progress
            .entry(key)
            .or_insert_with(|| Arc::clone(&progress));
        progress
    }

    /// Snapshot the subscriber list for `uri`. Returns an empty vec when no
    /// subscribers exist. Caller may `await` after the call returns; the
    /// underlying `DashMap` shard guard is released before the clone is
    /// returned.
    #[must_use]
    pub fn snapshot_subscribers(&self, uri: &str) -> Vec<SubscriberHandle> {
        self.subscribers
            .get(uri)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    /// Decrement every peer cursor on `uri` by `bytes_dropped` (saturating).
    /// Call when the underlying ring buffer drops bytes from the head.
    pub fn compensate_truncation(&self, uri: &str, bytes_dropped: u64) {
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

    fn spawn_debouncer(&self, kind: ResourceKind, resource_id: &str) {
        let key = (kind, resource_id.to_string());
        if self.debounce_tasks.contains_key(&key) {
            return;
        }
        let waker = Arc::new(Notify::new());
        let flush_now = Arc::new(Notify::new());
        let bytes_counter = Arc::new(AtomicUsize::new(0));
        self.wakers.insert(key.clone(), Arc::clone(&waker));
        self.flush_now.insert(key.clone(), Arc::clone(&flush_now));
        self.bytes_since_flush
            .insert(key.clone(), Arc::clone(&bytes_counter));
        let uri = format_uri(kind, resource_id);
        let debounce_ms = resolve_notify_debounce_ms();
        let force_flush_ms = resolve_notify_force_flush_ms();
        let keepalive_s = resolve_notify_keepalive_s();
        let task = tokio::spawn(debouncer_task(
            uri,
            waker,
            flush_now,
            bytes_counter,
            debounce_ms,
            force_flush_ms,
            keepalive_s,
        ));
        self.debounce_tasks.insert(key, task);
    }

    fn stop_debouncer(&self, kind: ResourceKind, resource_id: &str) {
        let key = (kind, resource_id.to_string());
        if let Some((_, handle)) = self.debounce_tasks.remove(&key) {
            handle.abort();
        }
        self.wakers.remove(&key);
        self.flush_now.remove(&key);
        self.bytes_since_flush.remove(&key);
    }

    /// ADR 0006 Amendment 1 — record `bytes_added` newly produced
    /// bytes for `(kind, id)`. Forces an immediate debouncer flush
    /// when the per-resource counter crosses
    /// `SSH_NOTIFY_FLUSH_BYTES`. Disabled (`0` threshold) /
    /// no-debouncer paths return immediately.
    pub fn record_bytes(&self, kind: ResourceKind, id: &str, bytes_added: usize) {
        if self.flush_bytes_threshold == 0 || bytes_added == 0 {
            return;
        }
        let key: ResourceKey = (kind, id.to_string());
        if !self.bytes_since_flush.contains_key(&key) {
            return;
        }
        let Some(counter_entry) = self.bytes_since_flush.get(&key) else {
            return;
        };
        let counter = Arc::clone(counter_entry.value());
        drop(counter_entry);
        let prev = counter.fetch_add(bytes_added, Ordering::Relaxed);
        let new = prev.saturating_add(bytes_added);
        if prev < self.flush_bytes_threshold
            && new >= self.flush_bytes_threshold
            && let Some(notify) = self.flush_now.get(&key)
        {
            notify.notify_one();
        }
    }

    /// Process-wide count of byte-threshold-triggered broadcasts.
    /// Surfaced via `ssh_daemon_stats`.
    #[must_use]
    pub fn byte_triggered_flushes_total(&self) -> u64 {
        self.byte_triggered_flushes.load(Ordering::Relaxed)
    }

    /// Cached byte-threshold (`SSH_NOTIFY_FLUSH_BYTES`). `0` means
    /// disabled.
    #[must_use]
    pub const fn flush_bytes_threshold(&self) -> usize {
        self.flush_bytes_threshold
    }
}

impl Default for SubscriptionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Format a URI for `(kind, id)`.
#[must_use]
pub fn format_uri(kind: ResourceKind, id: &str) -> String {
    let scheme = match kind {
        ResourceKind::Shell => "shell",
        ResourceKind::Command => "command",
        ResourceKind::Transfer => "transfer",
        ResourceKind::Session => "session",
        ResourceKind::Forward => "forward",
        ResourceKind::Serial => "serial",
    };
    let suffix = match kind {
        ResourceKind::Shell | ResourceKind::Command | ResourceKind::Serial => "output",
        ResourceKind::Transfer => "progress",
        ResourceKind::Session => "health",
        ResourceKind::Forward => "events",
    };
    format!("{scheme}://{id}/{suffix}")
}

/// Reverse of [`format_uri`]: parse `scheme://id/suffix` back into
/// `(kind, id)`. Returns `None` for unknown schemes.
#[must_use]
pub fn parse_uri(uri: &str) -> Option<(ResourceKind, String)> {
    let (scheme, rest) = uri.split_once("://")?;
    let kind = match scheme {
        "shell" => ResourceKind::Shell,
        "command" => ResourceKind::Command,
        "transfer" => ResourceKind::Transfer,
        "session" => ResourceKind::Session,
        "forward" => ResourceKind::Forward,
        "serial" => ResourceKind::Serial,
        _ => return None,
    };
    let id = rest.split_once('/').map_or(rest, |(id, _)| id);
    Some((kind, id.to_string()))
}

/// Global subscription registry instance.
pub static SUBSCRIPTION_REGISTRY: LazyLock<SubscriptionRegistry> =
    LazyLock::new(SubscriptionRegistry::new);

async fn legacy_broadcast_and_reset(uri: &str, bytes_counter: &AtomicUsize) {
    bytes_counter.store(0, Ordering::Relaxed);
    broadcast_resource_updated(uri).await;
}

async fn debouncer_task(
    uri: String,
    waker: Arc<Notify>,
    flush_now: Arc<Notify>,
    bytes_counter: Arc<AtomicUsize>,
    debounce_ms: u64,
    force_flush_ms: u64,
    keepalive_s: u64,
) {
    let mut keepalive_tick = interval(Duration::from_secs(keepalive_s));
    keepalive_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    keepalive_tick.tick().await; // initial tick fires immediately — drain.
    let mut force_flush_tick = interval(Duration::from_millis(force_flush_ms));
    force_flush_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    force_flush_tick.tick().await;
    let debounce = Duration::from_millis(debounce_ms);
    loop {
        tokio::select! {
            biased;
            () = flush_now.notified() => {
                SUBSCRIPTION_REGISTRY
                    .byte_triggered_flushes
                    .fetch_add(1, Ordering::Relaxed);
                legacy_broadcast_and_reset(&uri, &bytes_counter).await;
            }
            () = waker.notified() => {
                time::sleep(debounce).await;
                legacy_broadcast_and_reset(&uri, &bytes_counter).await;
            }
            _ = force_flush_tick.tick() => {
                legacy_broadcast_and_reset(&uri, &bytes_counter).await;
            }
            _ = keepalive_tick.tick() => {
                legacy_broadcast_and_reset(&uri, &bytes_counter).await;
            }
        }
    }
}

async fn broadcast_resource_updated(uri: &str) {
    let subs = SUBSCRIPTION_REGISTRY.snapshot_subscribers(uri);
    if subs.is_empty() {
        return;
    }
    for sub in subs {
        let params = ResourceUpdatedNotificationParam {
            uri: uri.to_string(),
        };
        if let Err(err) = sub.peer.notify_resource_updated(params).await {
            debug!(
                "notify_resource_updated failed for peer {}: {err}",
                sub.peer_id
            );
        }
    }
}

/// Spawn the periodic peer-GC pump.
///
/// rmcp 1.6 does not raise a callback on peer disconnect, so the binary
/// entry points poll instead. The task exits cleanly when `cancel` is
/// triggered.
///
/// `peer_table` is the v4 [`PeerTable`] shared between the rmcp resource
/// handlers and the
/// [`crate::adapters::notifier::rmcp_adapter::RmcpNotifier`]. When
/// supplied the GC pass also prunes its closed peers so the v4 side
/// table stays in sync with the legacy [`SUBSCRIPTION_REGISTRY`] ahead
/// of the runtime migration.
#[must_use]
pub fn spawn_peer_gc(
    interval_secs: u64,
    cancel: CancellationToken,
    peer_table: Option<Arc<PeerTable>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(interval_secs.max(1)));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // Drain the immediate first tick so the first real scan happens
        // after the configured interval.
        ticker.tick().await;
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    debug!("peer GC: shutdown signal received, exiting");
                    return;
                }
                _ = ticker.tick() => {
                    let dropped = SUBSCRIPTION_REGISTRY.gc_closed_peers();
                    let table_dropped = peer_table
                        .as_deref()
                        .map_or(0, PeerTable::gc_closed_peers);
                    if dropped > 0 || table_dropped > 0 {
                        debug!(
                            "peer GC: dropped {dropped} legacy peers, {table_dropped} v4 peers"
                        );
                    }
                }
            }
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests use unwrap for brevity")]
mod tests {
    use super::*;

    #[test]
    fn next_seq_increments_monotonically() {
        let reg = SubscriptionRegistry::new();
        let a = reg.next_seq(ResourceKind::Shell, "shell-1");
        let b = reg.next_seq(ResourceKind::Shell, "shell-1");
        let c = reg.next_seq(ResourceKind::Shell, "shell-1");
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert_eq!(c, 3);
        assert_eq!(reg.current_seq(ResourceKind::Shell, "shell-1"), 3);
    }

    #[test]
    fn next_seq_is_per_resource_independent() {
        let reg = SubscriptionRegistry::new();
        let s1 = reg.next_seq(ResourceKind::Shell, "shell-1");
        let s2 = reg.next_seq(ResourceKind::Command, "shell-1");
        assert_eq!(s1, 1);
        assert_eq!(s2, 1, "different kind should have its own counter");
    }

    #[test]
    fn current_seq_zero_when_never_allocated() {
        let reg = SubscriptionRegistry::new();
        assert_eq!(reg.current_seq(ResourceKind::Transfer, "x"), 0);
    }

    #[test]
    fn peer_progress_returns_same_arc_for_same_key() {
        let reg = SubscriptionRegistry::new();
        let a = reg.peer_progress("peer-1", "shell://abc/output");
        let b = reg.peer_progress("peer-1", "shell://abc/output");
        assert!(Arc::ptr_eq(&a, &b));
        a.byte_cursor.store(42, Ordering::Relaxed);
        assert_eq!(b.byte_cursor.load(Ordering::Relaxed), 42);
    }

    #[test]
    fn compensate_truncation_decrements_matching_uri_only() {
        let reg = SubscriptionRegistry::new();
        let p1 = reg.peer_progress("peer-1", "shell://a/output");
        let p2 = reg.peer_progress("peer-2", "shell://a/output");
        let p_other = reg.peer_progress("peer-1", "shell://b/output");
        p1.byte_cursor.store(100, Ordering::Relaxed);
        p2.byte_cursor.store(50, Ordering::Relaxed);
        p_other.byte_cursor.store(77, Ordering::Relaxed);

        reg.compensate_truncation("shell://a/output", 30);

        assert_eq!(p1.byte_cursor.load(Ordering::Relaxed), 70);
        assert_eq!(p2.byte_cursor.load(Ordering::Relaxed), 20);
        assert_eq!(p_other.byte_cursor.load(Ordering::Relaxed), 77);
    }

    #[test]
    fn compensate_truncation_saturates_at_zero() {
        let reg = SubscriptionRegistry::new();
        let p1 = reg.peer_progress("peer-1", "shell://a/output");
        p1.byte_cursor.store(10, Ordering::Relaxed);
        reg.compensate_truncation("shell://a/output", 100);
        assert_eq!(p1.byte_cursor.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn compensate_truncation_zero_dropped_is_noop() {
        let reg = SubscriptionRegistry::new();
        let p1 = reg.peer_progress("peer-1", "shell://a/output");
        p1.byte_cursor.store(10, Ordering::Relaxed);
        reg.compensate_truncation("shell://a/output", 0);
        assert_eq!(p1.byte_cursor.load(Ordering::Relaxed), 10);
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
            let parsed = parse_uri(&uri).unwrap();
            assert_eq!(parsed.0, kind);
            assert_eq!(parsed.1, "abc");
        }
    }

    #[test]
    fn parse_uri_rejects_unknown_scheme() {
        assert!(parse_uri("foo://x/output").is_none());
        assert!(parse_uri("shell-no-scheme").is_none());
    }

    #[test]
    fn poke_without_waker_is_noop() {
        let reg = SubscriptionRegistry::new();
        // Must not panic. With no subscribers there is no waker.
        reg.poke(ResourceKind::Shell, "missing");
    }

    #[test]
    fn snapshot_subscribers_empty_when_uri_unknown() {
        let reg = SubscriptionRegistry::new();
        assert!(reg.snapshot_subscribers("shell://nope/output").is_empty());
    }

    mod e15_extra {
        use super::*;

        #[test]
        fn next_seq_each_kind_independent() {
            let reg = SubscriptionRegistry::new();
            assert_eq!(reg.next_seq(ResourceKind::Shell, "id"), 1);
            assert_eq!(reg.next_seq(ResourceKind::Command, "id"), 1);
            assert_eq!(reg.next_seq(ResourceKind::Transfer, "id"), 1);
            assert_eq!(reg.next_seq(ResourceKind::Session, "id"), 1);
            assert_eq!(reg.next_seq(ResourceKind::Forward, "id"), 1);
        }

        #[test]
        fn next_seq_each_id_independent() {
            let reg = SubscriptionRegistry::new();
            assert_eq!(reg.next_seq(ResourceKind::Shell, "alpha"), 1);
            assert_eq!(reg.next_seq(ResourceKind::Shell, "beta"), 1);
            assert_eq!(reg.next_seq(ResourceKind::Shell, "alpha"), 2);
        }

        #[test]
        fn current_seq_matches_after_burst_allocations() {
            let reg = SubscriptionRegistry::new();
            for _ in 0..50_usize {
                let _ = reg.next_seq(ResourceKind::Command, "burst");
            }
            assert_eq!(reg.current_seq(ResourceKind::Command, "burst"), 50);
        }

        #[test]
        fn peer_progress_returns_independent_arc_for_different_peers() {
            let reg = SubscriptionRegistry::new();
            let a = reg.peer_progress("peer-A", "shell://x/output");
            let b = reg.peer_progress("peer-B", "shell://x/output");
            assert!(!Arc::ptr_eq(&a, &b));
            a.byte_cursor.store(10, Ordering::Relaxed);
            b.byte_cursor.store(20, Ordering::Relaxed);
            assert_eq!(a.byte_cursor.load(Ordering::Relaxed), 10);
            assert_eq!(b.byte_cursor.load(Ordering::Relaxed), 20);
        }

        #[test]
        fn peer_progress_for_same_peer_different_uri_is_independent() {
            let reg = SubscriptionRegistry::new();
            let p1 = reg.peer_progress("peer-A", "shell://x/output");
            let p2 = reg.peer_progress("peer-A", "shell://y/output");
            assert!(!Arc::ptr_eq(&p1, &p2));
        }

        #[test]
        fn compensate_truncation_no_progress_does_not_panic() {
            let reg = SubscriptionRegistry::new();
            // No registered peer_progress entries — the loop body is a no-op.
            reg.compensate_truncation("shell://no-one/output", 100);
        }

        #[test]
        fn compensate_truncation_does_not_affect_unrelated_uris() {
            let reg = SubscriptionRegistry::new();
            let p1 = reg.peer_progress("peer-1", "shell://a/output");
            let p2 = reg.peer_progress("peer-1", "command://a/output");
            p1.byte_cursor.store(100, Ordering::Relaxed);
            p2.byte_cursor.store(100, Ordering::Relaxed);
            reg.compensate_truncation("shell://a/output", 30);
            assert_eq!(p1.byte_cursor.load(Ordering::Relaxed), 70);
            assert_eq!(p2.byte_cursor.load(Ordering::Relaxed), 100);
        }

        #[test]
        fn parse_uri_round_trip_for_shell_command_transfer_session_forward() {
            for kind in [
                ResourceKind::Shell,
                ResourceKind::Command,
                ResourceKind::Transfer,
                ResourceKind::Session,
                ResourceKind::Forward,
            ] {
                let uri = format_uri(kind, "abc-123");
                let (parsed_kind, parsed_id) = parse_uri(&uri).expect("round trip must succeed");
                assert_eq!(parsed_kind, kind);
                assert_eq!(parsed_id, "abc-123");
            }
        }

        #[test]
        fn parse_uri_handles_id_with_no_suffix() {
            // `shell://abc` — no trailing `/output`; the parser still accepts
            // and returns just the id portion.
            let parsed = parse_uri("shell://abc");
            assert!(parsed.is_some());
            let (kind, id) = parsed.expect("parse_uri should succeed");
            assert_eq!(kind, ResourceKind::Shell);
            assert_eq!(id, "abc");
        }

        #[test]
        fn parse_uri_returns_none_when_separator_missing() {
            assert!(parse_uri("not-a-uri").is_none());
            assert!(parse_uri("").is_none());
            assert!(parse_uri("scheme:no-double-slash").is_none());
        }

        #[test]
        fn snapshot_subscribers_returns_empty_clone_for_unknown_uri() {
            let reg = SubscriptionRegistry::new();
            let snap = reg.snapshot_subscribers("shell://does-not-exist/output");
            assert!(snap.is_empty());
        }

        #[test]
        fn poke_with_no_subscribers_is_silent_noop() {
            let reg = SubscriptionRegistry::new();
            // Should not panic and should not allocate a waker.
            reg.poke(ResourceKind::Forward, "id");
            // Confirm: still no waker registered for that key.
            let key = (ResourceKind::Forward, "id".to_string());
            assert!(reg.wakers.get(&key).is_none());
        }
    }
}
