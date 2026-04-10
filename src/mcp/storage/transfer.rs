//! DashMap-based transfer storage implementation.
//!
//! Provides lock-free concurrent access to file transfers using `DashMap`.
//! Includes a secondary index for O(1) session-to-transfers lookups.

use std::collections::HashSet;

use std::sync::LazyLock;

use dashmap::DashMap;
use dashmap::mapref::one::Ref;

use crate::mcp::transfer::{RunningTransfer, TransferInfo};

use super::traits::TransferStorage;

/// `DashMap`-based implementation of `TransferStorage`.
///
/// Uses two `DashMap` instances:
/// - Primary storage: `transfer_id` -> `RunningTransfer`
/// - Secondary index: `session_id` -> `HashSet<transfer_id>` for O(1) session lookups
pub struct DashMapTransferStorage {
    transfers: DashMap<String, RunningTransfer>,
    transfers_by_session: DashMap<String, HashSet<String>>,
}

impl DashMapTransferStorage {
    /// Create a new transfer storage instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            transfers: DashMap::new(),
            transfers_by_session: DashMap::new(),
        }
    }
}

impl Default for DashMapTransferStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl TransferStorage for DashMapTransferStorage {
    fn register(&self, transfer_id: String, transfer: RunningTransfer) {
        let session_id = transfer.info.session_id.clone();

        // Insert into primary storage
        self.transfers.insert(transfer_id.clone(), transfer);

        // Update secondary index
        self.transfers_by_session
            .entry(session_id)
            .or_default()
            .insert(transfer_id);
    }

    fn unregister(&self, transfer_id: &str) -> Option<RunningTransfer> {
        // Remove from primary storage
        let removed = self
            .transfers
            .remove(transfer_id)
            .map(|(_, transfer)| transfer);

        // Update secondary index if transfer was found
        if let Some(transfer) = &removed
            && let Some(mut set) = self.transfers_by_session.get_mut(&transfer.info.session_id)
        {
            set.remove(transfer_id);
            if set.is_empty() {
                drop(set);
                self.transfers_by_session.remove(&transfer.info.session_id);
            }
        }

        removed
    }

    fn get_direct(&self, transfer_id: &str) -> Option<Ref<'_, String, RunningTransfer>> {
        self.transfers.get(transfer_id)
    }

    fn list_by_session(&self, session_id: &str) -> Vec<String> {
        self.transfers_by_session
            .get(session_id)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn count_by_session(&self, session_id: &str) -> usize {
        self.transfers_by_session
            .get(session_id)
            .map_or(0, |set| set.len())
    }

    fn list_all(&self) -> Vec<TransferInfo> {
        self.transfers
            .iter()
            .map(|entry| entry.info.clone())
            .collect()
    }

    fn list_filtered(&self, session_id: Option<&str>) -> Vec<TransferInfo> {
        self.transfers
            .iter()
            .filter(|entry| session_id.is_none_or(|sid| entry.info.session_id == sid))
            .map(|entry| entry.info.clone())
            .collect()
    }
}

/// Global transfer storage instance.
pub static TRANSFER_STORAGE: LazyLock<DashMapTransferStorage> =
    LazyLock::new(DashMapTransferStorage::new);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DashMapTransferStorage>();
    }

    #[test]
    fn test_default_implementation() {
        let storage = DashMapTransferStorage::default();
        let unique_session = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert_eq!(storage.count_by_session(&unique_session), 0);
    }

    #[test]
    fn test_list_by_session_empty() {
        let storage = DashMapTransferStorage::new();
        let unique_session = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert!(storage.list_by_session(&unique_session).is_empty());
    }

    #[test]
    fn test_count_by_session_empty() {
        let storage = DashMapTransferStorage::new();
        let unique_session = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert_eq!(storage.count_by_session(&unique_session), 0);
    }

    #[test]
    fn test_list_all_empty() {
        let storage = DashMapTransferStorage::new();
        assert!(storage.list_all().is_empty());
    }

    #[test]
    fn test_list_filtered_empty() {
        let storage = DashMapTransferStorage::new();
        let unique_session = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert!(storage.list_filtered(Some(&unique_session)).is_empty());
    }

    #[test]
    fn test_unregister_nonexistent() {
        let storage = DashMapTransferStorage::new();
        let unique_id = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert!(storage.unregister(&unique_id).is_none());
    }

    #[test]
    fn test_get_direct_nonexistent() {
        let storage = DashMapTransferStorage::new();
        let unique_id = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert!(storage.get_direct(&unique_id).is_none());
    }
}
