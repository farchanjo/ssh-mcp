//! Transfer tracking types for SFTP file operations.
//!
//! This module provides types for tracking file upload/download progress.
//! Transfers run asynchronously and can be polled for progress or cancelled.
//!
//! # Architecture
//!
//! - `RunningTransfer`: Contains all state for an active transfer including
//!   progress counters, cancellation token, and status.
//! - Storage is handled by `storage::TransferStorage` trait implementations.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

/// Direction of an SFTP file transfer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    /// Uploading a local file to remote
    Upload,
    /// Downloading a remote file to local
    Download,
}

impl fmt::Display for TransferDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Upload => write!(f, "upload"),
            Self::Download => write!(f, "download"),
        }
    }
}

/// Status of a file transfer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    /// Transfer is in progress
    Running,
    /// Transfer completed successfully
    Completed,
    /// Transfer failed (check error field)
    Failed,
    /// Transfer was cancelled by user
    Cancelled,
}

impl fmt::Display for TransferStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Metadata for a file transfer
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TransferInfo {
    /// Unique identifier for this transfer
    pub transfer_id: String,
    /// Session ID where the transfer is running
    pub session_id: String,
    /// Direction of the transfer
    pub direction: TransferDirection,
    /// Local file path
    pub local_path: String,
    /// Remote file path
    pub remote_path: String,
    /// When the transfer was started (RFC3339 format)
    pub started_at: String,
}

/// State for a running file transfer
pub struct RunningTransfer {
    /// Transfer metadata
    pub info: TransferInfo,
    /// Token to cancel the transfer
    pub cancel_token: CancellationToken,
    /// Bytes transferred so far (atomic for lock-free reads)
    pub bytes_transferred: Arc<AtomicU64>,
    /// Total bytes to transfer (atomic, set before transfer starts)
    pub total_bytes: Arc<AtomicU64>,
    /// Receiver for status updates
    pub status_rx: watch::Receiver<TransferStatus>,
    /// Sender for status updates (kept alive to prevent channel closure)
    #[allow(dead_code, reason = "kept alive to prevent watch channel closure")]
    pub status_tx: watch::Sender<TransferStatus>,
    /// Error message if transfer failed
    pub error: Arc<Mutex<Option<String>>>,
}

/// Maximum number of concurrent transfers per session
pub const MAX_TRANSFERS_PER_SESSION: usize = 10;

