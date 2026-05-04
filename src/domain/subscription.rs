//! Subscription primitives for the v5 channel-mux layer (Phase 2).
//!
//! v4 keys subscriber state on `(PeerId, Uri)` pairs — at most one
//! logical subscription per peer per resource. v5 introduces a
//! per-call [`SubId`] (`UUIDv7`) so a single peer can fan out to N
//! independent consumers (different filters, lag policies,
//! lifetimes), with self-attributing stats.
//!
//! This module is **pure data**: no `tokio`, no `Arc`, no `Atomic*`.
//! The atomic counters that drive the live lane state live in the
//! adapter (`crate::adapters::subscription::subscriber_lane`); the
//! [`SubscriberStats`] struct here is the read-only snapshot returned
//! by the port surface.
//!
//! See [ADR 0004](../docs/adr/0004-channel-mux-fairness.md) and
//! [ADR 0006](../docs/adr/0006-backpressure-policies.md) for the spec.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Stable, dyn-safe subscriber identifier minted at subscribe time.
///
/// Wraps a `UUIDv7` so identifiers are time-ordered (helpful for
/// closest-match `SUB_NOT_FOUND` suggestions and audit logs). The
/// adapter mints the value via the [`crate::ports::id_generator::IdGeneratorPort`]
/// surface — the domain layer never imports `uuid`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct SubId(String);

impl SubId {
    /// Wrap a raw identifier string. The caller is responsible for
    /// supplying a `UUIDv7` — production wiring goes through the
    /// [`crate::ports::id_generator::IdGeneratorPort`]; tests can
    /// use any opaque string.
    #[must_use]
    pub const fn new(id: String) -> Self {
        Self(id)
    }

    /// Borrow the underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the wrapper and return the owned [`String`].
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for SubId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Backpressure strategy applied to a per-[`SubId`] lane when its
/// mpsc buffer fills.
///
/// Default is [`Self::Snapshot`] — drop the backlog and rebuild the
/// next push from the resource ring buffer.
///
/// See ADR 0006 for the full decision matrix.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LagPolicy {
    /// Producer awaits `mpsc::Sender::send` until the consumer drains.
    /// Zero loss; latency cost grows with lag. Use for audit-class
    /// workloads where every event matters.
    BlockSlow,
    /// Pop the oldest event from the lane, push the new event. Emit
    /// a `lagged` marker. Use for monitoring with gap tolerance.
    DropOldest,
    /// Ignore the new event when the lane is full. Emit a `lagged`
    /// marker. Use when historical context is more valuable than
    /// keeping up with current production.
    DropNewest,
    /// (Default.) Drop the lane backlog and rebuild from the ring
    /// buffer on the next event. Zero loss as long as the ring buffer
    /// covers the gap; otherwise emits a `LAG_DETECTED` warning.
    #[default]
    Snapshot,
}

impl LagPolicy {
    /// Stable wire string for the policy. Mirrors the serde
    /// representation but is callable from `const` contexts so the
    /// adapter can render markdown body lines without an allocation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BlockSlow => "block_slow",
            Self::DropOldest => "drop_oldest",
            Self::DropNewest => "drop_newest",
            Self::Snapshot => "snapshot",
        }
    }

    /// Parse the wire string back into a [`LagPolicy`]. Returns
    /// `None` for any value not in the enum.
    #[must_use]
    pub fn from_str_opt(value: &str) -> Option<Self> {
        match value {
            "block_slow" => Some(Self::BlockSlow),
            "drop_oldest" => Some(Self::DropOldest),
            "drop_newest" => Some(Self::DropNewest),
            "snapshot" => Some(Self::Snapshot),
            _ => None,
        }
    }
}

/// Log-level filter for a lane.
///
/// Placeholder for Phase 3 wiring; only the [`FilterRule::Regex`]
/// arm is honoured by the Phase 2 adapter, but the variant exists
/// so the wire surface stays stable across phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Filter to events tagged at warn or above.
    Warn,
    /// Filter to events tagged at error.
    Error,
    /// Filter to events tagged at info or above.
    Info,
    /// Filter to events tagged at debug or above.
    Debug,
}

