//! Use case: list async commands, optionally filtered by session and/or
//! status.
//!
//! Translates the v3 `ssh_list_commands_impl` (from
//! `src/mcp/tools/execute.rs`) into a hexagonal use case orchestrating the
//! command repository and the configuration port. No rmcp / russh / tokio
//! types leak into the public surface.
//!
//! # Orchestration shape
//!
//! 1. Resolve the candidate universe via
//!    [`CommandRepository::list_filtered`] using the optional `session_id`
//!    and `status` predicates from the request.
//! 2. Sort the resulting vector by `started_at` ascending. The repository
//!    adapter already sorts internally, but the use case re-asserts the
//!    invariant so any future adapter that drops the guarantee remains
//!    compatible.
//! 3. Capture `total = filtered.len()` **before** truncation so callers can
//!    render a "showing N of M" hint without losing the unbounded figure.
//! 4. Truncate the result to `req.max_items.unwrap_or(default).clamp(1, cap)`,
//!    where the default and cap come from
//!    [`ConfigPort::list_max_items_default`] and
//!    [`ConfigPort::list_max_items_cap`].
//!
//! # Concurrency contract
//!
//! Mirrors the canary [`crate::application::connect_session`]: every
//! `.await` is performed outside any repository / fake guard. The repository
//! port returns owned values so no shard-spanning borrows escape.

use std::sync::Arc;

use crate::domain::command::CommandEntity;
use crate::domain::error::DomainError;
use crate::domain::ids::SessionId;
use crate::domain::policy::CommandStatusFilter;
use crate::ports::command_repo::CommandRepository;
use crate::ports::config::ConfigPort;

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone, Default)]
pub struct ListCommandsRequest {
    /// Optional session filter narrowing the universe to commands owned by
    /// the given session id. `None` returns commands across every session.
    pub filter_session_id: Option<SessionId>,
    /// Optional lifecycle filter. `None` returns every status.
    pub filter_status: Option<CommandStatusFilter>,
    /// Optional caller-supplied truncation cap. `None` defers to
    /// [`ConfigPort::list_max_items_default`]; supplied values are clamped
    /// to `1..=ConfigPort::list_max_items_cap`.
    pub max_items: Option<usize>,
}

/// Outbound DTO surfacing every observable result the rmcp tool wrapper
/// must render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListCommandsOutcome {
    /// Commands matching the filters, sorted by `started_at` ascending and
    /// truncated to the resolved `max_items` cap.
    pub commands: Vec<CommandEntity>,
    /// Total matches **before** the `max_items` truncation. Allows the
    /// caller to render a "showing N of M" hint without losing the
    /// unbounded figure.
    pub total: usize,
}

/// List-commands use case generic over every adapter dependency. The
/// composition root pins concrete adapter types per binary; tests inject
/// fakes via [`crate::adapters`].
#[derive(Debug)]
pub struct ListCommandsUseCase<CR, Cfg>
where
    CR: CommandRepository + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    commands: Arc<CR>,
    config: Arc<Cfg>,
}

impl<CR, Cfg> ListCommandsUseCase<CR, Cfg>
where
    CR: CommandRepository + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(commands: Arc<CR>, config: Arc<Cfg>) -> Self {
        Self { commands, config }
    }

    /// Drive the list-commands orchestration. See module-level docs for the
    /// step-by-step semantics.
    ///
    /// # Errors
    ///
    /// Propagates any [`DomainError::Storage`] surfaced by the command
    /// repository.
    pub async fn execute(
        &self,
        req: ListCommandsRequest,
    ) -> Result<ListCommandsOutcome, DomainError> {
        let mut filtered = self
            .commands
            .list_filtered(req.filter_session_id.as_ref(), req.filter_status)
            .await?;
        filtered.sort_by_key(|entity| entity.started_at);
        let total = filtered.len();
        let cap = self.resolve_max_items(req.max_items);
        filtered.truncate(cap);
        Ok(ListCommandsOutcome {
            commands: filtered,
            total,
        })
    }

    /// Resolve the truncation cap from the request override or the config
    /// defaults. Mirrors the v3 `clamp_list_items` helper: missing input
    /// falls back to the configured default; supplied values are clamped to
    /// `1..=cap`.
    fn resolve_max_items(&self, requested: Option<usize>) -> usize {
        let default = self.config.list_max_items_default();
        let cap = self.config.list_max_items_cap();
        requested.unwrap_or(default).clamp(1, cap)
    }
}

#[cfg(test)]
mod tests {
    use super::{ListCommandsRequest, ListCommandsUseCase};
    use crate::adapters::config::memory::MapConfig;
    use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
    use crate::domain::command::{CommandEntity, CommandStatus};
    use crate::domain::ids::{CommandId, SessionId};
    use crate::domain::policy::CommandStatusFilter;
    use crate::ports::command_repo::CommandRepository;
    use chrono::{DateTime, TimeZone, Utc};
    use std::sync::Arc;