/// Chunk size for streaming transfers (32KB)
pub const CHUNK_SIZE: usize = 32 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    mod transfer_direction {
        use super::*;

        #[test]
        fn test_serialize_upload() {
            let dir = TransferDirection::Upload;
            let json = serde_json::to_string(&dir).unwrap();
            assert_eq!(json, "\"upload\"");
        }

        #[test]
        fn test_serialize_download() {
            let dir = TransferDirection::Download;
            let json = serde_json::to_string(&dir).unwrap();
            assert_eq!(json, "\"download\"");
        }

        #[test]
        fn test_deserialize_all_variants() {
            assert_eq!(
                serde_json::from_str::<TransferDirection>("\"upload\"").unwrap(),
                TransferDirection::Upload
            );
            assert_eq!(
                serde_json::from_str::<TransferDirection>("\"download\"").unwrap(),
                TransferDirection::Download
            );
        }

        #[test]
        fn test_display_trait() {
            assert_eq!(format!("{}", TransferDirection::Upload), "upload");
            assert_eq!(format!("{}", TransferDirection::Download), "download");
        }

        #[test]
        fn test_clone_and_copy() {
            let dir = TransferDirection::Upload;
            let cloned = dir.clone();
            let copied = dir;
            assert_eq!(dir, cloned);
            assert_eq!(dir, copied);
        }
    }

    mod transfer_status {
        use super::*;

        #[test]
        fn test_serialize_all_variants() {
            assert_eq!(
                serde_json::to_string(&TransferStatus::Running).unwrap(),
                "\"running\""
            );
            assert_eq!(
                serde_json::to_string(&TransferStatus::Completed).unwrap(),
                "\"completed\""
            );
            assert_eq!(
                serde_json::to_string(&TransferStatus::Failed).unwrap(),
                "\"failed\""
            );
            assert_eq!(
                serde_json::to_string(&TransferStatus::Cancelled).unwrap(),
                "\"cancelled\""
            );
        }

        #[test]
        fn test_deserialize_all_variants() {
            assert_eq!(
                serde_json::from_str::<TransferStatus>("\"running\"").unwrap(),
                TransferStatus::Running
            );
            assert_eq!(
                serde_json::from_str::<TransferStatus>("\"completed\"").unwrap(),
                TransferStatus::Completed
            );
            assert_eq!(
                serde_json::from_str::<TransferStatus>("\"failed\"").unwrap(),
                TransferStatus::Failed
            );
            assert_eq!(
                serde_json::from_str::<TransferStatus>("\"cancelled\"").unwrap(),
                TransferStatus::Cancelled
            );
        }

        #[test]
        fn test_display_trait() {
            assert_eq!(format!("{}", TransferStatus::Running), "running");
            assert_eq!(format!("{}", TransferStatus::Completed), "completed");
            assert_eq!(format!("{}", TransferStatus::Failed), "failed");
            assert_eq!(format!("{}", TransferStatus::Cancelled), "cancelled");
        }

        #[test]
        fn test_clone_and_copy() {
            let status = TransferStatus::Running;
            let cloned = status.clone();
            let copied = status;
            assert_eq!(status, cloned);
            assert_eq!(status, copied);
        }

        #[test]
        fn test_equality() {
            assert_eq!(TransferStatus::Running, TransferStatus::Running);
            assert_ne!(TransferStatus::Running, TransferStatus::Completed);
        }
    }

    mod transfer_info {
        use super::*;

        #[test]
        fn test_serialize_and_deserialize() {
            let info = TransferInfo {
                transfer_id: "xfer-123".to_string(),
                session_id: "sess-456".to_string(),
                direction: TransferDirection::Upload,
                local_path: "/tmp/file.txt".to_string(),
                remote_path: "/home/user/file.txt".to_string(),
                started_at: "2024-01-15T10:30:00Z".to_string(),
            };

            let json = serde_json::to_string(&info).unwrap();
            let deserialized: TransferInfo = serde_json::from_str(&json).unwrap();

            assert_eq!(deserialized.transfer_id, "xfer-123");
            assert_eq!(deserialized.session_id, "sess-456");
            assert_eq!(deserialized.direction, TransferDirection::Upload);
            assert_eq!(deserialized.local_path, "/tmp/file.txt");
            assert_eq!(deserialized.remote_path, "/home/user/file.txt");
        }

        #[test]
        fn test_clone() {
            let info = TransferInfo {
                transfer_id: "xfer-123".to_string(),
                session_id: "sess-456".to_string(),
                direction: TransferDirection::Download,
                local_path: "/tmp/file.txt".to_string(),
                remote_path: "/home/user/file.txt".to_string(),
                started_at: "2024-01-15T10:30:00Z".to_string(),
            };

            let cloned = info.clone();
            assert_eq!(cloned.transfer_id, info.transfer_id);
            assert_eq!(cloned.direction, info.direction);
        }
    }

    mod constants {
        use super::*;

        #[test]
        fn test_max_transfers_per_session() {
            assert_eq!(MAX_TRANSFERS_PER_SESSION, 10);
        }

        #[test]
        fn test_max_transfers_is_reasonable() {
            assert!(MAX_TRANSFERS_PER_SESSION >= 1);
            assert!(MAX_TRANSFERS_PER_SESSION <= 50);
        }

        #[test]
        fn test_chunk_size() {
            assert_eq!(CHUNK_SIZE, 32 * 1024);
        }

        #[test]
        fn test_chunk_size_is_reasonable() {
            assert!(CHUNK_SIZE >= 4096);
            assert!(CHUNK_SIZE <= 1024 * 1024);
        }
    }

    mod running_transfer {
        use super::*;

        #[tokio::test]
        async fn test_cancellation_token() {
            let token = CancellationToken::new();
            assert!(!token.is_cancelled());

            token.cancel();
            assert!(token.is_cancelled());
        }

        #[tokio::test]
        async fn test_status_watch_channel() {
            let (tx, mut rx) = watch::channel(TransferStatus::Running);

            assert_eq!(*rx.borrow(), TransferStatus::Running);

            tx.send(TransferStatus::Completed).unwrap();
            rx.changed().await.unwrap();
            assert_eq!(*rx.borrow(), TransferStatus::Completed);
        }

        #[tokio::test]
        async fn test_progress_atomic_counter() {
            let bytes = Arc::new(AtomicU64::new(0));

            let bytes_clone = bytes.clone();
            let handle = tokio::spawn(async move {
                for _ in 0..100 {
                    bytes_clone.fetch_add(1024, std::sync::atomic::Ordering::SeqCst);
                }
            });

            handle.await.unwrap();
            assert_eq!(bytes.load(std::sync::atomic::Ordering::SeqCst), 100 * 1024);
        }

        #[tokio::test]
        async fn test_error_storage() {
            let error = Arc::new(Mutex::new(None::<String>));

            let error_clone = error.clone();
            let handle = tokio::spawn(async move {
                *error_clone.lock().await = Some("connection lost".to_string());
            });

            handle.await.unwrap();
            let val = error.lock().await;
            assert_eq!(val.as_deref(), Some("connection lost"));
        }
    }
}
