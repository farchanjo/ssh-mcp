//! ADR 0010 — public surface for the SFTP resume decision matrix.
//!
//! The two `decide_*_plan` helpers and the [`ResumePlan`] enum are pure
//! (no I/O) and live inside the otherwise-`pub(crate)` `internal::sftp`
//! module. This module exposes them as a thin, documented public
//! surface so external property tests under `tests/property_resume.rs`
//! can fuzz the matrix without paying for the real preflight I/O.
//!
//! Production callers go through
//! [`crate::adapters::sftp::russh_sftp_adapter`]; this module is
//! intentionally minimal — it never grows past the three names below.

#![expect(
    clippy::pub_use,
    reason = "narrow re-export for ADR 0010 property tests; the canonical definition stays in `super::sftp` where the streaming chunk loop consumes it"
)]

use super::sftp;

pub use sftp::ResumePlan;

/// Pure decision matrix for the upload preflight. Mirrors
/// `decide_upload_plan` in [`super::sftp`].
///
/// # Errors
///
/// Returns a `[RESUME_OVERSHOOT]`-tagged error string when
/// `remote_size > local_size`. The caller surfaces this through
/// `DomainError::Sftp`.
#[inline]
pub fn decide_upload_plan(local_size: u64, remote_size: u64) -> Result<ResumePlan, String> {
    sftp::decide_upload_plan(local_size, remote_size)
}

/// Pure decision matrix for the download preflight. Mirrors
/// `decide_download_plan` in [`super::sftp`].
///
/// # Errors
///
/// Returns a `[RESUME_OVERSHOOT]`-tagged error string when
/// `local_size > remote_size`.
#[inline]
pub fn decide_download_plan(local_size: u64, remote_size: u64) -> Result<ResumePlan, String> {
    sftp::decide_download_plan(local_size, remote_size)
}
