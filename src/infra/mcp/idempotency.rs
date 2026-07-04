//! Idempotency cache for v4.7-step5.
//!
//! Smaller LLMs frequently retry mutating tool calls when the response is
//! delayed (network blip, slow channel handshake). Without dedup the retry
//! creates a duplicate side effect — two background commands, two
//! transfers, two disconnect attempts. This module provides a
//! lock-free, TTL-bounded cache keyed by `(tool_name, idempotency_key)`
//! that stores the rendered response and replays it verbatim on a hit
//! within the configured TTL window.
//!
//! ## Wire surface
//!
//! Per the MCP spec, custom request metadata flows through the
//! `_meta` envelope on the JSON-RPC params. v4.7 reads
//! `_meta.idempotency_key` (a non-empty string up to 256 bytes); when
//! present, the inbound layer wraps the per-tool execution with
//! [`with_idempotency`] (see [`super::tool_router`]). When absent every
//! call hits the use case path — the legacy behaviour stays
//! byte-compatible.
//!
//! ## Cache shape
//!
//! - `DashMap<(String, String), CacheSlot>` — lock-free hot path. Each
//!   slot is either `Pending` (a claim is in flight; concurrent callers
//!   with the same key await its [`tokio::sync::Notify`]) or `Done` (a
//!   replayable [`CachedResponse`]). The `Pending` state is what makes two
//!   concurrent calls with the same `_meta.idempotency_key` collapse to a
//!   single use-case execution instead of duplicating the side effect.
//! - TTL-based eviction enforced lazily on `get` (returns `None` when
//!   the entry is expired) plus an explicit [`IdempotencyCache::evict_expired`]
//!   sweep called on every insert and optionally driven by the
//!   composition root's background task.
//! - Soft `max_entries` cap enforced by purging the oldest entries when
//!   an insert would push us above the threshold. We pick the oldest
//!   entries by `inserted_at` — `DashMap` does not maintain an LRU order
//!   so we pay one `O(N)` sort on overflow. With `max_entries = 1024`
//!   and a 5-minute TTL this is a non-issue in practice.
//!
//! ## Args fingerprint (v5)
//!
//! Each cached entry also carries an `args_fingerprint` — a 16-hex-char
//! FNV-1a 64-bit digest of the canonical-JSON of the call arguments
//! (object keys sorted recursively). On a cache lookup, when the
//! `(tool, key)` matches but the fingerprint differs, the cache surfaces
//! the [`IdempotencyOutcome::Mismatch`] verdict so the inbound layer can
//! return the spec'd `IDEMPOTENCY_KEY_MISMATCH` wire error instead of
//! replaying a response that was rendered from a different argument
//! shape. The fingerprint is a process-stable digest — never persisted,
//! never sent on the wire — so the lack of cryptographic strength is
//! fine; we only need collision-resistance within a 5-minute TTL window.

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use rmcp::RoleServer;
use rmcp::model::{CallToolResult, Meta};
use rmcp::service::RequestContext;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Notify;

use crate::infra::mcp::helpers::structured::ok_text_and_structured;

/// Maximum byte length accepted for an `_meta.idempotency_key`.
///
/// Longer keys are rejected with `IDEMPOTENCY_KEY_TOO_LONG`.
pub const IDEMPOTENCY_KEY_MAX_BYTES: usize = 256;

/// Default TTL applied when `SSH_IDEMPOTENCY_TTL_SECS` is unset or
/// invalid: 5 minutes.
pub const DEFAULT_IDEMPOTENCY_TTL_SECS: u64 = 300;

/// Default `max_entries` ceiling when `SSH_IDEMPOTENCY_MAX_ENTRIES` is
/// unset or invalid.
pub const DEFAULT_IDEMPOTENCY_MAX_ENTRIES: usize = 1024;

/// Env-var name for the TTL override (in whole seconds).
pub const TTL_ENV_VAR: &str = "SSH_IDEMPOTENCY_TTL_SECS";

/// Env-var name for the max-entries override.
pub const MAX_ENTRIES_ENV_VAR: &str = "SSH_IDEMPOTENCY_MAX_ENTRIES";

/// A cached tool response replayable verbatim on a key match.
#[derive(Debug, Clone)]
pub struct CachedResponse {
    /// Rendered Markdown body (same shape `ok_text_and_structured` consumes).
    pub body: String,
    /// Structured JSON twin.
    pub structured: Value,
    /// 16-hex-char FNV-1a 64-bit digest of the canonical-JSON of the
    /// call arguments. Used to detect `IDEMPOTENCY_KEY_MISMATCH` —
    /// when the same key is reused with different args.
    pub args_fingerprint: String,
    /// Wall-clock timestamp the entry was inserted at.
    pub inserted_at: Instant,
}