/// Per-lane filter definition. Phase 2 only honours [`Self::Regex`];
/// [`Self::Level`] is reserved for Phase 3 once events carry a
/// structured level field.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FilterRule {
    /// Forward every event (default).
    #[default]
    None,
    /// Forward events whose payload matches the regex.
    Regex(String),
    /// Forward events at-or-above the level (Phase 3 wiring).
    Level(LogLevel),
}

/// Lifetime knob applied to a lane.
///
/// [`Self::Manual`] mirrors v4 behaviour: the lane lives until the
/// consumer explicitly unsubscribes. [`Self::AutoClose`] ties the
/// lane lifetime to a grace window measured from the last consumer
/// interaction. [`Self::Lease`] caps the absolute lifetime regardless
/// of activity (Phase 3 wiring).
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SubscriptionLifetime {
    /// Default: lane stays open until explicit unsubscribe.
    #[default]
    Manual,
    /// Auto-close after `grace_ms` of consumer inactivity.
    AutoClose {
        /// Grace window in milliseconds.
        grace_ms: u32,
    },
    /// Hard cap on lane lifetime regardless of activity.
    Lease {
        /// TTL in seconds.
        ttl_secs: u32,
    },
}

/// Read-only snapshot of per-lane delivery statistics.
///
/// Each field corresponds to an atomic counter living in the adapter
/// — see [ADR 0006](../docs/adr/0006-backpressure-policies.md) for the
/// increment semantics. The snapshot itself is plain data so it can be
/// serialised straight onto the wire surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
pub struct SubscriberStats {
    /// Total push events delivered on the lane.
    pub events_sent: u64,
    /// Total payload bytes delivered.
    pub bytes_sent: u64,
    /// Events dropped under [`LagPolicy::DropOldest`] /
    /// [`LagPolicy::DropNewest`].
    pub lagged_drops: u64,
    /// [`LagPolicy::Snapshot`] rebuilds completed.
    pub lagged_recoveries: u64,
    /// Current depth of the lane mpsc (sampled).
    pub queue_depth: usize,
    /// Maximum observed depth.
    pub queue_high_watermark: usize,
    /// Cumulative time spent blocked in [`LagPolicy::BlockSlow`].
    pub block_total_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::{
        FilterRule, LagPolicy, LogLevel, SubId, SubscriberStats, SubscriptionLifetime,
    };

    // --- SubId ------------------------------------------------------

    #[test]
    fn sub_id_round_trips_via_serde() {
        let id = SubId::new("019028a3-0000-7abc-8def-000000000001".to_string());
        let json = serde_json::to_string(&id).expect("ser");
        let back: SubId = serde_json::from_str(&json).expect("de");
        assert_eq!(back, id);
    }

    #[test]
    fn sub_id_serde_is_transparent_string() {
        let id = SubId::new("abc".to_string());
        let json = serde_json::to_string(&id).expect("ser");
        assert_eq!(json, "\"abc\"");
    }

    #[test]
    fn sub_id_as_str_borrows_inner() {
        let id = SubId::new("xyz".to_string());
        assert_eq!(id.as_str(), "xyz");
    }

    #[test]
    fn sub_id_into_inner_consumes() {
        let id = SubId::new("zzz".to_string());
        let owned = id.into_inner();
        assert_eq!(owned, "zzz");
    }

    #[test]
    fn sub_id_display_renders_inner_string() {
        let id = SubId::new("disp".to_string());
        assert_eq!(format!("{id}"), "disp");
    }

