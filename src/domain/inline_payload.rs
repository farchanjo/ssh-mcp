//! Inline-push notification payload for the v7.1 (ADR 0012) layer.
//!
//! Pure data + algorithm. The struct mirrors the
//! `notifications/ssh/output` wire shape: a single contiguous slice
//! of bytes that a `LaneFanoutBridge` pulls from the producer's ring
//! buffer and hands to the notifier port for delivery.
//!
//! This module is intentionally adapter-free — no `tokio`, no
//! `Atomic*`, no `Arc`. The atomic counters that drive the per-lane
//! inline-push state live alongside the rest of
//! [`crate::domain::subscription`] consumers in
//! `crate::adapters::subscription::subscriber_lane`; the
//! [`InlinePayload`] here is the value the bridge composes and ships.
//!
//! See [ADR 0012](../docs/adr/0012-inline-push-notifications.md) for the
//! design rationale, capability handshake, and per-lane gating.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::domain::subscription::SubId;

/// Coalesced byte window destined for a single inline-push lane.
///
/// One [`InlinePayload`] corresponds to a single
/// `notifications/ssh/output` notification at the wire level. When
/// the byte window exceeds the per-notification cap negotiated at
/// `initialize` time, [`InlinePayload::split`] divides the payload
/// into contiguous fragments with monotonic `seq` and a
/// `cursor_after` that tracks the producer-side cumulative offset
/// at the end of each fragment.
///
/// Invariants:
/// - `bytes.len()` is the wire `len` field; the wire emits the byte
///   count derived from this `Vec<u8>` (no separate cached length).
/// - `seq` is strictly monotonic across the fragments returned from
///   [`Self::split`], starting from the input's `seq`.
/// - `cursor_after` of the final fragment from [`Self::split`] equals
///   the input's `cursor_after`; intermediate fragments expose the
///   producer offset at the boundary they finish.
/// - On text URIs the fragment boundary is UTF-8-safe — the helper
///   never splits inside a multi-byte sequence as long as `max` is
///   wider than the widest UTF-8 code point at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InlinePayload {
    /// Subscription identifier minted at `sub_open` time.
    ///
    /// Lets multiplexed hosts demultiplex inline notifications back
    /// onto the originating subscription when several lanes share a
    /// single peer.
    pub sub_id: SubId,
    /// Resource URI the bytes belong to (e.g.
    /// `shell://<id>/output`).
    ///
    /// The fragment splitter consults the scheme prefix via the
    /// `is_text` flag supplied to [`Self::split`] to decide whether
    /// to walk a UTF-8 continuation boundary.
    pub uri: String,
    /// Monotonic sequence number within the lane.
    ///
    /// Allocated by the lane's `inline_seq` atomic on the adapter
    /// side; preserved across [`Self::split`] fragments by simple
    /// in-place increment.
    pub seq: u64,
    /// Producer-side cumulative byte offset at the end of this
    /// payload.
    ///
    /// Equivalent to the `byte_cursor` value the legacy
    /// `resources/read?cursor=auto` path would return immediately
    /// after delivering these bytes.
    pub cursor_after: u64,
    /// Coalesced byte window. The wire encoding (base64) is the
    /// adapter's job; the domain layer carries raw bytes.
    pub bytes: Vec<u8>,
    /// `true` when this fragment is not the final fragment of the
    /// originating window (i.e. callers should wait for the next
    /// `seq` to reassemble the full window). `false` on single-shot
    /// payloads and on the final fragment of a split window.
    pub truncated: bool,
}

impl InlinePayload {
    /// Build a payload directly.
    ///
    /// Production paths route through the bridge's
    /// `compose_inline_payload` helper rather than this constructor;
    /// the function exists for test fixtures and for the future
    /// daemon adapter that round-trips the inline event through
    /// NDJSON. Internal invariants are checked with `debug_assert!`
    /// so release builds pay no cost.
    #[must_use]
    pub fn new(
        sub_id: SubId,
        uri: String,
        seq: u64,
        cursor_after: u64,
        bytes: Vec<u8>,
        truncated: bool,
    ) -> Self {
        debug_assert!(
            cursor_after >= u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            "cursor_after must include this fragment's bytes",
        );
        Self {
            sub_id,
            uri,
            seq,
            cursor_after,
            bytes,
            truncated,
        }
    }