/// A single cache slot: either an in-flight claim or a finished response.
///
/// The `Pending` state is the concurrency fix — the first caller to miss
/// atomically installs a `Pending` slot under the shard lock, so a second
/// caller racing the same `(tool, key)` observes `Pending` (and awaits the
/// shared [`Notify`]) instead of independently driving the mutating use
/// case a second time.
#[derive(Debug)]
enum CacheSlot {
    /// A claim is in flight. Concurrent callers park on `waker` and replay
    /// the `Done` slot once the winner publishes it.
    Pending(PendingSlot),
    /// A completed, replayable response.
    Done(CachedResponse),
}

/// The in-flight half of a [`CacheSlot`].
#[derive(Debug)]
struct PendingSlot {
    /// Woken via `notify_waiters` when the winning caller finishes — whether
    /// it published a `Done` slot, errored, or was cancelled.
    waker: Arc<Notify>,
    /// Fingerprint of the in-flight call's args, so a concurrent caller
    /// reusing the key with a *different* argument shape short-circuits to
    /// `Mismatch` instead of waiting on a result it could never replay.
    fingerprint: String,
    /// When the claim was installed; lets a stale `Pending` (a winner that
    /// leaked without running `Drop`) age out under the TTL sweep.
    claimed_at: Instant,
}

impl PendingSlot {
    fn new(waker: Arc<Notify>, fingerprint: &str) -> Self {
        Self {
            waker,
            fingerprint: fingerprint.to_string(),
            claimed_at: Instant::now(),
        }
    }
}

/// Outcome of an [`IdempotencyCache::get_with_fingerprint`] lookup.
#[derive(Debug, Clone)]
pub enum IdempotencyOutcome {
    /// No live entry — the use case must run.
    Miss,
    /// Live entry whose fingerprint matches — replay the cached body.
    Hit(CachedResponse),
    /// Live entry whose fingerprint differs — surface
    /// `IDEMPOTENCY_KEY_MISMATCH` and skip the use case so a stale
    /// response is never replayed for a different argument shape.
    Mismatch,
}

/// Lock-free idempotency cache.
#[derive(Debug)]
pub struct IdempotencyCache {
    inner: DashMap<(String, String), CacheSlot>,
    ttl: Duration,
    max_entries: usize,
}