    type UseCaseUnderTest = ListCommandsUseCase<DashMapCommandRepo, MapConfig>;

    fn build_use_case() -> (UseCaseUnderTest, Arc<DashMapCommandRepo>) {
        let commands = Arc::new(DashMapCommandRepo::new());
        let config = Arc::new(MapConfig::default_v3());
        let uc = ListCommandsUseCase::new(Arc::clone(&commands), Arc::clone(&config));
        (uc, commands)
    }

    fn build_use_case_with_config(
        config: MapConfig,
    ) -> (UseCaseUnderTest, Arc<DashMapCommandRepo>) {
        let commands = Arc::new(DashMapCommandRepo::new());
        let config = Arc::new(config);
        let uc = ListCommandsUseCase::new(Arc::clone(&commands), Arc::clone(&config));
        (uc, commands)
    }

    fn ts(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
    }

    async fn seed(
        repo: &DashMapCommandRepo,
        cmd_id: &str,
        session: &str,
        status: CommandStatus,
        started_at: DateTime<Utc>,
    ) -> CommandEntity {
        let entity = CommandEntity {
            id: CommandId::new(cmd_id.to_string()),
            session_id: SessionId::new(session.to_string()),
            command: format!("echo {cmd_id}"),
            status,
            started_at,
            exit_code: None,
            timed_out: false,
        };
        repo.insert(entity.clone()).await.expect("seed insert");
        entity
    }

