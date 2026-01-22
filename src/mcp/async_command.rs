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
//! - Maximum 10 concurrent async commands per session
//! - Completed commands are automatically cleaned up after 5 minutes

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

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

/// Global storage for async commands
pub static ASYNC_COMMANDS: Lazy<Mutex<HashMap<String, RunningCommand>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Maximum number of concurrent async commands per session
pub const MAX_ASYNC_COMMANDS_PER_SESSION: usize = 10;

/// Count async commands for a specific session
pub async fn count_session_commands(session_id: &str) -> usize {
    let commands = ASYNC_COMMANDS.lock().await;
    commands
        .values()
        .filter(|cmd| cmd.info.session_id == session_id)
        .count()
}

/// Get all command IDs for a session (for cleanup during disconnect)
pub async fn get_session_command_ids(session_id: &str) -> Vec<String> {
    let commands = ASYNC_COMMANDS.lock().await;
    commands
        .values()
        .filter(|cmd| cmd.info.session_id == session_id)
        .map(|cmd| cmd.info.command_id.clone())
        .collect()
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
            assert_eq!(MAX_ASYNC_COMMANDS_PER_SESSION, 10);
        }

        #[test]
        fn test_max_commands_is_reasonable() {
            assert!(MAX_ASYNC_COMMANDS_PER_SESSION >= 5);
            assert!(MAX_ASYNC_COMMANDS_PER_SESSION <= 100);
        }
    }

    mod helper_functions {
        use super::*;

        #[tokio::test]
        async fn test_count_session_commands_empty() {
            // Use unique session ID to avoid interference from parallel tests
            let unique_session = format!("nonexistent-session-{}", uuid::Uuid::new_v4());
            let count = count_session_commands(&unique_session).await;
            assert_eq!(count, 0);
        }

        #[tokio::test]
        async fn test_get_session_command_ids_empty() {
            // Use unique session ID to avoid interference from parallel tests
            let unique_session = format!("nonexistent-session-{}", uuid::Uuid::new_v4());
            let ids = get_session_command_ids(&unique_session).await;
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

            // Setup test data
            {
                let mut commands = ASYNC_COMMANDS.lock().await;

                let (tx, rx) = tokio::sync::watch::channel(AsyncCommandStatus::Running);

                // Add a test command
                commands.insert(
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
                        status_rx: rx.clone(),
                        status_tx: tx.clone(),
                        output: Arc::new(Mutex::new(OutputBuffer::default())),
                        exit_code: Arc::new(Mutex::new(None)),
                        error: Arc::new(Mutex::new(None)),
                        timed_out: Arc::new(AtomicBool::new(false)),
                    },
                );

                // Add another command for same session
                let (tx2, rx2) = tokio::sync::watch::channel(AsyncCommandStatus::Running);
                commands.insert(
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
                commands.insert(
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
            }

            // Test count_session_commands
            let count = count_session_commands(&test_session).await;
            assert_eq!(count, 2);

            let other_count = count_session_commands(&other_session).await;
            assert_eq!(other_count, 1);

            // Test get_session_command_ids
            let ids = get_session_command_ids(&test_session).await;
            assert_eq!(ids.len(), 2);
            assert!(ids.contains(&cmd1_id));
            assert!(ids.contains(&cmd2_id));

            let other_ids = get_session_command_ids(&other_session).await;
            assert_eq!(other_ids.len(), 1);
            assert!(other_ids.contains(&cmd3_id));

            // Cleanup only our test commands
            {
                let mut commands = ASYNC_COMMANDS.lock().await;
                commands.remove(&cmd1_id);
                commands.remove(&cmd2_id);
                commands.remove(&cmd3_id);
            }
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
