//! Cross-adapter v3 leftovers (auth, config, error, subscription).
//!
//! After H17.5a (hard-delete of the v3 MCP server) and H17.6 P1 (relocation
//! of the SSH/SFTP runtime carriers into `adapters::ssh::internal` and
//! `adapters::sftp::internal`), this module only hosts cross-cutting
//! concerns shared between adapters and the composition root:
//!
//! - [`auth`] — `AuthChain` strategies (still uses `async-trait`; left in
//!   place per Phase 1 scope).
//! - [`config`] — env-var resolvers; `adapters::config::env` delegates here.
//! - [`error`] — retry classification consumed by `adapters::ssh::internal::client`.
//! - [`subscription`] — `SUBSCRIPTION_REGISTRY` + peer GC task spawned by composition.
//!
//! Subsequent H17.6 phases will absorb these into the hexagonal layout and
//! retire the `mcp::` namespace entirely.

pub mod auth;
pub mod config;
pub(crate) mod error;
pub mod subscription;
