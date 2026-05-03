//! Concrete adapter implementations for the ports defined in
//! [`crate::ports`].
//!
//! Each port lives in its own submodule and follows the same shape: a
//! production adapter (real backing services such as `std::time`,
//! environment variables, or `russh`) plus, when useful for testing, a
//! deterministic fake gated behind `#[cfg(any(test, feature =
//! "test-fixtures"))]`.
//!
//! Adapters land incrementally across etapas H3-H9. Each slice is wired
//! by an independent agent so this module is intentionally minimal —
//! just `pub mod` declarations.

pub mod clock;
pub mod id_generator;
