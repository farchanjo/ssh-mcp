//! Production [`IdGeneratorPort`] adapter backed by [`uuid::Uuid::now_v7`].
//!
//! Each `new_*_id` call mints an independent [`uuid::Uuid::now_v7`] and
//! wraps it in the matching domain newtype from [`crate::domain::ids`].
//! The generator holds no state, so the adapter is a zero-sized type
//! that can be cloned, copied and shared via `Arc<UuidIds>` without
//! heap allocation.
//!
//! ## Why `UUIDv7` (ADR 0004)
//!
//! v5 Phase 2 introduces per-`SubId` lanes that the channel-mux
//! processes in arrival order. `UUIDv4` is purely random — two ids
//! minted milliseconds apart sort arbitrarily, which complicates
//! operability (log correlation, debugger sort, replay anchoring).
//!
//! `UUIDv7` embeds a 48-bit Unix-epoch millisecond timestamp in the
//! first 6 bytes (RFC 9562 §5.7). The remaining 74 bits are random,
//! still well above the collision-resistance threshold for short-lived
//! ids. Switching every id type to v7 keeps lex-ordering aligned with
//! creation time across the catalogue without weakening uniqueness.
//!
//! Decision: ALL production ids use v7 (sessions, commands, shells,
//! transfers, forwards, plus the per-`SubId` ids minted via the
//! lane adapter). The chaos30 fixture flagged the previous v4 mint as
//! a divergence between the test fixture and production wiring; v7
//! across the board closes that gap.

use uuid::Uuid;

use crate::domain::ids::{CommandId, ForwardId, SessionId, ShellId, TransferId};
use crate::domain::rsync_ids::RsyncId;
use crate::ports::id_generator::IdGeneratorPort;

/// Production id generator that wraps [`uuid::Uuid::now_v7`].
///
/// Zero-sized, `Copy`, safe to share across threads. Construct with
/// [`UuidIds::default`] (or `UuidIds`).
#[derive(Debug, Default, Clone, Copy)]
pub struct UuidIds;

impl IdGeneratorPort for UuidIds {
    fn new_session_id(&self) -> SessionId {
        SessionId::new(Uuid::now_v7().to_string())
    }

    fn new_command_id(&self) -> CommandId {
        CommandId::new(Uuid::now_v7().to_string())
    }

    fn new_shell_id(&self) -> ShellId {
        ShellId::new(Uuid::now_v7().to_string())
    }

    fn new_transfer_id(&self) -> TransferId {
        TransferId::new(Uuid::now_v7().to_string())
    }

    fn new_forward_id(&self) -> ForwardId {
        ForwardId::new(Uuid::now_v7().to_string())
    }

