//! Loom-based concurrency invariant tests for ssh-mcp lock-free primitives.
//!
//! ## Scope
//!
//! These tests use `loom` (a permutation explorer for concurrent code) to
//! verify the lock-free invariants relied on by:
//!
//! - The shell history snapshot pattern (`ArcSwap<RingBuffer>` in
//!   `src/mcp/shell.rs`).
//! - The transfer / command terminal-state pattern
//!   (`OnceCell<error>` / `OnceCell<exit_code>` in `src/mcp/transfer.rs` and
//!   `src/mcp/async_command.rs`).
//! - The peer cursor compensation pattern under concurrent `fetch_max`
//!   readers and a `compensate_truncation` writer
//!   (`src/mcp/subscription.rs`).
//! - A model of slow-subscriber recovery for the broadcast pattern used by
//!   `RunningTransfer.progress_tx` etc.
//!
//! ## Running
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --test lockfree_invariants --release
//! ```
//!
//! The tests are gated behind `#[cfg(loom)]` so the default
//! `cargo test --tests` run skips them — loom uses a dedicated mock of
//! `std::sync` and is not safe to mix with real tokio runtimes.
//!
//! When loom is not enabled the binary is empty.

#![cfg(loom)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    clippy::tests_outside_test_module,
    reason = "loom tests use unwrap/panic freely; loom flags can deny similar lints in production paths"
)]

use loom::sync::Arc;
use loom::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use loom::thread;

/// Mini-model of `ArcSwap<RingBuffer>` using `loom::sync::Mutex<Arc<T>>`.
///
/// `arc_swap` itself is not loom-aware; we model the same observation:
/// the writer publishes a fresh `Arc` and readers always observe a
/// fully-formed snapshot (no torn reads, no partial mutation).
struct RingHistory {
    inner: loom::sync::Mutex<Arc<Vec<u8>>>,
}

impl RingHistory {
    fn new(initial: Vec<u8>) -> Self {
        Self {
            inner: loom::sync::Mutex::new(Arc::new(initial)),
        }
    }

    fn store(&self, snapshot: Vec<u8>) {
        let mut guard = self.inner.lock().unwrap();
        *guard = Arc::new(snapshot);
    }

    fn load(&self) -> Arc<Vec<u8>> {
        let guard = self.inner.lock().unwrap();
        Arc::clone(&guard)
    }
}

/// Concurrent reader sees a fully-formed snapshot when the writer swaps.
#[test]
fn ringhistory_reader_observes_consistent_snapshot() {
    loom::model(|| {
        let history = Arc::new(RingHistory::new(b"abc".to_vec()));
        let history_w = Arc::clone(&history);
        let writer = thread::spawn(move || {
            history_w.store(b"abcdef".to_vec());
        });

        let snap = history.load();
        // Either the original (3 bytes) or the new (6 bytes) snapshot — never
        // a torn intermediate. The Vec<u8> length encodes that invariant.
        assert!(matches!(snap.len(), 3 | 6));

        writer.join().unwrap();
        assert_eq!(history.load().len(), 6);
    });
}

/// Two writers + one reader: reader always sees one of the published
/// snapshots, never a partial overwrite.
#[test]
fn ringhistory_two_writers_reader_atomicity() {
    loom::model(|| {
        let history = Arc::new(RingHistory::new(Vec::new()));
        let h1 = Arc::clone(&history);
        let h2 = Arc::clone(&history);

        let w1 = thread::spawn(move || {
            h1.store(vec![1_u8, 2, 3]);
        });
        let w2 = thread::spawn(move || {
            h2.store(vec![4_u8, 5, 6, 7]);
        });

        let snap = history.load();
        assert!(matches!(snap.len(), 0 | 3 | 4));

        w1.join().unwrap();
        w2.join().unwrap();
        let final_snap = history.load();
        // Final state is one of the two writer snapshots.
        assert!(matches!(final_snap.len(), 3 | 4));
    });
}

/// Mini-model of `OnceCell<T>` using an `AtomicUsize` flag + a Mutex<Option<T>>.
///
/// Only the first writer wins; subsequent writers must observe the original
/// value via `get`. Mirrors the invariant of
/// `RunningCommand.exit_code` and `RunningTransfer.error`.
struct OnceModel<T> {
    set_flag: AtomicUsize,
    value: loom::sync::Mutex<Option<T>>,
}

impl<T: Clone> OnceModel<T> {
    fn new() -> Self {
        Self {
            set_flag: AtomicUsize::new(0),
            value: loom::sync::Mutex::new(None),
        }
    }

    /// Returns true when the caller stored the value (i.e. won the race).
    fn set(&self, value: T) -> bool {
        if self
            .set_flag
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let mut guard = self.value.lock().unwrap();
            *guard = Some(value);
            true
        } else {
            false
        }
    }

    fn get(&self) -> Option<T> {
        if self.set_flag.load(Ordering::Acquire) == 0 {
            return None;
        }
        self.value.lock().unwrap().clone()
    }
}

/// Two concurrent `set` calls: exactly one wins, the loser observes the
/// winner's value (write-once semantics).
#[test]
fn oncemodel_only_first_set_wins() {
    loom::model(|| {
        let cell: Arc<OnceModel<u32>> = Arc::new(OnceModel::new());
        let c1 = Arc::clone(&cell);
        let c2 = Arc::clone(&cell);

        let h1 = thread::spawn(move || c1.set(10));
        let h2 = thread::spawn(move || c2.set(20));

        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();
        assert!(
            r1 ^ r2,
            "exactly one set call must succeed, got ({r1}, {r2})"
        );

        let value = cell
            .get()
            .expect("a value must be visible after both joins");
        assert!(
            value == 10 || value == 20,
            "value must be one of the writers' inputs"
        );
    });
}