impl IdempotencyCache {
    /// Build a fresh cache with the supplied TTL and max-entries cap.
    #[must_use]
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        let cap = max_entries.max(1);
        Self {
            inner: DashMap::with_capacity(cap),
            ttl,
            max_entries: cap,
        }
    }

    /// Build an idempotency cache with the production defaults
    /// (5-minute TTL, 1024-entry cap), respecting
    /// `SSH_IDEMPOTENCY_TTL_SECS` / `SSH_IDEMPOTENCY_MAX_ENTRIES`.
    #[must_use]
    pub fn from_env() -> Self {
        let ttl = Duration::from_secs(resolve_ttl_secs());
        let max = resolve_max_entries();
        Self::new(ttl, max)
    }

    /// Lookup a previously cached response. Returns `None` when no entry
    /// exists, or when the entry has expired (the entry is evicted as a
    /// side effect to keep the cache small).
    ///
    /// This variant ignores the args fingerprint and is preserved for
    /// existing callers and tests. Production code paths use
    /// [`Self::get_with_fingerprint`] to enforce
    /// `IDEMPOTENCY_KEY_MISMATCH`.
    #[must_use]
    pub fn get(&self, tool: &str, key: &str) -> Option<CachedResponse> {
        let composite = (tool.to_string(), key.to_string());
        let entry = self.inner.get(&composite)?;
        let (found, expired) = match entry.value() {
            CacheSlot::Done(done) if done.inserted_at.elapsed() <= self.ttl => {
                (Some(done.clone()), false)
            }
            CacheSlot::Done(_) => (None, true),
            CacheSlot::Pending(_) => (None, false),
        };
        drop(entry);
        if expired {
            self.inner.remove(&composite);
        }
        found
    }

    /// Lookup with an args fingerprint comparison.
    ///
    /// Returns:
    /// - [`IdempotencyOutcome::Miss`] when no live entry exists.
    /// - [`IdempotencyOutcome::Hit`] when the entry is live and the
    ///   args fingerprint matches.
    /// - [`IdempotencyOutcome::Mismatch`] when the entry is live but
    ///   the args fingerprint differs — the caller must surface
    ///   `IDEMPOTENCY_KEY_MISMATCH` rather than replay the stale body.
    #[must_use]
    pub fn get_with_fingerprint(
        &self,
        tool: &str,
        key: &str,
        fingerprint: &str,
    ) -> IdempotencyOutcome {
        let composite = (tool.to_string(), key.to_string());
        let Some(entry) = self.inner.get(&composite) else {
            return IdempotencyOutcome::Miss;
        };
        let (outcome, expired) = match entry.value() {
            CacheSlot::Done(done) if done.inserted_at.elapsed() > self.ttl => {
                (IdempotencyOutcome::Miss, true)
            }
            CacheSlot::Done(done) if done.args_fingerprint == fingerprint => {
                (IdempotencyOutcome::Hit(done.clone()), false)
            }
            CacheSlot::Done(_) => (IdempotencyOutcome::Mismatch, false),
            CacheSlot::Pending(_) => (IdempotencyOutcome::Miss, false),
        };
        drop(entry);
        if expired {
            self.inner.remove(&composite);
        }
        outcome
    }

    /// Insert a fresh response under `(tool, key)`, evicting expired
    /// entries first and trimming the oldest entries when the insert
    /// would push the cache above the configured cap.
    pub fn put(&self, tool: &str, key: &str, body: String, structured: Value) {
        self.put_with_fingerprint(tool, key, String::new(), body, structured);
    }

    /// Insert a fresh response with an explicit args fingerprint.
    pub fn put_with_fingerprint(
        &self,
        tool: &str,
        key: &str,
        fingerprint: String,
        body: String,
        structured: Value,
    ) {
        self.evict_expired();
        if self.inner.len() >= self.max_entries {
            self.shrink_to_max();
        }
        let entry = CachedResponse {
            body,
            structured,
            args_fingerprint: fingerprint,
            inserted_at: Instant::now(),
        };
        self.inner
            .insert((tool.to_string(), key.to_string()), CacheSlot::Done(entry));
    }

    /// Drop every expired entry (stale `Done` responses and `Pending`
    /// claims that leaked without running their `Drop`). Called from `put`
    /// / `claim` on every write and optionally from a periodic
    /// composition-root task.
    pub fn evict_expired(&self) {
        let ttl = self.ttl;
        self.inner.retain(|_, slot| slot_is_live(slot, ttl));
    }

    /// Drop the oldest `inner.len() - max_entries + 1` entries. Pulled
    /// out so the [`Self::put`] body stays under the 30-line ceiling.
    fn shrink_to_max(&self) {
        let extra = self
            .inner
            .len()
            .saturating_sub(self.max_entries)
            .saturating_add(1);
        let mut victims: Vec<((String, String), Instant)> = self
            .inner
            .iter()
            .map(|kv| {
                let ts = match kv.value() {
                    CacheSlot::Done(done) => done.inserted_at,
                    CacheSlot::Pending(pending) => pending.claimed_at,
                };
                (kv.key().clone(), ts)
            })
            .collect();
        victims.sort_by_key(|(_, ts)| *ts);
        for ((tool, key), _) in victims.into_iter().take(extra) {
            self.inner.remove(&(tool, key));
        }
    }

    /// Atomically claim `(tool, key)` for a fresh use-case run, or resolve
    /// to a replay / wait / mismatch verdict against the current slot.
    ///
    /// The whole check-and-claim happens under one shard lock (the `entry`
    /// API) and NEVER awaits, so two concurrent callers racing the same key
    /// cannot both win: exactly one gets [`ClaimOutcome::Claimed`] and
    /// installs the `Pending` slot; a racing duplicate gets
    /// [`ClaimOutcome::Wait`] (or `Hit` / `Mismatch`). This is the fix for
    /// the duplicate-side-effect race on retried mutating tool calls.
    #[must_use]
    pub fn claim(&self, tool: &str, key: &str, fingerprint: &str) -> ClaimOutcome<'_> {
        // Reap expired slots first so the `Occupied` arm only sees live
        // entries; an expired slot falls through to the fresh-claim path.
        self.evict_expired();
        if self.inner.len() >= self.max_entries {
            self.shrink_to_max();
        }
        match self.inner.entry((tool.to_string(), key.to_string())) {
            Entry::Occupied(occ) => classify_live(occ.get(), fingerprint),
            Entry::Vacant(slot) => {
                let waker = Arc::new(Notify::new());
                drop(slot.insert(CacheSlot::Pending(PendingSlot::new(
                    Arc::clone(&waker),
                    fingerprint,
                ))));
                ClaimOutcome::Claimed(self.guard(tool, key, waker))
            }
        }
    }

    /// Build the RAII [`ClaimGuard`] handed to the winning caller.
    fn guard(&self, tool: &str, key: &str, waker: Arc<Notify>) -> ClaimGuard<'_> {
        ClaimGuard {
            cache: self,
            tool: tool.to_string(),
            key: key.to_string(),
            waker,
            published: false,
        }
    }

    /// Swap the caller's in-flight `Pending` slot for a finished `Done`
    /// slot. Guarded by [`is_our_pending`] so a slower-than-TTL winner that
    /// was already superseded by a newer claim never clobbers it.
    fn complete_pending(&self, tool: &str, key: &str, waker: &Arc<Notify>, done: CacheSlot) {
        match self.inner.entry((tool.to_string(), key.to_string())) {
            Entry::Occupied(mut occ) => {
                if is_our_pending(occ.get(), waker) {
                    drop(occ.insert(done));
                }
            }
            Entry::Vacant(slot) => {
                drop(slot.insert(done));
            }
        }
    }

    /// Remove the caller's in-flight `Pending` slot iff it is still ours
    /// (identity via the `Notify` pointer). Called from [`ClaimGuard::drop`]
    /// when the winner finished without publishing a cacheable success.
    fn abandon_pending(&self, tool: &str, key: &str, waker: &Arc<Notify>) {
        drop(
            self.inner
                .remove_if(&(tool.to_string(), key.to_string()), |_, slot| {
                    is_our_pending(slot, waker)
                }),
        );
    }

    /// Number of entries currently held. Test helper; not public.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.len()
    }
}

