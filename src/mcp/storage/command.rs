//! DashMap-based command storage implementation.
//!
//! Provides lock-free concurrent access to async commands using `DashMap`.
//! Includes a secondary index for O(1) session-to-commands lookups.

use std::collections::HashSet;
use std::sync::Arc;

use dashmap::DashMap;
use once_cell::sync::Lazy;

use crate::mcp::async_command::RunningCommand;
use crate::mcp::types::{AsyncCommandInfo, AsyncCommandStatus};

use super::traits::{CommandRef, CommandStorage};

/// DashMap-based implementation of `CommandStorage`.
///
/// Uses two `DashMap` instances:
/// - Primary storage: command_id -> RunningCommand
/// - Secondary index: session_id -> HashSet<command_id> for O(1) session lookups
pub struct DashMapCommandStorage {
    commands: DashMap<String, RunningCommand>,
    commands_by_session: DashMap<String, HashSet<String>>,
}

impl DashMapCommandStorage {
    /// Create a new command storage instance.
    pub fn new() -> Self {
        Self {
            commands: DashMap::new(),
            commands_by_session: DashMap::new(),
        }
    }

    /// Get direct access to the underlying DashMap for iteration.
    ///
    /// This is available for advanced use cases that require custom iteration logic.
    /// For standard filtering, prefer `list_filtered()` from the `CommandStorage` trait.
    #[allow(dead_code)]
    pub fn iter(
        &self,
    ) -> impl Iterator<Item = dashmap::mapref::multiple::RefMulti<'_, String, RunningCommand>> {
        self.commands.iter()
    }

    /// Get a reference to a command for direct field access.
    ///
    /// Returns a DashMap reference guard that provides access to the underlying
    /// `RunningCommand`. This is useful when you need to access multiple fields
    /// without cloning the entire struct.
    pub fn get_direct(
        &self,
        command_id: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, RunningCommand>> {
        self.commands.get(command_id)
    }
}

impl Default for DashMapCommandStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandStorage for DashMapCommandStorage {
    fn register(&self, command_id: String, cmd: RunningCommand) {
        let session_id = cmd.info.session_id.clone();

        // Insert into primary storage
        self.commands.insert(command_id.clone(), cmd);

        // Update secondary index
        self.commands_by_session
            .entry(session_id)
            .or_default()
            .insert(command_id);
    }

    fn unregister(&self, command_id: &str) -> Option<RunningCommand> {
        // Remove from primary storage
        let removed = self.commands.remove(command_id).map(|(_, cmd)| cmd);

        // Update secondary index if command was found
        if let Some(ref cmd) = removed
            && let Some(mut set) = self.commands_by_session.get_mut(&cmd.info.session_id)
        {
            set.remove(command_id);
            if set.is_empty() {
                drop(set);
                self.commands_by_session.remove(&cmd.info.session_id);
            }
        }

        removed
    }

    fn get(&self, command_id: &str) -> Option<Arc<RunningCommand>> {
        // Note: This creates a new Arc each time. For the current use case,
        // callers typically access individual Arc-wrapped fields from the command.
        // The trait returns Option<Arc<RunningCommand>> for API consistency,
        // but in practice, get_direct() is preferred for direct field access.
        self.commands.get(command_id).map(|entry| {
            // Clone the RunningCommand's Arc-wrapped fields
            Arc::new(RunningCommand {
                info: entry.info.clone(),
                cancel_token: entry.cancel_token.clone(),
                status_rx: entry.status_rx.clone(),
                status_tx: entry.status_tx.clone(),
                output: entry.output.clone(),
                exit_code: entry.exit_code.clone(),
                error: entry.error.clone(),
                timed_out: entry.timed_out.clone(),
            })
        })
    }

    fn get_ref(&self, command_id: &str) -> Option<CommandRef> {
        self.commands.get(command_id).map(|entry| CommandRef {
            info: entry.info.clone(),
            running: Arc::new(RunningCommand {
                info: entry.info.clone(),
                cancel_token: entry.cancel_token.clone(),
                status_rx: entry.status_rx.clone(),
                status_tx: entry.status_tx.clone(),
                output: entry.output.clone(),
                exit_code: entry.exit_code.clone(),
                error: entry.error.clone(),
                timed_out: entry.timed_out.clone(),
            }),
        })
    }

