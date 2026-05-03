//! Async command storage and management.
//!
//! This module provides types for long-running SSH commands that execute
//! asynchronously. Commands can be polled for output, cancelled, and listed.
//!
//! # Architecture
//!
//! - `RunningCommand`: Lock-free state for an async command. The reader
//!   background task owns its `OutputBuffer` exclusively and publishes
//!   snapshots through `output_history` (`ArcSwap`) plus incremental
//!   chunks via the `output_tx` broadcast channel. Terminal values
//!   (`exit_code`, `error`) use `tokio::sync::OnceCell` for write-once
//!   semantics, eliminating every `Mutex` from the struct.
//! - Storage is handled by `storage::CommandStorage` trait implementations.
//!
//! # Limits
//!
//! - Maximum 100 concurrent async commands per session.
//! - Default broadcast channel capacity: 1024 frames (configurable via
//!   `SSH_COMMAND_BROADCAST_CAP`).
//! - Completed commands are automatically cleaned up when session disconnects.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use arc_swap::ArcSwap;
use bytes::Bytes;
use tokio::sync::{OnceCell, broadcast, watch};
use tokio_util::sync::CancellationToken;

use super::types::{AsyncCommandInfo, AsyncCommandStatus};
use crate::adapters::config::internal::resolve_command_broadcast_cap;

/// Output buffer for collecting command output
#[derive(Debug, Default, Clone)]
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

/// Incremental output frame broadcast to subscribers as the command runs.
///
/// Subscribers consume chunks in order; when they fall behind, the broadcast
/// channel surfaces `RecvError::Lagged`. Recovery is to load a fresh
/// snapshot from `RunningCommand::output_history` and continue subscribing.
///
/// Each chunk carries a `seq` allocated by
/// [`crate::adapters::subscription::legacy::SubscriptionRegistry::next_seq`] so a
/// subscriber that recovers from `Lagged` can detect gaps.
#[derive(Debug, Clone)]
pub enum OutputChunk {
    /// Stdout bytes appended since the previous frame.
    Stdout {
        /// Monotonic per-resource sequence number (starts at 1).
        seq: u64,
        /// Bytes appended since the previous frame.
        data: Bytes,
    },
    /// Stderr bytes appended since the previous frame.
    Stderr {
        /// Monotonic per-resource sequence number (starts at 1).
        seq: u64,
        /// Bytes appended since the previous frame.
        data: Bytes,
    },
    /// Terminal frame. `exit_code` is `None` when the command was cancelled,
    /// failed before producing an exit status, or timed out.
    Closed {
        /// Monotonic per-resource sequence number (starts at 1).
        seq: u64,
        /// Exit code (`None` when missing — see variant docs).
        exit_code: Option<i32>,
    },
}

/// State for a running async command.
///
/// All fields are lock-free: snapshots use `ArcSwap`, terminal values use
/// `OnceCell`, and incremental updates flow through a broadcast channel.
/// The reader task owns its local `OutputBuffer` exclusively and re-publishes
/// it after each chunk.
pub struct RunningCommand {
    /// Command metadata.
    pub info: AsyncCommandInfo,
    /// Token to cancel the command.
    pub cancel_token: CancellationToken,
    /// Receiver for status updates.
    pub status_rx: watch::Receiver<AsyncCommandStatus>,
    /// Sender for status updates (kept alive to prevent channel closure).
    #[allow(dead_code, reason = "kept alive to prevent watch channel closure")]
    pub status_tx: watch::Sender<AsyncCommandStatus>,
    /// Snapshot of the current `OutputBuffer`. The reader task publishes
    /// updates via `output_history.store(Arc::new(new_buf))`; readers obtain
    /// a consistent view via `output_history.load_full()`.
    pub output_history: Arc<ArcSwap<OutputBuffer>>,
    /// Live broadcast channel for incremental chunks. New subscribers join
    /// via `output_tx.subscribe()`.
    pub output_tx: broadcast::Sender<OutputChunk>,
    /// Write-once: set to `Some(code)` when the command terminates with an
    /// exit status.
    pub exit_code: Arc<OnceCell<i32>>,
    /// Write-once: set to a description on failure.
    pub error: Arc<OnceCell<String>>,
    /// Whether the command timed out.
    pub timed_out: Arc<AtomicBool>,
    /// Whether the output has been read by `ssh_get_command_output`.
    pub output_read: Arc<AtomicBool>,
}