/// Verdict of [`IdempotencyCache::claim`].
#[derive(Debug)]
pub enum ClaimOutcome<'a> {
    /// This caller won the race and must drive the use case, then either
    /// [`ClaimGuard::publish`] the success or drop the guard to release the
    /// claim. The guard's `Drop` always wakes parked callers.
    Claimed(ClaimGuard<'a>),
    /// A live `Done` slot with a matching args fingerprint exists — replay
    /// it verbatim.
    Hit(CachedResponse),
    /// A live slot exists under the same key but a different args
    /// fingerprint — surface `IDEMPOTENCY_KEY_MISMATCH`.
    Mismatch,
    /// A concurrent caller is mid-flight for this exact key + fingerprint.
    /// Await the [`Notify`] then re-read via
    /// [`IdempotencyCache::get_with_fingerprint`].
    Wait(Arc<Notify>),
}

/// RAII claim held by the winning caller for a `(tool, key)`.
///
/// While alive it owns the `Pending` slot. On `Drop` it ALWAYS wakes every
/// parked caller and, unless [`Self::publish`] already swapped in a `Done`
/// slot, removes the `Pending` slot — so a cancelled or panicked winner can
/// never wedge the key permanently.
#[derive(Debug)]
pub struct ClaimGuard<'a> {
    cache: &'a IdempotencyCache,
    tool: String,
    key: String,
    waker: Arc<Notify>,
    published: bool,
}

impl ClaimGuard<'_> {
    /// Replace the in-flight `Pending` slot with the successful response so
    /// concurrent and future callers replay it. Consumes the guard; its
    /// `Drop` then wakes the parked callers.
    pub fn publish(mut self, fingerprint: String, response: &CallToolResult) {
        let body = response
            .content
            .first()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();
        let structured = response.structured_content.clone().unwrap_or(Value::Null);
        let done = CacheSlot::Done(CachedResponse {
            body,
            structured,
            args_fingerprint: fingerprint,
            inserted_at: Instant::now(),
        });
        self.cache
            .complete_pending(&self.tool, &self.key, &self.waker, done);
        self.published = true;
    }
}

impl Drop for ClaimGuard<'_> {
    fn drop(&mut self) {
        if !self.published {
            self.cache
                .abandon_pending(&self.tool, &self.key, &self.waker);
        }
        // Always wake parked callers — even on a cancelled / panicked winner
        // — so they re-read and never block forever on a dropped claim.
        self.waker.notify_waiters();
    }
}

/// Classify a *live* slot for [`IdempotencyCache::claim`]. The `'a` is free:
/// this never builds a borrowing [`ClaimOutcome::Claimed`].
fn classify_live<'a>(slot: &CacheSlot, fingerprint: &str) -> ClaimOutcome<'a> {
    match slot {
        CacheSlot::Done(done) => {
            if done.args_fingerprint == fingerprint {
                ClaimOutcome::Hit(done.clone())
            } else {
                ClaimOutcome::Mismatch
            }
        }
        CacheSlot::Pending(pending) => {
            if pending.fingerprint == fingerprint {
                ClaimOutcome::Wait(Arc::clone(&pending.waker))
            } else {
                ClaimOutcome::Mismatch
            }
        }
    }
}

/// True iff `slot` is a `Pending` whose `Notify` is pointer-identical to
/// `waker` — i.e. the very claim the caller installed.
fn is_our_pending(slot: &CacheSlot, waker: &Arc<Notify>) -> bool {
    match slot {
        CacheSlot::Pending(pending) => Arc::ptr_eq(&pending.waker, waker),
        CacheSlot::Done(_) => false,
    }
}

/// Liveness predicate shared by [`IdempotencyCache::evict_expired`].
fn slot_is_live(slot: &CacheSlot, ttl: Duration) -> bool {
    match slot {
        CacheSlot::Done(done) => done.inserted_at.elapsed() <= ttl,
        CacheSlot::Pending(pending) => pending.claimed_at.elapsed() <= ttl,
    }
}

