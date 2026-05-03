//! Production [`ClockPort`] adapter.
//!
//! Backs `utc_now` with [`chrono::Utc::now`] and `instant_now` with
//! [`std::time::Instant::now`]. Both functions are infallible and
//! cheap, so the adapter is a zero-sized type that can be cloned and
//! shared via `Arc<SystemClock>` without heap allocation.

use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::ports::clock::ClockPort;

/// Production wall-clock and monotonic-clock adapter.
///
/// Zero-sized, `Copy`, safe to share across threads. Construct with
/// [`SystemClock::default`] (or `SystemClock`).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl ClockPort for SystemClock {
    fn utc_now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn instant_now(&self) -> Instant {
        Instant::now()
    }
}

#[cfg(test)]
mod tests {
    use super::SystemClock;
    use crate::ports::clock::ClockPort;

    #[test]
    fn utc_now_returns_recent_timestamp() {
        let before = chrono::Utc::now();
        let now = SystemClock.utc_now();
        let after = chrono::Utc::now();

        assert!(
            now >= before && now <= after,
            "SystemClock::utc_now must lie between two surrounding chrono::Utc::now() calls"
        );
    }

    #[test]
    fn utc_now_is_iso8601_with_t_separator() {
        let stamp = SystemClock.utc_now().to_rfc3339();
        assert!(
            stamp.contains('T'),
            "RFC3339 timestamps must contain the date/time separator 'T'; got {stamp}"
        );
    }

    #[test]
    fn instant_now_is_monotonic() {
        let first = SystemClock.instant_now();
        let second = SystemClock.instant_now();
        assert!(
            second >= first,
            "Instant::now must be monotonic non-decreasing"
        );
    }

    #[test]
    fn system_clock_is_dyn_safe() {
        fn _accepts_dyn(_p: &dyn ClockPort) {}
        let clock = SystemClock;
        _accepts_dyn(&clock);
    }
}
