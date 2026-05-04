//! Resource lifecycle entities (v5 Phase 1 — Lifecycle Binding).
//!
//! The lifecycle layer wraps every long-lived resource (shells, commands,
//! transfers, forwards) with a small state machine that tracks the number
//! of MCP subscribers currently observing it. When the last subscriber
//! detaches and the policy opts in to refcount-driven release, the
//! adapter arms a configurable grace timer and ultimately closes the
//! resource if no subscriber re-attaches in time.
//!
//! ```text
//!                  on_subscribe                  on_unsubscribe
//!    Owned ────────────────────►  Observed ────────────────────► Releasing
//!      ▲                            │                                │
//!      │ on_subscribe (cancel grace)│                                │
//!      └────────────────────────────┘                                │
//!                                                                    │
//!                                       grace_until elapsed          ▼
//!                                       OR force_close            Closed
//! ```
//!
//! This module is **pure data**: no `Atomic*`, no `Arc`, no `Notify`. The
//! atomic state machine that drives concurrent transitions lives in the
//! adapter (`crate::adapters::lifecycle::refcount`). Use cases consume the
//! port surface (`crate::ports::lifecycle_policy::LifecyclePolicyPort`).
//!
//! The `ResourceKind` discriminator is re-exported from the subscriber
//! registry port so the domain layer does not duplicate the enum that the
//! v4 hexagonal core has already coalesced on.

use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::ports::subscriber_registry::ResourceKind;

/// Default grace window in milliseconds the adapter waits between the
/// last unsubscribe and the actual close.
///
/// Five seconds matches the v3 `MaxSessions` pacing buffer documented on
/// the `cancel_command` use case.
pub const DEFAULT_GRACE_MS: u32 = 5_000;

/// State of a tracked resource at a single point in time.
///
/// Transitions are observable via
/// [`crate::ports::lifecycle_policy::LifecyclePolicyPort::snapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    /// The resource exists and has zero subscribers (or a positive
    /// `release_when_no_subs == false` policy keeps it pinned). Manual
    /// close paths transition directly from `Owned` to `Closed`.
    Owned,
    /// At least one subscriber is currently observing the resource.
    Observed,
    /// Subscribers dropped to zero, the policy requested release, and the
    /// adapter has armed a grace timer. A new subscribe before the timer
    /// fires cancels the timer and transitions back to [`Self::Observed`].
    Releasing,
    /// The resource has been torn down and any cascade hook has fired.
    /// Terminal state; further `on_subscribe` calls return
    /// [`crate::domain::error::DomainError::ResourceGone`].
    Closed,
}

impl LifecycleState {
    /// Stable byte tag used by the adapter's `AtomicU8` state encoding.
    /// Kept here so the adapter and domain layer agree on a single
    /// canonical encoding.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Owned => 0,
            Self::Observed => 1,
            Self::Releasing => 2,
            Self::Closed => 3,
        }
    }

    /// Decode a byte tag previously produced by [`Self::as_u8`]. Returns
    /// `None` for any other value so callers can flag the bug rather than
    /// silently coerce to a default state.
    #[must_use]
    pub const fn from_u8(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::Owned),
            1 => Some(Self::Observed),
            2 => Some(Self::Releasing),
            3 => Some(Self::Closed),
            _ => None,
        }
    }

    /// True when the state is terminal (no further transitions allowed).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Closed)
    }
}

/// Caller-controllable knobs that tweak how the adapter releases a
/// resource when its subscriber count reaches zero.
///
/// `Default` mirrors the v4 behaviour: refcount-driven release is
/// **disabled** so existing tools keep their explicit close semantics
/// while v5 phases roll out incrementally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LifecyclePolicy {
    /// When `true`, the adapter arms a grace timer the moment the
    /// subscriber count drops to zero and closes the resource if nobody
    /// re-attaches before the timer fires. When `false`, the resource is
    /// kept alive until an explicit close path runs (v4 default).
    pub release_when_no_subs: bool,
    /// Duration of the grace window in milliseconds. Honoured only when
    /// [`Self::release_when_no_subs`] is `true`.
    pub grace_ms: u32,
    /// When `true`, the adapter notifies the cascade coordinator
    /// (`crate::adapters::lifecycle::cascade`) as soon as the resource
    /// closes so the owning session can trigger an automatic disconnect
    /// once its active-resource refcount hits zero. Disabled by default
    /// so legacy callers do not see surprising session teardowns.
    pub cascade_session: bool,
}

impl Default for LifecyclePolicy {
    fn default() -> Self {
        Self {
            release_when_no_subs: false,
            grace_ms: DEFAULT_GRACE_MS,
            cascade_session: false,
        }
    }
}

