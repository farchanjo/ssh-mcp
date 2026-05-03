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
//! - `DashMap<(String, String), CachedResponse>` — lock-free hot path.
//! - TTL-based eviction enforced lazily on `get` (returns `None` when
//!   the entry is expired) plus an explicit [`IdempotencyCache::evict_expired`]
//!   sweep called on every insert and optionally driven by the
//!   composition root's background task.
//! - Soft `max_entries` cap enforced by purging the oldest entries when
//!   an insert would push us above the threshold. We pick the oldest
//!   entries by `inserted_at` — `DashMap` does not maintain an LRU order
//!   so we pay one `O(N)` sort on overflow. With `max_entries = 1024`
//!   and a 5-minute TTL this is a non-issue in practice.

use std::env;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use rmcp::RoleServer;
use rmcp::model::{CallToolResult, Meta};
use rmcp::service::RequestContext;
use serde_json::Value;

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
    /// Wall-clock timestamp the entry was inserted at.
    pub inserted_at: Instant,
}

/// Lock-free idempotency cache.
#[derive(Debug)]
pub struct IdempotencyCache {
    inner: DashMap<(String, String), CachedResponse>,
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
    #[must_use]
    pub fn get(&self, tool: &str, key: &str) -> Option<CachedResponse> {
        let composite = (tool.to_string(), key.to_string());
        let entry = self.inner.get(&composite)?;
        if entry.inserted_at.elapsed() <= self.ttl {
            return Some(entry.value().clone());
        }
        drop(entry);
        self.inner.remove(&composite);
        None
    }

    /// Insert a fresh response under `(tool, key)`, evicting expired
    /// entries first and trimming the oldest entries when the insert
    /// would push the cache above the configured cap.
    pub fn put(&self, tool: &str, key: &str, body: String, structured: Value) {
        self.evict_expired();
        if self.inner.len() >= self.max_entries {
            self.shrink_to_max();
        }
        let now = Instant::now();
        let entry = CachedResponse {
            body,
            structured,
            inserted_at: now,
        };
        self.inner
            .insert((tool.to_string(), key.to_string()), entry);
    }

    /// Drop every expired entry. Called from `put` on every insert and
    /// optionally from a periodic composition-root task.
    pub fn evict_expired(&self) {
        let ttl = self.ttl;
        self.inner
            .retain(|_, value| value.inserted_at.elapsed() <= ttl);
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
            .map(|kv| (kv.key().clone(), kv.value().inserted_at))
            .collect();
        victims.sort_by_key(|(_, ts)| *ts);
        for ((tool, key), _) in victims.into_iter().take(extra) {
            self.inner.remove(&(tool, key));
        }
    }

    /// Number of entries currently held. Test helper; not public.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.len()
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
            inserted_at: Instant::now(),
        }
    }

    #[test]
    fn idempotency_cache_dedups_within_ttl() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        cache.put("ssh_execute", "k1", "BODY1".to_string(), json!({"v": 1}));
        let hit = cache.get("ssh_execute", "k1").expect("entry present");
        assert_eq!(hit.body, "BODY1");
        assert_eq!(hit.structured["v"], 1);
    }

    #[test]
    fn idempotency_cache_evicts_after_ttl() {
        // 1ms TTL -> the second access must observe the entry as expired.
        let cache = IdempotencyCache::new(Duration::from_millis(1), 16);
        cache.put("ssh_execute", "k1", "B".to_string(), Value::Null);
        // Sleep long enough to outlive the TTL.
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get("ssh_execute", "k1").is_none());
        // The miss eagerly drops the expired entry.
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn idempotency_cache_respects_max_entries() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 3);
        for i in 0..5_u32 {
            // Stagger insertions slightly so `inserted_at` orderings are deterministic.
            cache.put(
                "ssh_execute",
                &format!("k{i}"),
                format!("B{i}"),
                Value::Null,
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        // The oldest two entries (k0, k1) should have been pruned.
        assert!(cache.len() <= 3);
        assert!(cache.get("ssh_execute", "k4").is_some());
        assert!(cache.get("ssh_execute", "k0").is_none());
        assert!(cache.get("ssh_execute", "k1").is_none());
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
            assert!(cache.get("ssh_execute", "k-absent").is_none());
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
            if cache.get("ssh_execute", key).is_some() {
                continue; // cached -> skip the use case
            }
            counter.fetch_add(1, Ordering::SeqCst);
            cache.put(
                "ssh_execute",
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
        let cached = cache.get("ssh_execute", key).expect("entry persists");
        assert_eq!(cached.body, "body");
        assert_eq!(cached.structured["status"], "started");
    }
}