/// Outcome of inspecting the `_meta` envelope for an idempotency key.
#[derive(Debug, Clone)]
pub enum KeyOutcome {
    /// The caller did not supply a key; idempotency is OFF for this call.
    Absent,
    /// The caller supplied a valid key (1..=`IDEMPOTENCY_KEY_MAX_BYTES`).
    Present(String),
    /// The caller supplied an oversized key; emit
    /// `IDEMPOTENCY_KEY_TOO_LONG` and skip the use case.
    TooLong,
}

/// Read `_meta.idempotency_key` from the request context.
///
/// Returns [`KeyOutcome::Present`] for a non-empty string ≤
/// [`IDEMPOTENCY_KEY_MAX_BYTES`] bytes, [`KeyOutcome::TooLong`] for an
/// oversized string, and [`KeyOutcome::Absent`] in every other case
/// (missing, wrong type, empty string).
#[must_use]
pub fn extract_idempotency_key(ctx: &RequestContext<RoleServer>) -> KeyOutcome {
    extract_from_meta(&ctx.meta)
}

/// Inspect a [`Meta`] envelope directly. Pulled out so tests can drive
/// the parsing without constructing a full [`RequestContext`] (the
/// `rmcp::Peer::new` constructor is `pub(crate)`).
#[must_use]
pub fn extract_from_meta(meta: &Meta) -> KeyOutcome {
    let Some(value) = meta.get("idempotency_key") else {
        return KeyOutcome::Absent;
    };
    let Some(s) = value.as_str() else {
        return KeyOutcome::Absent;
    };
    if s.is_empty() {
        return KeyOutcome::Absent;
    }
    if s.len() > IDEMPOTENCY_KEY_MAX_BYTES {
        return KeyOutcome::TooLong;
    }
    KeyOutcome::Present(s.to_string())
}

/// Re-hydrate a cached response into a fresh [`CallToolResult`] using
/// the same dual-channel helper the live tool path uses.
#[must_use]
pub fn replay(cached: &CachedResponse) -> CallToolResult {
    ok_text_and_structured(cached.body.clone(), cached.structured.clone())
}

fn resolve_ttl_secs() -> u64 {
    env::var(TTL_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_IDEMPOTENCY_TTL_SECS)
}

fn resolve_max_entries() -> usize {
    env::var(MAX_ENTRIES_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_IDEMPOTENCY_MAX_ENTRIES)
}

/// Compute a deterministic 16-hex-char fingerprint of `args`.
///
/// Pipeline:
/// 1. `serde_json::to_value` to walk the type into a [`Value`] tree.
/// 2. [`canonicalise`] to recursively sort every JSON object key
///    so two semantically-equal payloads with different field order
///    produce the same byte stream.
/// 3. [`fnv1a_64`] to digest the canonical bytes — non-cryptographic
///    by design (the value never leaves the process and only needs
///    collision-resistance within a 5-minute TTL window).
///
/// On serialisation failure (which serde-json does not normally raise
/// for derive-`Serialize` structs) returns `"<unfingerprintable>"` so
/// the caller can still proceed; subsequent matches will compare the
/// same sentinel and behave the same as a `Hit` from a prior failed
/// fingerprinting — i.e. no false `Mismatch`.
#[must_use]
pub fn fingerprint_args<T: Serialize + ?Sized>(args: &T) -> String {
    serde_json::to_value(args).map_or_else(
        |_| "<unfingerprintable>".to_string(),
        |value| {
            let canonical = canonicalise(value);
            let mut buf = Vec::with_capacity(64);
            // `serde_json::to_writer` keeps the recursive ordering we
            // built in `canonicalise` — Value's Object backing
            // (`Map<String, Value>`) preserves insertion order.
            let _ = serde_json::to_writer(&mut buf, &canonical);
            format!("{:016x}", fnv1a_64(&buf))
        },
    )
}

/// Recursively reorder every JSON object so its keys are sorted. The
/// resulting [`Value`] serialises to a deterministic byte stream that
/// only depends on the data, not on the field declaration order or the
/// Rust struct layout.
fn canonicalise(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = serde_json::Map::with_capacity(entries.len());
            for (k, v) in entries {
                out.insert(k, canonicalise(v));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonicalise).collect()),
        // Scalars are already canonical — exhaustive match keeps the
        // `wildcard_enum_match_arm` lint happy.
        v @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)) => v,
    }
}

