//! SSH adapter internals (relocated from `crate::mcp::*` in H17.6 P1
//! and P2).
//!
//! These modules are foundational SSH/PTY runtime carriers consumed by the
//! production [`super::russh_adapter::RusshAdapter`]. They were previously
//! exposed under `crate::mcp::*` (the v3 namespace). H17.6 Phase 1
//! relocated the SSH/PTY runtime carriers; Phase 2 relocated the russh
//! handshake strategy chain (`auth`) and converted it from
//! `#[async_trait]` to native AFIT, dropping the direct
//! `async-trait` dependency.
//!
//! No public re-exports: every consumer must use the fully-qualified path
//! `crate::adapters::ssh::internal::<module>::*`.

pub(crate) mod async_command;
pub(crate) mod auth;
pub(crate) mod client;
pub mod session;
pub(crate) mod shell;
pub mod types;
