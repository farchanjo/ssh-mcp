//! DashMap-based command storage implementation.
//!
//! Provides lock-free concurrent access to async commands using `DashMap`.
//! Includes a secondary index for O(1) session-to-commands lookups.

use std::collections::HashSet;
use std::sync::Arc;

use dashmap::DashMap;
use once_cell::sync::Lazy;

use crate::mcp::async_command::RunningCommand;
use crate::mcp::types::AsyncCommandInfo;

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
    /// This is needed for operations that require iterating over all commands
    /// with filtering, which the trait interface doesn't expose directly.
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
}
