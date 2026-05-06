//! SFTP transfer aggregate value objects.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ids::{SessionId, TransferId};

/// Direction of a single SFTP transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    /// Local file is being uploaded to the remote host.
    Upload,
    /// Remote file is being downloaded to the local host.
    Download,
}

impl fmt::Display for TransferDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Upload => f.write_str("upload"),
            Self::Download => f.write_str("download"),
        }
    }
}

/// Lifecycle states of a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    /// Transfer is currently in progress.
    Running,
    /// Transfer finished successfully.
    Completed,
    /// Transfer failed before completion.
    Failed,
    /// Transfer was cancelled by the operator.
    Cancelled,
}

impl fmt::Display for TransferStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => f.write_str("running"),
            Self::Completed => f.write_str("completed"),
            Self::Failed => f.write_str("failed"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

/// Snapshot of a single transfer aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferEntity {
    /// Stable identifier.
    pub id: TransferId,
    /// Owning session.
    pub session_id: SessionId,
    /// Direction of the transfer.
    pub direction: TransferDirection,
    /// Local filesystem path.
    pub local_path: String,
    /// Remote filesystem path.
    pub remote_path: String,
    /// Wall-clock timestamp the transfer was started.
    pub started_at: DateTime<Utc>,
    /// Lifecycle state.
    pub status: TransferStatus,
    /// Bytes present at the destination (resume offset + bytes moved by this
    /// transfer). For fresh transfers ramps from `0` to `total_bytes`; for
    /// resumed transfers ramps from `resumed_from` to `total_bytes`.
    pub bytes_transferred: u64,
    /// Total bytes to transfer (snapshot at start time).
    pub total_bytes: u64,
    /// Byte offset the transfer resumed from (ADR 0010). `0` when the
    /// caller did not request resume, when the destination did not exist,
    /// or when the destination was empty. Equal to `total_bytes` when the
    /// destination already matched and the transfer short-circuited via the
    /// Skip plan.
    #[serde(default)]
    pub resumed_from: u64,
    /// Optional terminal failure description (set when status is `Failed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TransferEntity {
    /// Build a fresh transfer snapshot in [`TransferStatus::Running`].
    #[must_use]
    pub const fn new(
        id: TransferId,
        session_id: SessionId,
        direction: TransferDirection,
        local_path: String,
        remote_path: String,
        started_at: DateTime<Utc>,
        total_bytes: u64,
    ) -> Self {
        Self {
            id,
            session_id,
            direction,
            local_path,
            remote_path,
            started_at,
            status: TransferStatus::Running,
            bytes_transferred: 0,
            total_bytes,
            resumed_from: 0,
            error: None,
        }
    }

    /// Update progress counters and return the new entity.
    #[must_use]
    pub const fn with_progress(mut self, bytes_transferred: u64) -> Self {
        self.bytes_transferred = bytes_transferred;
        self
    }

    /// Mark the transfer as resumed from `offset` bytes (ADR 0010).
    ///
    /// Sets both `resumed_from = offset` and `bytes_transferred = offset`.
    /// Progress reporting therefore shows the ramp from the resume point
    /// to `total_bytes` rather than from zero, matching the actual
    /// destination state.
    #[must_use]
    pub const fn with_resumed_from(mut self, offset: u64) -> Self {
        self.resumed_from = offset;
        self.bytes_transferred = offset;
        self
    }

    /// Flip status to [`TransferStatus::Completed`].
    #[must_use]
    pub const fn complete(mut self, bytes_transferred: u64) -> Self {
        self.status = TransferStatus::Completed;
        self.bytes_transferred = bytes_transferred;
        self
    }

    /// Flip status to [`TransferStatus::Failed`] and capture the reason.
    #[must_use]
    pub fn fail(mut self, error: String) -> Self {
        self.status = TransferStatus::Failed;
        self.error = Some(error);
        self
    }

    /// Flip status to [`TransferStatus::Cancelled`].
    #[must_use]
    pub const fn cancel(mut self) -> Self {
        self.status = TransferStatus::Cancelled;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionId, TransferDirection, TransferEntity, TransferId, TransferStatus};
    use chrono::Utc;

    fn sample() -> TransferEntity {
        TransferEntity::new(
            TransferId::new("t".to_string()),
            SessionId::new("s".to_string()),
            TransferDirection::Upload,
            "/tmp/x".to_string(),
            "/r/x".to_string(),
            Utc::now(),
            1024,
        )
    }

    #[test]
    fn lifecycle_transitions() {
        let t = sample();
        assert_eq!(t.status, TransferStatus::Running);
        assert_eq!(t.resumed_from, 0);
        let progressed = t.clone().with_progress(512);
        assert_eq!(progressed.bytes_transferred, 512);
        assert_eq!(progressed.resumed_from, 0);
        let done = t.clone().complete(1024);
        assert_eq!(done.status, TransferStatus::Completed);
        let cancelled = t.clone().cancel();
        assert_eq!(cancelled.status, TransferStatus::Cancelled);
        let failed = t.fail("disk full".to_string());
        assert_eq!(failed.status, TransferStatus::Failed);
        assert_eq!(failed.error.as_deref(), Some("disk full"));
    }

    #[test]
    fn resumed_from_sets_progress_and_offset() {
        let t = sample().with_resumed_from(512);
        assert_eq!(t.resumed_from, 512);
        assert_eq!(t.bytes_transferred, 512);
        assert_eq!(t.total_bytes, 1024);
        assert_eq!(t.status, TransferStatus::Running);
    }

    #[test]
    fn resumed_from_preserved_through_lifecycle() {
        let resumed = sample().with_resumed_from(256);
        let progressed = resumed.clone().with_progress(800);
        assert_eq!(progressed.resumed_from, 256);
        assert_eq!(progressed.bytes_transferred, 800);
        let done = resumed.clone().complete(1024);
        assert_eq!(done.resumed_from, 256);
        assert_eq!(done.bytes_transferred, 1024);
        let cancelled = resumed.clone().cancel();
        assert_eq!(cancelled.resumed_from, 256);
        let failed = resumed.fail("link drop".to_string());
        assert_eq!(failed.resumed_from, 256);
    }

    #[test]
    fn resumed_from_default_when_deserialised_from_v6_payload() {
        let v6_json = r#"{
            "id": "t",
            "session_id": "s",
            "direction": "upload",
            "local_path": "/tmp/x",
            "remote_path": "/r/x",
            "started_at": "2026-01-01T00:00:00Z",
            "status": "running",
            "bytes_transferred": 0,
            "total_bytes": 1024
        }"#;
        let entity: TransferEntity =
            serde_json::from_str(v6_json).expect("v6 payload deserialises");
        assert_eq!(entity.resumed_from, 0);
        assert_eq!(entity.total_bytes, 1024);
    }

    #[test]
    fn direction_display() {
        assert_eq!(TransferDirection::Upload.to_string(), "upload");
        assert_eq!(TransferDirection::Download.to_string(), "download");
    }
}
