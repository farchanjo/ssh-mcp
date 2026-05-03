//! Wall-clock and monotonic-time abstraction.
//!
//! Use cases depend on this port instead of calling `chrono::Utc::now()` or
//! `std::time::Instant::now()` directly so tests can inject a deterministic
//! clock without touching the production adapter. The port is sync and
//! dyn-safe; runtime types live entirely in `std::time`.

use std::time::Instant;

use chrono::{DateTime, Utc};

/// Wall-clock + monotonic-clock port.
pub trait ClockPort: Send + Sync + 'static {
    /// Current wall-clock instant in UTC. Used for `connected_at`,
    /// `started_at`, and similar timestamps surfaced to the caller.
    fn utc_now(&self) -> DateTime<Utc>;

    /// Current monotonic instant. Used for elapsed-duration computations
    /// (timeouts, throttling, throughput) where wall-clock skew would
    /// corrupt the result.
    fn instant_now(&self) -> Instant;
}

#[cfg(test)]
mod tests {
    use super::ClockPort;

    fn _assert_dyn_safe(_p: &dyn ClockPort) {}

    struct StubClock;

    impl ClockPort for StubClock {
        fn utc_now(&self) -> chrono::DateTime<chrono::Utc> {
            chrono::Utc::now()
        }

        fn instant_now(&self) -> std::time::Instant {
            std::time::Instant::now()
        }
    }

    #[test]
    fn stub_clock_implements_port() {
        let clock = StubClock;
        // Round-trip the calls so the trait surface gets touched in the
        // test binary. Concrete time values are non-deterministic so we
        // only assert the calls succeed.
        let _wall = clock.utc_now();
        let _mono = clock.instant_now();
    }
}
