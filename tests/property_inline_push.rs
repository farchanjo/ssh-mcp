//! ADR 0012 phase 8 -- property tests for the v7.1 inline-push
//! domain.
//!
//! The suite fuzzes the two pure-data surfaces ADR 0012 introduces:
//!
//! 1. `domain::inline_payload::InlinePayload::split` -- exhaustive
//!    cases for sequence + cursor monotonicity, UTF-8 boundary
//!    preservation on text URIs, byte-exact splits on binary URIs,
//!    and "fits" / "max=0" pathological branches. The splitter is
//!    the foundation for `LaneFanoutBridge::ship_inline_fragments`
//!    -- any drift here corrupts the wire payload before the bridge
//!    even sees it.
//! 2. `embed::formatter::Event::InlinePush` -- serde round-trip and
//!    field-set guard so the daemon's NDJSON wire shape cannot
//!    silently drift away from the seven-key contract
//!    (`ev`, `sub_id`, `uri`, `seq`, `cursor_after`, `len`,
//!    `bytes_b64`, `truncated`).
//!
//! Run with:
//!
//! ```text
//! cargo test --test property_inline_push
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::module_name_repetitions,
    clippy::default_numeric_fallback,
    clippy::wildcard_imports,
    clippy::needless_pass_by_value,
    reason = "property tests use unwrap and proptest macros that pull broad imports -- strict suppressions apply only to the test target"
)]

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use proptest::collection::vec as prop_vec;
use proptest::prelude::*;
use ssh_mcp::domain::inline_payload::InlinePayload;
use ssh_mcp::domain::subscription::SubId;
use ssh_mcp::embed::formatter::Event;

// ---------------------------------------------------------------------------
// Strategy helpers
// ---------------------------------------------------------------------------

const MAX_BYTES: usize = 16_384;
const MAX_FRAGMENT_CAP: usize = 16_384;

fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop_vec(any::<u8>(), 0..=MAX_BYTES)
}

fn arb_bytes_nonempty() -> impl Strategy<Value = Vec<u8>> {
    prop_vec(any::<u8>(), 1..=MAX_BYTES)
}

fn arb_utf8_string() -> impl Strategy<Value = String> {
    "\\PC{0,4096}".prop_map(|s| s.to_string())
}

fn arb_max_cap() -> impl Strategy<Value = usize> {
    1_usize..=MAX_FRAGMENT_CAP
}

fn shell_uri() -> String {
    "shell://0193f04e-3a2b-7c12-8d11-1f1f04ab92e1/output".to_string()
}

fn command_uri() -> String {
    "command://0193f04e-3a2b-7c12-8d11-1f1f04ab92e1/output".to_string()
}

fn fixed_sub_id() -> SubId {
    SubId::new("0193f04e-3a2b-7c12-8d11-1f1f04ab92e1".to_string())
}

fn payload_for(bytes: Vec<u8>, uri: String, seq: u64) -> InlinePayload {
    let cursor_after = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    InlinePayload::new(fixed_sub_id(), uri, seq, cursor_after, bytes, false)
}

fn arb_event_inline_push() -> impl Strategy<Value = Event> {
    (
        "[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}",
        prop_oneof![Just(shell_uri()), Just(command_uri())],
        0_u64..=u64::from(u32::MAX),
        0_u64..=u64::from(u32::MAX),
        0_u64..=4_096_u64,
        "[A-Za-z0-9+/]{0,128}={0,2}",
        any::<bool>(),
    )
        .prop_map(
            |(sub_id, uri, seq, cursor_after, len, bytes_b64, truncated)| Event::InlinePush {
                sub_id,
                uri,
                seq,
                cursor_after,
                len,
                bytes_b64,
                truncated,
            },
        )
}

