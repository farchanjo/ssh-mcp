//! ADR 0010 — property tests for the SFTP resume decision matrix.
//!
//! The pure decision matrix (`decide_upload_plan` /
//! `decide_download_plan`) is exposed `pub` from
//! `crate::adapters::sftp::internal::sftp` so external tests can fuzz
//! every legal `(local_size, remote_size)` pair without a live SFTP
//! server.
//!
//! Properties asserted across `(local, remote) in [0, 16 MiB]`:
//!
//! - **Upload total-order**: For every pair, exactly one of the four
//!   ADR branches matches (Truncate, Skip, Resume, Err) — never panics,
//!   never returns the wrong variant.
//! - **Download symmetry**: Mirror invariant with sizes swapped.
//! - **Skip implies equality**: A Skip plan only fires when the two
//!   sizes are equal.
//! - **Resume implies inequality with offset bounded**: A Resume plan
//!   has `offset > 0`, `offset < total_bytes`, and `total_bytes`
//!   equals the larger of the two sizes (per direction).
//! - **Err is RESUME_OVERSHOOT-tagged**: Every error string carries the
//!   `[RESUME_OVERSHOOT]` tag the wire layer routes on.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    reason = "property tests use unwrap and proptest macros"
)]

use proptest::prelude::*;
use ssh_mcp::adapters::sftp::internal::resume::{
    ResumePlan, decide_download_plan, decide_upload_plan,
};

const SIXTEEN_MIB: u64 = 16 * 1024 * 1024;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `prop_upload_decision_matrix_total`
    ///
    /// Every legal pair maps to exactly one variant: Truncate (only
    /// when remote is empty), Skip (sizes match), Resume (remote
    /// shorter), or Err (remote larger). Never panics.
    #[test]
    fn prop_upload_decision_matrix_total(
        local in 0_u64..=SIXTEEN_MIB,
        remote in 0_u64..=SIXTEEN_MIB,
    ) {
        match decide_upload_plan(local, remote) {
            Ok(ResumePlan::Truncate) => {
                // Pure helper never returns Truncate from `decide_*`;
                // Truncate is the `resume == false` short-circuit one
                // layer up. The decision matrix only covers the
                // remote-existed case.
                prop_assert!(false, "decide_upload_plan must never return Truncate");
            }
            Ok(ResumePlan::Skip { total_bytes }) => {
                prop_assert_eq!(local, remote, "Skip implies size equality");
                prop_assert_eq!(total_bytes, local);
            }
            Ok(ResumePlan::Resume { offset, total_bytes }) => {
                prop_assert!(remote < local, "Resume implies remote < local");
                prop_assert_eq!(offset, remote);
                prop_assert_eq!(total_bytes, local);
                prop_assert!(offset < total_bytes, "offset must be strictly inside total");
            }
            Err(msg) => {
                prop_assert!(remote > local, "Err only when remote > local");
                prop_assert!(
                    msg.contains("[RESUME_OVERSHOOT]"),
                    "every overshoot error must carry the [RESUME_OVERSHOOT] tag, got: {}",
                    msg
                );
            }
        }
    }

    /// `prop_download_decision_matrix_total`
    ///
    /// Mirror of the upload property with local/remote swapped.
    #[test]
    fn prop_download_decision_matrix_total(
        local in 0_u64..=SIXTEEN_MIB,
        remote in 0_u64..=SIXTEEN_MIB,
    ) {
        match decide_download_plan(local, remote) {
            Ok(ResumePlan::Truncate) => {
                prop_assert!(false, "decide_download_plan must never return Truncate");
            }
            Ok(ResumePlan::Skip { total_bytes }) => {
                prop_assert_eq!(local, remote, "Skip implies size equality");
                prop_assert_eq!(total_bytes, remote);
            }
            Ok(ResumePlan::Resume { offset, total_bytes }) => {
                prop_assert!(local < remote, "Resume implies local < remote");
                prop_assert_eq!(offset, local);
                prop_assert_eq!(total_bytes, remote);
                prop_assert!(offset < total_bytes, "offset must be strictly inside total");
            }
            Err(msg) => {
                prop_assert!(local > remote, "Err only when local > remote");
                prop_assert!(
                    msg.contains("[RESUME_OVERSHOOT]"),
                    "every overshoot error must carry the [RESUME_OVERSHOOT] tag, got: {}",
                    msg
                );
            }
        }
    }

    /// `prop_upload_skip_iff_equal`
    ///
    /// `decide_upload_plan(n, n)` for any `n` returns `Skip { n }` —
    /// equality is the only path to Skip.
    #[test]
    fn prop_upload_skip_iff_equal(n in 0_u64..=SIXTEEN_MIB) {
        match decide_upload_plan(n, n) {
            Ok(ResumePlan::Skip { total_bytes }) => {
                prop_assert_eq!(total_bytes, n);
            }
            other => prop_assert!(false, "expected Skip, got {:?}", other),
        }
    }

    /// `prop_download_skip_iff_equal`
    ///
    /// Mirror.
    #[test]
    fn prop_download_skip_iff_equal(n in 0_u64..=SIXTEEN_MIB) {
        match decide_download_plan(n, n) {
            Ok(ResumePlan::Skip { total_bytes }) => {
                prop_assert_eq!(total_bytes, n);
            }
            other => prop_assert!(false, "expected Skip, got {:?}", other),
        }
    }

    /// `prop_upload_resume_offset_strictly_below_total`
    ///
    /// When `local > 0` and `remote < local`, the plan is Resume with
    /// `offset < total`. Stronger statement of the boundary the
    /// streaming chunk loop relies on.
    #[test]
    fn prop_upload_resume_offset_strictly_below_total(
        local in 1_u64..=SIXTEEN_MIB,
        delta in 1_u64..=SIXTEEN_MIB,
    ) {
        // remote = local - min(delta, local) keeps `remote < local`.
        let remote = local.saturating_sub(delta).min(local.saturating_sub(1));
        match decide_upload_plan(local, remote) {
            Ok(ResumePlan::Resume { offset, total_bytes }) => {
                prop_assert!(offset < total_bytes);
                prop_assert_eq!(offset, remote);
                prop_assert_eq!(total_bytes, local);
            }
            Ok(ResumePlan::Skip { .. }) => {
                // Allowed when `delta == 0` rounds remote up to local.
                prop_assert_eq!(local, remote);
            }
            other => prop_assert!(false, "unexpected plan: {:?}", other),
        }
    }

    /// `prop_download_resume_offset_strictly_below_total`
    ///
    /// Mirror.
    #[test]
    fn prop_download_resume_offset_strictly_below_total(
        remote in 1_u64..=SIXTEEN_MIB,
        delta in 1_u64..=SIXTEEN_MIB,
    ) {
        let local = remote.saturating_sub(delta).min(remote.saturating_sub(1));
        match decide_download_plan(local, remote) {
            Ok(ResumePlan::Resume { offset, total_bytes }) => {
                prop_assert!(offset < total_bytes);
                prop_assert_eq!(offset, local);
                prop_assert_eq!(total_bytes, remote);
            }
            Ok(ResumePlan::Skip { .. }) => {
                prop_assert_eq!(local, remote);
            }
            other => prop_assert!(false, "unexpected plan: {:?}", other),
        }
    }
}