    // --- Scenario 1 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_repo_returns_empty_outcome() {
        let (uc, _repo) = build_use_case();
        let outcome = uc
            .execute(ListCommandsRequest::default())
            .await
            .expect("execute");
        assert!(outcome.commands.is_empty());
        assert_eq!(outcome.total, 0);
    }

    // --- Scenario 2 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn no_filters_returns_every_command() {
        let (uc, repo) = build_use_case();
        let _ = seed(&repo, "c1", "s1", CommandStatus::Running, ts(2025, 1, 1)).await;
        let _ = seed(&repo, "c2", "s2", CommandStatus::Completed, ts(2025, 1, 2)).await;
        let _ = seed(&repo, "c3", "s1", CommandStatus::Failed, ts(2025, 1, 3)).await;
        let outcome = uc
            .execute(ListCommandsRequest::default())
            .await
            .expect("execute");
        assert_eq!(outcome.total, 3);
        assert_eq!(outcome.commands.len(), 3);
    }

    // --- Scenario 3 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_by_session_only_returns_matching_session_commands() {
        let (uc, repo) = build_use_case();
        let _ = seed(&repo, "c1", "s1", CommandStatus::Running, ts(2025, 1, 1)).await;
        let _ = seed(&repo, "c2", "s2", CommandStatus::Completed, ts(2025, 1, 2)).await;
        let _ = seed(&repo, "c3", "s1", CommandStatus::Failed, ts(2025, 1, 3)).await;
        let req = ListCommandsRequest {
            filter_session_id: Some(SessionId::new("s1".to_string())),
            ..Default::default()
        };
        let outcome = uc.execute(req).await.expect("execute");
        assert_eq!(outcome.total, 2);
        assert!(
            outcome
                .commands
                .iter()
                .all(|c| c.session_id.as_str() == "s1")
        );
    }

    // --- Scenario 4 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_by_status_running_returns_only_running() {
        let (uc, repo) = build_use_case();
        let _ = seed(&repo, "c1", "s1", CommandStatus::Running, ts(2025, 1, 1)).await;
        let _ = seed(&repo, "c2", "s1", CommandStatus::Completed, ts(2025, 1, 2)).await;
        let _ = seed(&repo, "c3", "s1", CommandStatus::Cancelled, ts(2025, 1, 3)).await;
        let _ = seed(&repo, "c4", "s1", CommandStatus::Failed, ts(2025, 1, 4)).await;
        let req = ListCommandsRequest {
            filter_status: Some(CommandStatusFilter::Running),
            ..Default::default()
        };
        let outcome = uc.execute(req).await.expect("execute");
        assert_eq!(outcome.total, 1);
        assert_eq!(outcome.commands[0].id.as_str(), "c1");
    }

    // --- Scenario 5 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_by_each_status_variant_isolates_correctly() {
        let (uc, repo) = build_use_case();
        let _ = seed(&repo, "c1", "s1", CommandStatus::Running, ts(2025, 1, 1)).await;
        let _ = seed(&repo, "c2", "s1", CommandStatus::Completed, ts(2025, 1, 2)).await;
        let _ = seed(&repo, "c3", "s1", CommandStatus::Cancelled, ts(2025, 1, 3)).await;
        let _ = seed(&repo, "c4", "s1", CommandStatus::Failed, ts(2025, 1, 4)).await;
        for (filter, expected) in [
            (CommandStatusFilter::Completed, "c2"),
            (CommandStatusFilter::Cancelled, "c3"),
            (CommandStatusFilter::Failed, "c4"),
        ] {
            let req = ListCommandsRequest {
                filter_status: Some(filter),
                ..Default::default()
            };
            let outcome = uc.execute(req).await.expect("execute");
            assert_eq!(outcome.total, 1, "filter {filter:?}");
            assert_eq!(outcome.commands[0].id.as_str(), expected);
        }
    }

    // --- Scenario 6 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_by_session_and_status_intersects_both_predicates() {
        let (uc, repo) = build_use_case();
        let _ = seed(&repo, "c1", "s1", CommandStatus::Running, ts(2025, 1, 1)).await;
        let _ = seed(&repo, "c2", "s2", CommandStatus::Running, ts(2025, 1, 2)).await;
        let _ = seed(&repo, "c3", "s1", CommandStatus::Completed, ts(2025, 1, 3)).await;
        let req = ListCommandsRequest {
            filter_session_id: Some(SessionId::new("s1".to_string())),
            filter_status: Some(CommandStatusFilter::Running),
            ..Default::default()
        };
        let outcome = uc.execute(req).await.expect("execute");
        assert_eq!(outcome.total, 1);
        assert_eq!(outcome.commands[0].id.as_str(), "c1");
    }

    // --- Scenario 7 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn max_items_truncates_but_preserves_total() {
        let cfg = MapConfig::default_v3()
            .with_list_max_items_default(10)
            .with_list_max_items_cap(10);
        let (uc, repo) = build_use_case_with_config(cfg);
        let _ = seed(&repo, "c1", "s1", CommandStatus::Running, ts(2025, 1, 1)).await;
        let _ = seed(&repo, "c2", "s1", CommandStatus::Running, ts(2025, 1, 2)).await;
        let _ = seed(&repo, "c3", "s1", CommandStatus::Running, ts(2025, 1, 3)).await;
        let req = ListCommandsRequest {
            max_items: Some(2),
            ..Default::default()
        };
        let outcome = uc.execute(req).await.expect("execute");
        assert_eq!(outcome.total, 3, "total must reflect pre-truncation count");
        assert_eq!(outcome.commands.len(), 2);
        // Sorted ascending by started_at -> oldest first.
        assert_eq!(outcome.commands[0].id.as_str(), "c1");
        assert_eq!(outcome.commands[1].id.as_str(), "c2");
    }

    // --- Scenario 8 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn max_items_above_cap_is_clamped_to_cap() {
        let cfg = MapConfig::default_v3()
            .with_list_max_items_default(2)
            .with_list_max_items_cap(2);
        let (uc, repo) = build_use_case_with_config(cfg);
        for idx in 0..5 {
            let _ = seed(
                &repo,
                &format!("c{idx}"),
                "s1",
                CommandStatus::Running,
                ts(2025, 1, i32_to_day(idx)),
            )
            .await;
        }
        let req = ListCommandsRequest {
            max_items: Some(9_999),
            ..Default::default()
        };
        let outcome = uc.execute(req).await.expect("execute");
        assert_eq!(outcome.total, 5);
        assert_eq!(
            outcome.commands.len(),
            2,
            "request above cap must be clamped"
        );
    }

    // --- Scenario 9 -----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn results_are_sorted_by_started_at_ascending() {
        let (uc, repo) = build_use_case();
        // Insert deliberately out of chronological order.
        let _ = seed(
            &repo,
            "newest",
            "s1",
            CommandStatus::Running,
            ts(2025, 6, 1),
        )
        .await;
        let _ = seed(
            &repo,
            "oldest",
            "s1",
            CommandStatus::Running,
            ts(2024, 6, 1),
        )
        .await;
        let _ = seed(
            &repo,
            "middle",
            "s1",
            CommandStatus::Running,
            ts(2025, 1, 1),
        )
        .await;
        let outcome = uc
            .execute(ListCommandsRequest::default())
            .await
            .expect("execute");
        assert_eq!(outcome.commands.len(), 3);
        assert_eq!(outcome.commands[0].id.as_str(), "oldest");
        assert_eq!(outcome.commands[1].id.as_str(), "middle");
        assert_eq!(outcome.commands[2].id.as_str(), "newest");
    }

    // --- Scenario 10 ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_session_filter_returns_empty() {
        let (uc, repo) = build_use_case();
        let _ = seed(&repo, "c1", "s1", CommandStatus::Running, ts(2025, 1, 1)).await;
        let req = ListCommandsRequest {
            filter_session_id: Some(SessionId::new("does-not-exist".to_string())),
            ..Default::default()
        };
        let outcome = uc.execute(req).await.expect("execute");
        assert!(outcome.commands.is_empty());
        assert_eq!(outcome.total, 0);
    }

    /// Convert a small `usize` to the calendar-day component used by
    /// [`ts`]. Avoids `as_conversions` while keeping the seed loop concise.
    fn i32_to_day(idx: usize) -> u32 {
        let day = u32::try_from(idx + 1).unwrap_or(1);
        day.clamp(1, 28)
    }
}