impl LifecyclePolicy {
    /// Convenience constructor for the v5 opt-in shape: refcount-driven
    /// release with the default grace window.
    #[must_use]
    pub const fn release_with_default_grace() -> Self {
        Self {
            release_when_no_subs: true,
            grace_ms: DEFAULT_GRACE_MS,
            cascade_session: false,
        }
    }

    /// Convenience constructor that opts in to the cascade hook on top
    /// of the default release behaviour.
    #[must_use]
    pub const fn release_with_cascade() -> Self {
        Self {
            release_when_no_subs: true,
            grace_ms: DEFAULT_GRACE_MS,
            cascade_session: true,
        }
    }
}

/// Read-only snapshot returned by
/// [`crate::ports::lifecycle_policy::LifecyclePolicyPort::snapshot`] for
/// diagnostics, MCP `sub_stats` rendering and integration tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LifecycleSnapshot {
    /// Current state of the resource.
    pub state: LifecycleState,
    /// Number of subscribers currently attached.
    pub sub_count: usize,
    /// Milliseconds remaining on the grace timer when the state is
    /// [`LifecycleState::Releasing`]. `None` for any other state.
    pub grace_remaining_ms: Option<u32>,
    /// Policy in effect at the moment of the snapshot.
    pub policy: LifecyclePolicy,
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_GRACE_MS, LifecyclePolicy, LifecycleSnapshot, LifecycleState, ResourceKind,
    };

    // --- Encoding / decoding ----------------------------------------

    #[test]
    fn state_encodes_to_distinct_u8_tags() {
        assert_eq!(LifecycleState::Owned.as_u8(), 0);
        assert_eq!(LifecycleState::Observed.as_u8(), 1);
        assert_eq!(LifecycleState::Releasing.as_u8(), 2);
        assert_eq!(LifecycleState::Closed.as_u8(), 3);
    }

    #[test]
    fn state_from_u8_round_trips() {
        for state in [
            LifecycleState::Owned,
            LifecycleState::Observed,
            LifecycleState::Releasing,
            LifecycleState::Closed,
        ] {
            assert_eq!(LifecycleState::from_u8(state.as_u8()), Some(state));
        }
    }

    #[test]
    fn state_from_u8_rejects_unknown_tag() {
        assert!(LifecycleState::from_u8(4).is_none());
        assert!(LifecycleState::from_u8(255).is_none());
    }

    #[test]
    fn closed_is_the_only_terminal_state() {
        assert!(LifecycleState::Closed.is_terminal());
        assert!(!LifecycleState::Owned.is_terminal());
        assert!(!LifecycleState::Observed.is_terminal());
        assert!(!LifecycleState::Releasing.is_terminal());
    }

    // --- Equality, hashability, copy --------------------------------

    #[test]
    fn state_is_copy_and_hashable() {
        let mut set = std::collections::HashSet::new();
        set.insert(LifecycleState::Owned);
        set.insert(LifecycleState::Owned);
        set.insert(LifecycleState::Observed);
        assert_eq!(set.len(), 2);
        // Copy: a literal value can be passed twice without moving.
        let a = LifecycleState::Owned;
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn policy_default_disables_refcount_release() {
        let p = LifecyclePolicy::default();
        assert!(!p.release_when_no_subs);
        assert_eq!(p.grace_ms, DEFAULT_GRACE_MS);
        assert!(!p.cascade_session);
    }

    #[test]
    fn release_with_default_grace_helper_enables_release_only() {
        let p = LifecyclePolicy::release_with_default_grace();
        assert!(p.release_when_no_subs);
        assert_eq!(p.grace_ms, DEFAULT_GRACE_MS);
        assert!(!p.cascade_session);
    }

    #[test]
    fn release_with_cascade_helper_enables_release_and_cascade() {
        let p = LifecyclePolicy::release_with_cascade();
        assert!(p.release_when_no_subs);
        assert_eq!(p.grace_ms, DEFAULT_GRACE_MS);
        assert!(p.cascade_session);
    }

    #[test]
    fn policy_is_copy_and_hashable() {
        let p = LifecyclePolicy::default();
        let q = p;
        assert_eq!(p, q);
        let mut set = std::collections::HashSet::new();
        set.insert(p);
        set.insert(q);
        assert_eq!(set.len(), 1);
    }

    // --- Serde wire shape -------------------------------------------

    #[test]
    fn state_serde_uses_snake_case_strings() {
        let json = serde_json::to_string(&LifecycleState::Releasing).expect("serialize");
        assert_eq!(json, "\"releasing\"");
        let back: LifecycleState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, LifecycleState::Releasing);
    }

    #[test]
    fn state_serde_round_trips_every_variant() {
        for state in [
            LifecycleState::Owned,
            LifecycleState::Observed,
            LifecycleState::Releasing,
            LifecycleState::Closed,
        ] {
            let json = serde_json::to_string(&state).expect("ser");
            let back: LifecycleState = serde_json::from_str(&json).expect("de");
            assert_eq!(back, state);
        }
    }

    #[test]
    fn policy_serde_round_trips_default() {
        let p = LifecyclePolicy::default();
        let json = serde_json::to_string(&p).expect("ser");
        let back: LifecyclePolicy = serde_json::from_str(&json).expect("de");
        assert_eq!(back, p);
    }

    #[test]
    fn policy_serde_round_trips_release_variant() {
        let p = LifecyclePolicy::release_with_default_grace();
        let json = serde_json::to_string(&p).expect("ser");
        let back: LifecyclePolicy = serde_json::from_str(&json).expect("de");
        assert_eq!(back, p);
    }

    #[test]
    fn policy_serde_carries_grace_ms_field() {
        let mut p = LifecyclePolicy::release_with_default_grace();
        p.grace_ms = 10_000;
        let json = serde_json::to_string(&p).expect("ser");
        assert!(json.contains("\"grace_ms\":10000"), "json missing grace_ms: {json}");
    }

    // --- Snapshot ---------------------------------------------------

    #[test]
    fn snapshot_round_trips_via_serde() {
        let snap = LifecycleSnapshot {
            state: LifecycleState::Observed,
            sub_count: 3,
            grace_remaining_ms: None,
            policy: LifecyclePolicy::release_with_default_grace(),
        };
        let json = serde_json::to_string(&snap).expect("ser");
        let back: LifecycleSnapshot = serde_json::from_str(&json).expect("de");
        assert_eq!(back, snap);
    }

    #[test]
    fn snapshot_grace_remaining_only_set_in_releasing_state_in_practice() {
        // The struct itself does not enforce the invariant — the adapter
        // promises to populate `grace_remaining_ms` only when the state
        // is `Releasing`. We verify that callers can construct both
        // shapes so smaller LLMs are not blocked by a runtime guard.
        let owned = LifecycleSnapshot {
            state: LifecycleState::Owned,
            sub_count: 0,
            grace_remaining_ms: None,
            policy: LifecyclePolicy::default(),
        };
        let releasing = LifecycleSnapshot {
            state: LifecycleState::Releasing,
            sub_count: 0,
            grace_remaining_ms: Some(2_500),
            policy: LifecyclePolicy::release_with_default_grace(),
        };
        assert_eq!(owned.state, LifecycleState::Owned);
        assert_eq!(releasing.grace_remaining_ms, Some(2_500));
    }

    // --- ResourceKind interop --------------------------------------

    #[test]
    fn resource_kind_can_be_paired_with_lifecycle_state() {
        // Sanity-check that the re-exported `ResourceKind` is hashable
        // alongside `LifecycleState` so the adapter can key its
        // `DashMap<(ResourceKind, String), _>` storage off both.
        let mut set = std::collections::HashSet::new();
        set.insert((ResourceKind::Shell, LifecycleState::Owned));
        set.insert((ResourceKind::Shell, LifecycleState::Owned));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn default_grace_ms_constant_matches_five_seconds() {
        assert_eq!(DEFAULT_GRACE_MS, 5_000);
    }

    #[test]
    fn policy_grace_ms_can_be_zero_for_immediate_release() {
        let mut p = LifecyclePolicy::release_with_default_grace();
        p.grace_ms = 0;
        assert_eq!(p.grace_ms, 0);
    }

    #[test]
    fn policy_grace_ms_supports_long_windows() {
        let mut p = LifecyclePolicy::release_with_default_grace();
        p.grace_ms = u32::MAX;
        assert_eq!(p.grace_ms, u32::MAX);
    }

    #[test]
    fn snapshot_sub_count_zero_in_owned_state_is_legal() {
        let snap = LifecycleSnapshot {
            state: LifecycleState::Owned,
            sub_count: 0,
            grace_remaining_ms: None,
            policy: LifecyclePolicy::default(),
        };
        assert_eq!(snap.sub_count, 0);
    }

    #[test]
    fn snapshot_sub_count_supports_high_subscriber_counts() {
        let snap = LifecycleSnapshot {
            state: LifecycleState::Observed,
            sub_count: 1_024,
            grace_remaining_ms: None,
            policy: LifecyclePolicy::default(),
        };
        assert_eq!(snap.sub_count, 1_024);
    }

    #[test]
    fn state_pretty_serialises_distinct_variants() {
        let owned = serde_json::to_string(&LifecycleState::Owned).expect("ser");
        let observed = serde_json::to_string(&LifecycleState::Observed).expect("ser");
        let releasing = serde_json::to_string(&LifecycleState::Releasing).expect("ser");
        let closed = serde_json::to_string(&LifecycleState::Closed).expect("ser");
        let mut sorted = vec![owned, observed, releasing, closed];
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 4, "every variant must serialise to a distinct token");
    }

    #[test]
    fn snapshot_with_all_fields_deserialises_strictly() {
        let raw = "{\"state\":\"releasing\",\"sub_count\":2,\"grace_remaining_ms\":1500,\"policy\":{\"release_when_no_subs\":true,\"grace_ms\":5000,\"cascade_session\":false}}";
        let parsed: LifecycleSnapshot = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.state, LifecycleState::Releasing);
        assert_eq!(parsed.sub_count, 2);
        assert_eq!(parsed.grace_remaining_ms, Some(1_500));
        assert!(parsed.policy.release_when_no_subs);
    }

    #[test]
    fn lifecycle_state_implements_debug_for_logging() {
        // Touching Debug satisfies the `missing_debug_implementations`
        // expectation and lets the adapter emit informative tracing
        // output during transitions.
        let s = LifecycleState::Observed;
        let dbg = format!("{s:?}");
        assert!(dbg.contains("Observed"));
    }

    #[test]
    fn lifecycle_policy_implements_debug_for_logging() {
        let p = LifecyclePolicy::default();
        let dbg = format!("{p:?}");
        assert!(dbg.contains("LifecyclePolicy"));
    }

    #[test]
    fn lifecycle_snapshot_implements_debug_for_logging() {
        let snap = LifecycleSnapshot {
            state: LifecycleState::Owned,
            sub_count: 0,
            grace_remaining_ms: None,
            policy: LifecyclePolicy::default(),
        };
        let dbg = format!("{snap:?}");
        assert!(dbg.contains("LifecycleSnapshot"));
    }

    #[test]
    fn state_ordering_via_as_u8_is_strictly_monotonic() {
        // Used by tracing assertions: `state_old.as_u8() <= state_new.as_u8()`
        // is NOT a guaranteed monotonic transition (Releasing -> Observed
        // moves backwards), but the byte tags themselves must be a totally
        // ordered set so the AtomicU8 storage can use compare_exchange
        // without ambiguity.
        let mut tags = vec![
            LifecycleState::Owned.as_u8(),
            LifecycleState::Observed.as_u8(),
            LifecycleState::Releasing.as_u8(),
            LifecycleState::Closed.as_u8(),
        ];
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), 4);
    }

    #[test]
    fn policy_clone_creates_independent_copy() {
        let original = LifecyclePolicy::release_with_default_grace();
        let mut twin = original;
        twin.grace_ms = 10_000;
        assert_eq!(original.grace_ms, super::DEFAULT_GRACE_MS);
        assert_eq!(twin.grace_ms, 10_000);
    }

    #[test]
    fn snapshot_clone_creates_independent_copy() {
        let original = LifecycleSnapshot {
            state: LifecycleState::Releasing,
            sub_count: 1,
            grace_remaining_ms: Some(500),
            policy: LifecyclePolicy::default(),
        };
        let twin = original;
        assert_eq!(original, twin);
    }

    #[test]
    fn resource_kind_re_export_matches_subscriber_registry_enum() {
        // Documents the architectural decision: the lifecycle layer
        // reuses `ResourceKind` from the subscription port surface
        // instead of declaring a parallel enum. Compile-time check only.
        let _shell: ResourceKind = ResourceKind::Shell;
        let _command: ResourceKind = ResourceKind::Command;
        let _transfer: ResourceKind = ResourceKind::Transfer;
        let _forward: ResourceKind = ResourceKind::Forward;
        let _session: ResourceKind = ResourceKind::Session;
    }

    #[test]
    fn snapshot_grace_remaining_ms_is_optional_u32() {
        // The tunable grace window fits in u32 (max ~49 days), enough
        // for any realistic lifecycle TTL while keeping the snapshot
        // compact when serialised over JSON.
        let snap = LifecycleSnapshot {
            state: LifecycleState::Releasing,
            sub_count: 0,
            grace_remaining_ms: Some(u32::MAX),
            policy: LifecyclePolicy::release_with_default_grace(),
        };
        assert_eq!(snap.grace_remaining_ms, Some(u32::MAX));
    }
}
