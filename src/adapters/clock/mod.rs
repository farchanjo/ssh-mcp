//! [`crate::ports::clock::ClockPort`] adapters.
//!
//! - [`system::SystemClock`] — production adapter backed by
//!   `chrono::Utc::now()` and `std::time::Instant::now()`.
//! - [`fake::FakeClock`] — deterministic, manually advanced clock for
//!   tests. Compiled only under `#[cfg(test)]` or with the
//!   `test-fixtures` feature so it never reaches a release binary.

pub mod system;

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fake;
