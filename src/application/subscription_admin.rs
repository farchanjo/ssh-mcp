//! v5 Phase 3 subscription-administration use cases.
//!
//! Nine cold-path use cases that drive the Channel Mux / per-`SubId`
//! lane registry exposed by the [`LaneAdmin`] port. Each use case is a
//! small one-method orchestration over [`LaneAdmin`] (and, for
//! `Subscribe`, the URI parser from
//! [`crate::application::read_resource`]).
//!
//! The use cases intentionally bypass the peer-keyed
//! [`crate::ports::subscriber_registry::SubscriberRegistryAsync`] —
//! these tools manage lanes directly, returning the [`SubId`] for the
//! caller to drive instead of attaching the lane to a transport peer.
//! That keeps the wiring decoupled from `Mcp-Session-Id` and lets the
//! NDJSON daemon transport (Phase 4) consume the lanes through the
//! channel mux outbound sink.

use std::sync::Arc;

use crate::application::read_resource::{canonical_uri, parse_uri};
use crate::domain::error::DomainError;
use crate::domain::subscription::{
    FilterRule, LagPolicy, SubId, SubscriberStats, SubscriptionLifetime,
};
use crate::ports::subscriber_lane::{LaneAdmin, LanePolicy, SubSummary};
use crate::ports::subscriber_registry::ResourceKind;

// ---------------------------------------------------------------------------
// Subscribe
// ---------------------------------------------------------------------------

/// Inbound DTO for `ssh_subscribe`.
#[derive(Debug, Clone)]
pub struct SubscribeRequest {
    /// Caller-supplied resource URI.
    pub uri: String,
    /// Lifetime descriptor.
    pub lifetime: SubscriptionLifetime,
    /// Backpressure strategy.
    pub lag_policy: LagPolicy,
    /// Filter rule (regex / level / none).
    pub filter: FilterRule,
}

/// Outbound DTO for `ssh_subscribe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscribeOutcome {
    /// Newly minted lane id.
    pub sub_id: SubId,
    /// Canonical URI keyed against the lane.
    pub uri: String,
    /// Lifetime descriptor (echoed back to the caller for confirmation).
    pub lifetime: SubscriptionLifetime,
    /// Active lag policy.
    pub lag_policy: LagPolicy,
    /// Resolved grace window in ms (0 when not applicable).
    pub grace_ms: u32,
}

/// `ssh_subscribe` use case.
#[derive(Debug, Clone)]
pub struct SubscribeUseCase {
    lane: Arc<dyn LaneAdmin>,
}

impl SubscribeUseCase {
    /// Build the use case with the supplied lane handle.
    #[must_use]
    pub const fn new(lane: Arc<dyn LaneAdmin>) -> Self {
        Self { lane }
    }

    /// Drive the orchestration.
    ///
    /// # Errors
    ///
    /// - [`DomainError::InvalidArgument`] — URI fails to parse.
    /// - [`DomainError::MaxSubsPerUriExceeded`] / [`DomainError::MaxSubsTotalExceeded`]
    ///   — caps reached.
    /// - [`DomainError::InvalidArgument`] — regex fails to compile.
    pub async fn execute(&self, req: SubscribeRequest) -> Result<SubscribeOutcome, DomainError> {
        let parsed =
            parse_uri(&req.uri).map_err(|e| DomainError::InvalidArgument(e.to_string()))?;
        let canonical = canonical_uri(parsed.kind, &parsed.id);
        let policy = LanePolicy {
            lag_policy: req.lag_policy,
            lifetime: req.lifetime,
            filter: req.filter,
            buffer_size: 0,
        };
        let sub_id = self
            .lane
            .open(canonical.clone(), parsed.kind, parsed.id, policy)
            .await?;
        Ok(SubscribeOutcome {
            sub_id,
            uri: canonical,
            lifetime: req.lifetime,
            lag_policy: req.lag_policy,
            grace_ms: grace_ms_from(req.lifetime),
        })
    }
}

const fn grace_ms_from(lifetime: SubscriptionLifetime) -> u32 {
    match lifetime {
        SubscriptionLifetime::AutoClose { grace_ms } => grace_ms,
        SubscriptionLifetime::Manual | SubscriptionLifetime::Lease { .. } => 0,
    }
}

// ---------------------------------------------------------------------------
// Unsubscribe
// ---------------------------------------------------------------------------

