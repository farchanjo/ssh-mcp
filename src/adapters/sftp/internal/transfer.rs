//! Transfer status tracking constants for SFTP file operations.
//!
//! This module exposes the lock-free transfer status enum consumed by the
//! production [`crate::adapters::sftp::russh_sftp_adapter::RusshSftpAdapter`]
//! and the per-session/global limits enforced when starting transfers.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Status of a file transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    /// Transfer is in progress.
    Running,
    /// Transfer completed successfully.
    Completed,
    /// Transfer failed (check error field).
    Failed,
    /// Transfer was cancelled by user.
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

/// Maximum number of concurrent transfers per session.
pub const MAX_TRANSFERS_PER_SESSION: usize = 10;

/// Chunk size for streaming transfers (32KB).
pub const CHUNK_SIZE: usize = 32 * 1024;

#[cfg(test)]
mod tests {
    use super::{CHUNK_SIZE, MAX_TRANSFERS_PER_SESSION, TransferStatus};

    mod transfer_status {
        use super::TransferStatus;

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
            let cloned = status;
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

    mod constants {
        use super::{CHUNK_SIZE, MAX_TRANSFERS_PER_SESSION};

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
}
