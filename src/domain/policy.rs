//! Policy enums consumed by use cases.
//!
//! Pure value objects with no dependencies on the runtime so they can be
//! tested in isolation.

use serde::{Deserialize, Serialize};

use super::command::CommandStatus;

/// Reuse strategy applied by `connect_session` when an existing session
/// already matches the requested identity triple `(host, port, username)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReusePolicy {
    /// Surface candidate matches to the caller without taking action.
    Suggest,
    /// Reuse the first matching session automatically.
    Auto,
    /// Always open a fresh session regardless of matches.
    ForceNew,
}

impl Default for ReusePolicy {
    fn default() -> Self {
        Self::Suggest
    }
}

/// Filter applied to `list_commands` for the optional `status` predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatusFilter {
    /// Include commands currently running.
    Running,
    /// Include commands that completed (with any exit code).
    Completed,
    /// Include commands cancelled by the user.
    Cancelled,
    /// Include commands that failed to start.
    Failed,
}

impl CommandStatusFilter {
    /// `true` when this filter accepts a given [`CommandStatus`].
    #[must_use]
    pub const fn matches(self, status: CommandStatus) -> bool {
        match (self, status) {
            (Self::Running, CommandStatus::Running)
            | (Self::Completed, CommandStatus::Completed)
            | (Self::Cancelled, CommandStatus::Cancelled)
            | (Self::Failed, CommandStatus::Failed) => true,
            (
                Self::Running | Self::Completed | Self::Cancelled | Self::Failed,
                CommandStatus::Running
                | CommandStatus::Completed
                | CommandStatus::Cancelled
                | CommandStatus::Failed,
            ) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandStatus, CommandStatusFilter, ReusePolicy};

    #[test]
    fn default_reuse_policy_is_suggest() {
        assert_eq!(ReusePolicy::default(), ReusePolicy::Suggest);
    }

    #[test]
    fn command_status_filter_matches_only_same_variant() {
        for filter in [
            CommandStatusFilter::Running,
            CommandStatusFilter::Completed,
            CommandStatusFilter::Cancelled,
            CommandStatusFilter::Failed,
        ] {
            for status in [
                CommandStatus::Running,
                CommandStatus::Completed,
                CommandStatus::Cancelled,
                CommandStatus::Failed,
            ] {
                let expected = matches!(
                    (filter, status),
                    (CommandStatusFilter::Running, CommandStatus::Running)
                        | (CommandStatusFilter::Completed, CommandStatus::Completed)
                        | (CommandStatusFilter::Cancelled, CommandStatus::Cancelled)
                        | (CommandStatusFilter::Failed, CommandStatus::Failed)
                );
                assert_eq!(filter.matches(status), expected);
            }
        }
    }

    #[test]
    fn reuse_policy_serde_uses_snake_case() {
        let json = serde_json::to_string(&ReusePolicy::ForceNew).expect("serialise");
        assert_eq!(json, "\"force_new\"");
        let parsed: ReusePolicy = serde_json::from_str("\"auto\"").expect("deserialise");
        assert_eq!(parsed, ReusePolicy::Auto);
    }
}