/// Inbound DTO for `ssh_unsubscribe`.
#[derive(Debug, Clone)]
pub struct UnsubscribeRequest {
    /// `SubId` to close.
    pub sub_id: SubId,
}

/// Outbound DTO for `ssh_unsubscribe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsubscribeOutcome {
    /// Closed lane id.
    pub sub_id: SubId,
    /// URI the lane was bound to (when known — `None` for unknown lanes).
    pub uri: Option<String>,
    /// Lifecycle state observed at close time. Currently always
    /// `closed` after a successful close; reserved for richer
    /// lifecycle integrations.
    pub lifecycle_state: &'static str,
    /// Grace remaining in ms, when the lane was closed before its
    /// grace timer fired. `None` for plain manual closes.
    pub grace_remaining_ms: Option<u32>,
}

/// `ssh_unsubscribe` use case.
#[derive(Debug, Clone)]
pub struct UnsubscribeUseCase {
    lane: Arc<dyn LaneAdmin>,
}

impl UnsubscribeUseCase {
    /// Build the use case with the supplied lane handle.
    #[must_use]
    pub const fn new(lane: Arc<dyn LaneAdmin>) -> Self {
        Self { lane }
    }

    /// Drive the orchestration. Returns [`DomainError::SubNotFound`] for
    /// unknown lanes — callers may map that to a soft success if desired.
    ///
    /// # Errors
    ///
    /// - [`DomainError::SubNotFound`] when the lane has already closed
    ///   (e.g. lifetime auto-close fired first).
    pub async fn execute(
        &self,
        req: UnsubscribeRequest,
    ) -> Result<UnsubscribeOutcome, DomainError> {
        // Capture the URI before close so the outcome stays informative
        // for closed lanes too. Falls back to `None` when the lane is
        // already gone.
        let uri = self
            .lane
            .list()
            .into_iter()
            .find(|s| s.sub_id == req.sub_id)
            .map(|s| s.uri);
        self.lane.close(&req.sub_id).await?;
        Ok(UnsubscribeOutcome {
            sub_id: req.sub_id,
            uri,
            lifecycle_state: "closed",
            grace_remaining_ms: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Pause / Resume
// ---------------------------------------------------------------------------

/// Inbound DTO for `ssh_sub_pause` / `ssh_sub_resume`.
#[derive(Debug, Clone)]
pub struct SubToggleRequest {
    /// Lane id to mutate.
    pub sub_id: SubId,
}

/// Outbound DTO for pause/resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubToggleOutcome {
    /// Affected lane id.
    pub sub_id: SubId,
    /// New paused flag.
    pub paused: bool,
}

/// `ssh_sub_pause` use case.
#[derive(Debug, Clone)]
pub struct PauseSubUseCase {
    lane: Arc<dyn LaneAdmin>,
}

impl PauseSubUseCase {
    /// Build the use case.
    #[must_use]
    pub const fn new(lane: Arc<dyn LaneAdmin>) -> Self {
        Self { lane }
    }

    /// Pause the lane.
    ///
    /// # Errors
    ///
    /// [`DomainError::SubNotFound`] for unknown lanes.
    pub async fn execute(&self, req: SubToggleRequest) -> Result<SubToggleOutcome, DomainError> {
        self.lane.pause(&req.sub_id).await?;
        Ok(SubToggleOutcome {
            sub_id: req.sub_id,
            paused: true,
        })
    }
}

/// `ssh_sub_resume` use case.
#[derive(Debug, Clone)]
pub struct ResumeSubUseCase {
    lane: Arc<dyn LaneAdmin>,
}

impl ResumeSubUseCase {
    /// Build the use case.
    #[must_use]
    pub const fn new(lane: Arc<dyn LaneAdmin>) -> Self {
        Self { lane }
    }

    /// Resume the lane.
    ///
    /// # Errors
    ///
    /// [`DomainError::SubNotFound`] for unknown lanes.
    pub async fn execute(&self, req: SubToggleRequest) -> Result<SubToggleOutcome, DomainError> {
        self.lane.resume(&req.sub_id).await?;
        Ok(SubToggleOutcome {
            sub_id: req.sub_id,
            paused: false,
        })
    }
}

// ---------------------------------------------------------------------------
// Set filter
// ---------------------------------------------------------------------------

/// Inbound DTO for `ssh_sub_filter`.
#[derive(Debug, Clone)]
pub struct SetFilterRequest {
    /// Lane id whose filter is changing.
    pub sub_id: SubId,
    /// New filter rule.
    pub filter: FilterRule,
}

/// Outbound DTO for `ssh_sub_filter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetFilterOutcome {
    /// Affected lane id.
    pub sub_id: SubId,
    /// New filter rule.
    pub filter: FilterRule,
}

/// `ssh_sub_filter` use case.
#[derive(Debug, Clone)]
pub struct SetFilterUseCase {
    lane: Arc<dyn LaneAdmin>,
}

impl SetFilterUseCase {
    /// Build the use case.
    #[must_use]
    pub const fn new(lane: Arc<dyn LaneAdmin>) -> Self {
        Self { lane }
    }

    /// Apply the new filter.
    ///
    /// # Errors
    ///
    /// - [`DomainError::SubNotFound`] for unknown lanes.
    /// - [`DomainError::InvalidArgument`] when the regex fails to
    ///   compile.
    pub async fn execute(&self, req: SetFilterRequest) -> Result<SetFilterOutcome, DomainError> {
        self.lane
            .set_filter(&req.sub_id, req.filter.clone())
            .await?;
        Ok(SetFilterOutcome {
            sub_id: req.sub_id,
            filter: req.filter,
        })
    }
}

// ---------------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------------

/// Inbound DTO for `ssh_sub_replay`.
#[derive(Debug, Clone)]
pub struct ReplayRequest {
    /// Lane id to replay.
    pub sub_id: SubId,
    /// Cursor (byte offset) to replay from.
    pub from_cursor: u64,
}

/// Outbound DTO for `ssh_sub_replay`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayOutcome {
    /// Affected lane id.
    pub sub_id: SubId,
    /// Cursor the replay was anchored at.
    pub from_cursor: u64,
}

/// `ssh_sub_replay` use case.
#[derive(Debug, Clone)]
pub struct ReplaySubUseCase {
    lane: Arc<dyn LaneAdmin>,
}

impl ReplaySubUseCase {
    /// Build the use case.
    #[must_use]
    pub const fn new(lane: Arc<dyn LaneAdmin>) -> Self {
        Self { lane }
    }

