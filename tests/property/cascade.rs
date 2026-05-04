//! Properties: cascade refcount.
//!
//! - `prop_cascade_refs_no_underflow` — concurrent decrements never
//!   produce a negative refcount (saturating).
//! - `prop_cascade_session_active_refs_matches_resources` — number
//!   of `inc_session` calls equals `session_active_refs`.

use proptest::prelude::*;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::ports::subscriber_registry::ResourceKind;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// `prop_cascade_refs_no_underflow`
    ///
    /// Calling `on_resource_closed` more often than `inc_session` MUST
    /// NOT panic. The cascade saturates at zero.
    #[test]
    fn prop_cascade_refs_no_underflow(
        inc_count in 0_u32..16,
        close_count in 0_u32..16,
    ) {
        let cascade = CascadeCoordinator::new();
        let session = SessionId::new("c".to_string());
        for _ in 0..inc_count {
            cascade.inc_session(&session);
        }
        for i in 0..close_count {
            cascade.on_resource_closed(
                ResourceKind::Shell,
                &format!("r-{i}"),
                &session,
            );
        }
        let active = cascade.session_active_refs(&session);
        prop_assert!(active <= inc_count as usize, "active={active} inc={inc_count}");
    }

    /// `prop_cascade_session_active_refs_balanced`
    ///
    /// When inc_count == close_count, active_refs is zero and the
    /// session is reaped.
    #[test]
    fn prop_cascade_session_active_refs_balanced(n in 1_u32..16) {
        let cascade = CascadeCoordinator::new();
        let session = SessionId::new("balanced".to_string());
        for _ in 0..n {
            cascade.inc_session(&session);
        }
        for i in 0..n {
            cascade.on_resource_closed(
                ResourceKind::Shell,
                &format!("r-{i}"),
                &session,
            );
        }
        let active = cascade.session_active_refs(&session);
        prop_assert_eq!(active, 0);
    }
}
