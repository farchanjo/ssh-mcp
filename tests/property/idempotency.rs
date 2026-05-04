//! Properties: idempotency cache.
//!
//! - `prop_idempotency_key_dedup_deterministic` — same key + same
//!   tool returns the same response across repeats (within TTL).
//! - `prop_idempotency_distinct_keys_independent` — distinct keys
//!   never alias.

use std::time::Duration;

use proptest::prelude::*;
use serde_json::json;
use ssh_mcp::infra::mcp::idempotency::IdempotencyCache;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// `prop_idempotency_key_dedup_deterministic`
    ///
    /// The same `(tool, key)` always returns the cached response.
    #[test]
    fn prop_idempotency_key_dedup_deterministic(
        tool in "[a-z_]{2,16}",
        key in "[A-Za-z0-9-]{4,32}",
        repeats in 1_u32..32,
    ) {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 1024);
        let body = format!("OK_{tool}_{key}");
        let structured = json!({"tool": tool.clone(), "key": key.clone()});
        cache.put(&tool, &key, body.clone(), structured.clone());
        for _ in 0..repeats {
            let got = cache.get(&tool, &key).unwrap();
            prop_assert_eq!(&got.body, &body);
            prop_assert_eq!(&got.structured, &structured);
        }
    }

    /// `prop_idempotency_distinct_keys_independent`
    ///
    /// A key not present in the cache returns None even when other
    /// keys are populated.
    #[test]
    fn prop_idempotency_distinct_keys_independent(
        keys in proptest::collection::hash_set("[a-z0-9]{4,16}", 2..8),
    ) {
        let cache = IdempotencyCache::new(Duration::from_secs(60), 1024);
        let keys_vec: Vec<String> = keys.into_iter().collect();
        // Populate every key but the first.
        for k in keys_vec.iter().skip(1) {
            cache.put("tool", k, k.clone(), json!({"k": k}));
        }
        // First key MUST miss.
        prop_assert!(cache.get("tool", &keys_vec[0]).is_none());
        // Every other key MUST hit.
        for k in keys_vec.iter().skip(1) {
            prop_assert!(cache.get("tool", k).is_some());
        }
    }
}