    #[test]
    fn sub_id_is_hashable_for_dashmap_keys() {
        let mut set = std::collections::HashSet::new();
        set.insert(SubId::new("a".to_string()));
        set.insert(SubId::new("a".to_string()));
        set.insert(SubId::new("b".to_string()));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn sub_id_clone_preserves_value() {
        let id = SubId::new("clone".to_string());
        let twin = id.clone();
        assert_eq!(id, twin);
    }

    #[test]
    fn sub_id_debug_includes_value() {
        let id = SubId::new("dbg".to_string());
        let rendered = format!("{id:?}");
        assert!(rendered.contains("dbg"), "{rendered}");
    }

    // --- LagPolicy --------------------------------------------------

    #[test]
    fn lag_policy_default_is_snapshot() {
        assert_eq!(LagPolicy::default(), LagPolicy::Snapshot);
    }

    #[test]
    fn lag_policy_serde_uses_snake_case_strings() {
        for (variant, wire) in [
            (LagPolicy::BlockSlow, "\"block_slow\""),
            (LagPolicy::DropOldest, "\"drop_oldest\""),
            (LagPolicy::DropNewest, "\"drop_newest\""),
            (LagPolicy::Snapshot, "\"snapshot\""),
        ] {
            let json = serde_json::to_string(&variant).expect("ser");
            assert_eq!(json, wire, "variant {variant:?}");
        }
    }

    #[test]
    fn lag_policy_round_trips_via_serde() {
        for variant in [
            LagPolicy::BlockSlow,
            LagPolicy::DropOldest,
            LagPolicy::DropNewest,
            LagPolicy::Snapshot,
        ] {
            let json = serde_json::to_string(&variant).expect("ser");
            let back: LagPolicy = serde_json::from_str(&json).expect("de");
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn lag_policy_as_str_matches_wire_form() {
        assert_eq!(LagPolicy::BlockSlow.as_str(), "block_slow");
        assert_eq!(LagPolicy::DropOldest.as_str(), "drop_oldest");
        assert_eq!(LagPolicy::DropNewest.as_str(), "drop_newest");
        assert_eq!(LagPolicy::Snapshot.as_str(), "snapshot");
    }

    #[test]
    fn lag_policy_from_str_opt_round_trips_every_variant() {
        for variant in [
            LagPolicy::BlockSlow,
            LagPolicy::DropOldest,
            LagPolicy::DropNewest,
            LagPolicy::Snapshot,
        ] {
            let parsed = LagPolicy::from_str_opt(variant.as_str()).expect("parse");
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn lag_policy_from_str_opt_rejects_unknown_strings() {
        assert!(LagPolicy::from_str_opt("nope").is_none());
        assert!(LagPolicy::from_str_opt("BlockSlow").is_none());
        assert!(LagPolicy::from_str_opt("").is_none());
    }

    #[test]
    fn lag_policy_is_copy_and_hashable() {
        let p = LagPolicy::DropOldest;
        let q = p;
        assert_eq!(p, q);
        let mut set = std::collections::HashSet::new();
        set.insert(LagPolicy::Snapshot);
        set.insert(LagPolicy::Snapshot);
        assert_eq!(set.len(), 1);
    }

    // --- FilterRule -------------------------------------------------

    #[test]
    fn filter_rule_default_is_none() {
        assert_eq!(FilterRule::default(), FilterRule::None);
    }

    #[test]
    fn filter_rule_none_round_trips() {
        let rule = FilterRule::None;
        let json = serde_json::to_string(&rule).expect("ser");
        let back: FilterRule = serde_json::from_str(&json).expect("de");
        assert_eq!(back, rule);
    }

    #[test]
    fn filter_rule_regex_round_trips() {
        let rule = FilterRule::Regex("ERROR.*".to_string());
        let json = serde_json::to_string(&rule).expect("ser");
        let back: FilterRule = serde_json::from_str(&json).expect("de");
        assert_eq!(back, rule);
    }

    #[test]
    fn filter_rule_level_round_trips() {
        for level in [
            LogLevel::Warn,
            LogLevel::Error,
            LogLevel::Info,
            LogLevel::Debug,
        ] {
            let rule = FilterRule::Level(level);
            let json = serde_json::to_string(&rule).expect("ser");
            let back: FilterRule = serde_json::from_str(&json).expect("de");
            assert_eq!(back, rule);
        }
    }

    // --- SubscriptionLifetime --------------------------------------

    #[test]
    fn subscription_lifetime_default_is_manual() {
        assert_eq!(SubscriptionLifetime::default(), SubscriptionLifetime::Manual);
    }

    #[test]
    fn subscription_lifetime_manual_serde() {
        let lt = SubscriptionLifetime::Manual;
        let json = serde_json::to_string(&lt).expect("ser");
        let back: SubscriptionLifetime = serde_json::from_str(&json).expect("de");
        assert_eq!(back, lt);
    }

    #[test]
    fn subscription_lifetime_auto_close_serde() {
        let lt = SubscriptionLifetime::AutoClose { grace_ms: 5_000 };
        let json = serde_json::to_string(&lt).expect("ser");
        let back: SubscriptionLifetime = serde_json::from_str(&json).expect("de");
        assert_eq!(back, lt);
    }

    #[test]
    fn subscription_lifetime_lease_serde() {
        let lt = SubscriptionLifetime::Lease { ttl_secs: 60 };
        let json = serde_json::to_string(&lt).expect("ser");
        let back: SubscriptionLifetime = serde_json::from_str(&json).expect("de");
        assert_eq!(back, lt);
    }

    #[test]
    fn subscription_lifetime_carries_grace_ms_field_in_json() {
        let lt = SubscriptionLifetime::AutoClose { grace_ms: 1_500 };
        let json = serde_json::to_string(&lt).expect("ser");
        assert!(
            json.contains("\"grace_ms\":1500"),
            "missing grace_ms field: {json}"
        );
    }

    #[test]
    fn subscription_lifetime_is_copy_and_hashable() {
        let lt = SubscriptionLifetime::AutoClose { grace_ms: 1_000 };
        let twin = lt;
        assert_eq!(lt, twin);
        let mut set = std::collections::HashSet::new();
        set.insert(SubscriptionLifetime::Manual);
        set.insert(SubscriptionLifetime::Manual);
        assert_eq!(set.len(), 1);
    }

    // --- SubscriberStats -------------------------------------------

    #[test]
    fn subscriber_stats_default_is_all_zero() {
        let s = SubscriberStats::default();
        assert_eq!(s.events_sent, 0);
        assert_eq!(s.bytes_sent, 0);
        assert_eq!(s.lagged_drops, 0);
        assert_eq!(s.lagged_recoveries, 0);
        assert_eq!(s.queue_depth, 0);
        assert_eq!(s.queue_high_watermark, 0);
        assert_eq!(s.block_total_ms, 0);
    }

    #[test]
    fn subscriber_stats_round_trips_via_serde() {
        let s = SubscriberStats {
            events_sent: 100,
            bytes_sent: 4_096,
            lagged_drops: 3,
            lagged_recoveries: 1,
            queue_depth: 7,
            queue_high_watermark: 12,
            block_total_ms: 250,
        };
        let json = serde_json::to_string(&s).expect("ser");
        let back: SubscriberStats = serde_json::from_str(&json).expect("de");
        assert_eq!(back, s);
    }

    #[test]
    fn subscriber_stats_is_copy_and_hashable() {
        let s = SubscriberStats::default();
        let twin = s;
        assert_eq!(s, twin);
    }

    // --- JsonSchema sanity -----------------------------------------

    #[test]
    fn lag_policy_advertises_schema() {
        let schema = schemars::schema_for!(LagPolicy);
        let s = serde_json::to_string(&schema).expect("ser");
        assert!(s.contains("snapshot"), "schema must include snapshot variant");
    }

    #[test]
    fn sub_id_advertises_schema() {
        let schema = schemars::schema_for!(SubId);
        let s = serde_json::to_string(&schema).expect("ser");
        assert!(!s.is_empty());
    }

    #[test]
    fn subscription_lifetime_advertises_schema() {
        let schema = schemars::schema_for!(SubscriptionLifetime);
        let s = serde_json::to_string(&schema).expect("ser");
        assert!(s.contains("manual"));
    }
}