    fn list_by_session(&self, session_id: &str) -> Vec<String> {
        self.commands_by_session
            .get(session_id)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn count_by_session(&self, session_id: &str) -> usize {
        self.commands_by_session
            .get(session_id)
            .map(|set| set.len())
            .unwrap_or(0)
    }

    fn list_all(&self) -> Vec<AsyncCommandInfo> {
        self.commands
            .iter()
            .map(|entry| {
                let mut info = entry.info.clone();
                info.status = *entry.status_rx.borrow();
                info
            })
            .collect()
    }

    fn list_filtered(
        &self,
        session_id: Option<&str>,
        status: Option<AsyncCommandStatus>,
    ) -> Vec<AsyncCommandInfo> {
        self.commands
            .iter()
            .filter(|entry| {
                let session_matches = session_id
                    .map(|sid| entry.info.session_id == sid)
                    .unwrap_or(true);
                let status_matches = status
                    .map(|s| *entry.status_rx.borrow() == s)
                    .unwrap_or(true);
                session_matches && status_matches
            })
            .map(|entry| {
                let mut info = entry.info.clone();
                info.status = *entry.status_rx.borrow();
                info
            })
            .collect()
    }
}

/// Global command storage instance.
pub static COMMAND_STORAGE: Lazy<DashMapCommandStorage> = Lazy::new(DashMapCommandStorage::new);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::async_command::OutputBuffer;
    use crate::mcp::types::AsyncCommandStatus;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::{Mutex, watch};
    use tokio_util::sync::CancellationToken;

    fn create_test_command(command_id: &str, session_id: &str) -> RunningCommand {
        let (tx, rx) = watch::channel(AsyncCommandStatus::Running);
        RunningCommand {
            info: AsyncCommandInfo {
                command_id: command_id.to_string(),
                session_id: session_id.to_string(),
                command: "test".to_string(),
                status: AsyncCommandStatus::Running,
                started_at: "2024-01-15T10:30:00Z".to_string(),
            },
            cancel_token: CancellationToken::new(),
            status_rx: rx,
            status_tx: tx,
            output: Arc::new(Mutex::new(OutputBuffer::default())),
            exit_code: Arc::new(Mutex::new(None)),
            error: Arc::new(Mutex::new(None)),
            timed_out: Arc::new(AtomicBool::new(false)),
        }
    }

