//! Per-`SubId` lane port (v5 Phase 2 — Channel Mux + `SubId`).
//!
//! Use cases consume this surface to register, mutate, and inspect a
//! lane that fans events to a single MCP subscriber. The port is
//! split into a sync, dyn-safe slice ([`SubscriberLanePort`]) for
//! read-only stat / cursor / list calls and an async slice
//! ([`SubscriberLaneAsync`]) for mutations that may interact with
//! the runtime (lane creation, regex compile, replay).
//!
//! See [ADR 0004](../docs/adr/0004-channel-mux-fairness.md) for the
//! full design.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::subscription::{
    FilterRule, LagPolicy, SubId, SubscriberStats, SubscriptionLifetime,
};
use crate::ports::notifier::PeerHandle;
use crate::ports::subscriber_registry::ResourceKind;

/// Boxed future returned by [`LaneAdmin`] methods. Aliased so the
/// trait surface stays compact and the absolute-path lint stays
/// happy.
pub type LaneFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Caller-controllable knobs that flow into the lane at registration
/// time. Phase 3 expands the surface with auth-class metadata; Phase
/// 2 keeps the struct minimal so the new wire surface stays stable.
#[derive(Debug, Default, Clone)]
pub struct LanePolicy {
    /// Backpressure strategy when the lane mpsc fills.
    pub lag_policy: LagPolicy,
    /// Lifetime model.
    pub lifetime: SubscriptionLifetime,
    /// Filter pipeline.
    pub filter: FilterRule,
    /// Per-lane mpsc capacity. `0` means "use the configured default".
    pub buffer_size: usize,
    /// Optional rmcp peer the lane should notify on producer events.
    /// `None` for transports without a peer concept (NDJSON daemon).
    pub peer: Option<Arc<dyn PeerHandle>>,
}

/// Read-only summary for `sub_list` / `sub_stats_all`.
#[derive(Debug, Clone)]
pub struct SubSummary {
    /// Lane id.
    pub sub_id: SubId,
    /// Resource scheme.
    pub kind: ResourceKind,
    /// Resource id portion of the URI.
    pub resource_id: String,
    /// Canonical URI.
    pub uri: String,
    /// Backpressure strategy.
    pub lag_policy: LagPolicy,
    /// Lifetime model.
    pub lifetime: SubscriptionLifetime,
    /// Whether the lane is currently paused.
    pub paused: bool,
    /// Live stats snapshot.
    pub stats: SubscriberStats,
}

/// ADR 0012 phase 5 -- inline-push counters bundle returned by
/// [`LaneAdmin::inline_stats`].
///
/// Surfaces the lane opt-in flag together with the matching atomic
/// counters so `sub_stats` can render the v7.1 inline lines (and
/// keep the structured-content twin in sync) without exposing the
/// underlying `LaneState` to higher layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlineLaneCounters {
    /// Whether the lane has the inline-push gate flipped on.
    pub inline_push: bool,
    /// Cumulative inline notifications delivered to this lane.
    pub inline_events_sent: u64,
    /// Cumulative raw inline bytes delivered, pre-base64.
    pub inline_bytes_sent: u64,
}

/// Sync slice of the per-`SubId` lane registry.
pub trait SubscriberLanePort: Send + Sync + 'static {
    /// Read-only stats snapshot for `sub_id`. `None` when the lane
    /// does not exist.
    fn stats_snapshot(&self, sub_id: &SubId) -> Option<SubscriberStats>;

    /// Current byte cursor for the lane. Returns `0` when the lane
    /// does not exist.
    fn current_cursor(&self, sub_id: &SubId, uri: &str) -> u64;

    /// Advance the per-lane byte cursor to at least `target` (atomic
    /// max). Returns the cursor value AFTER the bump.
    fn advance_cursor(&self, sub_id: &SubId, uri: &str, target: u64) -> u64;

    /// List every active lane.
    fn list_subs(&self) -> Vec<SubSummary>;
}