    /// Split a payload that exceeds `max` bytes into contiguous
    /// fragments with monotonic `seq` and `cursor_after`.
    ///
    /// When `self.bytes.len() <= max` (or `max == 0`) the input is
    /// returned untouched as the single element of the returned
    /// vector. Otherwise the byte window is walked in chunks of
    /// `max`; the helper [`utf8_safe_split`] backs the boundary off
    /// continuation bytes when `is_text` is `true`.
    ///
    /// If a back-walk would leave a fragment empty (which can only
    /// happen when `max` is narrower than the widest UTF-8 code
    /// point at the boundary), the splitter falls back to the byte
    /// boundary so the loop always makes forward progress; the
    /// resulting fragment may carry a partial multi-byte sequence
    /// in that pathological case.
    #[must_use]
    pub fn split(self, max: usize, is_text: bool) -> Vec<Self> {
        if max == 0 || self.bytes.len() <= max {
            return vec![self];
        }
        let total = self.bytes.len();
        let mut out: Vec<Self> = Vec::new();
        let mut start: usize = 0;
        let mut seq = self.seq;
        while start < total {
            let proposed = (start + max).min(total);
            let safe = utf8_safe_split(&self.bytes, start, proposed, is_text);
            let end = if safe == start { proposed } else { safe };
            let trailing = u64::try_from(total - end).unwrap_or(u64::MAX);
            out.push(Self {
                sub_id: self.sub_id.clone(),
                uri: self.uri.clone(),
                seq,
                cursor_after: self.cursor_after - trailing,
                bytes: self.bytes[start..end].to_vec(),
                truncated: end < total,
            });
            seq = seq.wrapping_add(1);
            start = end;
        }
        out
    }
}

