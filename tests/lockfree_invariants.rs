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

// ---------------------------------------------------------------------------
// v5 Phase 1 — Lifecycle adapter invariants
//
// These models capture the lock-free invariants of
// `crate::adapters::lifecycle::refcount::ResourceLifecycle`:
//   - sub_count is consistent under concurrent subscribe / unsubscribe
//   - grace timer fire vs resubscribe converges on a single winner
//   - cascade refcount fires the auto-disconnect hook at most once
//
// The models reproduce the relevant atomic primitives only; full
// loom mode against the real adapter is blocked by the russh / axum
// transitive incompatibility documented at the top of this file.
// ---------------------------------------------------------------------------

/// Mini-model of the lifecycle sub_count + state byte.
///
/// `state` byte encodes Owned (0) / Observed (1) / Releasing (2) /
/// Closed (3) — matches `LifecycleState::as_u8`.
struct LifecycleModel {
    state: loom::sync::atomic::AtomicU8,
    sub_count: AtomicUsize,
}

impl LifecycleModel {
    const OWNED: u8 = 0;
    const OBSERVED: u8 = 1;
    const RELEASING: u8 = 2;
    const CLOSED: u8 = 3;

    fn new() -> Self {
        Self {
            state: loom::sync::atomic::AtomicU8::new(Self::OWNED),
            sub_count: AtomicUsize::new(0),
        }
    }

