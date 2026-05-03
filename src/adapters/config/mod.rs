//! [`crate::ports::config::ConfigPort`] adapters.
//!
//! - [`env::EnvConfig`] — production adapter that delegates every accessor
//!   to the matching `crate::mcp::config::resolve_*` helper, so v3 and v4
//!   share a single source of truth for environment-variable parsing.
//! - [`memory::MapConfig`] — deterministic, struct-backed config for tests.
//!   Compiled only under `#[cfg(test)]` or with the `test-fixtures` feature
//!   so it never reaches a release binary.

pub mod env;

#[cfg(any(test, feature = "test-fixtures"))]
pub mod memory;