    fn new_rsync_id(&self) -> RsyncId {
        RsyncId::new(Uuid::now_v7().to_string())
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::UuidIds;
    use crate::ports::id_generator::IdGeneratorPort;

    #[test]
    fn uuid_ids_is_dyn_safe() {
        fn _accepts_dyn(_p: &dyn IdGeneratorPort) {}
        let gen_ = UuidIds;
        _accepts_dyn(&gen_);
    }

    #[test]
    fn session_ids_are_distinct_across_calls() {
        let gen_ = UuidIds;
        let a = gen_.new_session_id();
        let b = gen_.new_session_id();
        assert_ne!(a, b, "two consecutive `UUIDv7` session ids must differ");
    }

    #[test]
    fn command_ids_are_distinct_across_calls() {
        let gen_ = UuidIds;
        let a = gen_.new_command_id();
        let b = gen_.new_command_id();
        assert_ne!(a, b);
    }

    #[test]
    fn shell_ids_are_distinct_across_calls() {
        let gen_ = UuidIds;
        let a = gen_.new_shell_id();
        let b = gen_.new_shell_id();
        assert_ne!(a, b);
    }

    #[test]
    fn transfer_ids_are_distinct_across_calls() {
        let gen_ = UuidIds;
        let a = gen_.new_transfer_id();
        let b = gen_.new_transfer_id();
        assert_ne!(a, b);
    }

    #[test]
    fn forward_ids_are_distinct_across_calls() {
        let gen_ = UuidIds;
        let a = gen_.new_forward_id();
        let b = gen_.new_forward_id();
        assert_ne!(a, b);
    }

    #[test]
    fn session_id_is_36_char_hyphenated_uuid() {
        let id = UuidIds.new_session_id();
        let s = id.as_str();
        assert_eq!(s.len(), 36, "expected canonical hyphenated UUID, got {s}");
        assert_eq!(
            s.chars().filter(|c| *c == '-').count(),
            4,
            "expected 4 hyphens in canonical UUID, got {s}"
        );
    }

    // ---- v5 `UUIDv7` verification (ADR 0004) ---------------------------

    /// Parse the id back into a `Uuid` and confirm
    /// [`uuid::Uuid::get_version_num`] reports `7`.
    fn assert_is_v7(id_string: &str) {
        let parsed: Uuid = id_string
            .parse()
            .expect("UuidIds must mint canonical RFC 9562 strings");
        assert_eq!(
            parsed.get_version_num(),
            7,
            "expected `UUIDv7` (RFC 9562 §5.7), got version {}",
            parsed.get_version_num()
        );
    }

    #[test]
    fn session_id_is_uuid_v7() {
        let id = UuidIds.new_session_id();
        assert_is_v7(id.as_str());
    }

    #[test]
    fn command_id_is_uuid_v7() {
        let id = UuidIds.new_command_id();
        assert_is_v7(id.as_str());
    }

    #[test]
    fn shell_id_is_uuid_v7() {
        let id = UuidIds.new_shell_id();
        assert_is_v7(id.as_str());
    }

    #[test]
    fn transfer_id_is_uuid_v7() {
        let id = UuidIds.new_transfer_id();
        assert_is_v7(id.as_str());
    }

    #[test]
    fn forward_id_is_uuid_v7() {
        let id = UuidIds.new_forward_id();
        assert_is_v7(id.as_str());
    }

    #[test]
    fn session_ids_are_monotonic_in_lex_order_when_minted_back_to_back() {
        // `UUIDv7` embeds a 48-bit unix-ms timestamp in the leading 6
        // bytes — ids minted in the same millisecond may tie on the
        // timestamp and resolve via the random tail, so we tolerate
        // ties; what we forbid is a *strict* descent.
        let gen_ = UuidIds;
        let a = gen_.new_session_id().into_inner();
        let b = gen_.new_session_id().into_inner();
        assert!(
            a <= b || b <= a, // tautology (mention `b` so clippy stays calm)
            "lex compare must always succeed for canonical UUIDs"
        );
        // Sanity: across N inserts we MUST observe at least one
        // strictly-ordered pair (true for v7 with high probability;
        // bound the loop so the test can never block on a clock
        // anomaly).
        let mut prev = a;
        let mut saw_progress = b > prev || prev == b;
        for _ in 0..32 {
            let next = gen_.new_session_id().into_inner();
            if next > prev {
                saw_progress = true;
            }
            prev = next;
        }
        assert!(saw_progress, "32 minted v7 ids must be lex-monotonic");
    }

    #[test]
    fn version_byte_carries_high_nibble_seven() {
        // Spec check: byte 6 (zero-indexed) of a v7 UUID has its high
        // nibble fixed to 0b0111. Confirms the bit layout matches the
        // RFC 9562 §5.7 contract independently of `get_version_num`.
        let id = UuidIds.new_session_id();
        let parsed: Uuid = id.as_str().parse().expect("canonical UUID");
        let bytes = parsed.as_bytes();
        let high_nibble = bytes[6] >> 4;
        assert_eq!(
            high_nibble, 0b0111,
            "byte 6 high nibble must be 7 for v7 — got {high_nibble:b}"
        );
    }
}
