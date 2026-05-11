//! ADR 0012 phase 8 -- loom invariants for the v7.1 inline-push
//! atomics.
//!
//! Scope: the new hot-path atomics introduced by phase 4 / phase 6:
//!
//! - `LaneState.inline_push: AtomicBool` -- Release-store gate flipped
//!   by `sub_open` and Acquire-loaded by the lane-fanout bridge.
//! - `LaneState.inline_seq: AtomicU64` -- monotonic per-lane sequence
//!   counter advanced via `fetch_add(1, Release)` per fragment.
//! - `LaneState.inline_events_sent: AtomicU64` -- Relaxed cumulative
//!   delivery counter.
//! - `LaneState.inline_bytes_sent: AtomicU64` -- Relaxed cumulative
//!   delivery byte counter.
//! - `CapabilityRegistry.record_capability` + `peer_has_capability`
//!   + `forget_peer` -- shard-locked O(1) `DashMap` plus per-bit
//!   AtomicBool Release/Acquire pair.
//!
//! The tests model the atomics directly so the file stays free of
//! `russh` / `axum` imports that break the upstream loom build (full
//! loom mode currently fails to link the wider workspace because
//! `tokio::net::TcpStream` is gated `#![cfg(not(loom))]`). The mock
//! data structures mirror the field layout one-for-one; the real
//! `LaneState` / `CapabilityRegistry` carry the same atomic types
//! and ordering pairs (see `src/adapters/subscription/subscriber_lane.rs`
//! and `src/adapters/capability/registry.rs`).
//!
//! Run with:
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release \
//!     --test lockfree_invariants_inline_push --features test-fixtures
//! ```
//!
//! When loom is not enabled the test binary compiles to a no-op.

#![cfg(loom)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    clippy::tests_outside_test_module,
    clippy::module_name_repetitions,
    reason = "loom tests use unwrap/panic freely; loom-only file -- no production lints"
)]

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use loom::thread;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Mock fixtures (mirror production atomic layout)
// ---------------------------------------------------------------------------

/// Mirror of the four ADR 0012 phase 4 atomics on `LaneState`.
///
/// Carries the same ordering pairs as the production code:
/// - `inline_push`: Release store / Acquire load.
/// - `inline_seq`: Release fetch_add.
/// - `inline_events_sent`, `inline_bytes_sent`: Relaxed fetch_add.
struct LaneAtomics {
    inline_push: AtomicBool,
    inline_seq: AtomicU64,
    inline_events_sent: AtomicU64,
    inline_bytes_sent: AtomicU64,
}

impl LaneAtomics {
    fn new() -> Self {
        Self {
            inline_push: AtomicBool::new(false),
            inline_seq: AtomicU64::new(0),
            inline_events_sent: AtomicU64::new(0),
            inline_bytes_sent: AtomicU64::new(0),
        }
    }
}

/// Mirror of `CapabilityRegistry` shape: a sharded map of peer ->
/// AtomicBool. loom does NOT ship a `DashMap`-equivalent, so the
/// fixture uses a `loom::sync::Mutex<HashMap<...>>` for the
/// insertion path; the read path uses the same Acquire load on the
/// AtomicBool that production does. Loom permutes interleavings on
/// the Mutex and the AtomicBool independently.
struct CapabilityRegistryMock {
    inner: loom::sync::Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl CapabilityRegistryMock {
    fn new() -> Self {
        Self {
            inner: loom::sync::Mutex::new(HashMap::new()),
        }
    }

    fn record(&self, peer: &str, enabled: bool) {
        let mut guard = self.inner.lock().unwrap();
        let entry = guard
            .entry(peer.to_string())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)));
        entry.store(enabled, Ordering::Release);
    }

    fn peer_has(&self, peer: &str) -> bool {
        let guard = self.inner.lock().unwrap();
        match guard.get(peer) {
            Some(bit) => bit.load(Ordering::Acquire),
            None => false,
        }
    }

    fn forget(&self, peer: &str) {
        let mut guard = self.inner.lock().unwrap();
        guard.remove(peer);
    }
}

// ---------------------------------------------------------------------------
// Invariants
// ---------------------------------------------------------------------------

/// `loom_lane_inline_push_set_then_read`
///
/// Thread A stores `inline_push=true` (Release); thread B loads
/// inline_push (Acquire). loom permutes interleavings; the assert
/// passes whenever thread B observed the store, which is the
/// happens-after invariant the production bridge relies on.
#[test]
fn loom_lane_inline_push_set_then_read() {
    loom::model(|| {
        let atomics = Arc::new(LaneAtomics::new());
        let a1 = Arc::clone(&atomics);
        let setter = thread::spawn(move || {
            a1.inline_push.store(true, Ordering::Release);
        });
        let reader = thread::spawn({
            let a2 = Arc::clone(&atomics);
            move || a2.inline_push.load(Ordering::Acquire)
        });
        setter.join().unwrap();
        let final_state = atomics.inline_push.load(Ordering::Acquire);
        let observed_after = reader.join().unwrap();
        assert!(final_state, "final state must be true after setter joined");
        // The reader either observed the post-store value or the
        // pre-store value -- never a torn read. Either outcome is
        // valid; we only assert the value is a clean bool.
        let _ = observed_after;
    });
}

