//! Cross-adapter v3 leftovers (config, error, subscription).
//!
//! After H17.5a (hard-delete of the v3 MCP server), H17.6 P1 (relocation
//! of the SSH/SFTP runtime carriers into `adapters::ssh::internal` and
//! `adapters::sftp::internal`), and H17.6 P2 (relocation of the russh
//! handshake strategy chain into `adapters::ssh::internal::auth` plus
//! AFIT conversion), this module only hosts cross-cutting concerns
//! shared between adapters and the composition root:
//!
//! - [`config`] — env-var resolvers; `adapters::config::env` delegates here.
//! - [`error`] — retry classification consumed by `adapters::ssh::internal::client`.
//! - [`subscription`] — `SUBSCRIPTION_REGISTRY` + peer GC task spawned by composition.
//!
//! Subsequent H17.6 phases will absorb these into the hexagonal layout and
//! retire the `mcp::` namespace entirely.

pub mod config;
pub(crate) mod error;
pub mod subscription;