    fn create_test_command_with_status(
        command_id: &str,
        session_id: &str,
        status: AsyncCommandStatus,
    ) -> RunningCommand {
        let (tx, rx) = watch::channel(status);
        RunningCommand {
            info: AsyncCommandInfo {
                command_id: command_id.to_string(),
                session_id: session_id.to_string(),
                command: "test".to_string(),
                status,
                started_at: "2024-01-15T10:30:00Z".to_string(),
            },
            cancel_token: CancellationToken::new(),
            status_rx: rx,
            status_tx: tx,
            output: Arc::new(Mutex::new(OutputBuffer::default())),
            exit_code: Arc::new(Mutex::new(None)),
            error: Arc::new(Mutex::new(None)),
            timed_out: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn test_register_and_unregister() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("test-session-{}", uuid::Uuid::new_v4());
        let cmd_id = format!("test-cmd-{}", uuid::Uuid::new_v4());

        let cmd = create_test_command(&cmd_id, &session_id);
        storage.register(cmd_id.clone(), cmd);

        assert_eq!(storage.count_by_session(&session_id), 1);

        let removed = storage.unregister(&cmd_id);
        assert!(removed.is_some());
        assert_eq!(storage.count_by_session(&session_id), 0);
    }

    #[test]
    fn test_list_by_session() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("test-session-{}", uuid::Uuid::new_v4());
        let cmd_id_1 = format!("cmd-1-{}", uuid::Uuid::new_v4());
        let cmd_id_2 = format!("cmd-2-{}", uuid::Uuid::new_v4());

        storage.register(
            cmd_id_1.clone(),
            create_test_command(&cmd_id_1, &session_id),
        );
        storage.register(
            cmd_id_2.clone(),
            create_test_command(&cmd_id_2, &session_id),
        );

        let ids = storage.list_by_session(&session_id);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&cmd_id_1));
        assert!(ids.contains(&cmd_id_2));

        // Cleanup
        storage.unregister(&cmd_id_1);
        storage.unregister(&cmd_id_2);
    }

    #[test]
    fn test_count_by_session_empty() {
        let storage = DashMapCommandStorage::new();
        let unique_session = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert_eq!(storage.count_by_session(&unique_session), 0);
    }

    #[test]
    fn test_list_by_session_empty() {
        let storage = DashMapCommandStorage::new();
        let unique_session = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert!(storage.list_by_session(&unique_session).is_empty());
    }

    #[test]
    fn test_default_implementation() {
        let storage = DashMapCommandStorage::default();
        let unique_session = format!("nonexistent-{}", uuid::Uuid::new_v4());
        assert_eq!(storage.count_by_session(&unique_session), 0);
    }

    #[test]
    fn test_get_command() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("test-session-{}", uuid::Uuid::new_v4());
        let cmd_id = format!("test-cmd-{}", uuid::Uuid::new_v4());

        let cmd = create_test_command(&cmd_id, &session_id);
        storage.register(cmd_id.clone(), cmd);

        let retrieved = storage.get(&cmd_id);
        assert!(retrieved.is_some());
        let retrieved = retrieved.expect("command should exist");
        assert_eq!(retrieved.info.command_id, cmd_id);
        assert_eq!(retrieved.info.session_id, session_id);

        // Cleanup
        storage.unregister(&cmd_id);
    }

    #[test]
    fn test_get_nonexistent_command() {
        let storage = DashMapCommandStorage::new();
        let unique_id = format!("nonexistent-{}", uuid::Uuid::new_v4());

        assert!(storage.get(&unique_id).is_none());
    }

    #[test]
    fn test_get_ref() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("test-session-{}", uuid::Uuid::new_v4());
        let cmd_id = format!("test-cmd-{}", uuid::Uuid::new_v4());

        let cmd = create_test_command(&cmd_id, &session_id);
        storage.register(cmd_id.clone(), cmd);

        let cmd_ref = storage.get_ref(&cmd_id);
        assert!(cmd_ref.is_some());
        let cmd_ref = cmd_ref.expect("command ref should exist");
        assert_eq!(cmd_ref.info.command_id, cmd_id);
        assert_eq!(cmd_ref.info.session_id, session_id);

        // Cleanup
        storage.unregister(&cmd_id);
    }

    #[test]
    fn test_get_ref_nonexistent() {
        let storage = DashMapCommandStorage::new();
        let unique_id = format!("nonexistent-{}", uuid::Uuid::new_v4());

        assert!(storage.get_ref(&unique_id).is_none());
    }

    #[test]
    fn test_get_direct() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("test-session-{}", uuid::Uuid::new_v4());
        let cmd_id = format!("test-cmd-{}", uuid::Uuid::new_v4());

        let cmd = create_test_command(&cmd_id, &session_id);
        storage.register(cmd_id.clone(), cmd);

        // Scope the reference to release the read lock before unregister
        {
            let direct = storage.get_direct(&cmd_id);
            assert!(direct.is_some());
            let direct = direct.expect("direct ref should exist");
            assert_eq!(direct.info.command_id, cmd_id);
        }

        // Cleanup - now safe since read lock is released
        storage.unregister(&cmd_id);
    }

    #[test]
    fn test_get_direct_nonexistent() {
        let storage = DashMapCommandStorage::new();
        let unique_id = format!("nonexistent-{}", uuid::Uuid::new_v4());

        assert!(storage.get_direct(&unique_id).is_none());
    }

    #[test]
    fn test_list_all() {
        let storage = DashMapCommandStorage::new();
        let session_id_1 = format!("session-1-{}", uuid::Uuid::new_v4());
        let session_id_2 = format!("session-2-{}", uuid::Uuid::new_v4());
        let cmd_id_1 = format!("cmd-1-{}", uuid::Uuid::new_v4());
        let cmd_id_2 = format!("cmd-2-{}", uuid::Uuid::new_v4());

        storage.register(
            cmd_id_1.clone(),
            create_test_command(&cmd_id_1, &session_id_1),
        );
        storage.register(
            cmd_id_2.clone(),
            create_test_command(&cmd_id_2, &session_id_2),
        );

        let all = storage.list_all();
        // Note: list_all returns ALL commands including from other tests running in parallel
        // So we just check our commands are present
        let our_commands: Vec<_> = all
            .iter()
            .filter(|info| info.command_id == cmd_id_1 || info.command_id == cmd_id_2)
            .collect();
        assert_eq!(our_commands.len(), 2);

        // Cleanup
        storage.unregister(&cmd_id_1);
        storage.unregister(&cmd_id_2);
    }

    #[test]
    fn test_list_all_empty() {
        let storage = DashMapCommandStorage::new();
        // New storage instance should be empty
        assert!(storage.list_all().is_empty());
    }

    #[test]
    fn test_unregister_nonexistent() {
        let storage = DashMapCommandStorage::new();
        let unique_id = format!("nonexistent-{}", uuid::Uuid::new_v4());

        // Should return None, not panic
        assert!(storage.unregister(&unique_id).is_none());
    }

    #[test]
    fn test_unregister_cleans_secondary_index() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("test-session-{}", uuid::Uuid::new_v4());
        let cmd_id = format!("test-cmd-{}", uuid::Uuid::new_v4());

        let cmd = create_test_command(&cmd_id, &session_id);
        storage.register(cmd_id.clone(), cmd);

        assert_eq!(storage.count_by_session(&session_id), 1);

        storage.unregister(&cmd_id);

        // Secondary index should be cleaned up
        assert_eq!(storage.count_by_session(&session_id), 0);
        assert!(storage.list_by_session(&session_id).is_empty());
    }

    #[test]
    fn test_iter() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("test-session-{}", uuid::Uuid::new_v4());
        let cmd_id = format!("test-cmd-{}", uuid::Uuid::new_v4());

        let cmd = create_test_command(&cmd_id, &session_id);
        storage.register(cmd_id.clone(), cmd);

        let count = storage.iter().filter(|e| e.key() == &cmd_id).count();
        assert_eq!(count, 1);

        // Cleanup
        storage.unregister(&cmd_id);
    }

    #[test]
    fn test_multiple_commands_same_session() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("test-session-{}", uuid::Uuid::new_v4());
        let cmd_id_1 = format!("cmd-1-{}", uuid::Uuid::new_v4());
        let cmd_id_2 = format!("cmd-2-{}", uuid::Uuid::new_v4());
        let cmd_id_3 = format!("cmd-3-{}", uuid::Uuid::new_v4());

        storage.register(
            cmd_id_1.clone(),
            create_test_command(&cmd_id_1, &session_id),
        );
        storage.register(
            cmd_id_2.clone(),
            create_test_command(&cmd_id_2, &session_id),
        );
        storage.register(
            cmd_id_3.clone(),
            create_test_command(&cmd_id_3, &session_id),
        );

        assert_eq!(storage.count_by_session(&session_id), 3);

        // Unregister one
        storage.unregister(&cmd_id_2);
        assert_eq!(storage.count_by_session(&session_id), 2);

        let remaining = storage.list_by_session(&session_id);
        assert!(remaining.contains(&cmd_id_1));
        assert!(!remaining.contains(&cmd_id_2));
        assert!(remaining.contains(&cmd_id_3));

        // Cleanup
        storage.unregister(&cmd_id_1);
        storage.unregister(&cmd_id_3);
    }

    #[test]
    fn test_list_all_with_status_from_watch_channel() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("test-session-{}", uuid::Uuid::new_v4());
        let cmd_id = format!("test-cmd-{}", uuid::Uuid::new_v4());

        // Create command with Completed status
        let cmd =
            create_test_command_with_status(&cmd_id, &session_id, AsyncCommandStatus::Completed);
        storage.register(cmd_id.clone(), cmd);

        let all = storage.list_all();
        let our_cmd = all.iter().find(|info| info.command_id == cmd_id);
        assert!(our_cmd.is_some());
        let our_cmd = our_cmd.expect("our command should be in list");
        assert_eq!(our_cmd.status, AsyncCommandStatus::Completed);

        // Cleanup
        storage.unregister(&cmd_id);
    }

    #[test]
    fn test_commands_across_multiple_sessions() {
        let storage = DashMapCommandStorage::new();
        let session_id_1 = format!("session-1-{}", uuid::Uuid::new_v4());
        let session_id_2 = format!("session-2-{}", uuid::Uuid::new_v4());
        let cmd_id_1a = format!("cmd-1a-{}", uuid::Uuid::new_v4());
        let cmd_id_1b = format!("cmd-1b-{}", uuid::Uuid::new_v4());
        let cmd_id_2a = format!("cmd-2a-{}", uuid::Uuid::new_v4());

        storage.register(
            cmd_id_1a.clone(),
            create_test_command(&cmd_id_1a, &session_id_1),
        );
        storage.register(
            cmd_id_1b.clone(),
            create_test_command(&cmd_id_1b, &session_id_1),
        );
        storage.register(
            cmd_id_2a.clone(),
            create_test_command(&cmd_id_2a, &session_id_2),
        );

        assert_eq!(storage.count_by_session(&session_id_1), 2);
        assert_eq!(storage.count_by_session(&session_id_2), 1);

        // Cleanup
        storage.unregister(&cmd_id_1a);
        storage.unregister(&cmd_id_1b);
        storage.unregister(&cmd_id_2a);
    }

    #[test]
    fn test_many_commands_same_session() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("session-{}", uuid::Uuid::new_v4());
        let mut cmd_ids: Vec<String> = Vec::new();

        // Register 50 commands
        for i in 0..50 {
            let cmd_id = format!("cmd-{}-{}", i, uuid::Uuid::new_v4());
            storage.register(cmd_id.clone(), create_test_command(&cmd_id, &session_id));
            cmd_ids.push(cmd_id);
        }

        assert_eq!(storage.count_by_session(&session_id), 50);

        // Verify all commands are listed
        let listed = storage.list_by_session(&session_id);
        assert_eq!(listed.len(), 50);
        for cmd_id in &cmd_ids {
            assert!(listed.contains(cmd_id));
        }

        // Cleanup
        for cmd_id in &cmd_ids {
            storage.unregister(cmd_id);
        }
        assert_eq!(storage.count_by_session(&session_id), 0);
    }

    #[test]
    fn test_unregister_middle_command() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("session-{}", uuid::Uuid::new_v4());
        let cmd_ids: Vec<String> = (0..5)
            .map(|i| format!("cmd-{}-{}", i, uuid::Uuid::new_v4()))
            .collect();

        for cmd_id in &cmd_ids {
            storage.register(cmd_id.clone(), create_test_command(cmd_id, &session_id));
        }

        // Remove middle command
        storage.unregister(&cmd_ids[2]);

        assert_eq!(storage.count_by_session(&session_id), 4);
        let remaining = storage.list_by_session(&session_id);
        assert!(!remaining.contains(&cmd_ids[2]));
        assert!(remaining.contains(&cmd_ids[0]));
        assert!(remaining.contains(&cmd_ids[1]));
        assert!(remaining.contains(&cmd_ids[3]));
        assert!(remaining.contains(&cmd_ids[4]));

        // Cleanup
        for cmd_id in &cmd_ids {
            storage.unregister(cmd_id);
        }
    }

    #[test]
    fn test_double_unregister() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("session-{}", uuid::Uuid::new_v4());
        let cmd_id = format!("cmd-{}", uuid::Uuid::new_v4());

        storage.register(cmd_id.clone(), create_test_command(&cmd_id, &session_id));

        // First unregister succeeds
        let first = storage.unregister(&cmd_id);
        assert!(first.is_some());

        // Second unregister returns None
        let second = storage.unregister(&cmd_id);
        assert!(second.is_none());
    }

    #[test]
    fn test_register_same_command_id_twice() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("session-{}", uuid::Uuid::new_v4());
        let cmd_id = format!("cmd-{}", uuid::Uuid::new_v4());

        storage.register(cmd_id.clone(), create_test_command(&cmd_id, &session_id));

        // Register with same ID (this will overwrite in DashMap)
        storage.register(cmd_id.clone(), create_test_command(&cmd_id, &session_id));

        // Count may increase in secondary index if not handled
        // Since HashSet is used, duplicate session entries are prevented
        // But the command entry count stays at 1 in primary storage
        let cmd = storage.get(&cmd_id);
        assert!(cmd.is_some());

        // Cleanup
        storage.unregister(&cmd_id);
    }

    #[test]
    fn test_list_all_with_multiple_statuses() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("session-{}", uuid::Uuid::new_v4());

        let running_id = format!("running-{}", uuid::Uuid::new_v4());
        let completed_id = format!("completed-{}", uuid::Uuid::new_v4());
        let cancelled_id = format!("cancelled-{}", uuid::Uuid::new_v4());
        let failed_id = format!("failed-{}", uuid::Uuid::new_v4());

        storage.register(
            running_id.clone(),
            create_test_command_with_status(&running_id, &session_id, AsyncCommandStatus::Running),
        );
        storage.register(
            completed_id.clone(),
            create_test_command_with_status(
                &completed_id,
                &session_id,
                AsyncCommandStatus::Completed,
            ),
        );
        storage.register(
            cancelled_id.clone(),
            create_test_command_with_status(
                &cancelled_id,
                &session_id,
                AsyncCommandStatus::Cancelled,
            ),
        );
        storage.register(
            failed_id.clone(),
            create_test_command_with_status(&failed_id, &session_id, AsyncCommandStatus::Failed),
        );

        let all = storage.list_all();

        // Filter for our commands
        let our_cmds: Vec<_> = all
            .iter()
            .filter(|c| {
                c.command_id == running_id
                    || c.command_id == completed_id
                    || c.command_id == cancelled_id
                    || c.command_id == failed_id
            })
            .collect();

        assert_eq!(our_cmds.len(), 4);

        // Verify statuses
        assert!(
            our_cmds
                .iter()
                .any(|c| c.command_id == running_id && c.status == AsyncCommandStatus::Running)
        );
        assert!(
            our_cmds
                .iter()
                .any(|c| c.command_id == completed_id && c.status == AsyncCommandStatus::Completed)
        );
        assert!(
            our_cmds
                .iter()
                .any(|c| c.command_id == cancelled_id && c.status == AsyncCommandStatus::Cancelled)
        );
        assert!(
            our_cmds
                .iter()
                .any(|c| c.command_id == failed_id && c.status == AsyncCommandStatus::Failed)
        );

        // Cleanup
        storage.unregister(&running_id);
        storage.unregister(&completed_id);
        storage.unregister(&cancelled_id);
        storage.unregister(&failed_id);
    }

    #[test]
    fn test_empty_session_id() {
        let storage = DashMapCommandStorage::new();
        let empty_session = "";
        let cmd_id = format!("cmd-{}", uuid::Uuid::new_v4());

        storage.register(cmd_id.clone(), create_test_command(&cmd_id, empty_session));

        assert_eq!(storage.count_by_session(empty_session), 1);
        assert!(storage.list_by_session(empty_session).contains(&cmd_id));

        // Cleanup
        storage.unregister(&cmd_id);
    }

    #[test]
    fn test_empty_command_id() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("session-{}", uuid::Uuid::new_v4());
        let empty_cmd = "";

        storage.register(
            empty_cmd.to_string(),
            create_test_command(empty_cmd, &session_id),
        );

        assert!(storage.get(empty_cmd).is_some());

        // Cleanup
        storage.unregister(empty_cmd);
    }

    #[test]
    fn test_unicode_ids() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("会话-{}", uuid::Uuid::new_v4());
        let cmd_id = format!("命令-{}", uuid::Uuid::new_v4());

        storage.register(cmd_id.clone(), create_test_command(&cmd_id, &session_id));

        assert!(storage.get(&cmd_id).is_some());
        assert_eq!(storage.count_by_session(&session_id), 1);

        // Cleanup
        storage.unregister(&cmd_id);
    }

    #[test]
    fn test_storage_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DashMapCommandStorage>();
    }

    #[test]
    fn test_iter_filter_by_session() {
        let storage = DashMapCommandStorage::new();
        let session_id_1 = format!("session-1-{}", uuid::Uuid::new_v4());
        let session_id_2 = format!("session-2-{}", uuid::Uuid::new_v4());

        let cmd_1 = format!("cmd-1-{}", uuid::Uuid::new_v4());
        let cmd_2 = format!("cmd-2-{}", uuid::Uuid::new_v4());
        let cmd_3 = format!("cmd-3-{}", uuid::Uuid::new_v4());

        storage.register(cmd_1.clone(), create_test_command(&cmd_1, &session_id_1));
        storage.register(cmd_2.clone(), create_test_command(&cmd_2, &session_id_1));
        storage.register(cmd_3.clone(), create_test_command(&cmd_3, &session_id_2));

        // Use iter to filter by session
        let session_1_cmds: Vec<_> = storage
            .iter()
            .filter(|e| e.info.session_id == session_id_1)
            .map(|e| e.key().clone())
            .collect();

        assert_eq!(session_1_cmds.len(), 2);
        assert!(session_1_cmds.contains(&cmd_1));
        assert!(session_1_cmds.contains(&cmd_2));

        // Cleanup
        storage.unregister(&cmd_1);
        storage.unregister(&cmd_2);
        storage.unregister(&cmd_3);
    }

    #[test]
    fn test_get_direct_returns_reference() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("session-{}", uuid::Uuid::new_v4());
        let cmd_id = format!("cmd-{}", uuid::Uuid::new_v4());

        storage.register(cmd_id.clone(), create_test_command(&cmd_id, &session_id));

        // Scope the reference to release the read lock before unregister
        {
            let direct_ref = storage.get_direct(&cmd_id);
            assert!(direct_ref.is_some());

            if let Some(ref guard) = direct_ref {
                assert_eq!(guard.info.command_id, cmd_id);
                assert_eq!(guard.info.session_id, session_id);
            }
        }

        // Cleanup - now safe since read lock is released
        storage.unregister(&cmd_id);
    }

    #[test]
    fn test_list_filtered_by_session() {
        let storage = DashMapCommandStorage::new();
        let session_id_1 = format!("session-1-{}", uuid::Uuid::new_v4());
        let session_id_2 = format!("session-2-{}", uuid::Uuid::new_v4());

        let cmd_1 = format!("cmd-1-{}", uuid::Uuid::new_v4());
        let cmd_2 = format!("cmd-2-{}", uuid::Uuid::new_v4());
        let cmd_3 = format!("cmd-3-{}", uuid::Uuid::new_v4());

        storage.register(cmd_1.clone(), create_test_command(&cmd_1, &session_id_1));
        storage.register(cmd_2.clone(), create_test_command(&cmd_2, &session_id_1));
        storage.register(cmd_3.clone(), create_test_command(&cmd_3, &session_id_2));

        // Filter by session_id_1
        let filtered = storage.list_filtered(Some(&session_id_1), None);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().any(|c| c.command_id == cmd_1));
        assert!(filtered.iter().any(|c| c.command_id == cmd_2));

        // Filter by session_id_2
        let filtered = storage.list_filtered(Some(&session_id_2), None);
        assert_eq!(filtered.len(), 1);
        assert!(filtered.iter().any(|c| c.command_id == cmd_3));

        // Cleanup
        storage.unregister(&cmd_1);
        storage.unregister(&cmd_2);
        storage.unregister(&cmd_3);
    }

    #[test]
    fn test_list_filtered_by_status() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("session-{}", uuid::Uuid::new_v4());

        let running_id = format!("running-{}", uuid::Uuid::new_v4());
        let completed_id = format!("completed-{}", uuid::Uuid::new_v4());

        storage.register(
            running_id.clone(),
            create_test_command_with_status(&running_id, &session_id, AsyncCommandStatus::Running),
        );
        storage.register(
            completed_id.clone(),
            create_test_command_with_status(
                &completed_id,
                &session_id,
                AsyncCommandStatus::Completed,
            ),
        );

        // Filter by Running status
        let running = storage.list_filtered(None, Some(AsyncCommandStatus::Running));
        let our_running: Vec<_> = running
            .iter()
            .filter(|c| c.command_id == running_id)
            .collect();
        assert_eq!(our_running.len(), 1);

        // Filter by Completed status
        let completed = storage.list_filtered(None, Some(AsyncCommandStatus::Completed));
        let our_completed: Vec<_> = completed
            .iter()
            .filter(|c| c.command_id == completed_id)
            .collect();
        assert_eq!(our_completed.len(), 1);

        // Cleanup
        storage.unregister(&running_id);
        storage.unregister(&completed_id);
    }

    #[test]
    fn test_list_filtered_by_session_and_status() {
        let storage = DashMapCommandStorage::new();
        let session_id_1 = format!("session-1-{}", uuid::Uuid::new_v4());
        let session_id_2 = format!("session-2-{}", uuid::Uuid::new_v4());

        let cmd_1 = format!("cmd-1-{}", uuid::Uuid::new_v4());
        let cmd_2 = format!("cmd-2-{}", uuid::Uuid::new_v4());
        let cmd_3 = format!("cmd-3-{}", uuid::Uuid::new_v4());

        storage.register(
            cmd_1.clone(),
            create_test_command_with_status(&cmd_1, &session_id_1, AsyncCommandStatus::Running),
        );
        storage.register(
            cmd_2.clone(),
            create_test_command_with_status(&cmd_2, &session_id_1, AsyncCommandStatus::Completed),
        );
        storage.register(
            cmd_3.clone(),
            create_test_command_with_status(&cmd_3, &session_id_2, AsyncCommandStatus::Running),
        );

        // Filter by session_id_1 and Running status
        let filtered =
            storage.list_filtered(Some(&session_id_1), Some(AsyncCommandStatus::Running));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].command_id, cmd_1);

        // Filter by session_id_1 and Completed status
        let filtered =
            storage.list_filtered(Some(&session_id_1), Some(AsyncCommandStatus::Completed));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].command_id, cmd_2);

        // Cleanup
        storage.unregister(&cmd_1);
        storage.unregister(&cmd_2);
        storage.unregister(&cmd_3);
    }

    #[test]
    fn test_list_filtered_no_filters() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("session-{}", uuid::Uuid::new_v4());
        let cmd_id = format!("cmd-{}", uuid::Uuid::new_v4());

        storage.register(cmd_id.clone(), create_test_command(&cmd_id, &session_id));

        // No filters should return all (at least our command)
        let all = storage.list_filtered(None, None);
        assert!(all.iter().any(|c| c.command_id == cmd_id));

        // Cleanup
        storage.unregister(&cmd_id);
    }

    #[test]
    fn test_list_filtered_empty_result() {
        let storage = DashMapCommandStorage::new();
        let session_id = format!("session-{}", uuid::Uuid::new_v4());
        let cmd_id = format!("cmd-{}", uuid::Uuid::new_v4());

        storage.register(
            cmd_id.clone(),
            create_test_command_with_status(&cmd_id, &session_id, AsyncCommandStatus::Running),
        );

        // Filter by Cancelled status (our command is Running)
        let filtered =
            storage.list_filtered(Some(&session_id), Some(AsyncCommandStatus::Cancelled));
        assert!(filtered.is_empty());

        // Cleanup
        storage.unregister(&cmd_id);
    }

    #[test]
    fn test_list_filtered_nonexistent_session() {
        let storage = DashMapCommandStorage::new();
        let unique_session = format!("nonexistent-{}", uuid::Uuid::new_v4());

        let filtered = storage.list_filtered(Some(&unique_session), None);
        assert!(filtered.is_empty());
    }
}
