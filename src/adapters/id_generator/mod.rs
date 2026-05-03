//! [`crate::ports::id_generator::IdGeneratorPort`] adapters.
//!
//! - [`uuid::UuidIds`] — production adapter that mints fresh
//!   [`uuid::Uuid::new_v4`] values per call and wraps them in the
//!   domain newtypes from [`crate::domain::ids`].
//! - [`deterministic::SequentialIds`] — test-only adapter backed by
//!   per-kind [`std::sync::atomic::AtomicU64`] counters so snapshots
//!   stay stable. Compiled only under `#[cfg(test)]` or with the
//!   `test-fixtures` feature so it never reaches a release binary.

pub mod uuid;

#[cfg(any(test, feature = "test-fixtures"))]
pub mod deterministic;
