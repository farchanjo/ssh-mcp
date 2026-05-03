//! Repository adapters for the persistence ports defined in
//! [`crate::ports`] (one port per aggregate: session, command, shell,
//! transfer, forward).
//!
//! Each backing technology lives in its own submodule. The H4 canary
//! ships only the in-process `dashmap` family — H5 fills out the
//! remaining four repos and any future remote backends (Redis, Postgres)
//! will land as sibling submodules without touching `dashmap`.

pub mod dashmap;
