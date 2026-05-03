//! Async command aggregate value objects.
//!
//! `CommandRequest` is the input description handed to `execute_command`.
//! `CommandEntity` is the snapshot the use case publishes back to callers.
//! `CommandStatus` is the lifecycle enum used both inside the aggregate and
//! by the [`super::policy::CommandStatusFilter`] predicate.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ids::{CommandId, SessionId};

/// Lifecycle states of an async command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// Command is currently executing.
    Running,
    /// Command finished, exit code captured.
    Completed,
    /// Command was cancelled by the operator.
    Cancelled,
    /// Command failed before producing an exit status.
    Failed,
}

impl fmt::Display for CommandStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => f.write_str("running"),
            Self::Completed => f.write_str("completed"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Failed => f.write_str("failed"),
        }
    }
}

/// Description of a command to execute on an established session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRequest {
    /// Session that hosts the channel.
    pub session_id: SessionId,
    /// Verbatim shell command line.
    pub command: String,
    /// Optional timeout overriding the per-session default.
    pub timeout: Option<Duration>,
    /// Whether the channel must allocate a PTY.
    pub pty: bool,
}

impl CommandRequest {
    /// Build a non-PTY command request with the per-session default timeout.
    #[must_use]
    pub const fn new(session_id: SessionId, command: String) -> Self {
        Self {
            session_id,
            command,
            timeout: None,
            pty: false,
        }
    }

    /// Override the timeout.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Force PTY allocation.
    #[must_use]
    pub const fn with_pty(mut self, pty: bool) -> Self {
        self.pty = pty;
        self
    }
}

/// Snapshot of an async command as published by storage / list endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEntity {
    /// Stable identifier minted at execute time.
    pub id: CommandId,
    /// Owning session.
    pub session_id: SessionId,
    /// Verbatim shell command line.
    pub command: String,
    /// Lifecycle state.
    pub status: CommandStatus,
    /// Wall-clock timestamp the command was started.
    pub started_at: DateTime<Utc>,
    /// Captured exit code (set when [`status`] becomes `Completed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Whether the command exited because the configured timeout fired.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub timed_out: bool,
}

impl CommandEntity {
    /// Build a fresh [`CommandEntity`] in [`CommandStatus::Running`] state.
    #[must_use]
    pub const fn new(
        id: CommandId,
        session_id: SessionId,
        command: String,
        started_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            session_id,
            command,
            status: CommandStatus::Running,
            started_at,
            exit_code: None,
            timed_out: false,
        }
    }

    /// Apply a terminal exit code and flip status to [`CommandStatus::Completed`].
    #[must_use]
    pub const fn complete(mut self, exit_code: i32, timed_out: bool) -> Self {
        self.status = CommandStatus::Completed;
        self.exit_code = Some(exit_code);
        self.timed_out = timed_out;
        self
    }

    /// Flip status to [`CommandStatus::Cancelled`].
    #[must_use]
    pub const fn cancel(mut self) -> Self {
        self.status = CommandStatus::Cancelled;
        self
    }

    /// Flip status to [`CommandStatus::Failed`].
    #[must_use]
    pub const fn fail(mut self) -> Self {
        self.status = CommandStatus::Failed;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandEntity, CommandId, CommandRequest, CommandStatus, SessionId};
    use chrono::Utc;
    use std::time::Duration;

    #[test]
    fn command_status_display_matches_serde() {
        assert_eq!(CommandStatus::Running.to_string(), "running");
        assert_eq!(CommandStatus::Completed.to_string(), "completed");
        assert_eq!(CommandStatus::Cancelled.to_string(), "cancelled");
        assert_eq!(CommandStatus::Failed.to_string(), "failed");
    }

    #[test]
    fn command_request_builder_sets_timeout_and_pty() {
        let req = CommandRequest::new(SessionId::new("s".to_string()), "ls".to_string())
            .with_timeout(Duration::from_secs(5))
            .with_pty(true);
        assert_eq!(req.command, "ls");
        assert_eq!(req.timeout, Some(Duration::from_secs(5)));
        assert!(req.pty);
    }

    #[test]
    fn command_entity_lifecycle_transitions() {
        let entity = CommandEntity::new(
            CommandId::new("c".to_string()),
            SessionId::new("s".to_string()),
            "echo".to_string(),
            Utc::now(),
        );
        assert_eq!(entity.status, CommandStatus::Running);
        let done = entity.clone().complete(0, false);
        assert_eq!(done.status, CommandStatus::Completed);
        assert_eq!(done.exit_code, Some(0));
        assert!(!done.timed_out);

        let cancelled = entity.clone().cancel();
        assert_eq!(cancelled.status, CommandStatus::Cancelled);

        let failed = entity.fail();
        assert_eq!(failed.status, CommandStatus::Failed);
    }
}
