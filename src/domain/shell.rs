//! Interactive shell aggregate value objects.
//!
//! Mirrors the v3 `crate::adapters::ssh::internal::types::ShellInfo` payload
//! and the lifecycle enum
//! `crate::adapters::ssh::internal::types::ShellStatus` without any tokio
//! coupling.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ids::{SessionId, ShellId};

/// Lifecycle states of an interactive shell session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellStatus {
    /// Shell is open and accepting input.
    Open,
    /// Shell has been closed (either by the operator or by the inactivity TTL).
    Closed,
}

impl fmt::Display for ShellStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open => f.write_str("open"),
            Self::Closed => f.write_str("closed"),
        }
    }
}

/// Terminal description used when allocating the PTY.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellTerminal {
    /// `TERM` value (e.g. `xterm-256color`).
    pub term_type: String,
    /// PTY width in columns.
    pub cols: u32,
    /// PTY height in rows.
    pub rows: u32,
}

impl ShellTerminal {
    /// Build a terminal description.
    #[must_use]
    pub const fn new(term_type: String, cols: u32, rows: u32) -> Self {
        Self {
            term_type,
            cols,
            rows,
        }
    }
}

/// Snapshot of a single interactive shell session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellEntity {
    /// Stable identifier.
    pub id: ShellId,
    /// Owning session.
    pub session_id: SessionId,
    /// Terminal description.
    pub terminal: ShellTerminal,
    /// Wall-clock timestamp the shell was opened.
    pub opened_at: DateTime<Utc>,
    /// Lifecycle state.
    pub status: ShellStatus,
    /// Auto-close TTL applied when no read or write occurs.
    pub inactivity_ttl: Duration,
    /// Maximum number of bytes retained in the rolling output buffer.
    pub max_buffer_size: u64,
}

impl ShellEntity {
    /// Build a fresh shell snapshot in [`ShellStatus::Open`].
    #[must_use]
    pub const fn new(
        id: ShellId,
        session_id: SessionId,
        terminal: ShellTerminal,
        opened_at: DateTime<Utc>,
        inactivity_ttl: Duration,
        max_buffer_size: u64,
    ) -> Self {
        Self {
            id,
            session_id,
            terminal,
            opened_at,
            status: ShellStatus::Open,
            inactivity_ttl,
            max_buffer_size,
        }
    }

    /// Flip the lifecycle to [`ShellStatus::Closed`].
    #[must_use]
    pub const fn close(mut self) -> Self {
        self.status = ShellStatus::Closed;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionId, ShellEntity, ShellId, ShellStatus, ShellTerminal};
    use chrono::Utc;
    use std::time::Duration;

    #[test]
    fn shell_status_display_matches_serde() {
        assert_eq!(ShellStatus::Open.to_string(), "open");
        assert_eq!(ShellStatus::Closed.to_string(), "closed");
    }

    #[test]
    fn shell_entity_close_flips_status() {
        let entity = ShellEntity::new(
            ShellId::new("sh-1".to_string()),
            SessionId::new("s-1".to_string()),
            ShellTerminal::new("xterm".to_string(), 80, 24),
            Utc::now(),
            Duration::from_secs(600),
            10 * 1024 * 1024,
        );
        assert_eq!(entity.status, ShellStatus::Open);
        let closed = entity.close();
        assert_eq!(closed.status, ShellStatus::Closed);
    }
}