    /// Trigger the replay.
    ///
    /// # Errors
    ///
    /// [`DomainError::SubNotFound`] for unknown lanes.
    pub async fn execute(&self, req: ReplayRequest) -> Result<ReplayOutcome, DomainError> {
        self.lane.replay(&req.sub_id, req.from_cursor).await?;
        Ok(ReplayOutcome {
            sub_id: req.sub_id,
            from_cursor: req.from_cursor,
        })
    }
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

/// Inbound DTO for `ssh_sub_list`.
#[derive(Debug, Clone, Default)]
pub struct ListSubsRequest {
    /// Optional URI prefix filter (e.g. `shell://`).
    pub uri_prefix: Option<String>,
    /// Optional peer-id filter (currently a no-op — Phase 3 lanes are
    /// not peer-keyed; reserved for future wiring).
    pub peer_id: Option<String>,
}

/// Outbound DTO for `ssh_sub_list`.
#[derive(Debug, Clone)]
pub struct ListSubsOutcome {
    /// Filtered lane summaries.
    pub subs: Vec<SubSummary>,
}

/// `ssh_sub_list` use case.
#[derive(Debug, Clone)]
pub struct ListSubsUseCase {
    lane: Arc<dyn LaneAdmin>,
}

impl ListSubsUseCase {
    /// Build the use case.
    #[must_use]
    pub const fn new(lane: Arc<dyn LaneAdmin>) -> Self {
        Self { lane }
    }

    /// Snapshot the lane registry.
    ///
    /// # Errors
    ///
    /// Currently never fails — the `Result` shape is kept for symmetry
    /// with the other use cases.
    pub fn execute(&self, req: &ListSubsRequest) -> Result<ListSubsOutcome, DomainError> {
        let mut subs = self.lane.list();
        if let Some(prefix) = req.uri_prefix.as_deref() {
            subs.retain(|s| s.uri.starts_with(prefix));
        }
        // peer_id filter is reserved — Phase 3 lanes are not peer-keyed.
        // The argument is accepted (and validated as `Option<String>`) so
        // callers can roll forward into Phase 4 / Phase 5 wiring without
        // a wire-format break.
        let _ = &req.peer_id;
        Ok(ListSubsOutcome { subs })
    }
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

/// Inbound DTO for `ssh_sub_stats`.
#[derive(Debug, Clone)]
pub struct SubStatsRequest {
    /// Lane id to inspect.
    pub sub_id: SubId,
}

/// Outbound DTO for `ssh_sub_stats`.
#[derive(Debug, Clone)]
pub struct SubStatsOutcome {
    /// Affected lane id.
    pub sub_id: SubId,
    /// Stats snapshot.
    pub stats: SubscriberStats,
}

/// `ssh_sub_stats` use case.
#[derive(Debug, Clone)]
pub struct SubStatsUseCase {
    lane: Arc<dyn LaneAdmin>,
}

impl SubStatsUseCase {
    /// Build the use case.
    #[must_use]
    pub const fn new(lane: Arc<dyn LaneAdmin>) -> Self {
        Self { lane }
    }