/// Adjust a proposed split index so it falls on a UTF-8 boundary.
///
/// When `is_text` is `false`, returns `end` unchanged. Otherwise
/// walks backwards while `bytes[end]` is a UTF-8 continuation byte
/// (top two bits `10xxxxxx`) and `end > start`. Bounded by three
/// iterations in the worst case (a 4-byte code point at the
/// boundary).
fn utf8_safe_split(bytes: &[u8], start: usize, end: usize, is_text: bool) -> usize {
    if !is_text {
        return end;
    }
    let mut idx = end;
    while idx > start && idx < bytes.len() && (bytes[idx] & 0b1100_0000) == 0b1000_0000 {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::{InlinePayload, utf8_safe_split};
    use crate::domain::subscription::SubId;

    fn sample_sub_id() -> SubId {
        SubId::new("0193f04e-3a2b-7c12-8d11-1f1f04ab92e1".to_string())
    }

    fn sample_uri() -> String {
        "shell://0193f04e-3a2b-7c12-8d11-1f1f04ab92e1/output".to_string()
    }

    fn payload(bytes: Vec<u8>, cursor_after: u64, seq: u64) -> InlinePayload {
        InlinePayload::new(
            sample_sub_id(),
            sample_uri(),
            seq,
            cursor_after,
            bytes,
            false,
        )
    }

    #[test]
    fn split_returns_self_when_under_max() {
        let bytes = vec![b'a'; 100];
        let p = payload(bytes.clone(), 1_000, 7);
        let out = p.clone().split(200, true);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], p);
        assert_eq!(out[0].bytes, bytes);
        assert_eq!(out[0].cursor_after, 1_000);
        assert_eq!(out[0].seq, 7);
        assert!(!out[0].truncated);
    }

    #[test]
    fn split_returns_two_when_over_max() {
        let bytes = vec![b'a'; 250];
        let cursor_after: u64 = 10_000;
        let p = payload(bytes, cursor_after, 1);
        let out = p.split(128, true);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].bytes.len(), 128);
        assert_eq!(out[1].bytes.len(), 122);
        assert!(out[0].truncated);
        assert!(!out[1].truncated);
        assert_eq!(out[0].seq, 1);
        assert_eq!(out[1].seq, 2);
        assert_eq!(out[0].cursor_after, cursor_after - 122);
        assert_eq!(out[1].cursor_after, cursor_after);
    }

    #[test]
    fn split_respects_utf8_boundary_for_text_uri() {
        // 127 ASCII bytes then a 2-byte `é` (0xC3, 0xA9) at index
        // 127..=128. With max = 128 the proposed boundary lands on
        // index 128 (the continuation byte 0xA9); the safe split
        // must back off to index 127 so the `é` ships intact in the
        // second fragment.
        let mut bytes = vec![b'a'; 127];
        bytes.extend_from_slice(&[0xC3, 0xA9]);
        let total = bytes.len();
        let cursor_after: u64 = total as u64;
        let p = payload(bytes.clone(), cursor_after, 0);
        let out = p.split(128, true);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].bytes.len(), 127);
        assert_eq!(out[1].bytes, vec![0xC3, 0xA9]);
        assert!(out[0].truncated);
        assert!(!out[1].truncated);
    }

    #[test]
    fn split_ignores_utf8_for_binary_uri() {
        let mut bytes = vec![b'a'; 127];
        bytes.extend_from_slice(&[0xC3, 0xA9]);
        let total = bytes.len();
        let p = payload(bytes, total as u64, 0);
        let out = p.split(128, false);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].bytes.len(), 128);
        assert_eq!(out[0].bytes[127], 0xC3);
        assert_eq!(out[1].bytes, vec![0xA9]);
    }

    #[test]
    fn split_three_byte_utf8() {
        // 126 ASCII then `€` (0xE2, 0x82, 0xAC). With max = 128 the
        // proposed boundary lands on index 128 (continuation
        // 0xAC). The helper walks back over 0xAC and 0x82 to the
        // lead byte 0xE2 at index 126.
        let mut bytes = vec![b'a'; 126];
        bytes.extend_from_slice(&[0xE2, 0x82, 0xAC]);
        let total = bytes.len();
        let p = payload(bytes, total as u64, 0);
        let out = p.split(128, true);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].bytes.len(), 126);
        assert_eq!(out[1].bytes, vec![0xE2, 0x82, 0xAC]);
    }

    #[test]
    fn cursor_after_monotonic() {
        let bytes = vec![b'x'; 1_000];
        let cursor_after: u64 = 5_000;
        let p = payload(bytes, cursor_after, 0);
        let out = p.split(128, true);
        assert!(out.len() >= 2);
        for window in out.windows(2) {
            assert!(
                window[0].cursor_after < window[1].cursor_after,
                "cursor_after must strictly increase: {} !< {}",
                window[0].cursor_after,
                window[1].cursor_after,
            );
        }
        let last_cursor = out.last().map(|p| p.cursor_after).unwrap_or_default();
        assert_eq!(last_cursor, cursor_after);
    }

    #[test]
    fn seq_monotonic() {
        let bytes = vec![b'y'; 700];
        let p = payload(bytes, 700, 42);
        let out = p.split(100, true);
        assert!(out.len() >= 2);
        for (offset, frag) in out.iter().enumerate() {
            let expected = 42_u64 + offset as u64;
            assert_eq!(frag.seq, expected, "fragment {offset} seq mismatch");
        }
    }

    #[test]
    fn len_matches_bytes_len_in_every_fragment() {
        let bytes = vec![b'z'; 555];
        let p = payload(bytes.clone(), 555, 0);
        let out = p.split(100, true);
        let recovered: usize = out.iter().map(|f| f.bytes.len()).sum();
        assert_eq!(recovered, bytes.len());
        for frag in &out {
            // The wire `len` field is always derived from
            // `bytes.len()` — the domain never carries a separate
            // cached length.
            assert!(!frag.bytes.is_empty());
        }
    }

    #[test]
    fn split_max_equals_len_returns_self() {
        let bytes = vec![b'q'; 64];
        let p = payload(bytes, 64, 9);
        let out = p.clone().split(64, true);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], p);
    }

    #[test]
    fn split_max_one_byte_text_safe() {
        // Pathological case: 4-byte `🚀` and max=1. The back-walk
        // would emit zero bytes, so the splitter falls back to the
        // byte boundary and produces four 1-byte fragments. The
        // contract: forward progress is guaranteed; UTF-8 fidelity
        // is sacrificed only when `max` is narrower than the widest
        // code point at the boundary.
        let bytes = vec![0xF0, 0x9F, 0x9A, 0x80];
        let p = payload(bytes.clone(), 4, 0);
        let out = p.split(1, true);
        assert_eq!(out.len(), 4);
        let recovered: Vec<u8> = out.iter().flat_map(|f| f.bytes.clone()).collect();
        assert_eq!(recovered, bytes);
    }

    #[test]
    fn split_max_zero_returns_self() {
        // Defensive: max == 0 must not infinite-loop. We treat it
        // the same way as the "fits" branch.
        let bytes = vec![b'a'; 32];
        let p = payload(bytes, 32, 0);
        let out = p.clone().split(0, true);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], p);
    }

    #[test]
    fn utf8_safe_split_binary_passthrough() {
        let bytes = [0xC3, 0xA9, 0xC3, 0xA9];
        assert_eq!(utf8_safe_split(&bytes, 0, 3, false), 3);
    }

    #[test]
    fn utf8_safe_split_walks_back_to_lead() {
        let bytes = [b'a', 0xE2, 0x82, 0xAC, b'b'];
        // Proposed split at 3 — continuation byte 0x82. Helper
        // walks back to 1 (lead 0xE2).
        assert_eq!(utf8_safe_split(&bytes, 0, 3, true), 1);
    }
}
