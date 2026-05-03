//! SFTP adapter internals (relocated from the legacy v3 namespace in
//! H17.6 P1).
//!
//! These modules host the lock-free transfer state machine and the
//! streaming SFTP chunk loop consumed by the production
//! [`super::russh_sftp_adapter::RusshSftpAdapter`]. They were previously
//! exposed under the legacy v3 SFTP/transfer/types namespace.
//! H17.6 Phase 1 only relocates source files and updates imports.
//!
//! No public re-exports: every consumer must use the fully-qualified path
//! `crate::adapters::sftp::internal::<module>::*`.

pub(crate) mod sftp;
pub(crate) mod transfer;
pub mod types;