    /// Snapshot the stats.
    ///
    /// # Errors
    ///
    /// [`DomainError::SubNotFound`] for unknown lanes.
    pub fn execute(&self, req: &SubStatsRequest) -> Result<SubStatsOutcome, DomainError> {
        self.lane.stats(&req.sub_id).map_or_else(
            || Err(DomainError::SubNotFound(req.sub_id.clone())),
            |stats| {
                Ok(SubStatsOutcome {
                    sub_id: req.sub_id.clone(),
                    stats,
                })
            },
        )
    }
}

// ---------------------------------------------------------------------------
// Daemon stats
// ---------------------------------------------------------------------------

/// Outbound DTO for `ssh_daemon_stats`.
#[derive(Debug, Clone, Default)]
pub struct DaemonStatsOutcome {
    /// Total lanes currently open.
    pub lanes_total: usize,
    /// Sum of `events_sent` across every lane.
    pub events_sent_total: u64,
    /// Sum of `bytes_sent` across every lane.
    pub bytes_sent_total: u64,
    /// Sum of `lagged_drops` across every lane.
    pub lagged_drops_total: u64,
    /// Sum of `lagged_recoveries` across every lane.
    pub lagged_recoveries_total: u64,
    /// Highest queue high-watermark observed across every lane.
    pub queue_high_watermark_max: usize,
}

/// `ssh_daemon_stats` use case.
#[derive(Debug, Clone)]
pub struct DaemonStatsUseCase {
    lane: Arc<dyn LaneAdmin>,
}

impl DaemonStatsUseCase {
    /// Build the use case.
    #[must_use]
    pub const fn new(lane: Arc<dyn LaneAdmin>) -> Self {
        Self { lane }
    }

