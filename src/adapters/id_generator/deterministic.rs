//! Test-only deterministic [`IdGeneratorPort`] adapter.
//!
//! Each id kind has its own [`AtomicU64`] counter, incremented per call,
//! formatted as `"{prefix}-{n}"`:
//!
//! | Kind        | Prefix |
//! |-------------|--------|
//! | session     | `sess` |
//! | command     | `cmd`  |
//! | shell       | `shell`|
//! | transfer    | `xfer` |
//! | forward     | `fwd`  |
//!
//! Counters are independent — calling [`SequentialIds::new_session_id`]
//! does not advance the command counter — so tests can assert on exact
//! id sequences per kind.
//!
//! Compiled only under `#[cfg(test)]` or with the `test-fixtures`
//! feature so it never reaches a release binary.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::domain::ids::{CommandId, ForwardId, SessionId, ShellId, TransferId};
use crate::ports::id_generator::IdGeneratorPort;

/// Deterministic id generator for tests and fixtures.
///
/// Each kind has an independent monotonically incrementing counter
/// starting at `0`. Construct with [`SequentialIds::default`].
#[derive(Debug, Default)]
pub struct SequentialIds {
    session: AtomicU64,
    command: AtomicU64,
    shell: AtomicU64,
    transfer: AtomicU64,
    forward: AtomicU64,
}

impl SequentialIds {
    fn next(counter: &AtomicU64, prefix: &str) -> String {
        let n = counter.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{n}")
    }
}

impl IdGeneratorPort for SequentialIds {
    fn new_session_id(&self) -> SessionId {
        SessionId::new(Self::next(&self.session, "sess"))
    }

    fn new_command_id(&self) -> CommandId {
        CommandId::new(Self::next(&self.command, "cmd"))
    }

    fn new_shell_id(&self) -> ShellId {
        ShellId::new(Self::next(&self.shell, "shell"))
    }

    fn new_transfer_id(&self) -> TransferId {
        TransferId::new(Self::next(&self.transfer, "xfer"))
    }

    fn new_forward_id(&self) -> ForwardId {
        ForwardId::new(Self::next(&self.forward, "fwd"))
    }
}

#[cfg(test)]
mod tests {
    use super::SequentialIds;
    use crate::ports::id_generator::IdGeneratorPort;

    #[test]
    fn sequential_ids_is_dyn_safe() {
        fn _accepts_dyn(_p: &dyn IdGeneratorPort) {}
        let gen_ = SequentialIds::default();
        _accepts_dyn(&gen_);
    }

    #[test]
    fn session_counter_starts_at_zero_and_increments() {
        let gen_ = SequentialIds::default();
        assert_eq!(gen_.new_session_id().as_str(), "sess-0");
        assert_eq!(gen_.new_session_id().as_str(), "sess-1");
        assert_eq!(gen_.new_session_id().as_str(), "sess-2");
    }

    #[test]
    fn each_kind_uses_its_own_prefix_and_counter() {
        let gen_ = SequentialIds::default();
        assert_eq!(gen_.new_session_id().as_str(), "sess-0");
        assert_eq!(gen_.new_command_id().as_str(), "cmd-0");
        assert_eq!(gen_.new_shell_id().as_str(), "shell-0");
        assert_eq!(gen_.new_transfer_id().as_str(), "xfer-0");
        assert_eq!(gen_.new_forward_id().as_str(), "fwd-0");
    }

    #[test]
    fn session_calls_do_not_advance_other_counters() {
        let gen_ = SequentialIds::default();
        // Burn three session ids.
        let _ = gen_.new_session_id();
        let _ = gen_.new_session_id();
        let _ = gen_.new_session_id();
        // Other kinds must still start at 0.
        assert_eq!(gen_.new_command_id().as_str(), "cmd-0");
        assert_eq!(gen_.new_shell_id().as_str(), "shell-0");
        assert_eq!(gen_.new_transfer_id().as_str(), "xfer-0");
        assert_eq!(gen_.new_forward_id().as_str(), "fwd-0");
        // And the next session id continues from where we left off.
        assert_eq!(gen_.new_session_id().as_str(), "sess-3");
    }

    #[test]
    fn command_counter_is_isolated() {
        let gen_ = SequentialIds::default();
        let _ = gen_.new_command_id();
        let _ = gen_.new_command_id();
        assert_eq!(gen_.new_session_id().as_str(), "sess-0");
        assert_eq!(gen_.new_command_id().as_str(), "cmd-2");
    }

    #[test]
    fn ids_are_distinct_within_a_kind() {
        let gen_ = SequentialIds::default();
        let a = gen_.new_shell_id();
        let b = gen_.new_shell_id();
        assert_ne!(a, b);
    }

    #[test]
    fn parallel_threads_yield_unique_ids_per_kind() {
        use std::collections::HashSet;
        use std::sync::Arc;
        use std::thread;

        const THREADS: usize = 8;
        const PER_THREAD: usize = 64;

        let gen_ = Arc::new(SequentialIds::default());
        let mut handles = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            let gen_thread = Arc::clone(&gen_);
            handles.push(thread::spawn(move || {
                let mut local = Vec::with_capacity(PER_THREAD);
                for _ in 0..PER_THREAD {
                    local.push(gen_thread.new_session_id());
                }
                local
            }));
        }

        let mut all = HashSet::with_capacity(THREADS * PER_THREAD);
        let mut joined_threads = 0_usize;
        for h in handles {
            // `join` returns `Err` only if a worker panicked; we avoid
            // `unwrap`/`panic!` (Layer-A forbid) even in tests, so we
            // skip such workers and assert on the final tally below.
            if let Ok(batch) = h.join() {
                joined_threads += 1;
                for id in batch {
                    assert!(all.insert(id), "duplicate id minted across threads");
                }
            }
        }
        assert_eq!(joined_threads, THREADS, "every worker must join cleanly");
        assert_eq!(all.len(), THREADS * PER_THREAD);
    }
}
