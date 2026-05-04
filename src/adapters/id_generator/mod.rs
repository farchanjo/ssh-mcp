//! [`crate::ports::id_generator::IdGeneratorPort`] adapters.
//!
//! - [`uuid::UuidIds`] — production adapter that mints fresh
//!   [`uuid::Uuid::now_v7`] values per call (RFC 9562 §5.7,
//!   monotonic time-ordering — see ADR 0004) and wraps them in
//!   the domain newtypes from [`crate::domain::ids`].
//! - [`deterministic::SequentialIds`] — test-only adapter backed by
//!   per-kind [`std::sync::atomic::AtomicU64`] counters so snapshots
//!   stay stable. Compiled only under `#[cfg(test)]` or with the
//!   `test-fixtures` feature so it never reaches a release binary.

pub mod uuid;

#[cfg(any(test, feature = "test-fixtures"))]
pub mod deterministic;
