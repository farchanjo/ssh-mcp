//! In-process repository adapters backed by [`dashmap::DashMap`].
//!
//! Targets the single-binary deployment: state lives in shard-locked
//! concurrent maps with `O(1)` lookups and no external dependency.
//! Each adapter is `Clone` (cheap, internally `Arc`-shared) so the
//! composition root can hand the same instance to every use case.
//!
//! The H4 canary ships only the session repository; the remaining four
//! (`command`, `shell`, `transfer`, `forward`) land in H5 as sibling
//! submodules behind this same module path.

pub mod command;
#[cfg(feature = "port_forward")]
pub mod forward;
pub mod rsync;
pub mod session;
pub mod shell;
pub mod transfer;
