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

    mod e15_extra {
        use super::*;
        use crate::mcp::transfer::{TransferDirection, TransferInfo};

        fn make_transfer(transfer_id: &str, session_id: &str) -> RunningTransfer {
            let info = TransferInfo {
                transfer_id: transfer_id.to_string(),
                session_id: session_id.to_string(),
                direction: TransferDirection::Upload,
                local_path: "/tmp/x".to_string(),
                remote_path: "/r/x".to_string(),
                started_at: "2025-01-01T00:00:00Z".to_string(),
            };
            RunningTransfer::new(info, 1024, 32)
        }

        #[test]
        fn register_and_unregister_round_trip() {
            let storage = DashMapTransferStorage::new();
            let session_id = format!("sess-{}", uuid::Uuid::new_v4());
            let xfer_id = format!("xfer-{}", uuid::Uuid::new_v4());
            storage.register(xfer_id.clone(), make_transfer(&xfer_id, &session_id));
            assert_eq!(storage.count_by_session(&session_id), 1);
            assert!(storage.get_direct(&xfer_id).is_some());

            let removed = storage.unregister(&xfer_id).expect("must return owned transfer");
            assert_eq!(removed.info.transfer_id, xfer_id);
            assert_eq!(storage.count_by_session(&session_id), 0);
            assert!(storage.get_direct(&xfer_id).is_none());
        }

        #[test]
        fn count_by_session_grows_with_each_register() {
            let storage = DashMapTransferStorage::new();
            let session_id = format!("sess-{}", uuid::Uuid::new_v4());
            let mut ids = Vec::new();
            for i in 0..5_usize {
                let xfer_id = format!("xfer-{i}-{}", uuid::Uuid::new_v4());
                storage.register(xfer_id.clone(), make_transfer(&xfer_id, &session_id));
                ids.push(xfer_id);
            }
            assert_eq!(storage.count_by_session(&session_id), 5);
            for id in &ids {
                storage.unregister(id);
            }
        }

        #[test]
        fn list_by_session_returns_all_registered_ids() {
            let storage = DashMapTransferStorage::new();
            let session_id = format!("sess-{}", uuid::Uuid::new_v4());
            let id1 = format!("xfer-1-{}", uuid::Uuid::new_v4());
            let id2 = format!("xfer-2-{}", uuid::Uuid::new_v4());
            storage.register(id1.clone(), make_transfer(&id1, &session_id));
            storage.register(id2.clone(), make_transfer(&id2, &session_id));
            let listed = storage.list_by_session(&session_id);
            assert_eq!(listed.len(), 2);
            assert!(listed.contains(&id1));
            assert!(listed.contains(&id2));
            storage.unregister(&id1);
            storage.unregister(&id2);
        }

        #[test]
        fn unregister_cleans_secondary_index() {
            let storage = DashMapTransferStorage::new();
            let session_id = format!("sess-{}", uuid::Uuid::new_v4());
            let xfer_id = format!("xfer-{}", uuid::Uuid::new_v4());
            storage.register(xfer_id.clone(), make_transfer(&xfer_id, &session_id));
            assert_eq!(storage.count_by_session(&session_id), 1);
            storage.unregister(&xfer_id);
            assert_eq!(storage.count_by_session(&session_id), 0);
            assert!(storage.list_by_session(&session_id).is_empty());
        }

        #[test]
        fn list_filtered_returns_only_matching_session() {
            let storage = DashMapTransferStorage::new();
            let s1 = format!("sess-1-{}", uuid::Uuid::new_v4());
            let s2 = format!("sess-2-{}", uuid::Uuid::new_v4());
            let id1 = format!("xfer-1-{}", uuid::Uuid::new_v4());
            let id2 = format!("xfer-2-{}", uuid::Uuid::new_v4());
            storage.register(id1.clone(), make_transfer(&id1, &s1));
            storage.register(id2.clone(), make_transfer(&id2, &s2));

            let only_s1 = storage.list_filtered(Some(&s1));
            assert_eq!(only_s1.len(), 1);
            assert_eq!(only_s1[0].transfer_id, id1);

            storage.unregister(&id1);
            storage.unregister(&id2);
        }

        #[test]
        fn list_filtered_with_no_filter_includes_registered_entry() {
            let storage = DashMapTransferStorage::new();
            let s = format!("sess-{}", uuid::Uuid::new_v4());
            let id = format!("xfer-{}", uuid::Uuid::new_v4());
            storage.register(id.clone(), make_transfer(&id, &s));
            let all = storage.list_filtered(None);
            assert!(all.iter().any(|info| info.transfer_id == id));
            storage.unregister(&id);
        }

        #[test]
        fn double_unregister_returns_none_second_time() {
            let storage = DashMapTransferStorage::new();
            let s = format!("sess-{}", uuid::Uuid::new_v4());
            let id = format!("xfer-{}", uuid::Uuid::new_v4());
            storage.register(id.clone(), make_transfer(&id, &s));
            assert!(storage.unregister(&id).is_some());
            assert!(storage.unregister(&id).is_none());
        }

        #[test]
        fn multiple_sessions_isolated() {
            let storage = DashMapTransferStorage::new();
            let s1 = format!("sess-A-{}", uuid::Uuid::new_v4());
            let s2 = format!("sess-B-{}", uuid::Uuid::new_v4());
            for i in 0..3_usize {
                let id = format!("xfer-A-{i}-{}", uuid::Uuid::new_v4());
                storage.register(id.clone(), make_transfer(&id, &s1));
            }
            for i in 0..2_usize {
                let id = format!("xfer-B-{i}-{}", uuid::Uuid::new_v4());
                storage.register(id.clone(), make_transfer(&id, &s2));
            }
            assert_eq!(storage.count_by_session(&s1), 3);
            assert_eq!(storage.count_by_session(&s2), 2);

            for id in storage.list_by_session(&s1) {
                storage.unregister(&id);
            }
            for id in storage.list_by_session(&s2) {
                storage.unregister(&id);
            }
        }

        #[test]
        fn list_filtered_for_unknown_session_is_empty() {
            let storage = DashMapTransferStorage::new();
            let unknown = format!("unknown-{}", uuid::Uuid::new_v4());
            assert!(storage.list_filtered(Some(&unknown)).is_empty());
        }

        #[test]
        fn fixture_lifecycle_register_get_drop() {
            let storage = DashMapTransferStorage::new();
            let s = format!("sess-{}", uuid::Uuid::new_v4());
            let id = format!("xfer-{}", uuid::Uuid::new_v4());
            storage.register(id.clone(), make_transfer(&id, &s));
            // Limit the borrow scope so unregister can take the entry.
            {
                let guard = storage.get_direct(&id).expect("must exist");
                assert_eq!(guard.info.transfer_id, id);
            }
            storage.unregister(&id);
        }
    }
}
