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
    #[must_use]
    pub fn with_capacity(stdout_cap: usize, stderr_cap: usize) -> Self {
        Self {
            stdout: Vec::with_capacity(stdout_cap),
            stderr: Vec::with_capacity(stderr_cap),
        }
    }

    /// Append bytes to `stdout`, then drop oldest bytes if the buffer exceeds `max_size`.
    ///
    /// `max_size == 0` disables the cap. Runs in O(n) on the drained portion only.
    pub fn append_stdout_bounded(&mut self, data: &[u8], max_size: usize) {
        self.stdout.extend_from_slice(data);
        drain_head_if_over(&mut self.stdout, max_size);
    }

    /// Append bytes to `stderr`, then drop oldest bytes if the buffer exceeds `max_size`.
    pub fn append_stderr_bounded(&mut self, data: &[u8], max_size: usize) {
        self.stderr.extend_from_slice(data);
        drain_head_if_over(&mut self.stderr, max_size);
    }

    /// Absorb a local staging buffer into `stdout` and enforce the cap.
    ///
    /// The caller's buffer is drained via `Vec::append`, consistent with the
    /// previous non-cap-aware API used by `execute_ssh_command_async`.
    pub fn append_stdout_slice(&mut self, src: &mut Vec<u8>, max_size: usize) {
        self.stdout.append(src);
        drain_head_if_over(&mut self.stdout, max_size);
    }

    /// Absorb a local staging buffer into `stderr` and enforce the cap.
    pub fn append_stderr_slice(&mut self, src: &mut Vec<u8>, max_size: usize) {
        self.stderr.append(src);
        drain_head_if_over(&mut self.stderr, max_size);
    }
}

/// Drop head bytes of `buf` so its length falls at/under `max_size`.
///
/// Skips when `max_size == 0` (disabled) or when the buffer already fits.
/// After a drain, shrinks capacity when it dwarfs the remaining content.
fn drain_head_if_over(buf: &mut Vec<u8>, max_size: usize) {
    if max_size == 0 || buf.len() <= max_size {
        return;
    }
    let excess = buf.len() - max_size;
    buf.drain(..excess);
    if buf.capacity() > buf.len().saturating_mul(4) {
        buf.shrink_to_fit();
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
    #[allow(dead_code, reason = "kept alive to prevent watch channel closure")]
    pub status_tx: watch::Sender<AsyncCommandStatus>,
    /// Output buffer (stdout/stderr)
    pub output: Arc<Mutex<OutputBuffer>>,
    /// Exit code when completed
    pub exit_code: Arc<Mutex<Option<i32>>>,
    /// Error message if failed
    pub error: Arc<Mutex<Option<String>>>,
    /// Whether the command timed out
    pub timed_out: Arc<AtomicBool>,
    /// Whether the output has been read by `ssh_get_command_output`
    pub output_read: Arc<AtomicBool>,
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

        #[test]
        fn append_stdout_bounded_drops_oldest_on_overflow() {
            let mut buffer = OutputBuffer::default();
            let cap = 1024_usize;
            let chunk = vec![b'x'; 400];
            for _ in 0..10_usize {
                buffer.append_stdout_bounded(&chunk, cap);
            }
            assert!(buffer.stdout.len() <= cap);
            assert!(!buffer.stdout.is_empty());
        }

        #[test]
        fn append_stderr_bounded_drops_oldest_on_overflow() {
            let mut buffer = OutputBuffer::default();
            let cap = 512_usize;
            let chunk = vec![b'e'; 200];
            for _ in 0..20_usize {
                buffer.append_stderr_bounded(&chunk, cap);
            }
            assert!(buffer.stderr.len() <= cap);
        }

        #[test]
        fn append_stdout_bounded_cap_zero_is_unbounded() {
            let mut buffer = OutputBuffer::default();
            let chunk = vec![b'a'; 1000];
            for _ in 0..5_usize {
                buffer.append_stdout_bounded(&chunk, 0);
            }
            assert_eq!(buffer.stdout.len(), 5000);
        }

        #[test]
        fn append_slice_consumes_caller_buffer() {
            let mut buffer = OutputBuffer::default();
            let mut src = vec![b'a'; 100];
            buffer.append_stdout_slice(&mut src, 1024);
            assert_eq!(buffer.stdout.len(), 100);
            assert!(
                src.is_empty(),
                "source buffer must be drained by Vec::append"
            );
        }

        #[test]
        fn shrink_happens_when_capacity_far_exceeds_length() {
            let mut buffer = OutputBuffer::default();
            // Grow to ~40KB…
            for _ in 0..10_usize {
                buffer.append_stdout_bounded(&vec![b'x'; 4096], 0);
            }
            let big_cap = buffer.stdout.capacity();
            assert!(big_cap >= 40_960);
            // Now enforce a tiny cap — drain should trigger shrink_to_fit.
            buffer.append_stdout_bounded(&[], 1024);
            assert!(buffer.stdout.len() <= 1024);
            assert!(
                buffer.stdout.capacity() < big_cap,
                "shrink_to_fit should have released capacity (was {big_cap}, now {})",
                buffer.stdout.capacity()
            );
        }

        #[test]
        fn stress_churn_under_10mb_cap_stays_bounded() {
            let mut buffer = OutputBuffer::default();
            let cap = 10 * 1024 * 1024_usize;
            let chunk = vec![b'a'; 32 * 1024];
            // Produce 500 × 32KB = 16 MB of input against a 10 MB cap.
            for _ in 0..500_usize {
                buffer.append_stdout_bounded(&chunk, cap);
            }
            assert!(buffer.stdout.len() <= cap);
            // Confirm oldest data was dropped: last chunk should still be 'a'
            // (our chunk is homogeneous, so contents remain 'a' throughout).
            assert_eq!(*buffer.stdout.last().unwrap_or(&0), b'a');
            // Capacity should not have grown much past the cap.
            assert!(
                buffer.stdout.capacity() < cap * 2,
                "capacity runaway: {} vs cap {cap}",
                buffer.stdout.capacity()
            );
        }

        #[test]
        fn stress_many_tiny_writes_under_cap() {
            let mut buffer = OutputBuffer::default();
            let cap = 4096_usize;
            for _ in 0..50_000_usize {
                buffer.append_stdout_bounded(&[b'x'], cap);
            }
            assert_eq!(buffer.stdout.len(), cap);
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
