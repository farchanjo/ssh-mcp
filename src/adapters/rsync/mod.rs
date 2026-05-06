//! Adapters for the ADR 0011 rsync hybrid transport.
//!
//! v7.0.0-alpha.2 architectural retrenchment: the deployed-agent path
//! was retracted in favour of two integrated transports living inside
//! the host crate. The user-visible behaviour for this slice is that
//! every `ssh_rsync*` call surfaces a "being implemented" wire error
//! tagged with the chosen transport — the surface is honest, and the
//! next slice ships the real implementations without churning the
//! adapter shape.
//!
//! ## Module layout
//!
//! - [`types`] — value objects + push-lane projections shared by every
//!   transport (replaces the deleted `ssh-mcp-rsync-proto` crate).
//! - [`fake`] — deterministic in-memory adapters used by use-case
//!   tests (gated behind `#[cfg(any(test, feature = "test-fixtures"))]`).
//! - [`wire`] — wire-compat client speaking rsync protocol v31 against
//!   a remote `rsync --server` process. Stub today; real
//!   implementation lands in the next slice.
//! - [`sftp`] — SFTP fallback driving plain `readdir` + `stat` +
//!   `read` + `write` + `setstat`, no remote helper required. Stub
//!   today; real implementation lands in the next slice.

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fake;
pub mod sftp;
pub mod types;
pub mod wire;

// Re-export the SFTP transport's submodules so downstream code can
// reach the walker / comparator / executor / fake without repeating
// the `sftp::` prefix every time. Public re-exports stay opt-in via
// the `#[cfg(any(test, feature = "test-fixtures"))]` on `fake`.