    /// Aggregate per-lane stats.
    ///
    /// # Errors
    ///
    /// Currently never fails.
    pub fn execute(&self) -> Result<DaemonStatsOutcome, DomainError> {
        let summaries = self.lane.list();
        let lanes_total = summaries.len();
        let mut out = DaemonStatsOutcome {
            lanes_total,
            ..DaemonStatsOutcome::default()
        };
        for summary in summaries {
            out.events_sent_total = out
                .events_sent_total
                .saturating_add(summary.stats.events_sent);
            out.bytes_sent_total = out
                .bytes_sent_total
                .saturating_add(summary.stats.bytes_sent);
            out.lagged_drops_total = out
                .lagged_drops_total
                .saturating_add(summary.stats.lagged_drops);
            out.lagged_recoveries_total = out
                .lagged_recoveries_total
                .saturating_add(summary.stats.lagged_recoveries);
            out.queue_high_watermark_max = out
                .queue_high_watermark_max
                .max(summary.stats.queue_high_watermark);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resource-aware view useful when rendering markdown — exposes the
/// `kind` portion of a [`SubSummary`] without leaking the full struct.
#[must_use]
pub const fn summary_kind_str(summary: &SubSummary) -> &'static str {
    match summary.kind {
        ResourceKind::Shell => "shell",
        ResourceKind::Command => "command",
        ResourceKind::Transfer => "transfer",
        ResourceKind::Session => "session",
        ResourceKind::Forward => "forward",
        ResourceKind::Serial => "serial",
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests use unwrap for brevity per CLAUDE.md test policy"
)]
mod tests {
    use super::{
        DaemonStatsUseCase, ListSubsRequest, ListSubsUseCase, PauseSubUseCase, ReplayRequest,
        ReplaySubUseCase, ResumeSubUseCase, SetFilterRequest, SetFilterUseCase, SubStatsRequest,
        SubStatsUseCase, SubToggleRequest, SubscribeRequest, SubscribeUseCase, UnsubscribeRequest,
        UnsubscribeUseCase, summary_kind_str,
    };
    use crate::adapters::id_generator::uuid::UuidIds;
    use crate::adapters::subscription::subscriber_lane::SubscriberLaneAdapter;
    use crate::domain::error::DomainError;
    use crate::domain::subscription::{FilterRule, LagPolicy, SubId, SubscriptionLifetime};
    use crate::ports::subscriber_lane::{
        LaneAdmin, LanePolicy, SubscriberLaneAsync, SubscriberLanePort,
    };
    use crate::ports::subscriber_registry::ResourceKind;
    use std::sync::Arc;

    fn lane_admin() -> Arc<dyn LaneAdmin> {
        let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 16, 8, 64);
        adapter as Arc<dyn LaneAdmin>
    }

    fn lane_admin_concrete() -> Arc<SubscriberLaneAdapter<UuidIds>> {
        SubscriberLaneAdapter::new(Arc::new(UuidIds), 16, 8, 64)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_returns_sub_id_and_canonical_uri() {
        let uc = SubscribeUseCase::new(lane_admin());
        let outcome = uc
            .execute(SubscribeRequest {
                uri: "shell://sh-1/output?cursor=auto".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.uri, "shell://sh-1/output");
        assert!(!outcome.sub_id.as_str().is_empty());
        assert_eq!(outcome.lifetime, SubscriptionLifetime::Manual);
        assert_eq!(outcome.lag_policy, LagPolicy::Snapshot);
        assert_eq!(outcome.grace_ms, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_with_auto_close_carries_grace_ms() {
        let uc = SubscribeUseCase::new(lane_admin());
        let outcome = uc
            .execute(SubscribeRequest {
                uri: "shell://sh-2/output".to_string(),
                lifetime: SubscriptionLifetime::AutoClose { grace_ms: 5_000 },
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.grace_ms, 5_000);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_invalid_uri_returns_invalid_argument() {
        let uc = SubscribeUseCase::new(lane_admin());
        let err = uc
            .execute(SubscribeRequest {
                uri: "ftp://x/output".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::InvalidArgument(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_invalid_regex_returns_invalid_argument() {
        let uc = SubscribeUseCase::new(lane_admin());
        let err = uc
            .execute(SubscribeRequest {
                uri: "shell://x/output".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::Regex("([".to_string()),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::InvalidArgument(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsubscribe_closes_lane_and_returns_uri() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        let sub_id = adapter
            .open_lane(
                "shell://sh-x/output".to_string(),
                ResourceKind::Shell,
                "sh-x".to_string(),
                LanePolicy::default(),
            )
            .await
            .unwrap();
        let uc = UnsubscribeUseCase::new(lane);
        let outcome = uc
            .execute(UnsubscribeRequest {
                sub_id: sub_id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(outcome.sub_id, sub_id);
        assert_eq!(outcome.uri.as_deref(), Some("shell://sh-x/output"));
        assert_eq!(outcome.lifecycle_state, "closed");
        // Closing again returns SubNotFound.
        let err = uc
            .execute(UnsubscribeRequest {
                sub_id: sub_id.clone(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::SubNotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsubscribe_unknown_returns_sub_not_found() {
        let uc = UnsubscribeUseCase::new(lane_admin());
        let err = uc
            .execute(UnsubscribeRequest {
                sub_id: SubId::new("ghost".to_string()),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::SubNotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pause_resume_round_trip() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        let sub_id = adapter
            .open_lane(
                "shell://sh-y/output".to_string(),
                ResourceKind::Shell,
                "sh-y".to_string(),
                LanePolicy::default(),
            )
            .await
            .unwrap();
        let pause = PauseSubUseCase::new(Arc::clone(&lane));
        let resume = ResumeSubUseCase::new(lane);
        let p = pause
            .execute(SubToggleRequest {
                sub_id: sub_id.clone(),
            })
            .await
            .unwrap();
        assert!(p.paused);
        let r = resume
            .execute(SubToggleRequest {
                sub_id: sub_id.clone(),
            })
            .await
            .unwrap();
        assert!(!r.paused);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pause_unknown_returns_sub_not_found() {
        let uc = PauseSubUseCase::new(lane_admin());
        let err = uc
            .execute(SubToggleRequest {
                sub_id: SubId::new("ghost".to_string()),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::SubNotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_unknown_returns_sub_not_found() {
        let uc = ResumeSubUseCase::new(lane_admin());
        let err = uc
            .execute(SubToggleRequest {
                sub_id: SubId::new("ghost".to_string()),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::SubNotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_filter_hot_reloads_regex() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        let sub_id = adapter
            .open_lane(
                "shell://sh-f/output".to_string(),
                ResourceKind::Shell,
                "sh-f".to_string(),
                LanePolicy::default(),
            )
            .await
            .unwrap();
        let uc = SetFilterUseCase::new(lane);
        let outcome = uc
            .execute(SetFilterRequest {
                sub_id: sub_id.clone(),
                filter: FilterRule::Regex("ERR.*".to_string()),
            })
            .await
            .unwrap();
        assert_eq!(outcome.sub_id, sub_id);
        match outcome.filter {
            FilterRule::Regex(s) => assert_eq!(s, "ERR.*"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_filter_invalid_regex_returns_invalid_argument() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        let sub_id = adapter
            .open_lane(
                "shell://sh-f2/output".to_string(),
                ResourceKind::Shell,
                "sh-f2".to_string(),
                LanePolicy::default(),
            )
            .await
            .unwrap();
        let uc = SetFilterUseCase::new(lane);
        let err = uc
            .execute(SetFilterRequest {
                sub_id,
                filter: FilterRule::Regex("([".to_string()),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::InvalidArgument(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_filter_unknown_returns_sub_not_found() {
        let uc = SetFilterUseCase::new(lane_admin());
        let err = uc
            .execute(SetFilterRequest {
                sub_id: SubId::new("ghost".to_string()),
                filter: FilterRule::None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::SubNotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replay_advances_cursor() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        let sub_id = adapter
            .open_lane(
                "shell://sh-r/output".to_string(),
                ResourceKind::Shell,
                "sh-r".to_string(),
                LanePolicy::default(),
            )
            .await
            .unwrap();
        let uc = ReplaySubUseCase::new(lane);
        let outcome = uc
            .execute(ReplayRequest {
                sub_id: sub_id.clone(),
                from_cursor: 100,
            })
            .await
            .unwrap();
        assert_eq!(outcome.sub_id, sub_id);
        assert_eq!(outcome.from_cursor, 100);
        assert_eq!(adapter.current_cursor(&sub_id, ""), 100);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replay_unknown_returns_sub_not_found() {
        let uc = ReplaySubUseCase::new(lane_admin());
        let err = uc
            .execute(ReplayRequest {
                sub_id: SubId::new("ghost".to_string()),
                from_cursor: 0,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::SubNotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_subs_returns_all_when_no_filter() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        for path in ["shell://a/output", "shell://b/output", "command://c/output"] {
            let parts: Vec<&str> = path.split('/').collect();
            let kind = if path.starts_with("shell") {
                ResourceKind::Shell
            } else {
                ResourceKind::Command
            };
            adapter
                .open_lane(
                    path.to_string(),
                    kind,
                    parts[2].to_string(),
                    LanePolicy::default(),
                )
                .await
                .unwrap();
        }
        let uc = ListSubsUseCase::new(lane);
        let outcome = uc.execute(&ListSubsRequest::default()).unwrap();
        assert_eq!(outcome.subs.len(), 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_subs_filters_by_uri_prefix() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        for (kind, uri, id) in [
            (ResourceKind::Shell, "shell://a/output", "a"),
            (ResourceKind::Shell, "shell://b/output", "b"),
            (ResourceKind::Command, "command://c/output", "c"),
        ] {
            adapter
                .open_lane(uri.to_string(), kind, id.to_string(), LanePolicy::default())
                .await
                .unwrap();
        }
        let uc = ListSubsUseCase::new(lane);
        let outcome = uc
            .execute(&ListSubsRequest {
                uri_prefix: Some("shell://".to_string()),
                peer_id: None,
            })
            .unwrap();
        assert_eq!(outcome.subs.len(), 2);
        for s in outcome.subs {
            assert!(s.uri.starts_with("shell://"));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sub_stats_returns_snapshot() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        let sub_id = adapter
            .open_lane(
                "shell://sh-stats/output".to_string(),
                ResourceKind::Shell,
                "sh-stats".to_string(),
                LanePolicy::default(),
            )
            .await
            .unwrap();
        let uc = SubStatsUseCase::new(lane);
        let outcome = uc
            .execute(&SubStatsRequest {
                sub_id: sub_id.clone(),
            })
            .unwrap();
        assert_eq!(outcome.sub_id, sub_id);
        assert_eq!(outcome.stats.events_sent, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sub_stats_unknown_returns_sub_not_found() {
        let uc = SubStatsUseCase::new(lane_admin());
        let err = uc
            .execute(&SubStatsRequest {
                sub_id: SubId::new("ghost".to_string()),
            })
            .unwrap_err();
        assert!(matches!(err, DomainError::SubNotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn daemon_stats_aggregates_lane_counters() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        for (uri, id) in [("shell://a/output", "a"), ("shell://b/output", "b")] {
            adapter
                .open_lane(
                    uri.to_string(),
                    ResourceKind::Shell,
                    id.to_string(),
                    LanePolicy::default(),
                )
                .await
                .unwrap();
        }
        let uc = DaemonStatsUseCase::new(lane);
        let stats = uc.execute().unwrap();
        assert_eq!(stats.lanes_total, 2);
        assert_eq!(stats.events_sent_total, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_with_lease_lifetime_carries_zero_grace_ms() {
        let uc = SubscribeUseCase::new(lane_admin());
        let outcome = uc
            .execute(SubscribeRequest {
                uri: "shell://l/output".to_string(),
                lifetime: SubscriptionLifetime::Lease { ttl_secs: 60 },
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.grace_ms, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_with_drop_oldest_lag_policy_round_trips() {
        let uc = SubscribeUseCase::new(lane_admin());
        let outcome = uc
            .execute(SubscribeRequest {
                uri: "shell://o/output".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::DropOldest,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.lag_policy, LagPolicy::DropOldest);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_with_block_slow_lag_policy_round_trips() {
        let uc = SubscribeUseCase::new(lane_admin());
        let outcome = uc
            .execute(SubscribeRequest {
                uri: "shell://b/output".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::BlockSlow,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.lag_policy, LagPolicy::BlockSlow);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_carries_canonical_uri_after_query_strip() {
        let uc = SubscribeUseCase::new(lane_admin());
        let outcome = uc
            .execute(SubscribeRequest {
                uri: "shell://q/output?cursor=auto".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.uri, "shell://q/output");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_command_uri_succeeds() {
        let uc = SubscribeUseCase::new(lane_admin());
        let outcome = uc
            .execute(SubscribeRequest {
                uri: "command://c/output".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.uri, "command://c/output");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_transfer_uri_succeeds() {
        let uc = SubscribeUseCase::new(lane_admin());
        let outcome = uc
            .execute(SubscribeRequest {
                uri: "transfer://t/progress".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.uri, "transfer://t/progress");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_session_uri_succeeds() {
        let uc = SubscribeUseCase::new(lane_admin());
        let outcome = uc
            .execute(SubscribeRequest {
                uri: "session://s/health".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.uri, "session://s/health");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_forward_uri_succeeds() {
        let uc = SubscribeUseCase::new(lane_admin());
        let outcome = uc
            .execute(SubscribeRequest {
                uri: "forward://f/events".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.uri, "forward://f/events");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_max_subs_total_returns_typed_error() {
        // Tiny adapter: max_total = 2 so the third subscribe fails.
        let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 16, 16, 2);
        let lane: Arc<dyn LaneAdmin> = adapter as Arc<dyn LaneAdmin>;
        let uc = SubscribeUseCase::new(Arc::clone(&lane));
        let _ = uc
            .execute(SubscribeRequest {
                uri: "shell://x1/output".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        let _ = uc
            .execute(SubscribeRequest {
                uri: "shell://x2/output".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        let err = uc
            .execute(SubscribeRequest {
                uri: "shell://x3/output".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::MaxSubsTotalExceeded { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscribe_max_subs_per_uri_returns_typed_error() {
        let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 16, 1, 64);
        let lane: Arc<dyn LaneAdmin> = adapter as Arc<dyn LaneAdmin>;
        let uc = SubscribeUseCase::new(Arc::clone(&lane));
        let _ = uc
            .execute(SubscribeRequest {
                uri: "shell://same/output".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        let err = uc
            .execute(SubscribeRequest {
                uri: "shell://same/output".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::MaxSubsPerUriExceeded { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsubscribe_outcome_carries_uri_when_lane_known() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        let sub_id = adapter
            .open_lane(
                "shell://uri/output".to_string(),
                ResourceKind::Shell,
                "uri".to_string(),
                LanePolicy::default(),
            )
            .await
            .unwrap();
        let uc = UnsubscribeUseCase::new(lane);
        let outcome = uc
            .execute(UnsubscribeRequest {
                sub_id: sub_id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(outcome.uri.as_deref(), Some("shell://uri/output"));
        assert_eq!(outcome.lifecycle_state, "closed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pause_idempotent_when_called_twice() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        let sub_id = adapter
            .open_lane(
                "shell://2x/output".to_string(),
                ResourceKind::Shell,
                "2x".to_string(),
                LanePolicy::default(),
            )
            .await
            .unwrap();
        let uc = PauseSubUseCase::new(Arc::clone(&lane));
        let r1 = uc
            .execute(SubToggleRequest {
                sub_id: sub_id.clone(),
            })
            .await
            .unwrap();
        let r2 = uc
            .execute(SubToggleRequest {
                sub_id: sub_id.clone(),
            })
            .await
            .unwrap();
        assert!(r1.paused && r2.paused);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_filter_clear_with_none_round_trips() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        let sub_id = adapter
            .open_lane(
                "shell://clear/output".to_string(),
                ResourceKind::Shell,
                "clear".to_string(),
                LanePolicy {
                    lag_policy: LagPolicy::Snapshot,
                    lifetime: SubscriptionLifetime::Manual,
                    filter: FilterRule::Regex("ERR".to_string()),
                    buffer_size: 16,
                },
            )
            .await
            .unwrap();
        let uc = SetFilterUseCase::new(lane);
        let outcome = uc
            .execute(SetFilterRequest {
                sub_id,
                filter: FilterRule::None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.filter, FilterRule::None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replay_at_zero_cursor_is_a_noop() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        let sub_id = adapter
            .open_lane(
                "shell://z/output".to_string(),
                ResourceKind::Shell,
                "z".to_string(),
                LanePolicy::default(),
            )
            .await
            .unwrap();
        let uc = ReplaySubUseCase::new(lane);
        let outcome = uc
            .execute(ReplayRequest {
                sub_id: sub_id.clone(),
                from_cursor: 0,
            })
            .await
            .unwrap();
        assert_eq!(outcome.from_cursor, 0);
        assert_eq!(adapter.current_cursor(&sub_id, ""), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_subs_returns_empty_when_registry_is_empty() {
        let uc = ListSubsUseCase::new(lane_admin());
        let outcome = uc.execute(&ListSubsRequest::default()).unwrap();
        assert!(outcome.subs.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn daemon_stats_returns_zero_when_registry_is_empty() {
        let uc = DaemonStatsUseCase::new(lane_admin());
        let outcome = uc.execute().unwrap();
        assert_eq!(outcome.lanes_total, 0);
        assert_eq!(outcome.events_sent_total, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_subs_with_unmatched_prefix_returns_empty_set() {
        let adapter = lane_admin_concrete();
        let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
        adapter
            .open_lane(
                "shell://x/output".to_string(),
                ResourceKind::Shell,
                "x".to_string(),
                LanePolicy::default(),
            )
            .await
            .unwrap();
        let uc = ListSubsUseCase::new(lane);
        let outcome = uc
            .execute(&ListSubsRequest {
                uri_prefix: Some("transfer://".to_string()),
                peer_id: None,
            })
            .unwrap();
        assert!(outcome.subs.is_empty());
    }

    #[test]
    fn summary_kind_str_returns_lowercase_label() {
        let s = crate::ports::subscriber_lane::SubSummary {
            sub_id: SubId::new("x".to_string()),
            kind: ResourceKind::Shell,
            resource_id: "r".to_string(),
            uri: "shell://r/output".to_string(),
            lag_policy: LagPolicy::Snapshot,
            lifetime: SubscriptionLifetime::Manual,
            paused: false,
            stats: crate::domain::subscription::SubscriberStats::default(),
        };
        assert_eq!(summary_kind_str(&s), "shell");
    }

    #[test]
    fn summary_kind_str_covers_every_resource_kind() {
        for (kind, expected) in [
            (ResourceKind::Shell, "shell"),
            (ResourceKind::Command, "command"),
            (ResourceKind::Transfer, "transfer"),
            (ResourceKind::Session, "session"),
            (ResourceKind::Forward, "forward"),
        ] {
            let s = crate::ports::subscriber_lane::SubSummary {
                sub_id: SubId::new("x".to_string()),
                kind,
                resource_id: "r".to_string(),
                uri: "x".to_string(),
                lag_policy: LagPolicy::Snapshot,
                lifetime: SubscriptionLifetime::Manual,
                paused: false,
                stats: crate::domain::subscription::SubscriberStats::default(),
            };
            assert_eq!(summary_kind_str(&s), expected);
        }
    }
}