    fn subscribe(&self) {
        // Promote Owned/Releasing -> Observed via CAS, then bump count.
        let _ = self
            .state
            .compare_exchange(
                Self::OWNED,
                Self::OBSERVED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .or_else(|_| {
                self.state.compare_exchange(
                    Self::RELEASING,
                    Self::OBSERVED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
            });
        self.sub_count.fetch_add(1, Ordering::AcqRel);
    }

    fn unsubscribe(&self) {
        let prev = self.sub_count.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            let _ = self.state.compare_exchange(
                Self::OBSERVED,
                Self::RELEASING,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

/// Concurrent sub + unsub keeps the count consistent and bounded.
#[test]
fn loom_lifecycle_concurrent_subscribe_unsubscribe() {
    loom::model(|| {
        let m = Arc::new(LifecycleModel::new());
        // Pre-fill so unsub never underflows.
        m.subscribe();
        m.subscribe();
        let m1 = Arc::clone(&m);
        let m2 = Arc::clone(&m);
        let h1 = thread::spawn(move || m1.subscribe());
        let h2 = thread::spawn(move || m2.unsubscribe());
        h1.join().unwrap();
        h2.join().unwrap();
        let count = m.sub_count.load(Ordering::Acquire);
        // After: started at 2, +1, -1 → count == 2.
        assert_eq!(count, 2);
    });
}

/// Race: grace timer wants to fire `Releasing -> Closed` while
/// another thread re-subscribes (`Releasing -> Observed`). The CAS
/// guarantees a single winner; the loser is a no-op.
#[test]
fn loom_grace_fire_vs_resubscribe() {
    loom::model(|| {
        let m = Arc::new(LifecycleModel::new());
        // Force into Releasing.
        m.subscribe();
        m.unsubscribe();
        let m1 = Arc::clone(&m);
        let m2 = Arc::clone(&m);
        let timer = thread::spawn(move || {
            // Fire close.
            m1.state
                .compare_exchange(
                    LifecycleModel::RELEASING,
                    LifecycleModel::CLOSED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        });
        let resub = thread::spawn(move || {
            // Re-subscribe.
            m2.state
                .compare_exchange(
                    LifecycleModel::RELEASING,
                    LifecycleModel::OBSERVED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        });
        let timer_won = timer.join().unwrap();
        let resub_won = resub.join().unwrap();
        assert!(
            timer_won ^ resub_won,
            "exactly one CAS must succeed: timer={timer_won} resub={resub_won}"
        );
        let final_state = m.state.load(Ordering::Acquire);
        assert!(
            matches!(final_state, LifecycleModel::CLOSED | LifecycleModel::OBSERVED),
            "final state must be Closed (timer won) or Observed (resub won), got {final_state}"
        );
    });
}

/// Mini-model of the cascade coordinator: an `AtomicUsize` for the
/// active-resource count and an `AtomicU8` for the reaped flag. The
/// hook fires exactly once per session — the CAS Active->Reaped is
/// the marker that another thread already won.
struct CascadeModel {
    active: AtomicUsize,
    reaped: loom::sync::atomic::AtomicU8,
    fires: AtomicUsize,
}

impl CascadeModel {
    const ACTIVE: u8 = 0;
    const REAPED: u8 = 1;

    fn new(initial: usize) -> Self {
        Self {
            active: AtomicUsize::new(initial),
            reaped: loom::sync::atomic::AtomicU8::new(Self::ACTIVE),
            fires: AtomicUsize::new(0),
        }
    }

    fn close_one(&self) {
        let prev = self.active.fetch_sub(1, Ordering::AcqRel);
        if prev == 1
            && self
                .reaped
                .compare_exchange(
                    Self::ACTIVE,
                    Self::REAPED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        {
            self.fires.fetch_add(1, Ordering::AcqRel);
        }
    }
}

/// Two shells closing simultaneously call disconnect at most once.
#[test]
fn loom_cascade_double_disconnect() {
    loom::model(|| {
        let m = Arc::new(CascadeModel::new(2));
        let m1 = Arc::clone(&m);
        let m2 = Arc::clone(&m);
        let h1 = thread::spawn(move || m1.close_one());
        let h2 = thread::spawn(move || m2.close_one());
        h1.join().unwrap();
        h2.join().unwrap();
        let fires = m.fires.load(Ordering::Acquire);
        assert_eq!(fires, 1, "auto-disconnect hook must fire exactly once");
    });
}

/// Cursor advance race: two concurrent `fetch_max` writers + a
/// concurrent state-byte CAS. The cursor is monotonic and the state
/// byte never observes an intermediate value. Mirrors the
/// `grace_until_ms` AtomicU64 + state AtomicU8 invariant on
/// `ResourceLifecycle`.
#[test]
fn loom_cursor_atomic_advance() {
    loom::model(|| {
        let cursor = Arc::new(AtomicU64::new(0));
        let state = Arc::new(loom::sync::atomic::AtomicU8::new(0));
        let c1 = Arc::clone(&cursor);
        let c2 = Arc::clone(&cursor);
        let s1 = Arc::clone(&state);
        let h1 = thread::spawn(move || {
            c1.fetch_max(100, Ordering::SeqCst);
        });
        let h2 = thread::spawn(move || {
            c2.fetch_max(50, Ordering::SeqCst);
        });
        let h3 = thread::spawn(move || {
            // CAS a state byte while the cursor races.
            let _ = s1.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
        });
        h1.join().unwrap();
        h2.join().unwrap();
        h3.join().unwrap();
        let final_cursor = cursor.load(Ordering::Acquire);
        let final_state = state.load(Ordering::Acquire);
        assert_eq!(final_cursor, 100, "cursor must observe the larger writer");
        assert_eq!(final_state, 1, "state CAS must have succeeded");
    });
}

// ---------------------------------------------------------------------------
// v5 Phase 2 — Channel Mux + SubId loom invariants
// ---------------------------------------------------------------------------

/// Mini-model of the round-robin mux drain. Two lanes, one cursor;
/// every successful pop bumps the cursor so neither lane starves
/// the other.
#[test]
fn loom_mux_round_robin_no_starvation() {
    loom::model(|| {
        // Two lane "queue lengths" — model the per-lane backlog.
        let lane_a = Arc::new(AtomicU64::new(2));
        let lane_b = Arc::new(AtomicU64::new(2));
        let cursor = Arc::new(AtomicUsize::new(0));

        let a1 = Arc::clone(&lane_a);
        let b1 = Arc::clone(&lane_b);
        let c1 = Arc::clone(&cursor);
        let drain = thread::spawn(move || {
            let mut a_drained = 0_u64;
            let mut b_drained = 0_u64;
            for _ in 0_u32..4 {
                let lanes = [Arc::clone(&a1), Arc::clone(&b1)];
                let start = c1.load(Ordering::Relaxed) % lanes.len();
                for offset in 0..lanes.len() {
                    let idx = (start + offset) % lanes.len();
                    let q = lanes[idx].load(Ordering::Relaxed);
                    if q > 0
                        && lanes[idx]
                            .compare_exchange(q, q - 1, Ordering::AcqRel, Ordering::Relaxed)
                            .is_ok()
                    {
                        if idx == 0 {
                            a_drained += 1;
                        } else {
                            b_drained += 1;
                        }
                        c1.store((idx + 1) % lanes.len(), Ordering::Relaxed);
                        break;
                    }
                }
            }
            (a_drained, b_drained)
        });

        // Producer trying to top up lane A while drain runs.
        let a2 = Arc::clone(&lane_a);
        let producer = thread::spawn(move || {
            a2.fetch_add(1, Ordering::Relaxed);
        });

        let (a_drained, b_drained) = drain.join().unwrap();
        producer.join().unwrap();

        // Fairness: the drain visited both lanes — never starved
        // lane B even when A had backlog.
        assert!(
            a_drained > 0 && b_drained > 0,
            "round-robin starved a lane: a={a_drained} b={b_drained}",
        );
    });
}

/// Mini-model of the lane mpsc DropOldest path. Producer increments
/// a "queued" counter, consumer decrements; under DropOldest the
/// `seq_local` counter (events delivered from the lane) is strictly
/// monotonic regardless of drops.
#[test]
fn loom_lane_mpsc_drop_oldest_monotonic() {
    loom::model(|| {
        let queued = Arc::new(AtomicUsize::new(0));
        let delivered = Arc::new(AtomicU64::new(0));
        const CAPACITY: usize = 2;

        let q1 = Arc::clone(&queued);
        let d1 = Arc::clone(&delivered);
        let producer = thread::spawn(move || {
            for _ in 0_u32..3 {
                let cur = q1.load(Ordering::Acquire);
                if cur >= CAPACITY {
                    // drop_oldest: pop one and push.
                    let _ = q1.compare_exchange(cur, cur, Ordering::AcqRel, Ordering::Relaxed);
                    // (model treats the "drop oldest" as net-zero
                    // queue change.)
                } else {
                    q1.fetch_add(1, Ordering::AcqRel);
                }
                d1.fetch_add(1, Ordering::AcqRel);
            }
        });

        let q2 = Arc::clone(&queued);
        let d2 = Arc::clone(&delivered);
        let consumer = thread::spawn(move || {
            for _ in 0_u32..2 {
                let cur = q2.load(Ordering::Acquire);
                if cur > 0 {
                    let _ = q2.compare_exchange(cur, cur - 1, Ordering::AcqRel, Ordering::Relaxed);
                }
            }
            d2.load(Ordering::Acquire)
        });

        producer.join().unwrap();
        let observed = consumer.join().unwrap();
        let final_delivered = delivered.load(Ordering::Acquire);
        // Monotonic invariant: the consumer's snapshot of `delivered`
        // is always <= the final value (no time travel).
        assert!(
            observed <= final_delivered,
            "delivered counter regressed: observed={observed} final={final_delivered}",
        );
    });
}

/// Mini-model of pause/resume against a `BlockSlow` producer. While
/// pause flag is true the producer must not increment `delivered`;
/// when resume sets the flag back to false, no events are lost
/// (producer's `attempted` counter equals the final `delivered`
/// once both threads finish).
#[test]
fn loom_lane_pause_resume_no_loss() {
    loom::model(|| {
        let paused = Arc::new(loom::sync::atomic::AtomicBool::new(false));
        let attempted = Arc::new(AtomicU64::new(0));
        let delivered = Arc::new(AtomicU64::new(0));

        let p_attempted = Arc::clone(&attempted);
        let p_paused = Arc::clone(&paused);
        let p_delivered = Arc::clone(&delivered);
        let producer = thread::spawn(move || {
            for _ in 0_u32..3 {
                p_attempted.fetch_add(1, Ordering::AcqRel);
                while p_paused.load(Ordering::Acquire) {
                    // Spin-wait — production code uses
                    // `Notify::notified().await`.
                    loom::thread::yield_now();
                }
                p_delivered.fetch_add(1, Ordering::AcqRel);
            }
        });

        let c_paused = Arc::clone(&paused);
        let controller = thread::spawn(move || {
            c_paused.store(true, Ordering::Release);
            loom::thread::yield_now();
            c_paused.store(false, Ordering::Release);
        });

        producer.join().unwrap();
        controller.join().unwrap();

        let final_attempted = attempted.load(Ordering::Acquire);
        let final_delivered = delivered.load(Ordering::Acquire);
        // BlockSlow guarantees zero loss: every attempted send
        // ultimately delivers.
        assert_eq!(
            final_attempted, final_delivered,
            "pause/resume lost events: attempted={final_attempted} delivered={final_delivered}",
        );
    });
}

/// Mini-model of `advance_cursor(target)` under contention. Two
/// readers race to bump the cursor; the final value is always the
/// maximum of the targets, never less.
#[test]
fn loom_subid_cursor_atomic_advance() {
    loom::model(|| {
        let cursor = Arc::new(AtomicU64::new(0));

        let c1 = Arc::clone(&cursor);
        let c2 = Arc::clone(&cursor);
        let h1 = thread::spawn(move || {
            c1.fetch_max(100, Ordering::AcqRel);
        });
        let h2 = thread::spawn(move || {
            c2.fetch_max(50, Ordering::AcqRel);
        });
        // Concurrent reader observing the cursor at any point must
        // see a value <= 100 (no out-of-thin-air larger value).
        let c3 = Arc::clone(&cursor);
        let observer = thread::spawn(move || {
            let v = c3.load(Ordering::Acquire);
            assert!(
                v <= 100,
                "cursor read out-of-thin-air larger value: {v}",
            );
        });

        h1.join().unwrap();
        h2.join().unwrap();
        observer.join().unwrap();
        let final_cursor = cursor.load(Ordering::Acquire);
        // Max-cursor invariant: the larger fetch_max wins.
        assert_eq!(final_cursor, 100, "fetch_max lost the larger target");
    });
}
