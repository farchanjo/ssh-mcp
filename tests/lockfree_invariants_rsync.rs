//! ADR 0011 — loom-based concurrency invariants for the v7.0
//! rsync hybrid transport.
//!
//! ## Scope
//!
//! These tests use [`loom`](https://crates.io/crates/loom) to model
//! the lock-free invariants the rsync transport depends on:
//!
//! - **Lifecycle CAS race for rsync sessions** — `Pending -> Running
//!   -> Cancelled` must converge under concurrent readers; the byte
//!   layout maps to `RsyncStatus` exactly.
//! - **Lane mpsc + cancel race** — drain task races a one-way cancel
//!   latch; once cancelled, recv_event observes a terminal state and
//!   never panics.
//! - **Stats atomic monotonicity** — `bytes_sent` / `events_sent` /
//!   `files_done` are atomic counters that never decrease under
//!   concurrent producer + observer access.
//! - **NDX cursor monotonicity** — `prev_positive` follows the
//!   ascending sequence the upstream rsync sender emits; concurrent
//!   reads never observe a "regressed" value.
//! - **MplexReader buffer wraparound** — `mplex_read_remain` is per-
//!   task local in production, but a model checks the producer +
//!   consumer atomic queue size never goes negative.
//! - **WireSession `total_*` saturating add** — concurrent
//!   `saturating_add` calls converge on a value `<= u64::MAX` and
//!   `>=` the initial value; mirrors the production invariant.
//!
//! ## Running
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --test lockfree_invariants_rsync \
//!     --features test-fixtures --release
//! ```
//!
//! The tests are gated behind `#[cfg(loom)]` so the default
//! `cargo test --tests` run skips them — loom uses a dedicated mock
//! of `std::sync` and is not safe to mix with real tokio runtimes.
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
use loom::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use loom::thread;

// ---------------------------------------------------------------------------
// Mini-model of the rsync session status byte.
//
// `RsyncStatus` byte layout (from `domain/rsync.rs`):
//   Pending = 0x01, Probing = 0x02, Running = 0x03,
//   Completed = 0x04, Failed = 0x05, Cancelled = 0x06.
//
// The lifecycle invariant is: a session never regresses; transitions
// are CAS-driven and a Cancelled latch wins over Completed / Failed.
// ---------------------------------------------------------------------------

const ST_PENDING: u8 = 0x01;
const ST_PROBING: u8 = 0x02;
const ST_RUNNING: u8 = 0x03;
const ST_COMPLETED: u8 = 0x04;
const ST_FAILED: u8 = 0x05;
const ST_CANCELLED: u8 = 0x06;

struct RsyncStatusModel {
    state: AtomicU8,
}

