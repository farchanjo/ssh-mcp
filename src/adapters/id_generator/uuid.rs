//! Production [`IdGeneratorPort`] adapter backed by [`uuid::Uuid::new_v4`].
//!
//! Each `new_*_id` call mints an independent [`uuid::Uuid::new_v4`] and
//! wraps it in the matching domain newtype from [`crate::domain::ids`].
//! The generator holds no state, so the adapter is a zero-sized type
//! that can be cloned, copied and shared via `Arc<UuidIds>` without
//! heap allocation.

use uuid::Uuid;

use crate::domain::ids::{CommandId, ForwardId, SessionId, ShellId, TransferId};
use crate::ports::id_generator::IdGeneratorPort;

/// Production id generator that wraps [`uuid::Uuid::new_v4`].
///
/// Zero-sized, `Copy`, safe to share across threads. Construct with
/// [`UuidIds::default`] (or `UuidIds`).
#[derive(Debug, Default, Clone, Copy)]
pub struct UuidIds;

impl IdGeneratorPort for UuidIds {
    fn new_session_id(&self) -> SessionId {
        SessionId::new(Uuid::new_v4().to_string())
    }

    fn new_command_id(&self) -> CommandId {
        CommandId::new(Uuid::new_v4().to_string())
    }

    fn new_shell_id(&self) -> ShellId {
        ShellId::new(Uuid::new_v4().to_string())
    }

    fn new_transfer_id(&self) -> TransferId {
        TransferId::new(Uuid::new_v4().to_string())
    }

    fn new_forward_id(&self) -> ForwardId {
        ForwardId::new(Uuid::new_v4().to_string())
    }
}

#[cfg(test)]
mod tests {
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
        assert_ne!(a, b, "two consecutive UUIDv4 session ids must differ");
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
}
