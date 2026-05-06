//! Identifier-generation port.
//!
//! Use cases mint stable ids through this port instead of pulling `uuid` or
//! `rand` directly. The production adapter wraps `uuid::Uuid::now_v7`
//! (RFC 9562 §5.7 — monotonic time-ordering, see ADR 0004), while tests
//! can swap a deterministic counter so snapshots stay stable.

use crate::domain::ids::{CommandId, ForwardId, SessionId, ShellId, TransferId};
use crate::domain::rsync_ids::RsyncId;

/// Sync, dyn-safe id generator.
pub trait IdGeneratorPort: Send + Sync + 'static {
    /// Mint a fresh [`SessionId`].
    fn new_session_id(&self) -> SessionId;

    /// Mint a fresh [`CommandId`].
    fn new_command_id(&self) -> CommandId;

    /// Mint a fresh [`ShellId`].
    fn new_shell_id(&self) -> ShellId;

    /// Mint a fresh [`TransferId`].
    fn new_transfer_id(&self) -> TransferId;

    /// Mint a fresh [`ForwardId`].
    fn new_forward_id(&self) -> ForwardId;

    /// Mint a fresh [`RsyncId`] (ADR 0011).
    ///
    /// Default impl falls back to a session-id-shaped value so existing
    /// adapters (test stubs, downstream forks) keep compiling without an
    /// explicit override; the production [`crate::adapters::id_generator::uuid::UuidIds`]
    /// adapter overrides this with a fresh `UUIDv7`.
    fn new_rsync_id(&self) -> RsyncId {
        RsyncId::new(self.new_session_id().into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::IdGeneratorPort;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn _assert_dyn_safe(_p: &dyn IdGeneratorPort) {}

    struct CounterGenerator {
        counter: AtomicU64,
    }

    impl CounterGenerator {
        fn new() -> Self {
            Self {
                counter: AtomicU64::new(0),
            }
        }

        fn next(&self, prefix: &str) -> String {
            let n = self.counter.fetch_add(1, Ordering::Relaxed);
            format!("{prefix}-{n}")
        }
    }

    impl IdGeneratorPort for CounterGenerator {
        fn new_session_id(&self) -> crate::domain::ids::SessionId {
            crate::domain::ids::SessionId::new(self.next("sess"))
        }
        fn new_command_id(&self) -> crate::domain::ids::CommandId {
            crate::domain::ids::CommandId::new(self.next("cmd"))
        }
        fn new_shell_id(&self) -> crate::domain::ids::ShellId {
            crate::domain::ids::ShellId::new(self.next("shell"))
        }
        fn new_transfer_id(&self) -> crate::domain::ids::TransferId {
            crate::domain::ids::TransferId::new(self.next("xfer"))
        }
        fn new_forward_id(&self) -> crate::domain::ids::ForwardId {
            crate::domain::ids::ForwardId::new(self.next("fwd"))
        }
    }

    #[test]
    fn counter_generator_emits_distinct_ids() {
        let gen_ = CounterGenerator::new();
        let s1 = gen_.new_session_id();
        let s2 = gen_.new_session_id();
        let c1 = gen_.new_command_id();
        assert_ne!(s1, s2);
        assert_eq!(s1.as_str(), "sess-0");
        assert_eq!(s2.as_str(), "sess-1");
        assert_eq!(c1.as_str(), "cmd-2");
    }
}