impl RunningCommand {
    /// Construct a `RunningCommand` with fresh lock-free primitives.
    ///
    /// The broadcast channel capacity is resolved from `SSH_COMMAND_BROADCAST_CAP`
    /// via [`crate::adapters::config::internal::resolve_command_broadcast_cap`], with a default of
    /// 1024 frames, a floor of 16, and a hard cap of 65536.
    #[must_use]
    pub fn new(info: AsyncCommandInfo) -> Self {
        let (status_tx, status_rx) = watch::channel(AsyncCommandStatus::Running);
        let cap = resolve_command_broadcast_cap();
        let (output_tx, _output_rx) = broadcast::channel(cap);
        Self {
            info,
            cancel_token: CancellationToken::new(),
            status_rx,
            status_tx,
            output_history: Arc::new(ArcSwap::from_pointee(OutputBuffer::with_capacity(
                4096, 1024,
            ))),
            output_tx,
            exit_code: Arc::new(OnceCell::new()),
            error: Arc::new(OnceCell::new()),
            timed_out: Arc::new(AtomicBool::new(false)),
            output_read: Arc::new(AtomicBool::new(false)),
        }
    }
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
        async fn test_arc_swap_output_history_is_lock_free() {
            let history = Arc::new(ArcSwap::from_pointee(OutputBuffer::default()));

            let history_writer = Arc::clone(&history);
            let writer = tokio::spawn(async move {
                let mut buf = OutputBuffer::default();
                buf.append_stdout_bounded(b"first chunk", 0);
                history_writer.store(Arc::new(buf.clone()));
                buf.append_stderr_bounded(b"err chunk", 0);
                history_writer.store(Arc::new(buf));
            });

            writer.await.unwrap();

            let snapshot = history.load_full();
            assert_eq!(snapshot.stdout, b"first chunk");
            assert_eq!(snapshot.stderr, b"err chunk");
        }

        #[tokio::test]
        async fn test_running_command_new_has_lock_free_state() {
            let info = AsyncCommandInfo {
                command_id: "cmd-1".to_string(),
                session_id: "sess-1".to_string(),
                command: "echo".to_string(),
                status: AsyncCommandStatus::Running,
                started_at: "2024-01-15T10:30:00Z".to_string(),
            };
            let cmd = RunningCommand::new(info);

            // OnceCell starts empty.
            assert!(cmd.exit_code.get().is_none());
            assert!(cmd.error.get().is_none());

            // Setting the exit code once succeeds; setting it again is a no-op.
            cmd.exit_code.set(0).unwrap();
            assert_eq!(cmd.exit_code.get(), Some(&0));
            assert!(cmd.exit_code.set(1).is_err());

            // Output history snapshot is reachable without locks.
            let snap = cmd.output_history.load_full();
            assert!(snap.stdout.is_empty());
            assert!(snap.stderr.is_empty());

            // Broadcast channel accepts subscribers.
            let mut sub = cmd.output_tx.subscribe();
            cmd.output_tx
                .send(OutputChunk::Stdout {
                    seq: 1,
                    data: Bytes::from_static(b"hello"),
                })
                .unwrap();
            match sub.recv().await {
                Ok(OutputChunk::Stdout { seq, data }) => {
                    assert_eq!(seq, 1);
                    assert_eq!(data.as_ref(), b"hello");
                }
                Ok(OutputChunk::Stderr { .. } | OutputChunk::Closed { .. }) | Err(_) => {
                    panic!("expected stdout chunk");
                }
            }
        }

        #[tokio::test]
        async fn test_output_chunk_closed_carries_exit_code() {
            let chunk = OutputChunk::Closed {
                seq: 1,
                exit_code: Some(0),
            };
            match chunk {
                OutputChunk::Closed { seq, exit_code } => {
                    assert_eq!(seq, 1);
                    assert_eq!(exit_code, Some(0));
                }
                OutputChunk::Stdout { .. } | OutputChunk::Stderr { .. } => panic!("wrong variant"),
            }
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
