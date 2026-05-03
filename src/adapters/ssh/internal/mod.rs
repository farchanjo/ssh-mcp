//! SSH adapter internals (relocated from the legacy v3 namespace across
//! H17.6 P1, P2 and P3).
//!
//! These modules are foundational SSH/PTY runtime carriers consumed by the
//! production [`super::russh_adapter::RusshAdapter`]. They were previously
//! exposed under the legacy v3 namespace. H17.6 Phase 1
//! relocated the SSH/PTY runtime carriers; Phase 2 relocated the russh
//! handshake strategy chain (`auth`) and converted it from
//! `#[async_trait]` to native AFIT, dropping the direct
//! `async-trait` dependency. Phase 3 relocated the retry-classification
//! helper into [`error`] so the SSH adapter no longer depends on the
//! legacy retry-classification module.
//!
//! No public re-exports: every consumer must use the fully-qualified path
//! `crate::adapters::ssh::internal::<module>::*`.

pub(crate) mod async_command;
pub(crate) mod auth;
pub(crate) mod client;
pub(crate) mod error;
pub mod session;
pub(crate) mod shell;
pub mod status_sink;
pub mod types;