/// Cold-path, dyn-safe shim for lane administration calls.
///
/// Used by the subscribe / unsubscribe use cases AND the v5 Phase 3
/// `ssh_sub_*` tools. Methods box their futures so the use cases can
/// hold a single `Arc<dyn LaneAdmin>` without caring about the
/// concrete adapter type.
///
/// This trait is **not** for the hot path — Phase 2 producers reach
/// the lane through the concrete [`SubscriberLanePort`] +
/// [`SubscriberLaneAsync`] surface to keep the per-event path
/// virtual-call free.
pub trait LaneAdmin: fmt::Debug + Send + Sync + 'static {
    /// Open a lane and return its [`SubId`]. Boxed for dyn dispatch.
    fn open(
        &self,
        uri: String,
        kind: ResourceKind,
        resource_id: String,
        policy: LanePolicy,
    ) -> LaneFuture<'_, Result<SubId, DomainError>>;

    /// Close a lane.
    fn close<'a>(&'a self, sub_id: &'a SubId) -> LaneFuture<'a, Result<(), DomainError>>;

    /// Pause drain on `sub_id`. Subsequent events accumulate in the
    /// mpsc until [`Self::resume`].
    fn pause<'a>(&'a self, sub_id: &'a SubId) -> LaneFuture<'a, Result<(), DomainError>>;

    /// Resume drain on `sub_id`.
    fn resume<'a>(&'a self, sub_id: &'a SubId) -> LaneFuture<'a, Result<(), DomainError>>;

    /// Hot-reload the lane filter.
    fn set_filter<'a>(
        &'a self,
        sub_id: &'a SubId,
        filter: FilterRule,
    ) -> LaneFuture<'a, Result<(), DomainError>>;

    /// Re-emit the lane buffer starting at `cursor` (within the
    /// adapter's replay window).
    fn replay<'a>(
        &'a self,
        sub_id: &'a SubId,
        cursor: u64,
    ) -> LaneFuture<'a, Result<(), DomainError>>;

    /// Snapshot per-lane statistics. Returns `None` for unknown lanes.
    fn stats(&self, sub_id: &SubId) -> Option<SubscriberStats>;

    /// Read-only summary list for `sub_list`.
    fn list(&self) -> Vec<SubSummary>;

    /// ADR 0012 phase 5 -- toggle the inline-push opt-in gate on the
    /// lane identified by `sub_id`. Returns `false` when the lane is
    /// unknown (silent no-op); `true` when the gate was flipped.
    /// Cold-path -- runs once per `sub_open`, never on the producer
    /// path. Sync because the underlying atomic store needs no
    /// `.await`.
    fn set_inline_push(&self, sub_id: &SubId, enabled: bool) -> bool;

    /// ADR 0012 phase 5 -- read the inline counters bundle for
    /// `sub_id`. Returns `None` for unknown lanes. The `inline_push`
    /// flag is `true` iff the lane opted in AND the capability was
    /// honoured at `sub_open` time.
    fn inline_stats(&self, sub_id: &SubId) -> Option<InlineLaneCounters>;
}

/// Async slice of the per-`SubId` lane registry.
#[trait_variant::make(SubscriberLaneAsync: Send)]
pub trait LocalSubscriberLaneAsync: Sync {
    /// Register a fresh lane.
    ///
    /// # Errors
    ///
    /// - [`DomainError::MaxSubsPerUriExceeded`] when the per-URI cap
    ///   is reached.
    /// - [`DomainError::MaxSubsTotalExceeded`] when the global cap is
    ///   reached.
    /// - [`DomainError::InvalidLagPolicy`] / [`DomainError::InvalidLifetime`]
    ///   when validation fails (regex compile, etc).
    async fn open_lane(
        &self,
        uri: String,
        kind: ResourceKind,
        resource_id: String,
        policy: LanePolicy,
    ) -> Result<SubId, DomainError>;

    /// Close `sub_id`. Idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::SubNotFound`] when the lane does not
    /// exist (so callers can distinguish "already closed" from
    /// "never existed" if they care).
    async fn close_lane(&self, sub_id: &SubId) -> Result<(), DomainError>;

    /// Pause drain on `sub_id`. Subsequent events accumulate in the
    /// mpsc until [`Self::resume_lane`].
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::SubNotFound`] for unknown lanes.
    async fn pause_lane(&self, sub_id: &SubId) -> Result<(), DomainError>;

    /// Resume drain on `sub_id`.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::SubNotFound`] for unknown lanes.
    async fn resume_lane(&self, sub_id: &SubId) -> Result<(), DomainError>;

    /// Hot-reload the lane filter.
    ///
    /// # Errors
    ///
    /// - [`DomainError::SubNotFound`] for unknown lanes.
    /// - [`DomainError::InvalidArgument`] when the regex fails to
    ///   compile.
    async fn set_filter(&self, sub_id: &SubId, filter: FilterRule) -> Result<(), DomainError>;

    /// Re-emit the lane buffer starting at `cursor` (within the
    /// adapter's replay window).
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::SubNotFound`] for unknown lanes.
    async fn replay_from_cursor(&self, sub_id: &SubId, cursor: u64) -> Result<(), DomainError>;
}

#[cfg(test)]
mod tests {
    use super::{LanePolicy, SubSummary, SubscriberLaneAsync, SubscriberLanePort};
    use crate::domain::subscription::{
        FilterRule, LagPolicy, SubId, SubscriberStats, SubscriptionLifetime,
    };
    use crate::ports::subscriber_registry::ResourceKind;

    fn _assert_sync_dyn_safe(_p: &dyn SubscriberLanePort) {}

    fn _assert_async_port<T: SubscriberLaneAsync>() {}

    #[test]
    fn lane_policy_default_uses_snapshot_and_manual() {
        let p = LanePolicy::default();
        assert_eq!(p.lag_policy, LagPolicy::Snapshot);
        assert_eq!(p.lifetime, SubscriptionLifetime::Manual);
        assert_eq!(p.filter, FilterRule::None);
        assert_eq!(p.buffer_size, 0);
    }

    #[test]
    fn sub_summary_can_be_constructed_for_rendering() {
        let summary = SubSummary {
            sub_id: SubId::new("s".to_string()),
            kind: ResourceKind::Shell,
            resource_id: "sh-1".to_string(),
            uri: "shell://sh-1/output".to_string(),
            lag_policy: LagPolicy::Snapshot,
            lifetime: SubscriptionLifetime::Manual,
            paused: false,
            stats: SubscriberStats::default(),
        };
        assert_eq!(summary.sub_id.as_str(), "s");
        assert_eq!(summary.kind, ResourceKind::Shell);
        assert!(!summary.paused);
    }
}
