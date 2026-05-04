//! Chaos 13 — idempotency replay.
//!
//! When the same `idempotency_key` is replayed N times in rapid
//! succession, the cache MUST replay the cached response from the
//! first call without falling through to the use case.

use std::time::Duration;

use serde_json::json;
use ssh_mcp::infra::mcp::idempotency::IdempotencyCache;

const REPLAYS: u32 = 100;

#[test]
fn chaos13_rapid_idempotency_replay_returns_one_unique_response() {
    let cache = IdempotencyCache::new(Duration::from_secs(60), 1024);
    let tool = "ssh_connect";
    let key = "abc-123-def-456";
    let body = "SSH_CONNECT: OK\nSESSION_ID: sess-1\n".to_string();
    let structured = json!({
        "tool": "ssh_connect",
        "status": "ok",
        "session_id": "sess-1",
    });

    // First call populates the cache.
    cache.put(tool, key, body.clone(), structured.clone());

    // 99 replays MUST hit the cache and return identical output.
    let mut hits = 0_u32;
    for _ in 0..REPLAYS {
        if let Some(cached) = cache.get(tool, key) {
            assert_eq!(cached.body, body);
            assert_eq!(cached.structured, structured);
            hits += 1;
        }
    }

    // Every replay returned the cached entry.
    assert_eq!(hits, REPLAYS);

    // Distinct tool/key never collides.
    assert!(cache.get("ssh_disconnect", key).is_none());
    assert!(cache.get(tool, "different-key").is_none());
}

#[test]
fn chaos13_idempotency_ttl_evicts_after_window() {
    // 1ms TTL guarantees expiry without sleeping.
    let cache = IdempotencyCache::new(Duration::from_millis(1), 16);
    cache.put("t", "k", String::new(), json!({}));
    std::thread::sleep(Duration::from_millis(10));
    assert!(cache.get("t", "k").is_none());
}