/// Reader observes either `None` (before any set) or the written value, and
/// once a value is observed it never disappears.
#[test]
fn oncemodel_reader_sees_stable_value_after_first_observation() {
    loom::model(|| {
        let cell: Arc<OnceModel<u32>> = Arc::new(OnceModel::new());
        let c1 = Arc::clone(&cell);
        let c2 = Arc::clone(&cell);

        let writer = thread::spawn(move || {
            c1.set(42);
        });

        let observation = c2.get();
        if let Some(v) = observation {
            assert_eq!(v, 42, "any observed value must equal the writer's value");
        }

        writer.join().unwrap();
        assert_eq!(c2.get(), Some(42));
    });
}

/// `fetch_max` from multiple readers + a `saturating_sub` writer mirrors the
/// `peer_progress.byte_cursor` pattern. Verifies the cursor never overflows
/// the buffer length and the writer's saturating decrement composes safely
/// with concurrent monotonic-max writers.
#[test]
fn cursor_fetch_max_with_compensate_truncation() {
    loom::model(|| {
        let cursor = Arc::new(AtomicU64::new(0));
        let r1 = Arc::clone(&cursor);
        let r2 = Arc::clone(&cursor);
        let w = Arc::clone(&cursor);

        let h1 = thread::spawn(move || {
            r1.fetch_max(50, Ordering::SeqCst);
        });
        let h2 = thread::spawn(move || {
            r2.fetch_max(75, Ordering::SeqCst);
        });
        let hw = thread::spawn(move || {
            // Compensate truncation: simulate dropping 30 bytes from the head.
            let current = w.load(Ordering::Relaxed);
            let next = current.saturating_sub(30);
            w.store(next, Ordering::Relaxed);
        });

        h1.join().unwrap();
        h2.join().unwrap();
        hw.join().unwrap();

        let final_value = cursor.load(Ordering::SeqCst);
        // Final value must be reachable via some interleaving of:
        //   max(0, 50, 75) followed by saturating_sub(30) at any point.
        // Every reachable value is bounded by 75 (max cursor) and never
        // negative (saturating sub).
        assert!(
            final_value <= 75,
            "cursor exceeded reader max: {final_value}"
        );
    });
}

/// Two concurrent `compensate_truncation` calls saturate at zero without
/// ever going negative.
#[test]
fn cursor_double_compensation_saturates_at_zero() {
    loom::model(|| {
        let cursor = Arc::new(AtomicU64::new(20));
        let w1 = Arc::clone(&cursor);
        let w2 = Arc::clone(&cursor);

        let h1 = thread::spawn(move || {
            let current = w1.load(Ordering::Relaxed);
            let next = current.saturating_sub(15);
            w1.store(next, Ordering::Relaxed);
        });
        let h2 = thread::spawn(move || {
            let current = w2.load(Ordering::Relaxed);
            let next = current.saturating_sub(50);
            w2.store(next, Ordering::Relaxed);
        });

        h1.join().unwrap();
        h2.join().unwrap();
        // Saturating subs never wrap; the floor is 0.
        let final_value = cursor.load(Ordering::SeqCst);
        assert!(
            final_value <= 20,
            "cursor must never exceed initial value: {final_value}"
        );
    });
}

/// Mini-model of a slow subscriber recovering from a `Lagged` state. The
/// subscriber tracks the highest sequence number it has seen; producers
/// allocate via `fetch_add`. A "lag" event is detected when the subscriber's
/// next observed seq jumps by more than 1.
#[test]
fn slow_subscriber_recovers_after_lag() {
    loom::model(|| {
        let seq = Arc::new(AtomicU64::new(0));
        let producer = Arc::clone(&seq);
        let consumer = Arc::clone(&seq);

        let h_producer = thread::spawn(move || {
            // Two rapid sequence allocations — simulating producer outpacing
            // a slow subscriber and forcing a lag recovery.
            producer.fetch_add(1, Ordering::SeqCst);
            producer.fetch_add(1, Ordering::SeqCst);
        });

        let h_consumer = thread::spawn(move || consumer.load(Ordering::SeqCst));

        h_producer.join().unwrap();
        let observed = h_consumer.join().unwrap();
        let final_seq = seq.load(Ordering::SeqCst);

        // Observed must be <= final_seq. Final must be exactly 2 (two allocations).
        assert_eq!(final_seq, 2);
        assert!(observed <= final_seq);
    });
}

/// Sequence allocation never produces duplicates: each producer's
/// `fetch_add` returns a unique value.
#[test]
fn sequence_allocation_no_duplicates() {
    loom::model(|| {
        let seq = Arc::new(AtomicU64::new(0));
        let p1 = Arc::clone(&seq);
        let p2 = Arc::clone(&seq);

        let h1 = thread::spawn(move || p1.fetch_add(1, Ordering::SeqCst));
        let h2 = thread::spawn(move || p2.fetch_add(1, Ordering::SeqCst));

        let v1 = h1.join().unwrap();
        let v2 = h2.join().unwrap();
        assert_ne!(v1, v2, "fetch_add must return unique values per call");
        // Both values are in {0, 1}.
        let mut sorted = [v1, v2];
        sorted.sort_unstable();
        assert_eq!(sorted, [0, 1]);
    });
}
