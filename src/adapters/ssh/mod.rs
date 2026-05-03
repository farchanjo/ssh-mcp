//! SSH client adapter family.
//!
//! Production wiring of the [`crate::ports::ssh_client::SshClientPort`]
//! port. The current member is
//! [`russh_adapter::RusshAdapter`], which reuses the relocated v3 helpers in
//! [`internal::client`] internally so the retry, authentication, and
//! channel-pacing logic is not duplicated.
//!
//! ## Module shape
//!
//! - `russh_adapter` — production adapter that wraps a
//!   `russh::client::Handle` per `SessionId` and translates every domain
//!   primitive on the way in and every transport result on the way out.
//! - `internal` — adapter-private SSH/PTY runtime carriers relocated from
//!   the legacy v3 namespace in H17.6 P1.
//!
//! Future siblings (e.g. an in-process fake for tests, or an alternate
//! implementation for a non-russh backend) land as additional submodules
//! without touching this layout.

pub mod internal;
pub mod russh_adapter;

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fake;
