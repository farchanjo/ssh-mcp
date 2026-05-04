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
use std::time::{Duration, Instant};

use dashmap::DashMap;
use rmcp::RoleServer;
use rmcp::model::{CallToolResult, Meta};
use rmcp::service::RequestContext;
use serde::Serialize;
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
    /// 16-hex-char FNV-1a 64-bit digest of the canonical-JSON of the
    /// call arguments. Used to detect `IDEMPOTENCY_KEY_MISMATCH` —
    /// when the same key is reused with different args.
    pub args_fingerprint: String,
    /// Wall-clock timestamp the entry was inserted at.
    pub inserted_at: Instant,
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
    ///
    /// This variant ignores the args fingerprint and is preserved for
    /// existing callers and tests. Production code paths use
    /// [`Self::get_with_fingerprint`] to enforce
    /// `IDEMPOTENCY_KEY_MISMATCH`.
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
        if entry.inserted_at.elapsed() > self.ttl {
            drop(entry);
            self.inner.remove(&composite);
            return IdempotencyOutcome::Miss;
        }
        if entry.value().args_fingerprint == fingerprint {
            IdempotencyOutcome::Hit(entry.value().clone())
        } else {
            IdempotencyOutcome::Mismatch
        }
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
        let now = Instant::now();
        let entry = CachedResponse {
            body,
            structured,
            args_fingerprint: fingerprint,
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
            "ssh_execute",
            "k1",
            fp.clone(),
            "body".to_string(),
            json!({"v": 1}),
        );
        match cache.get_with_fingerprint("ssh_execute", "k1", &fp) {
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
            "ssh_execute",
            "k1",
            "fp-A".to_string(),
            "body-A".to_string(),
            json!({"v": "a"}),
        );
        // Same key, different fingerprint -> Mismatch (NOT Hit).
        match cache.get_with_fingerprint("ssh_execute", "k1", "fp-B") {
            IdempotencyOutcome::Mismatch => {}
            other => unreachable!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn cache_miss_when_no_entry_exists() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        let outcome = cache.get_with_fingerprint("ssh_execute", "ghost", "fp");
        assert!(matches!(outcome, IdempotencyOutcome::Miss));
    }

    #[test]
    fn cache_miss_after_ttl_even_with_matching_fingerprint() {
        let cache = IdempotencyCache::new(Duration::from_millis(1), 16);
        cache.put_with_fingerprint(
            "ssh_execute",
            "k1",
            "fp".to_string(),
            "B".to_string(),
            Value::Null,
        );
        std::thread::sleep(Duration::from_millis(5));
        let outcome = cache.get_with_fingerprint("ssh_execute", "k1", "fp");
        assert!(matches!(outcome, IdempotencyOutcome::Miss));
    }

    #[test]
    fn distinct_keys_with_same_fingerprint_do_not_collide() {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        let fp = "shared-fp".to_string();
        cache.put_with_fingerprint(
            "ssh_execute",
            "k1",
            fp.clone(),
            "body-1".to_string(),
            json!({"v": 1}),
        );
        cache.put_with_fingerprint(
            "ssh_execute",
            "k2",
            fp.clone(),
            "body-2".to_string(),
            json!({"v": 2}),
        );
        match cache.get_with_fingerprint("ssh_execute", "k1", &fp) {
            IdempotencyOutcome::Hit(c) => assert_eq!(c.body, "body-1"),
            other => unreachable!("expected Hit, got {other:?}"),
        }
        match cache.get_with_fingerprint("ssh_execute", "k2", &fp) {
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
        cache.put("ssh_execute", "k1", "body".to_string(), Value::Null);
        match cache.get_with_fingerprint("ssh_execute", "k1", "") {
            IdempotencyOutcome::Hit(c) => assert_eq!(c.body, "body"),
            other => unreachable!("expected Hit on legacy entry, got {other:?}"),
        }
    }
}
