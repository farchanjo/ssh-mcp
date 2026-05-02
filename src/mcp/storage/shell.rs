//! DashMap-based shell storage implementation.
//!
//! Provides lock-free concurrent access to interactive shells using `DashMap`.
//! Includes a secondary index for O(1) session-to-shells lookups.

use std::collections::HashSet;

use std::sync::LazyLock;

use dashmap::DashMap;
use dashmap::mapref::one::Ref;

use crate::mcp::shell::RunningShell;
use crate::mcp::types::ShellInfo;

use super::traits::ShellStorage;

/// `DashMap`-based implementation of `ShellStorage`.
///
/// Uses two `DashMap` instances:
/// - Primary storage: `shell_id` -> `RunningShell`
/// - Secondary index: `session_id` -> `HashSet<shell_id>` for O(1) session lookups
pub struct DashMapShellStorage {
    shells: DashMap<String, RunningShell>,
    shells_by_session: DashMap<String, HashSet<String>>,
}

impl DashMapShellStorage {
    /// Create a new shell storage instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shells: DashMap::new(),
            shells_by_session: DashMap::new(),
        }
    }
}

impl Default for DashMapShellStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellStorage for DashMapShellStorage {
    fn register(&self, shell_id: String, shell: RunningShell) {
        let session_id = shell.info.session_id.clone();

        // Insert into primary storage
        self.shells.insert(shell_id.clone(), shell);

        // Update secondary index
        self.shells_by_session
            .entry(session_id)
            .or_default()
            .insert(shell_id);
    }

    fn unregister(&self, shell_id: &str) -> Option<RunningShell> {
        // Remove from primary storage
        let removed = self.shells.remove(shell_id).map(|(_, shell)| shell);

        // Update secondary index if shell was found
        if let Some(shell) = &removed
            && let Some(mut set) = self.shells_by_session.get_mut(&shell.info.session_id)
        {
            set.remove(shell_id);
            if set.is_empty() {
                drop(set);
                self.shells_by_session.remove(&shell.info.session_id);
            }
        }

        removed
    }

    fn get_direct(&self, shell_id: &str) -> Option<Ref<'_, String, RunningShell>> {
        self.shells.get(shell_id)
    }

    fn list_by_session(&self, session_id: &str) -> Vec<String> {
        self.shells_by_session
            .get(session_id)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn count_by_session(&self, session_id: &str) -> usize {
        self.shells_by_session
            .get(session_id)
            .map_or(0, |set| set.len())
    }

    fn list_all(&self) -> Vec<ShellInfo> {
        self.shells.iter().map(|entry| entry.info.clone()).collect()
    }

    fn list_filtered(&self, session_id: Option<&str>) -> Vec<ShellInfo> {
        self.shells
            .iter()
            .filter(|entry| session_id.is_none_or(|sid| entry.info.session_id == sid))
            .map(|entry| entry.info.clone())
            .collect()
    }
}

/// Global shell storage instance.
pub static SHELL_STORAGE: LazyLock<DashMapShellStorage> = LazyLock::new(DashMapShellStorage::new);

#[cfg(test)]
mod tests {
    use super::*;

    // Note: We cannot create real RunningShell instances in unit tests because
    // they require a real russh::Channel. Instead, we test the storage trait
    // methods that don't require channel access, and the full integration
    // is tested via cargo test with SSH connections.

    // Helper to verify trait bounds
    #[test]
    fn test_storage_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DashMapShellStorage>();
    }

    #[test]
    fn test_default_implementation() {
        let storage = DashMapShellStorage::default();
        let unique_session = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert_eq!(storage.count_by_session(&unique_session), 0);
    }

    #[test]
    fn test_list_by_session_empty() {
        let storage = DashMapShellStorage::new();
        let unique_session = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert!(storage.list_by_session(&unique_session).is_empty());
    }

    #[test]
    fn test_count_by_session_empty() {
        let storage = DashMapShellStorage::new();
        let unique_session = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert_eq!(storage.count_by_session(&unique_session), 0);
    }

    #[test]
    fn test_list_all_empty() {
        let storage = DashMapShellStorage::new();
        assert!(storage.list_all().is_empty());
    }

    #[test]
    fn test_list_filtered_empty() {
        let storage = DashMapShellStorage::new();
        let unique_session = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert!(storage.list_filtered(Some(&unique_session)).is_empty());
    }

    #[test]
    fn test_unregister_nonexistent() {
        let storage = DashMapShellStorage::new();
        let unique_id = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert!(storage.unregister(&unique_id).is_none());
    }

    #[test]
    fn test_get_direct_nonexistent() {
        let storage = DashMapShellStorage::new();
        let unique_id = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert!(storage.get_direct(&unique_id).is_none());
    }

    mod e15_extra {
        use super::*;

        #[test]
        fn list_all_returns_empty_vec() {
            let storage = DashMapShellStorage::new();
            assert_eq!(storage.list_all().len(), 0);
        }

        #[test]
        fn count_by_session_for_unknown_returns_zero() {
            let storage = DashMapShellStorage::new();
            for _ in 0..10_usize {
                let unknown = format!("unknown-{}", uuid::Uuid::new_v4());
                assert_eq!(storage.count_by_session(&unknown), 0);
            }
        }

        #[test]
        fn list_by_session_for_unknown_returns_empty() {
            let storage = DashMapShellStorage::new();
            let unknown = format!("unknown-{}", uuid::Uuid::new_v4());
            assert_eq!(storage.list_by_session(&unknown).len(), 0);
        }

        #[test]
        fn list_filtered_with_none_returns_empty_when_no_shells() {
            let storage = DashMapShellStorage::new();
            let listed = storage.list_filtered(None);
            assert_eq!(listed.len(), 0);
        }

        #[test]
        fn list_filtered_with_unknown_session_returns_empty() {
            let storage = DashMapShellStorage::new();
            let unknown = format!("unknown-{}", uuid::Uuid::new_v4());
            assert_eq!(storage.list_filtered(Some(&unknown)).len(), 0);
        }

        #[test]
        fn unregister_unknown_id_is_none_repeatedly() {
            let storage = DashMapShellStorage::new();
            for _ in 0..5_usize {
                let id = format!("unknown-{}", uuid::Uuid::new_v4());
                assert!(storage.unregister(&id).is_none());
            }
        }

        #[test]
        fn get_direct_unknown_id_is_none() {
            let storage = DashMapShellStorage::new();
            let id = format!("unknown-{}", uuid::Uuid::new_v4());
            assert!(storage.get_direct(&id).is_none());
        }

        #[test]
        fn default_storage_is_independent_from_global() {
            // Verify a fresh instance is empty regardless of SHELL_STORAGE state.
            let storage = DashMapShellStorage::new();
            assert_eq!(storage.list_all().len(), 0);
        }

        #[test]
        fn distinct_storage_instances_isolated() {
            let s1 = DashMapShellStorage::new();
            let s2 = DashMapShellStorage::new();
            assert_eq!(s1.list_all().len(), 0);
            assert_eq!(s2.list_all().len(), 0);
        }

        #[test]
        fn empty_session_id_count_is_zero() {
            let storage = DashMapShellStorage::new();
            assert_eq!(storage.count_by_session(""), 0);
        }
    }
}