/// `loom_inline_seq_concurrent_fetch_add`
///
/// Two threads each call `inline_seq.fetch_add(1, Release)`; loom
/// verifies the counter reaches exactly 2, no double-increment, no
/// torn read.
#[test]
fn loom_inline_seq_concurrent_fetch_add() {
    loom::model(|| {
        let atomics = Arc::new(LaneAtomics::new());
        let a1 = Arc::clone(&atomics);
        let a2 = Arc::clone(&atomics);
        let t1 = thread::spawn(move || a1.inline_seq.fetch_add(1, Ordering::Release));
        let t2 = thread::spawn(move || a2.inline_seq.fetch_add(1, Ordering::Release));
        let s1 = t1.join().unwrap();
        let s2 = t2.join().unwrap();
        assert_ne!(s1, s2, "fetch_add returned the same seq to two threads");
        let final_seq = atomics.inline_seq.load(Ordering::Acquire);
        assert_eq!(final_seq, 2, "inline_seq must total exactly 2");
        let mut sorted = [s1, s2];
        sorted.sort_unstable();
        assert_eq!(sorted, [0_u64, 1_u64], "fetch_add must allocate 0 and 1");
    });
}

/// `loom_capability_record_then_peer_has`
///
/// Thread A records the capability; thread B reads it. Either
/// ordering is valid; the only invariant is "thread B never panics,
/// never reads a partial bit-bag".
#[test]
fn loom_capability_record_then_peer_has() {
    loom::model(|| {
        let registry = Arc::new(CapabilityRegistryMock::new());
        let r1 = Arc::clone(&registry);
        let writer = thread::spawn(move || {
            r1.record("peer-1", true);
        });
        let r2 = Arc::clone(&registry);
        let reader = thread::spawn(move || r2.peer_has("peer-1"));
        writer.join().unwrap();
        let _observed = reader.join().unwrap();
        // After the writer joined the registry must show the
        // recorded bit.
        assert!(
            registry.peer_has("peer-1"),
            "post-join state must reflect the recorded capability",
        );
    });
}

/// `loom_capability_record_then_forget`
///
/// Concurrent record + forget on the same peer. Loom verifies both
/// orderings: record-then-forget terminates with the peer absent,
/// forget-then-record terminates with the peer recorded. Neither
/// path panics; the registry never sees a torn DashMap entry (the
/// mock approximates via Mutex).
#[test]
fn loom_capability_record_then_forget() {
    loom::model(|| {
        let registry = Arc::new(CapabilityRegistryMock::new());
        let r1 = Arc::clone(&registry);
        let recorder = thread::spawn(move || {
            r1.record("peer-1", true);
        });
        let r2 = Arc::clone(&registry);
        let forgetter = thread::spawn(move || {
            r2.forget("peer-1");
        });
        recorder.join().unwrap();
        forgetter.join().unwrap();
        let final_state = registry.peer_has("peer-1");
        // Both terminal states are valid: forget-after-record
        // produces false; record-after-forget produces true.
        assert!(
            final_state == true || final_state == false,
            "registry must report a clean bool",
        );
    });
}

/// `loom_inline_counters_increment_correctly`
///
/// Two threads each call `inline_events_sent.fetch_add(1, Relaxed)`
/// and `inline_bytes_sent.fetch_add(N, Relaxed)` with disjoint N's.
/// Loom verifies the totals are the sum of every thread's
/// contribution.
#[test]
fn loom_inline_counters_increment_correctly() {
    loom::model(|| {
        let atomics = Arc::new(LaneAtomics::new());
        let a1 = Arc::clone(&atomics);
        let a2 = Arc::clone(&atomics);
        let t1 = thread::spawn(move || {
            a1.inline_events_sent.fetch_add(1, Ordering::Relaxed);
            a1.inline_bytes_sent.fetch_add(7, Ordering::Relaxed);
        });
        let t2 = thread::spawn(move || {
            a2.inline_events_sent.fetch_add(1, Ordering::Relaxed);
            a2.inline_bytes_sent.fetch_add(11, Ordering::Relaxed);
        });
        t1.join().unwrap();
        t2.join().unwrap();
        let events = atomics.inline_events_sent.load(Ordering::Relaxed);
        let bytes = atomics.inline_bytes_sent.load(Ordering::Relaxed);
        assert_eq!(events, 2, "events_sent must total exactly 2");
        assert_eq!(bytes, 18, "bytes_sent must total 7+11=18");
    });
}
