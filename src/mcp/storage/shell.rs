//! DashMap-based shell storage implementation.
//!
//! Provides lock-free concurrent access to interactive shells using `DashMap`.
//! Includes a secondary index for O(1) session-to-shells lookups.

use std::collections::HashSet;

use dashmap::DashMap;
use once_cell::sync::Lazy;

use crate::mcp::shell::RunningShell;
use crate::mcp::types::ShellInfo;

/// Trait for shell storage operations.
///
/// Implementations must be thread-safe (`Send + Sync`) for use across
/// async tasks. The default implementation uses `DashMap` for lock-free
/// concurrent access with a secondary index for O(1) session lookups.
#[allow(dead_code)]
pub trait ShellStorage: Send + Sync {
    /// Register a new shell.
    fn register(&self, shell_id: String, shell: RunningShell);

    /// Unregister a shell by ID, returning it if it existed.
    fn unregister(&self, shell_id: &str) -> Option<RunningShell>;

    /// Get a direct reference to a shell.
    fn get_direct(
        &self,
        shell_id: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, RunningShell>>;

    /// List all shell IDs for a session.
    fn list_by_session(&self, session_id: &str) -> Vec<String>;

    /// Count shells for a session.
    fn count_by_session(&self, session_id: &str) -> usize;

    /// List all shell info entries.
    fn list_all(&self) -> Vec<ShellInfo>;

    /// List shell info filtered by session.
    fn list_filtered(&self, session_id: Option<&str>) -> Vec<ShellInfo>;
}

/// DashMap-based implementation of `ShellStorage`.
///
/// Uses two `DashMap` instances:
/// - Primary storage: shell_id -> RunningShell
/// - Secondary index: session_id -> HashSet<shell_id> for O(1) session lookups
pub struct DashMapShellStorage {
    shells: DashMap<String, RunningShell>,
    shells_by_session: DashMap<String, HashSet<String>>,
}

impl DashMapShellStorage {
    /// Create a new shell storage instance.
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
        if let Some(ref shell) = removed
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

    fn get_direct(
        &self,
        shell_id: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, RunningShell>> {
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
            .map(|set| set.len())
            .unwrap_or(0)
    }

    fn list_all(&self) -> Vec<ShellInfo> {
        self.shells.iter().map(|entry| entry.info.clone()).collect()
    }

    fn list_filtered(&self, session_id: Option<&str>) -> Vec<ShellInfo> {
        self.shells
            .iter()
            .filter(|entry| {
                session_id
                    .map(|sid| entry.info.session_id == sid)
                    .unwrap_or(true)
            })
            .map(|entry| entry.info.clone())
            .collect()
    }
}

/// Global shell storage instance.
pub static SHELL_STORAGE: Lazy<DashMapShellStorage> = Lazy::new(DashMapShellStorage::new);

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
}
