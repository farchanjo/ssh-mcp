//! Async command storage and management.
//!
//! This module provides storage and management for long-running SSH commands
//! that execute asynchronously. Commands can be polled for output, cancelled,
//! and listed.
//!
//! # Architecture
//!
//! - `RunningCommand`: Contains all state for an async command including
//!   output buffers, cancellation token, and status.
//! - `ASYNC_COMMANDS`: Global static storage for all async commands, keyed by UUID.
//!
//! # Limits
//!
//! - Maximum 30 concurrent async commands per session
//! - Completed commands are automatically cleaned up after 5 minutes

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use dashmap::DashMap;
use once_cell::sync::Lazy;
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

use super::types::{AsyncCommandInfo, AsyncCommandStatus};

/// Output buffer for collecting command output
#[derive(Debug, Default)]
pub struct OutputBuffer {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl OutputBuffer {
    /// Create a new output buffer with pre-allocated capacity.
    ///
    /// Pre-allocating reduces reallocations during output collection.
    pub fn with_capacity(stdout_cap: usize, stderr_cap: usize) -> Self {
        Self {
            stdout: Vec::with_capacity(stdout_cap),
            stderr: Vec::with_capacity(stderr_cap),
        }
    }
}

/// State for a running async command
pub struct RunningCommand {
    /// Command metadata
    pub info: AsyncCommandInfo,
    /// Token to cancel the command
    pub cancel_token: CancellationToken,
    /// Receiver for status updates
    pub status_rx: watch::Receiver<AsyncCommandStatus>,
    /// Sender for status updates (kept alive to prevent channel closure)
    #[allow(dead_code)]
    pub status_tx: watch::Sender<AsyncCommandStatus>,
    /// Output buffer (stdout/stderr)
    pub output: Arc<Mutex<OutputBuffer>>,
    /// Exit code when completed
    pub exit_code: Arc<Mutex<Option<i32>>>,
    /// Error message if failed
    pub error: Arc<Mutex<Option<String>>>,
    /// Whether the command timed out
    pub timed_out: Arc<AtomicBool>,
}

/// Global storage for async commands using lock-free DashMap.
pub static ASYNC_COMMANDS: Lazy<DashMap<String, RunningCommand>> = Lazy::new(DashMap::new);

/// Secondary index: session_id -> set of command_ids for O(1) session lookup.
pub static COMMANDS_BY_SESSION: Lazy<DashMap<String, HashSet<String>>> = Lazy::new(DashMap::new);

/// Maximum number of concurrent async commands per session
pub const MAX_ASYNC_COMMANDS_PER_SESSION: usize = 30;

/// Count async commands for a specific session (O(1) lookup).
pub fn count_session_commands(session_id: &str) -> usize {
    COMMANDS_BY_SESSION
        .get(session_id)
        .map(|set| set.len())
        .unwrap_or(0)
}

/// Get all command IDs for a session (for cleanup during disconnect).
pub fn get_session_command_ids(session_id: &str) -> Vec<String> {
    COMMANDS_BY_SESSION
        .get(session_id)
        .map(|set| set.iter().cloned().collect())
        .unwrap_or_default()
}

/// Register a new async command with proper indexing.
pub fn register_command(command_id: String, cmd: RunningCommand) {
    let session_id = cmd.info.session_id.clone();

    // Insert into primary storage
    ASYNC_COMMANDS.insert(command_id.clone(), cmd);

    // Update secondary index
    COMMANDS_BY_SESSION
        .entry(session_id)
        .or_default()
        .insert(command_id);
}

/// Unregister an async command and clean up indexes.
pub fn unregister_command(command_id: &str) -> Option<RunningCommand> {
    // Remove from primary storage
    let removed = ASYNC_COMMANDS.remove(command_id).map(|(_, cmd)| cmd);

    // Update secondary index if command was found
    if let Some(ref cmd) = removed
        && let Some(mut set) = COMMANDS_BY_SESSION.get_mut(&cmd.info.session_id)
    {
        set.remove(command_id);
        // Clean up empty sets
        if set.is_empty() {
            drop(set);
            COMMANDS_BY_SESSION.remove(&cmd.info.session_id);
        }
    }

    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    mod output_buffer {
        use super::*;

        #[test]
        fn test_default() {
            let buffer = OutputBuffer::default();
            assert!(buffer.stdout.is_empty());
            assert!(buffer.stderr.is_empty());
        }

        #[test]
        fn test_with_capacity() {
            let buffer = OutputBuffer::with_capacity(4096, 1024);
            assert!(buffer.stdout.is_empty());
            assert!(buffer.stderr.is_empty());
            assert!(buffer.stdout.capacity() >= 4096);
            assert!(buffer.stderr.capacity() >= 1024);
        }

        #[test]
        fn test_extend_stdout() {
            let mut buffer = OutputBuffer::default();
            buffer.stdout.extend_from_slice(b"hello");
            buffer.stdout.extend_from_slice(b" world");
            assert_eq!(buffer.stdout, b"hello world");
        }

        #[test]
        fn test_extend_stderr() {
            let mut buffer = OutputBuffer::default();
            buffer.stderr.extend_from_slice(b"error: ");
            buffer.stderr.extend_from_slice(b"something failed");
            assert_eq!(buffer.stderr, b"error: something failed");
        }
    }

    mod constants {
        use super::*;

        #[test]
        fn test_max_async_commands_per_session() {
            assert_eq!(MAX_ASYNC_COMMANDS_PER_SESSION, 30);
        }

        #[test]
        fn test_max_commands_is_reasonable() {
            assert!(MAX_ASYNC_COMMANDS_PER_SESSION >= 5);
            assert!(MAX_ASYNC_COMMANDS_PER_SESSION <= 100);
        }
    }

    mod helper_functions {
        use super::*;

        #[test]
        fn test_count_session_commands_empty() {
            // Use unique session ID to avoid interference from parallel tests
            let unique_session = format!("nonexistent-session-{}", uuid::Uuid::new_v4());
            let count = count_session_commands(&unique_session);
            assert_eq!(count, 0);
        }

        #[test]
        fn test_get_session_command_ids_empty() {
            // Use unique session ID to avoid interference from parallel tests
            let unique_session = format!("nonexistent-session-{}", uuid::Uuid::new_v4());
            let ids = get_session_command_ids(&unique_session);
            assert!(ids.is_empty());
        }

        #[tokio::test]
        async fn test_count_and_get_with_commands() {
            // Use unique session IDs to avoid interference from parallel tests
            let test_session = format!("test-session-{}", uuid::Uuid::new_v4());
            let other_session = format!("other-session-{}", uuid::Uuid::new_v4());
            let cmd1_id = format!("test-cmd-1-{}", uuid::Uuid::new_v4());
            let cmd2_id = format!("test-cmd-2-{}", uuid::Uuid::new_v4());
            let cmd3_id = format!("test-cmd-3-{}", uuid::Uuid::new_v4());

            // Setup test data using register_command
            let (tx1, rx1) = tokio::sync::watch::channel(AsyncCommandStatus::Running);
            register_command(
                cmd1_id.clone(),
                RunningCommand {
                    info: AsyncCommandInfo {
                        command_id: cmd1_id.clone(),
                        session_id: test_session.clone(),
                        command: "echo test".to_string(),
                        status: AsyncCommandStatus::Running,
                        started_at: "2024-01-15T10:30:00Z".to_string(),
                    },
                    cancel_token: CancellationToken::new(),
                    status_rx: rx1,
                    status_tx: tx1,
                    output: Arc::new(Mutex::new(OutputBuffer::default())),
                    exit_code: Arc::new(Mutex::new(None)),
                    error: Arc::new(Mutex::new(None)),
                    timed_out: Arc::new(AtomicBool::new(false)),
                },
            );

            // Add another command for same session
            let (tx2, rx2) = tokio::sync::watch::channel(AsyncCommandStatus::Running);
            register_command(
                cmd2_id.clone(),
                RunningCommand {
                    info: AsyncCommandInfo {
                        command_id: cmd2_id.clone(),
                        session_id: test_session.clone(),
                        command: "ls -la".to_string(),
                        status: AsyncCommandStatus::Running,
                        started_at: "2024-01-15T10:31:00Z".to_string(),
                    },
                    cancel_token: CancellationToken::new(),
                    status_rx: rx2,
                    status_tx: tx2,
                    output: Arc::new(Mutex::new(OutputBuffer::default())),
                    exit_code: Arc::new(Mutex::new(None)),
                    error: Arc::new(Mutex::new(None)),
                    timed_out: Arc::new(AtomicBool::new(false)),
                },
            );

            // Add command for different session
            let (tx3, rx3) = tokio::sync::watch::channel(AsyncCommandStatus::Running);
            register_command(
                cmd3_id.clone(),
                RunningCommand {
                    info: AsyncCommandInfo {
                        command_id: cmd3_id.clone(),
                        session_id: other_session.clone(),
                        command: "pwd".to_string(),
                        status: AsyncCommandStatus::Running,
                        started_at: "2024-01-15T10:32:00Z".to_string(),
                    },
                    cancel_token: CancellationToken::new(),
                    status_rx: rx3,
                    status_tx: tx3,
                    output: Arc::new(Mutex::new(OutputBuffer::default())),
                    exit_code: Arc::new(Mutex::new(None)),
                    error: Arc::new(Mutex::new(None)),
                    timed_out: Arc::new(AtomicBool::new(false)),
                },
            );

            // Test count_session_commands (now sync, O(1))
            let count = count_session_commands(&test_session);
            assert_eq!(count, 2);

            let other_count = count_session_commands(&other_session);
            assert_eq!(other_count, 1);

            // Test get_session_command_ids (now sync, O(1))
            let ids = get_session_command_ids(&test_session);
            assert_eq!(ids.len(), 2);
            assert!(ids.contains(&cmd1_id));
            assert!(ids.contains(&cmd2_id));

            let other_ids = get_session_command_ids(&other_session);
            assert_eq!(other_ids.len(), 1);
            assert!(other_ids.contains(&cmd3_id));

            // Cleanup using unregister_command
            unregister_command(&cmd1_id);
            unregister_command(&cmd2_id);
            unregister_command(&cmd3_id);

            // Verify cleanup
            assert_eq!(count_session_commands(&test_session), 0);
            assert_eq!(count_session_commands(&other_session), 0);
        }

        #[test]
        fn test_register_and_unregister() {
            let session_id = format!("test-register-{}", uuid::Uuid::new_v4());
            let cmd_id = format!("test-cmd-{}", uuid::Uuid::new_v4());

            let (tx, rx) = tokio::sync::watch::channel(AsyncCommandStatus::Running);
            register_command(
                cmd_id.clone(),
                RunningCommand {
                    info: AsyncCommandInfo {
                        command_id: cmd_id.clone(),
                        session_id: session_id.clone(),
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
                },
            );

            assert_eq!(count_session_commands(&session_id), 1);
            assert!(ASYNC_COMMANDS.contains_key(&cmd_id));

            let removed = unregister_command(&cmd_id);
            assert!(removed.is_some());
            assert_eq!(count_session_commands(&session_id), 0);
            assert!(!ASYNC_COMMANDS.contains_key(&cmd_id));
        }
    }

    mod running_command {
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
            let (tx, mut rx) = tokio::sync::watch::channel(AsyncCommandStatus::Running);

            assert_eq!(*rx.borrow(), AsyncCommandStatus::Running);

            tx.send(AsyncCommandStatus::Completed).unwrap();
            rx.changed().await.unwrap();
            assert_eq!(*rx.borrow(), AsyncCommandStatus::Completed);
        }

        #[tokio::test]
        async fn test_output_buffer_concurrent_access() {
            let output = Arc::new(Mutex::new(OutputBuffer::default()));

            let output1 = output.clone();
            let output2 = output.clone();

            let handle1 = tokio::spawn(async move {
                let mut buf = output1.lock().await;
                buf.stdout.extend_from_slice(b"from task 1");
            });

            let handle2 = tokio::spawn(async move {
                let mut buf = output2.lock().await;
                buf.stderr.extend_from_slice(b"from task 2");
            });

            handle1.await.unwrap();
            handle2.await.unwrap();

            let buf = output.lock().await;
            assert_eq!(buf.stdout, b"from task 1");
            assert_eq!(buf.stderr, b"from task 2");
        }

        #[tokio::test]
        async fn test_timed_out_atomic() {
            let timed_out = Arc::new(AtomicBool::new(false));

            assert!(!timed_out.load(std::sync::atomic::Ordering::SeqCst));

            timed_out.store(true, std::sync::atomic::Ordering::SeqCst);
            assert!(timed_out.load(std::sync::atomic::Ordering::SeqCst));
        }
    }
}
