//! Deterministic [`ClockPort`] adapter for tests.
//!
//! [`FakeClock`] freezes time at a caller-supplied millisecond epoch and
//! only moves forward when the test calls [`FakeClock::advance`] or
//! [`FakeClock::set`]. Both clones share the same underlying counter
//! through an [`Arc<AtomicU64>`], so a use case under test and the
//! controlling test thread observe the same wall clock without any
//! locking.
//!
//! ## Race-safety pattern
//!
//! The shared counter is an [`AtomicU64`] manipulated with
//! `Acquire`/`Release` orderings. `advance` uses `fetch_add` and `set`
//! uses `store(Release)`, so any reader on another thread that performs
//! `load(Acquire)` synchronises-with the latest mutation. No mutex,
//! no possibility of a torn read on the supported platforms (64-bit
//! atomics are lock-free on every Tier-1 target).
//!
//! Compiled only under `#[cfg(test)]` or with the `test-fixtures`
//! Cargo feature so this type never appears in a release binary.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::ports::clock::ClockPort;

/// Frozen, manually advanced clock. Shared via [`Arc`] internally so
/// every clone observes the same time.
#[derive(Debug, Clone)]
pub struct FakeClock {
    /// Current frozen wall-clock time, in milliseconds since the Unix
    /// epoch.
    now_ms: Arc<AtomicU64>,
    /// Anchor for synthesising monotonic [`Instant`] values. Captured
    /// once at construction so successive `instant_now` calls are
    /// totally ordered with the underlying counter.
    base_instant: Instant,
}

impl FakeClock {
    /// Construct a fake clock frozen at `initial_ms` milliseconds since
    /// the Unix epoch.
    #[must_use]
    pub fn new(initial_ms: u64) -> Self {
        Self {
            now_ms: Arc::new(AtomicU64::new(initial_ms)),
            base_instant: Instant::now(),
        }
    }

    /// Advance the frozen time by `by`. Saturates at [`u64::MAX`] ms
    /// (~584 million years) so tests never panic on overflow.
    pub fn advance(&self, by: Duration) {
        let delta_ms = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
        // `fetch_add` wraps on overflow, so guard with a CAS loop that
        // saturates at u64::MAX.
        let mut current = self.now_ms.load(Ordering::Acquire);
        loop {
            let next = current.saturating_add(delta_ms);
            match self.now_ms.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    /// Reset the frozen time to exactly `ms` milliseconds since the
    /// Unix epoch.
    pub fn set(&self, ms: u64) {
        self.now_ms.store(ms, Ordering::Release);
    }

    /// Read the current frozen time in milliseconds. Exposed for tests
    /// that need to assert on the raw counter without going through the
    /// [`ClockPort`] surface.
    #[must_use]
    pub fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::Acquire)
    }
}

impl Default for FakeClock {
    /// Default to `0` (Unix epoch). Tests can advance from there.
    fn default() -> Self {
        Self::new(0)
    }
}

impl ClockPort for FakeClock {
    fn utc_now(&self) -> DateTime<Utc> {
        let ms = self.now_ms();
        let signed = i64::try_from(ms).unwrap_or(i64::MAX);
        DateTime::<Utc>::from_timestamp_millis(signed).unwrap_or_else(|| {
            // `from_timestamp_millis` only returns `None` for values
            // outside chrono's representable range. We clamp to
            // `i64::MAX` above so this branch is functionally
            // unreachable; fall back to the Unix epoch defensively
            // rather than panic — the strict lint baseline forbids
            // `unwrap`/`expect`/`panic` even in unreachable arms.
            DateTime::<Utc>::from_timestamp_millis(0).unwrap_or_default()
        })
    }

    fn instant_now(&self) -> Instant {
        self.base_instant + Duration::from_millis(self.now_ms())
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::FakeClock;
    use crate::ports::clock::ClockPort;

    #[test]
    fn new_freezes_at_initial_ms() {
        let clock = FakeClock::new(1_700_000_000_000_u64);
        assert_eq!(clock.now_ms(), 1_700_000_000_000_u64);
    }

    #[test]
    fn advance_increases_now_ms() {
        let clock = FakeClock::new(1_000_u64);
        clock.advance(Duration::from_millis(500));
        assert_eq!(clock.now_ms(), 1_500_u64);
        clock.advance(Duration::from_secs(2));
        assert_eq!(clock.now_ms(), 3_500_u64);
    }

    #[test]
    fn advance_saturates_on_overflow() {
        let clock = FakeClock::new(u64::MAX - 10);
        clock.advance(Duration::from_secs(60));
        assert_eq!(
            clock.now_ms(),
            u64::MAX,
            "advance must saturate instead of wrapping"
        );
    }

    #[test]
    fn set_resets_now_ms() {
        let clock = FakeClock::new(10_000_u64);
        clock.set(42_u64);
        assert_eq!(clock.now_ms(), 42_u64);
    }

    #[test]
    fn instant_now_reflects_advance() {
        let clock = FakeClock::new(0);
        let before = clock.instant_now();
        clock.advance(Duration::from_millis(250));
        let after = clock.instant_now();
        assert_eq!(
            after.duration_since(before),
            Duration::from_millis(250),
            "instant_now must move forward by the same amount as advance"
        );
    }

    #[test]
    fn utc_now_reflects_advance() {
        let clock = FakeClock::new(1_700_000_000_000_u64);
        let before = clock.utc_now();
        clock.advance(Duration::from_secs(1));
        let after = clock.utc_now();
        assert_eq!((after - before).num_milliseconds(), 1_000_i64);
    }

    #[test]
    fn utc_now_renders_as_rfc3339_with_t_separator() {
        let clock = FakeClock::new(1_700_000_000_000_u64);
        let stamp = clock.utc_now().to_rfc3339();
        assert!(stamp.contains('T'), "RFC3339 must contain 'T'; got {stamp}");
    }

    #[test]
    fn clones_share_underlying_counter() {
        let clock = FakeClock::new(0);
        let twin = clock.clone();
        clock.advance(Duration::from_millis(123));
        assert_eq!(twin.now_ms(), 123_u64);
        twin.set(0);
        assert_eq!(clock.now_ms(), 0_u64);
    }

    #[test]
    fn concurrent_advance_is_race_free() {
        let clock = FakeClock::new(0);
        let mut handles = Vec::with_capacity(8);
        for _ in 0_u8..8_u8 {
            let twin = clock.clone();
            handles.push(thread::spawn(move || {
                for _ in 0_u32..1_000_u32 {
                    twin.advance(Duration::from_millis(1));
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap_or_default();
        }
        assert_eq!(
            clock.now_ms(),
            8 * 1_000_u64,
            "compare_exchange loop must coalesce 8 000 concurrent +1ms advances"
        );
    }

    #[test]
    fn fake_clock_is_dyn_safe() {
        fn _accepts_dyn(_p: &dyn ClockPort) {}
        let clock = FakeClock::default();
        _accepts_dyn(&clock);
    }
}
