//! Lock-free filter pipeline used by the per-`SubId` lane (Phase 2).
//!
//! Wraps a hot-reloadable [`regex::Regex`] inside an
//! [`arc_swap::ArcSwap`] so producers can swap the filter without
//! touching a [`std::sync::Mutex`]. The hot path (`apply` / `passes`)
//! is `Acquire`-loaded `Arc<Option<Regex>>` — one atomic load + one
//! pointer chase.
//!
//! Phase 2 only honours [`FilterRule::Regex`]; [`FilterRule::Level`]
//! is reserved for Phase 3 once events carry structured level
//! metadata. The Phase 2 adapter therefore short-circuits the level
//! arm to "always pass" — documented here so the wire surface stays
//! stable across phases.

use std::fmt;
use std::str;
use std::sync::Arc;

use arc_swap::ArcSwap;
use regex::Regex;

use super::subscriber_lane::LaneMsg;
use crate::domain::error::DomainError;
use crate::domain::subscription::FilterRule;

/// Compiled filter pipeline. Cheap to clone via `Arc`, mutated via
/// `ArcSwap::store`.
pub struct FilterPipeline {
    inner: ArcSwap<FilterState>,
}

/// Snapshot of the current filter state. The compiled regex (if any)
/// lives here so the hot path is one atomic load.
struct FilterState {
    regex: Option<Regex>,
}

impl fmt::Debug for FilterPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let snap = self.inner.load();
        f.debug_struct("FilterPipeline")
            .field("has_regex", &snap.regex.is_some())
            .finish_non_exhaustive()
    }
}

impl FilterPipeline {
    /// Compile `pattern` and return a `Regex`. Wraps any compile error
    /// as [`DomainError::InvalidArgument`] so callers can render a
    /// short cure on the wire.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidArgument`] when the pattern is
    /// not a valid regex.
    pub fn compile_regex(pattern: &str) -> Result<Regex, DomainError> {
        Regex::new(pattern)
            .map_err(|e| DomainError::InvalidArgument(format!("filter regex compile failed: {e}")))
    }

    /// Build a fresh pipeline from a [`FilterRule`].
    #[must_use]
    pub fn new(rule: FilterRule) -> Self {
        let state = match rule {
            FilterRule::Regex(pattern) => FilterState {
                regex: Regex::new(&pattern).ok(),
            },
            FilterRule::None | FilterRule::Level(_) => FilterState { regex: None },
        };
        Self {
            inner: ArcSwap::from_pointee(state),
        }
    }

    /// Hot-reload the pipeline. Validates the rule before swapping so
    /// concurrent producers either see the old or the new pipeline,
    /// never a half-built one.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidArgument`] when the regex fails
    /// to compile.
    pub fn set(&self, rule: FilterRule) -> Result<(), DomainError> {
        let state = match rule {
            FilterRule::Regex(pattern) => FilterState {
                regex: Some(Self::compile_regex(&pattern)?),
            },
            FilterRule::None | FilterRule::Level(_) => FilterState { regex: None },
        };
        self.inner.store(Arc::new(state));
        Ok(())
    }

    /// Return `true` when the message should be forwarded.
    #[must_use]
    pub fn passes(&self, msg: &LaneMsg) -> bool {
        let snap = self.inner.load();
        let Some(regex) = snap.regex.as_ref() else {
            return true;
        };
        // Snapshot/Lagged markers always pass — they are control
        // events, not payload.
        let payload: &[u8] = match msg {
            LaneMsg::Data { payload, .. } => payload.as_slice(),
            LaneMsg::Snapshot { .. } | LaneMsg::Lagged { .. } => return true,
        };
        str::from_utf8(payload).map_or(true, |text| regex.is_match(text))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests use unwrap for brevity per CLAUDE.md test policy"
)]
mod tests {
    use super::{FilterPipeline, FilterRule, LaneMsg};

    fn data(payload: &[u8]) -> LaneMsg {
        LaneMsg::Data {
            seq: 1,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn passthrough_filter_lets_every_message_through() {
        let f = FilterPipeline::new(FilterRule::None);
        assert!(f.passes(&data(b"anything")));
        assert!(f.passes(&LaneMsg::Snapshot {
            cursor: 0,
            delta: vec![],
        }));
    }

    #[test]
    fn regex_filter_matches_payload() {
        let f = FilterPipeline::new(FilterRule::Regex("ERROR".to_string()));
        assert!(f.passes(&data(b"ERROR: boom")));
        assert!(!f.passes(&data(b"INFO: hi")));
    }

    #[test]
    fn regex_filter_passes_snapshot_and_lagged_unconditionally() {
        let f = FilterPipeline::new(FilterRule::Regex("nope".to_string()));
        assert!(f.passes(&LaneMsg::Snapshot {
            cursor: 0,
            delta: vec![],
        }));
        assert!(f.passes(&LaneMsg::Lagged { dropped: 5 }));
    }

    #[test]
    fn invalid_regex_falls_back_to_passthrough_in_constructor() {
        // Constructor swallows compile errors so a misconfigured
        // initial filter does not crash the adapter; subsequent
        // `set()` calls surface the error to the caller.
        let f = FilterPipeline::new(FilterRule::Regex("([".to_string()));
        assert!(f.passes(&data(b"anything")));
    }

    #[test]
    fn set_returns_invalid_argument_for_bad_regex() {
        let f = FilterPipeline::new(FilterRule::None);
        let err = f.set(FilterRule::Regex("([".to_string())).unwrap_err();
        match err {
            crate::domain::error::DomainError::InvalidArgument(m) => {
                assert!(m.contains("regex"))
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn set_swaps_active_regex() {
        let f = FilterPipeline::new(FilterRule::None);
        f.set(FilterRule::Regex("ERROR".to_string())).unwrap();
        assert!(f.passes(&data(b"ERROR")));
        assert!(!f.passes(&data(b"WARN")));
    }

    #[test]
    fn set_back_to_none_re_enables_passthrough() {
        let f = FilterPipeline::new(FilterRule::Regex("ERROR".to_string()));
        f.set(FilterRule::None).unwrap();
        assert!(f.passes(&data(b"WARN")));
    }

    #[test]
    fn level_filter_is_passthrough_in_phase_2() {
        let f = FilterPipeline::new(FilterRule::Level(
            crate::domain::subscription::LogLevel::Warn,
        ));
        // Phase 2 short-circuits `Level` to passthrough; Phase 3 wires
        // structured filtering once events carry a level field.
        assert!(f.passes(&data(b"anything")));
    }

    #[test]
    fn non_utf8_payload_forwards_unconditionally() {
        let f = FilterPipeline::new(FilterRule::Regex("ERROR".to_string()));
        let bytes = vec![0xFF_u8, 0xFE, 0xFD];
        assert!(f.passes(&data(&bytes)));
    }

    #[test]
    fn compile_regex_returns_invalid_argument_for_bad_input() {
        let err = FilterPipeline::compile_regex("([").unwrap_err();
        assert!(matches!(
            err,
            crate::domain::error::DomainError::InvalidArgument(_)
        ));
    }

    #[test]
    fn compile_regex_accepts_valid_pattern() {
        let r = FilterPipeline::compile_regex("^ERROR.*$").unwrap();
        assert!(r.is_match("ERROR: boom"));
    }

    #[test]
    fn debug_renders_has_regex_state() {
        let f = FilterPipeline::new(FilterRule::Regex("X".to_string()));
        let dbg = format!("{f:?}");
        assert!(dbg.contains("has_regex"));
    }
}
