//! SSH adapter internals (relocated from `crate::mcp::*` in H17.6 P1).
//!
//! These modules are foundational SSH/PTY runtime carriers consumed by the
//! production [`super::russh_adapter::RusshAdapter`]. They were previously
//! exposed under `crate::mcp::*` (the v3 namespace). H17.6 Phase 1 only
//! relocates the source files and updates imports; subsequent phases will
//! inline / collapse this surface as the v3 namespace is retired.
//!
//! No public re-exports: every consumer must use the fully-qualified path
//! `crate::adapters::ssh::internal::<module>::*`.

pub(crate) mod async_command;
pub(crate) mod client;
pub mod session;
pub(crate) mod shell;
pub mod types;
