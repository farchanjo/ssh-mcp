//! [`crate::ports::config::ConfigPort`] adapters.
//!
//! - [`internal`] — environment-variable resolvers (relocated from the
//!   legacy v3 config module in H17.6 P3). Single source of truth for
//!   env-var parsing across the workspace.
//! - [`env::EnvConfig`] — production adapter that delegates every
//!   accessor to the matching [`internal`] helper.
//! - [`memory::MapConfig`] — deterministic, struct-backed config for tests.
//!   Compiled only under `#[cfg(test)]` or with the `test-fixtures` feature
//!   so it never reaches a release binary.

pub mod env;
pub mod internal;

#[cfg(any(test, feature = "test-fixtures"))]
pub mod memory;
