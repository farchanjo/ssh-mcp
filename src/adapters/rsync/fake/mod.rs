//! Test-only adapters for the ADR 0011 rsync transport port. Gated
//! behind `#[cfg(any(test, feature = "test-fixtures"))]` so the fakes
//! never reach a release binary.
//!
//! - [`transport::FakeRsyncTransport`] — scripted
//!   [`crate::ports::rsync_transport::RsyncTransportPort`] outcomes.
//!
//! The fake mirrors the FIFO-queue style used by
//! [`crate::adapters::sftp::fake::FakeSftpClient`]; the queue storage
//! is lock-free (a [`dashmap::DashMap`] indexed by an
//! [`std::sync::atomic::AtomicU64`] head / tail pair) so the workspace
//! `mutex_atomic` / `await_holding_lock` invariants stay intact.

pub mod transport;
