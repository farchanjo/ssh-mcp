//! Async command storage and management.
//!
//! This module provides types for long-running SSH commands that execute
//! asynchronously. Commands can be polled for output, cancelled, and listed.
//!
//! # Architecture
//!
//! - `RunningCommand`: Contains all state for an async command including
//!   output buffers, cancellation token, and status.
//! - Storage is handled by `storage::CommandStorage` trait implementations.
//!
//! # Limits
//!
//! - Maximum 100 concurrent async commands per session
//! - Completed commands are automatically cleaned up when session disconnects

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

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

/// Maximum number of concurrent async commands (multiplexed channels) per session
pub const MAX_ASYNC_COMMANDS_PER_SESSION: usize = 100;

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
            assert_eq!(MAX_ASYNC_COMMANDS_PER_SESSION, 100);
        }

        #[test]
        fn test_max_commands_is_reasonable() {
            // Should support at least 10 concurrent commands
            assert!(MAX_ASYNC_COMMANDS_PER_SESSION >= 10);
            // Should not exceed SSH multiplexing practical limits
            assert!(MAX_ASYNC_COMMANDS_PER_SESSION <= 256);
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