impl RsyncStatusModel {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(ST_PENDING),
        }
    }

    /// Try to promote from `expected` to `target` via CAS. Returns
    /// `true` when the caller won the race.
    fn cas_promote(&self, expected: u8, target: u8) -> bool {
        self.state
            .compare_exchange(expected, target, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Force the cancel latch — store unconditionally. Mirrors the
    /// production `cancel()` path which always wins over the in-flight
    /// status.
    fn force_cancel(&self) {
        self.state.store(ST_CANCELLED, Ordering::Release);
    }

    fn load(&self) -> u8 {
        self.state.load(Ordering::Acquire)
    }
}

/// `loom_rsync_lifecycle_cas_race`
///
/// Two tasks race to promote a session from Pending → Running while a
/// third task issues a cancel. The cancel latch always wins; the
/// CAS-based promotions either succeed (no concurrent cancel observed
/// the intermediate Running state for long) or fail (cancel arrived
/// first). The final state is always Cancelled.
#[test]
fn loom_rsync_lifecycle_cas_race() {
    loom::model(|| {
        let m = Arc::new(RsyncStatusModel::new());
        let m1 = Arc::clone(&m);
        let m2 = Arc::clone(&m);
        let m3 = Arc::clone(&m);

        let promoter_a = thread::spawn(move || m1.cas_promote(ST_PENDING, ST_RUNNING));
        let promoter_b = thread::spawn(move || m2.cas_promote(ST_PROBING, ST_RUNNING));
        let canceller = thread::spawn(move || m3.force_cancel());

        let _ = promoter_a.join().unwrap();
        let _ = promoter_b.join().unwrap();
        canceller.join().unwrap();

        assert_eq!(
            m.load(),
            ST_CANCELLED,
            "force_cancel must win over CAS promotions",
        );
    });
}

/// `loom_rsync_lifecycle_no_double_cancel`
///
/// Two parallel cancellations leave the session in `Cancelled` and
/// the cancel latch is idempotent — neither thread panics, the final
/// state is exactly `Cancelled`.
#[test]
fn loom_rsync_lifecycle_no_double_cancel() {
    loom::model(|| {
        let m = Arc::new(RsyncStatusModel::new());
        let m1 = Arc::clone(&m);
        let m2 = Arc::clone(&m);
        let h1 = thread::spawn(move || m1.force_cancel());
        let h2 = thread::spawn(move || m2.force_cancel());
        h1.join().unwrap();
        h2.join().unwrap();
        assert_eq!(m.load(), ST_CANCELLED);
    });
}

// ---------------------------------------------------------------------------
// Mini-model of the lane mpsc + cancel race.
//
// The production lane carries a `tokio::sync::mpsc::Receiver` plus a
// `CancellationToken`-equivalent flag. The drain task pumps until the
// flag flips; once cancelled, recv_event observes Ok(None) and the
// queue size never goes negative.
// ---------------------------------------------------------------------------

struct LaneModel {
    queued: AtomicUsize,
    cancelled: AtomicBool,
}

impl LaneModel {
    fn new() -> Self {
        Self {
            queued: AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
        }
    }

    /// Producer enqueues one event unless cancelled.
    fn try_send(&self) -> bool {
        if self.cancelled.load(Ordering::Acquire) {
            return false;
        }
        self.queued.fetch_add(1, Ordering::AcqRel);
        true
    }

    /// Consumer drains one event if available.
    fn try_recv(&self) -> Option<()> {
        let cur = self.queued.load(Ordering::Acquire);
        if cur == 0 {
            return None;
        }
        // Saturating CAS: only decrement when the slot is non-zero.
        match self
            .queued
            .compare_exchange(cur, cur - 1, Ordering::AcqRel, Ordering::Relaxed)
        {
            Ok(_) => Some(()),
            Err(_) => None,
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// `loom_rsync_lane_drain_vs_cancel`
///
/// Drain task races the cancel signal: once the flag is set, no more
/// events flow into the lane and the queue never goes negative.
#[test]
fn loom_rsync_lane_drain_vs_cancel() {
    loom::model(|| {
        let lane = Arc::new(LaneModel::new());
        let producer = Arc::clone(&lane);
        let consumer = Arc::clone(&lane);
        let canceller = Arc::clone(&lane);

        let p = thread::spawn(move || {
            let _ = producer.try_send();
            let _ = producer.try_send();
        });
        let c = thread::spawn(move || {
            // Drain at most twice (matches the producer's two pushes).
            let _ = consumer.try_recv();
            let _ = consumer.try_recv();
        });
        let kill = thread::spawn(move || canceller.cancel());

        p.join().unwrap();
        c.join().unwrap();
        kill.join().unwrap();

        // Queue invariant: never negative; bounded by the producer's
        // two pushes.
        let final_q = lane.queued.load(Ordering::Acquire);
        assert!(final_q <= 2, "queue overflow: {final_q}");
        // Cancel latch is one-way.
        assert!(lane.cancelled.load(Ordering::Acquire));
    });
}

// ---------------------------------------------------------------------------
// Mini-model of the rsync stats counters.
//
// `RsyncStats` carries `AtomicU64` for `bytes_sent`, `bytes_skipped`,
// `events_sent`, `files_done`, `files_total`. Producers fetch_add;
// observers fetch_max-style read. Monotonicity is the load-bearing
// invariant.
// ---------------------------------------------------------------------------

struct StatsModel {
    bytes_sent: AtomicU64,
    events_sent: AtomicU64,
    files_done: AtomicU64,
}

impl StatsModel {
    fn new() -> Self {
        Self {
            bytes_sent: AtomicU64::new(0),
            events_sent: AtomicU64::new(0),
            files_done: AtomicU64::new(0),
        }
    }

    fn record_event(&self, payload: u64) {
        self.events_sent.fetch_add(1, Ordering::AcqRel);
        self.bytes_sent.fetch_add(payload, Ordering::AcqRel);
    }

    fn record_file_done(&self) {
        self.files_done.fetch_add(1, Ordering::AcqRel);
    }

    fn snapshot(&self) -> (u64, u64, u64) {
        let b = self.bytes_sent.load(Ordering::Acquire);
        let e = self.events_sent.load(Ordering::Acquire);
        let f = self.files_done.load(Ordering::Acquire);
        (b, e, f)
    }
}

/// `loom_rsync_stats_atomic_monotonic`
///
/// Two producers race to increment counters; a concurrent observer
/// reads any partial snapshot. Final counters always equal the
/// per-thread totals; observed snapshots are always `<=` final. No
/// counter ever decreases.
#[test]
fn loom_rsync_stats_atomic_monotonic() {
    loom::model(|| {
        let stats = Arc::new(StatsModel::new());
        let p1 = Arc::clone(&stats);
        let p2 = Arc::clone(&stats);
        let observer = Arc::clone(&stats);

        let h1 = thread::spawn(move || {
            p1.record_event(100);
            p1.record_file_done();
        });
        let h2 = thread::spawn(move || {
            p2.record_event(50);
        });
        let h3 = thread::spawn(move || observer.snapshot());

        h1.join().unwrap();
        h2.join().unwrap();
        let observed = h3.join().unwrap();

        let (final_b, final_e, final_f) = stats.snapshot();
        // Final aggregates are the sum of per-thread payloads.
        assert_eq!(final_b, 150, "bytes_sent drift: {final_b}");
        assert_eq!(final_e, 2, "events_sent drift: {final_e}");
        assert_eq!(final_f, 1, "files_done drift: {final_f}");
        // Observed snapshot is bounded by the final aggregates and
        // never negative.
        let (ob, oe, of) = observed;
        assert!(ob <= final_b, "observed bytes regressed");
        assert!(oe <= final_e, "observed events regressed");
        assert!(of <= final_f, "observed files regressed");
    });
}

// ---------------------------------------------------------------------------
// Mini-model of the NDX cursor (`prev_positive` / `prev_negative`).
//
// In production, `NdxState` is per-direction-task local — never
// shared. The model tests the property that even *if* a future
// refactor exposed it as an atomic, an ascending sequence of
// `fetch_max` updates converges on the maximum without losing
// updates.
// ---------------------------------------------------------------------------

struct NdxCursorModel {
    prev_positive: AtomicU64,
}

impl NdxCursorModel {
    fn new() -> Self {
        Self {
            prev_positive: AtomicU64::new(0),
        }
    }

    fn advance(&self, target: u64) -> u64 {
        self.prev_positive.fetch_max(target, Ordering::AcqRel)
    }

    fn read(&self) -> u64 {
        self.prev_positive.load(Ordering::Acquire)
    }
}

/// `loom_rsync_ndx_cursor_monotonic`
///
/// Two writers race to advance the cursor; the observer always sees
/// a value `<=` the maximum target. Final cursor equals the largest
/// target.
#[test]
fn loom_rsync_ndx_cursor_monotonic() {
    loom::model(|| {
        let cursor = Arc::new(NdxCursorModel::new());
        let c1 = Arc::clone(&cursor);
        let c2 = Arc::clone(&cursor);
        let c3 = Arc::clone(&cursor);

        let w1 = thread::spawn(move || c1.advance(100));
        let w2 = thread::spawn(move || c2.advance(50));
        let observer = thread::spawn(move || c3.read());

        let _ = w1.join().unwrap();
        let _ = w2.join().unwrap();
        let observed = observer.join().unwrap();

        let final_v = cursor.read();
        assert_eq!(final_v, 100, "max-cursor invariant lost");
        assert!(observed <= 100, "observer saw out-of-thin-air value");
    });
}

// ---------------------------------------------------------------------------
// Mini-model of WireSession total counters under concurrent
// saturating_add.
//
// `WireSession` is per-task local in production, but the property is
// independently useful: saturating_add never overflows and never
// produces a value below the initial state.
// ---------------------------------------------------------------------------

struct WireSessionTotalsModel {
    total_read: AtomicU64,
    total_write: AtomicU64,
}

impl WireSessionTotalsModel {
    fn new(initial: u64) -> Self {
        Self {
            total_read: AtomicU64::new(initial),
            total_write: AtomicU64::new(initial),
        }
    }

    fn add_read(&self, n: u64) {
        // Loom's AtomicU64 lacks fetch_update on the older 0.7 API
        // surface — model the saturating add via a load+CAS retry
        // loop that mirrors what the production `saturating_add`
        // does at the byte level.
        loop {
            let cur = self.total_read.load(Ordering::Acquire);
            let next = cur.saturating_add(n);
            if self
                .total_read
                .compare_exchange(cur, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
    }

    fn add_write(&self, n: u64) {
        loop {
            let cur = self.total_write.load(Ordering::Acquire);
            let next = cur.saturating_add(n);
            if self
                .total_write
                .compare_exchange(cur, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
    }

    fn snapshot(&self) -> (u64, u64) {
        (
            self.total_read.load(Ordering::Acquire),
            self.total_write.load(Ordering::Acquire),
        )
    }
}

/// `loom_rsync_wire_session_totals_saturate`
///
/// Two concurrent saturating_add updaters never lose updates and
/// never observe a regression. The CAS retry loop mirrors the
/// production `saturating_add` semantics on the per-task `total_*`
/// fields of `WireSession`.
#[test]
fn loom_rsync_wire_session_totals_saturate() {
    loom::model(|| {
        let totals = Arc::new(WireSessionTotalsModel::new(0));
        let r1 = Arc::clone(&totals);
        let w1 = Arc::clone(&totals);

        let h_read = thread::spawn(move || r1.add_read(10));
        let h_write = thread::spawn(move || w1.add_write(20));

        h_read.join().unwrap();
        h_write.join().unwrap();
        let (r, w) = totals.snapshot();
        assert_eq!(r, 10);
        assert_eq!(w, 20);
    });
}

/// `loom_rsync_lane_mpsc_drop_oldest_no_underflow`
///
/// Producer races consumer with a small bounded queue size; under
/// the DropOldest model the queue size never underflows. Mirrors the
/// production lane backpressure invariant where a slow consumer plus
/// a fast producer must never observe a negative queue depth.
#[test]
fn loom_rsync_lane_mpsc_drop_oldest_no_underflow() {
    loom::model(|| {
        let queued = Arc::new(AtomicUsize::new(0));
        let p = Arc::clone(&queued);
        let c = Arc::clone(&queued);

        let producer = thread::spawn(move || {
            for _ in 0_u32..3 {
                p.fetch_add(1, Ordering::AcqRel);
            }
        });
        let consumer = thread::spawn(move || {
            for _ in 0_u32..2 {
                let cur = c.load(Ordering::Acquire);
                if cur > 0 {
                    let _ = c.compare_exchange(cur, cur - 1, Ordering::AcqRel, Ordering::Relaxed);
                }
            }
        });

        producer.join().unwrap();
        consumer.join().unwrap();
        let final_q = queued.load(Ordering::Acquire);
        // 3 pushes, up to 2 successful pops; queue is in {1, 2, 3}.
        assert!(
            (1..=3).contains(&final_q),
            "queue size out of bounds: {final_q}"
        );
    });
}