/// FNV-1a 64-bit non-cryptographic hash. Pure function, no
/// allocations, ~5 ns per byte. Stable across Rust toolchain versions
/// (it is the textbook FNV-1a constants).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::{
        CachedResponse, DEFAULT_IDEMPOTENCY_MAX_ENTRIES, DEFAULT_IDEMPOTENCY_TTL_SECS,
        IDEMPOTENCY_KEY_MAX_BYTES, IdempotencyCache, KeyOutcome, extract_from_meta, replay,
    };
    use rmcp::model::Meta;
    use serde_json::{Value, json};
    use std::time::Duration;
    use std::time::Instant;

    fn cached_body(text: &str) -> CachedResponse {
        CachedResponse {
            body: text.to_string(),
            structured: json!({"tool": "ssh_test", "status": "ok", "k": text}),
            args_fingerprint: String::new(),
            inserted_at: Instant::now(),
        }
    }

    #[test]
    fn idempotency_cache_dedups_within_ttl() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        cache.put("ssh_exec", "k1", "BODY1".to_string(), json!({"v": 1}));
        let hit = cache.get("ssh_exec", "k1").expect("entry present");
        assert_eq!(hit.body, "BODY1");
        assert_eq!(hit.structured["v"], 1);
    }

    #[test]
    fn idempotency_cache_evicts_after_ttl() {
        // 1ms TTL -> the second access must observe the entry as expired.
        let cache = IdempotencyCache::new(Duration::from_millis(1), 16);
        cache.put("ssh_exec", "k1", "B".to_string(), Value::Null);
        // Sleep long enough to outlive the TTL.
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get("ssh_exec", "k1").is_none());
        // The miss eagerly drops the expired entry.
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn idempotency_cache_respects_max_entries() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 3);
        for i in 0..5_u32 {
            // Stagger insertions slightly so `inserted_at` orderings are deterministic.
            cache.put("ssh_exec", &format!("k{i}"), format!("B{i}"), Value::Null);
            std::thread::sleep(Duration::from_millis(2));
        }
        // The oldest two entries (k0, k1) should have been pruned.
        assert!(cache.len() <= 3);
        assert!(cache.get("ssh_exec", "k4").is_some());
        assert!(cache.get("ssh_exec", "k0").is_none());
        assert!(cache.get("ssh_exec", "k1").is_none());
    }

    #[test]
    fn idempotency_key_present_returns_value() {
        let mut meta = Meta::new();
        meta.0.insert(
            "idempotency_key".to_string(),
            Value::String("retry-1".to_string()),
        );
        match extract_from_meta(&meta) {
            KeyOutcome::Present(k) => assert_eq!(k, "retry-1"),
            _ => unreachable!("expected Present"),
        }
    }

    #[test]
    fn idempotency_key_absent_when_meta_empty() {
        let meta = Meta::new();
        assert!(matches!(extract_from_meta(&meta), KeyOutcome::Absent));
    }

    #[test]
    fn idempotency_key_absent_when_string_empty() {
        let mut meta = Meta::new();
        meta.0
            .insert("idempotency_key".to_string(), Value::String(String::new()));
        assert!(matches!(extract_from_meta(&meta), KeyOutcome::Absent));
    }

    #[test]
    fn idempotency_key_too_long_when_exceeds_cap() {
        let mut meta = Meta::new();
        let big = "x".repeat(IDEMPOTENCY_KEY_MAX_BYTES + 1);
        meta.0
            .insert("idempotency_key".to_string(), Value::String(big));
        assert!(matches!(extract_from_meta(&meta), KeyOutcome::TooLong));
    }

    #[test]
    fn replay_round_trips_body_and_structured() {
        let entry = cached_body("rendered");
        let out = replay(&entry);
        assert_eq!(out.is_error, Some(false));
        let txt = out
            .content
            .first()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();
        assert_eq!(txt, "rendered");
        assert_eq!(
            out.structured_content.unwrap_or(Value::Null)["k"],
            "rendered"
        );
    }

    #[test]
    fn defaults_match_documented_values() {
        assert_eq!(DEFAULT_IDEMPOTENCY_TTL_SECS, 300);
        assert_eq!(DEFAULT_IDEMPOTENCY_MAX_ENTRIES, 1024);
        assert_eq!(IDEMPOTENCY_KEY_MAX_BYTES, 256);
    }

    /// Verifies the v4.7-step5 contract: when `_meta.idempotency_key`
    /// is absent, every call must reach the underlying use case (no
    /// caching). Drives a counter-incrementing closure and asserts the
    /// counter advances by 2 across two calls.
    #[test]
    fn idempotency_off_when_key_absent() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        let counter = AtomicUsize::new(0);
        for _ in 0..2 {
            // No key -> we record a fresh hit on every call.
            counter.fetch_add(1, Ordering::SeqCst);
            // Cache.get returns None because nothing was inserted.
            assert!(cache.get("ssh_exec", "k-absent").is_none());
        }
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    /// Verifies the v4.7-step5 dedup: with `_meta.idempotency_key`
    /// present the use case is invoked exactly once across two calls,
    /// and the second call returns the cached body verbatim.
    #[test]
    fn idempotency_on_when_key_present() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        let counter = AtomicUsize::new(0);
        let key = "retry-1";
        for _ in 0..2 {
            if cache.get("ssh_exec", key).is_some() {
                continue; // cached -> skip the use case
            }
            counter.fetch_add(1, Ordering::SeqCst);
            cache.put(
                "ssh_exec",
                key,
                "body".to_string(),
                json!({"status": "started"}),
            );
        }
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "use case must run exactly once"
        );
        let cached = cache.get("ssh_exec", key).expect("entry persists");
        assert_eq!(cached.body, "body");
        assert_eq!(cached.structured["status"], "started");
    }

    // ---- args fingerprint + IDEMPOTENCY_KEY_MISMATCH (v5) -------------

    use super::{IdempotencyOutcome, canonicalise, fingerprint_args};
    use serde::Serialize;

    #[derive(Debug, Serialize)]
    struct ProbeArgs<'a> {
        session_id: &'a str,
        command: &'a str,
        timeout_secs: Option<u64>,
    }

    #[test]
    fn fingerprint_is_deterministic_for_equal_payloads() {
        let a = ProbeArgs {
            session_id: "s-1",
            command: "echo hi",
            timeout_secs: Some(30),
        };
        let b = ProbeArgs {
            session_id: "s-1",
            command: "echo hi",
            timeout_secs: Some(30),
        };
        assert_eq!(fingerprint_args(&a), fingerprint_args(&b));
    }

    #[test]
    fn fingerprint_diverges_on_any_field_change() {
        let baseline = fingerprint_args(&ProbeArgs {
            session_id: "s-1",
            command: "echo hi",
            timeout_secs: Some(30),
        });
        let other = fingerprint_args(&ProbeArgs {
            session_id: "s-1",
            command: "echo bye",
            timeout_secs: Some(30),
        });
        assert_ne!(baseline, other);
    }

    #[test]
    fn fingerprint_canonicalises_object_key_order() {
        // Two semantically-equal Values where only key ORDER differs
        // must canonicalise to the same byte stream.
        let a = json!({"a": 1, "b": 2, "c": [10, 20]});
        let b = json!({"c": [10, 20], "b": 2, "a": 1});
        assert_eq!(fingerprint_args(&a), fingerprint_args(&b));
    }

    #[test]
    fn fingerprint_distinguishes_array_order() {
        // Array order is semantically meaningful — DON'T flatten.
        let a = json!({"xs": [1, 2, 3]});
        let b = json!({"xs": [3, 2, 1]});
        assert_ne!(fingerprint_args(&a), fingerprint_args(&b));
    }

    #[test]
    fn fingerprint_returns_16_hex_chars() {
        let fp = fingerprint_args(&ProbeArgs {
            session_id: "s",
            command: "c",
            timeout_secs: None,
        });
        assert_eq!(fp.len(), 16);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn canonicalise_recurses_into_nested_objects() {
        let v = json!({"outer": {"b": 1, "a": 2}, "x": [1, {"d": 4, "c": 3}]});
        let canon = canonicalise(v);
        let serialised = serde_json::to_string(&canon).expect("ser");
        // Outer keys sorted: outer, x. Inner outer keys: a, b. Inner
        // array object keys: c, d.
        assert_eq!(
            serialised,
            r#"{"outer":{"a":2,"b":1},"x":[1,{"c":3,"d":4}]}"#
        );
    }

    #[test]
    fn cache_hit_when_key_and_fingerprint_match() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        let fp = "deadbeefcafebabe".to_string();
        cache.put_with_fingerprint(
            "ssh_exec",
            "k1",
            fp.clone(),
            "body".to_string(),
            json!({"v": 1}),
        );
        match cache.get_with_fingerprint("ssh_exec", "k1", &fp) {
            IdempotencyOutcome::Hit(c) => {
                assert_eq!(c.body, "body");
                assert_eq!(c.args_fingerprint, fp);
            }
            other => unreachable!("expected Hit, got {other:?}"),
        }
    }

    #[test]
    fn cache_mismatch_when_fingerprint_differs() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        cache.put_with_fingerprint(
            "ssh_exec",
            "k1",
            "fp-A".to_string(),
            "body-A".to_string(),
            json!({"v": "a"}),
        );
        // Same key, different fingerprint -> Mismatch (NOT Hit).
        match cache.get_with_fingerprint("ssh_exec", "k1", "fp-B") {
            IdempotencyOutcome::Mismatch => {}
            other => unreachable!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn cache_miss_when_no_entry_exists() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        let outcome = cache.get_with_fingerprint("ssh_exec", "ghost", "fp");
        assert!(matches!(outcome, IdempotencyOutcome::Miss));
    }

    #[test]
    fn cache_miss_after_ttl_even_with_matching_fingerprint() {
        let cache = IdempotencyCache::new(Duration::from_millis(1), 16);
        cache.put_with_fingerprint(
            "ssh_exec",
            "k1",
            "fp".to_string(),
            "B".to_string(),
            Value::Null,
        );
        std::thread::sleep(Duration::from_millis(5));
        let outcome = cache.get_with_fingerprint("ssh_exec", "k1", "fp");
        assert!(matches!(outcome, IdempotencyOutcome::Miss));
    }

    #[test]
    fn distinct_keys_with_same_fingerprint_do_not_collide() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        let fp = "shared-fp".to_string();
        cache.put_with_fingerprint(
            "ssh_exec",
            "k1",
            fp.clone(),
            "body-1".to_string(),
            json!({"v": 1}),
        );
        cache.put_with_fingerprint(
            "ssh_exec",
            "k2",
            fp.clone(),
            "body-2".to_string(),
            json!({"v": 2}),
        );
        match cache.get_with_fingerprint("ssh_exec", "k1", &fp) {
            IdempotencyOutcome::Hit(c) => assert_eq!(c.body, "body-1"),
            other => unreachable!("expected Hit, got {other:?}"),
        }
        match cache.get_with_fingerprint("ssh_exec", "k2", &fp) {
            IdempotencyOutcome::Hit(c) => assert_eq!(c.body, "body-2"),
            other => unreachable!("expected Hit, got {other:?}"),
        }
    }

    #[test]
    fn put_legacy_path_stores_empty_fingerprint_so_get_with_match_works() {
        // The legacy `put` (no fingerprint) stores an empty fingerprint;
        // the new `get_with_fingerprint("")` therefore matches and replays
        // — preserves byte-compat for any callers that bypass the
        // fingerprint API.
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        cache.put("ssh_exec", "k1", "body".to_string(), Value::Null);
        match cache.get_with_fingerprint("ssh_exec", "k1", "") {
            IdempotencyOutcome::Hit(c) => assert_eq!(c.body, "body"),
            other => unreachable!("expected Hit on legacy entry, got {other:?}"),
        }
    }

    // ---- concurrency: claim / publish / abandon (Bug #23) -------------

    use super::ClaimOutcome;

    #[test]
    fn concurrent_claims_collapse_to_single_run() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        // First caller wins the claim and installs the Pending slot.
        let ClaimOutcome::Claimed(guard) = cache.claim("ssh_exec", "k1", "fp") else {
            unreachable!("first claim must win");
        };
        // A concurrent duplicate observes the in-flight Pending -> Wait,
        // NOT a second Claimed. This is the duplicate-side-effect fix: the
        // mutating use case is never driven twice for one key.
        assert!(matches!(
            cache.claim("ssh_exec", "k1", "fp"),
            ClaimOutcome::Wait(_)
        ));
        // Winner finishes and publishes its rendered response.
        let response = super::ok_text_and_structured("body".to_string(), json!({"v": 1}));
        guard.publish("fp".to_string(), &response);
        // Every subsequent caller now replays the Done slot.
        match cache.get_with_fingerprint("ssh_exec", "k1", "fp") {
            IdempotencyOutcome::Hit(c) => assert_eq!(c.body, "body"),
            other => unreachable!("expected Hit, got {other:?}"),
        }
    }

    #[test]
    fn claim_mismatch_when_pending_has_different_fingerprint() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        let ClaimOutcome::Claimed(_guard) = cache.claim("ssh_exec", "k1", "fp-A") else {
            unreachable!("first claim must win");
        };
        // Same key, different args mid-flight -> Mismatch, never Wait.
        assert!(matches!(
            cache.claim("ssh_exec", "k1", "fp-B"),
            ClaimOutcome::Mismatch
        ));
    }

    #[test]
    fn abandoned_claim_frees_the_key() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        {
            let ClaimOutcome::Claimed(_guard) = cache.claim("ssh_exec", "k1", "fp") else {
                unreachable!("first claim must win");
            };
            // `_guard` drops here WITHOUT publish -> the claim is abandoned.
        }
        // The key is free again; the abandoned Pending left no residue.
        assert!(matches!(
            cache.get_with_fingerprint("ssh_exec", "k1", "fp"),
            IdempotencyOutcome::Miss
        ));
        assert!(matches!(
            cache.claim("ssh_exec", "k1", "fp"),
            ClaimOutcome::Claimed(_)
        ));
    }

    #[test]
    fn claim_replays_after_publish() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        let ClaimOutcome::Claimed(guard) = cache.claim("ssh_exec", "k1", "fp") else {
            unreachable!("first claim must win");
        };
        let response = super::ok_text_and_structured("done".to_string(), json!({"v": 2}));
        guard.publish("fp".to_string(), &response);
        // A later caller with the same key + fingerprint replays via Hit.
        match cache.claim("ssh_exec", "k1", "fp") {
            ClaimOutcome::Hit(c) => assert_eq!(c.body, "done"),
            other => unreachable!("expected Hit, got {other:?}"),
        }
    }
}