// ---------------------------------------------------------------------------
// Properties -- InlinePayload::split
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `prop_split_preserves_total_bytes`
    #[test]
    fn prop_split_preserves_total_bytes(
        bytes in arb_bytes(),
        max in arb_max_cap(),
        is_text in any::<bool>(),
    ) {
        let original_len = bytes.len();
        let p = payload_for(bytes, shell_uri(), 0);
        let out = p.split(max, is_text);
        let summed: usize = out.iter().map(|f| f.bytes.len()).sum();
        prop_assert_eq!(summed, original_len, "bytes lost or duplicated across fragments");
    }

    /// `prop_split_seq_monotonic`
    #[test]
    fn prop_split_seq_monotonic(
        bytes in arb_bytes_nonempty(),
        max in arb_max_cap(),
        starting_seq in any::<u64>(),
    ) {
        let p = payload_for(bytes, shell_uri(), starting_seq);
        let out = p.split(max, true);
        prop_assert!(!out.is_empty(), "split must always return at least one fragment");
        for (offset, frag) in out.iter().enumerate() {
            let expected = starting_seq.wrapping_add(offset as u64);
            prop_assert_eq!(frag.seq, expected, "seq drift at fragment offset {}", offset);
        }
    }

    /// `prop_split_cursor_monotonic`
    #[test]
    fn prop_split_cursor_monotonic(
        bytes in arb_bytes_nonempty(),
        max in arb_max_cap(),
    ) {
        let p = payload_for(bytes.clone(), shell_uri(), 0);
        let expected_final_cursor = p.cursor_after;
        let out = p.split(max, true);
        for window in out.windows(2) {
            prop_assert!(
                window[0].cursor_after < window[1].cursor_after,
                "cursor_after must strictly increase: {} !< {}",
                window[0].cursor_after,
                window[1].cursor_after,
            );
        }
        if let Some(last) = out.last() {
            prop_assert_eq!(
                last.cursor_after,
                expected_final_cursor,
                "final fragment cursor must equal input cursor_after",
            );
        }
    }

    /// `prop_split_each_fragment_under_max`
    #[test]
    fn prop_split_each_fragment_under_max(
        bytes in arb_bytes_nonempty(),
        max in arb_max_cap(),
        is_text in any::<bool>(),
    ) {
        let original_len = bytes.len();
        let p = payload_for(bytes, shell_uri(), 0);
        let out = p.split(max, is_text);
        if original_len <= max {
            prop_assert_eq!(out.len(), 1, "fits path must return a single fragment");
            prop_assert_eq!(out[0].bytes.len(), original_len);
        } else {
            for frag in &out {
                prop_assert!(
                    frag.bytes.len() <= max,
                    "fragment exceeds max: len={} max={}",
                    frag.bytes.len(),
                    max,
                );
            }
        }
    }

    /// `prop_split_first_fragment_starts_at_input_cursor`
    #[test]
    fn prop_split_first_fragment_starts_at_input_cursor(
        bytes in arb_bytes_nonempty(),
        max in arb_max_cap(),
    ) {
        let total = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let cursor_after = total + 1_000;
        let p = InlinePayload::new(
            fixed_sub_id(),
            shell_uri(),
            0,
            cursor_after,
            bytes,
            false,
        );
        let input_baseline = cursor_after.saturating_sub(total);
        let out = p.split(max, true);
        let first = out.first().expect("at least one fragment");
        let first_len = u64::try_from(first.bytes.len()).unwrap_or(u64::MAX);
        let first_baseline = first.cursor_after.saturating_sub(first_len);
        prop_assert_eq!(
            first_baseline,
            input_baseline,
            "first fragment shifted the producer baseline",
        );
    }

    /// `prop_split_concat_equals_original`
    #[test]
    fn prop_split_concat_equals_original(
        bytes in arb_bytes(),
        max in arb_max_cap(),
        is_text in any::<bool>(),
    ) {
        let original = bytes.clone();
        let p = payload_for(bytes, shell_uri(), 0);
        let out = p.split(max, is_text);
        let recovered: Vec<u8> = out.iter().flat_map(|f| f.bytes.clone()).collect();
        prop_assert_eq!(recovered, original, "concat drift");
    }

    /// `prop_split_text_uri_preserves_utf8`
    #[test]
    fn prop_split_text_uri_preserves_utf8(
        s in arb_utf8_string(),
        max_seed in 4_usize..=4_096_usize,
    ) {
        prop_assume!(!s.is_empty());
        let bytes = s.into_bytes();
        let max = max_seed.min(bytes.len().max(4));
        let p = payload_for(bytes, shell_uri(), 0);
        let out = p.split(max, true);
        for (i, frag) in out.iter().enumerate() {
            prop_assert!(
                std::str::from_utf8(&frag.bytes).is_ok(),
                "fragment {i} is not valid UTF-8 (max={max})",
            );
        }
    }

    /// `prop_split_binary_uri_byte_exact`
    #[test]
    fn prop_split_binary_uri_byte_exact(
        bytes in arb_bytes_nonempty(),
        max in arb_max_cap(),
    ) {
        let total = bytes.len();
        prop_assume!(total > max);
        let p = payload_for(bytes, command_uri(), 0);
        let out = p.split(max, false);
        prop_assert!(out.len() >= 2, "split must produce >= 2 fragments");
        for (i, frag) in out.iter().enumerate() {
            if i + 1 == out.len() {
                let trailing = total - max * (out.len() - 1);
                prop_assert_eq!(
                    frag.bytes.len(),
                    trailing,
                    "final fragment carries the remainder",
                );
            } else {
                prop_assert_eq!(
                    frag.bytes.len(),
                    max,
                    "non-final fragment must be exactly max bytes on binary URI",
                );
            }
        }
    }

    /// `prop_event_inline_push_roundtrips_via_serde`
    ///
    /// `Event` is `Serialize`-only (the daemon never deserialises
    /// its own NDJSON output -- consumers are external `jq` / `tee`
    /// pipelines). The "round-trip" property therefore serialises
    /// the event, parses the JSON into a `serde_json::Value`, and
    /// verifies every typed field appears with the expected value.
    #[test]
    fn prop_event_inline_push_roundtrips_via_serde(event in arb_event_inline_push()) {
        let encoded = serde_json::to_string(&event).expect("serialize ok");
        let value: serde_json::Value = serde_json::from_str(&encoded).expect("parse json ok");
        let Event::InlinePush {
            ref sub_id,
            ref uri,
            seq,
            cursor_after,
            len,
            ref bytes_b64,
            truncated,
        } = event
        else {
            unreachable!("strategy only emits InlinePush");
        };
        prop_assert_eq!(value.get("ev").and_then(|v| v.as_str()), Some("inline_push"));
        prop_assert_eq!(value.get("sub_id").and_then(|v| v.as_str()), Some(sub_id.as_str()));
        prop_assert_eq!(value.get("uri").and_then(|v| v.as_str()), Some(uri.as_str()));
        prop_assert_eq!(value.get("seq").and_then(|v| v.as_u64()), Some(seq));
        prop_assert_eq!(value.get("cursor_after").and_then(|v| v.as_u64()), Some(cursor_after));
        prop_assert_eq!(value.get("len").and_then(|v| v.as_u64()), Some(len));
        prop_assert_eq!(value.get("bytes_b64").and_then(|v| v.as_str()), Some(bytes_b64.as_str()));
        prop_assert_eq!(value.get("truncated").and_then(|v| v.as_bool()), Some(truncated));
    }

    /// `prop_event_inline_push_json_has_expected_field_set`
    #[test]
    fn prop_event_inline_push_json_has_expected_field_set(event in arb_event_inline_push()) {
        let encoded = serde_json::to_string(&event).expect("serialize ok");
        let value: serde_json::Value = serde_json::from_str(&encoded).expect("parse ok");
        let object = value.as_object().expect("InlinePush serialises as object");
        let mut keys: Vec<&String> = object.keys().collect();
        keys.sort();
        let expected: Vec<&str> = vec![
            "bytes_b64",
            "cursor_after",
            "ev",
            "len",
            "seq",
            "sub_id",
            "truncated",
            "uri",
        ];
        let got: Vec<&str> = keys.iter().map(|k| k.as_str()).collect();
        prop_assert_eq!(got, expected, "inline_push event field set drift");
        prop_assert_eq!(
            object.get("ev").and_then(|v| v.as_str()),
            Some("inline_push"),
            "ev tag must be `inline_push`",
        );
    }
}

// ---------------------------------------------------------------------------
// Property -- base64 fidelity helper
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// `prop_inline_push_event_bytes_b64_is_passthrough`
    #[test]
    fn prop_inline_push_event_bytes_b64_is_passthrough(bytes in arb_bytes()) {
        let encoded = B64.encode(&bytes);
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let event = Event::InlinePush {
            sub_id: "0193f04e-3a2b-7c12-8d11-1f1f04ab92e1".to_string(),
            uri: shell_uri(),
            seq: 0,
            cursor_after: len,
            len,
            bytes_b64: encoded.clone(),
            truncated: false,
        };
        let json = serde_json::to_string(&event).expect("encode ok");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse ok");
        let shipped = value
            .get("bytes_b64")
            .and_then(|v| v.as_str())
            .expect("bytes_b64 field must be present");
        prop_assert_eq!(shipped, encoded.as_str(), "encoder must ship the verbatim string");
        let decoded = B64.decode(shipped.as_bytes()).expect("base64 decode ok");
        prop_assert_eq!(decoded, bytes, "base64 round-trip lost bytes");
    }
}
