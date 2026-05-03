//! Interactive shell session management.
//!
//! This module provides types for persistent interactive PTY shell sessions.
//! Shells maintain a continuous output buffer that can be read incrementally
//! and accept input via channel writes.
//!
//! # Architecture (v3.0 — lock-free)
//!
//! - `RunningShell`: lock-free state. The reader task owns its local growth
//!   buffer exclusively and republishes it through `history` (`ArcSwap`)
//!   plus incremental chunks via `output_tx` (`broadcast`). Writers send
//!   to a dedicated writer task through an `mpsc::Sender<WriteRequest>`,
//!   so the russh `ChannelWriteHalf` is owned by exactly one task with
//!   no `Mutex` involved.
//! - `RingBuffer`: cheap-to-clone snapshot wrapping a `Bytes` payload.
//! - `WriteRequest`: outbound input frame or close signal.
//! - Storage is handled by `storage::ShellStorage` trait implementations.
//!
//! # Use Cases
//!
//! - Interactive sessions (SOL/IPMI/OOB console access)
//! - Commands requiring PTY allocation (sudo, top, htop)
//! - Persistent shell sessions for multi-step workflows

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use bytes::Bytes;
use russh::ChannelWriteHalf;
use russh::client;
use tokio::sync::{Notify, broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::types::{ShellInfo, ShellStatus};
use crate::mcp::config::resolve_shell_broadcast_cap;

/// Write handle for sending input to a shell channel.
///
/// Wraps `russh::ChannelWriteHalf` to provide a `Send` interface for the
/// dedicated writer task. The task owns this exclusively — there is no
/// `Mutex` around it.
pub struct ChannelWriter {
    pub(crate) write_half: ChannelWriteHalf<client::Msg>,
}

impl ChannelWriter {
    /// Create a new channel writer from a channel write half.
    pub const fn new(write_half: ChannelWriteHalf<client::Msg>) -> Self {
        Self { write_half }
    }

    /// Send data (text, keystrokes, escape sequences) to the shell.
    pub async fn write(&self, data: &[u8]) -> Result<(), String> {
        self.write_half
            .data(data)
            .await
            .map_err(|e| format!("Failed to write to shell: {e}"))
    }

    /// Close the channel gracefully.
    pub async fn close(&self) -> Result<(), String> {
        self.write_half
            .close()
            .await
            .map_err(|e| format!("Failed to close shell channel: {e}"))
    }
}

/// Lightweight rolling buffer holding the most recent N bytes (head-truncated).
///
/// Cloning is cheap because the underlying `Bytes` is reference-counted.
/// Readers obtain a consistent view via `RunningShell::history.load_full()`
/// without locking.
#[derive(Debug, Clone, Default)]
pub struct RingBuffer {
    /// Current contents of the rolling buffer.
    pub data: Bytes,
}

/// Outbound write frame consumed by the dedicated writer task.
///
/// `Data` carries arbitrary input bytes; `Close` requests a graceful
/// channel close and ends the writer loop.
#[derive(Debug)]
pub enum WriteRequest {
    /// Bytes to forward through `ChannelWriter::write`.
    Data(Bytes),
    /// Graceful close request — the writer task closes the channel and exits.
    Close,
}

/// State for a running interactive shell session.
///
/// All fields are lock-free: snapshots use `ArcSwap`, the writer queue
/// uses `mpsc::Sender`, activity tracking is an `AtomicU64` (epoch ms),
/// and live fan-out uses a broadcast channel + `Notify`.
pub struct RunningShell {
    /// Shell metadata.
    pub info: ShellInfo,
    /// Token to cancel the background reader and writer tasks.
    pub cancel_token: CancellationToken,
    /// Lock-free snapshot of the rolling output buffer.
    /// Reader task swaps a fresh `Arc` after each chunk; consumers
    /// (`ssh_shell_read`, future MCP resources) `load_full()` to obtain
    /// a consistent snapshot without locking.
    pub history: Arc<ArcSwap<RingBuffer>>,
    /// Live broadcast of incremental chunks to MCP subscribers and any
    /// long-poll / `wait_for` paths. Capacity from
    /// `SSH_SHELL_BROADCAST_CAP`.
    pub output_tx: broadcast::Sender<Bytes>,
    /// Wake source for intra-server long-poll readers (`ssh_shell_read.wait`,
    /// `ssh_shell_wait_for`). Notified after every successful append.
    pub data_notify: Arc<Notify>,
    /// Sink for input. The dedicated writer task owns the underlying
    /// `ChannelWriter` exclusively — no `Mutex`.
    pub input_tx: mpsc::Sender<WriteRequest>,
    /// Sender for status updates (kept alive to prevent channel closure).
    #[allow(
        dead_code,
        reason = "kept alive to prevent watch channel closure for status_rx"
    )]
    pub status_tx: watch::Sender<ShellStatus>,
    /// Receiver for status updates.
    pub status_rx: watch::Receiver<ShellStatus>,
    /// Last read/write activity in epoch millis. Tracked atomically so
    /// the inactivity monitor can read it without taking a lock.
    pub last_activity_ms: Arc<AtomicU64>,
    /// Maximum output buffer size in bytes (oldest data is truncated when
    /// exceeded). Wrapped in an `Arc<AtomicU64>` so it can be tuned at
    /// runtime without touching the reader task.
    pub max_buffer_size: Arc<AtomicU64>,
}

impl RunningShell {
    /// Build a `RunningShell` with fresh lock-free primitives.
    ///
    /// The broadcast channel capacity is resolved via
    /// [`crate::mcp::config::resolve_shell_broadcast_cap`] (default 1024,
    /// floor 16, hard cap 65536). The mpsc input queue is sized to the
    /// same value so a single noisy producer cannot swamp the writer.
    #[must_use]
    pub fn new(
        info: ShellInfo,
        cancel_token: CancellationToken,
        input_tx: mpsc::Sender<WriteRequest>,
        max_buffer_size: u64,
    ) -> Self {
        let (status_tx, status_rx) = watch::channel(ShellStatus::Open);
        let cap = resolve_shell_broadcast_cap();
        let (output_tx, _output_rx) = broadcast::channel(cap);
        Self {
            info,
            cancel_token,
            history: Arc::new(ArcSwap::from_pointee(RingBuffer::default())),
            output_tx,
            data_notify: Arc::new(Notify::new()),
            input_tx,
            status_tx,
            status_rx,
            last_activity_ms: Arc::new(AtomicU64::new(now_ms())),
            max_buffer_size: Arc::new(AtomicU64::new(max_buffer_size)),
        }
    }
}

/// Maximum number of concurrent shells per session
pub const MAX_SHELLS_PER_SESSION: usize = 10;

/// Current wall-clock time as milliseconds since the Unix epoch.
///
/// Falls back to `0` when the system clock is set before 1970, which
/// is fine for the inactivity TTL comparisons that consume this value.
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Update `last_activity_ms` atomically to "now".
pub fn touch_activity(activity: &AtomicU64) {
    activity.store(now_ms(), Ordering::Relaxed);
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test assertions may use unwrap on infallible mpsc / broadcast / watch operations"
)]
mod tests {
    use super::*;

    mod constants {
        use super::*;

        #[test]
        fn test_max_shells_per_session() {
            assert_eq!(MAX_SHELLS_PER_SESSION, 10);
        }

        #[test]
        fn test_max_shells_is_reasonable() {
            assert!(MAX_SHELLS_PER_SESSION >= 1);
            assert!(MAX_SHELLS_PER_SESSION <= 50);
        }
    }

    mod running_shell {
        use super::*;
        use crate::adapters::ssh::internal::types::ShellInfo;

        fn sample_info() -> ShellInfo {
            ShellInfo {
                shell_id: "shell-1".to_string(),
                session_id: "sess-1".to_string(),
                term_type: "xterm".to_string(),
                cols: 80,
                rows: 24,
                opened_at: "2024-01-15T10:30:00Z".to_string(),
            }
        }

        #[tokio::test]
        async fn test_cancellation_token() {
            let token = CancellationToken::new();
            assert!(!token.is_cancelled());

            token.cancel();
            assert!(token.is_cancelled());
        }

        #[tokio::test]
        async fn test_status_watch_channel() {
            let (tx, mut rx) = watch::channel(ShellStatus::Open);

            assert_eq!(*rx.borrow(), ShellStatus::Open);

            tx.send(ShellStatus::Closed).unwrap();
            rx.changed().await.unwrap();
            assert_eq!(*rx.borrow(), ShellStatus::Closed);
        }

        #[tokio::test]
        async fn test_history_arc_swap_is_lock_free() {
            let history = Arc::new(ArcSwap::from_pointee(RingBuffer::default()));

            let writer_history = Arc::clone(&history);
            let writer = tokio::spawn(async move {
                writer_history.store(Arc::new(RingBuffer {
                    data: Bytes::from_static(b"first"),
                }));
                writer_history.store(Arc::new(RingBuffer {
                    data: Bytes::from_static(b"second chunk"),
                }));
            });
            writer.await.unwrap();

            let snap = history.load_full();
            assert_eq!(snap.data.as_ref(), b"second chunk");
        }

        #[tokio::test]
        async fn test_running_shell_new_has_lock_free_state() {
            let (input_tx, _input_rx) = mpsc::channel::<WriteRequest>(16);
            let shell = RunningShell::new(
                sample_info(),
                CancellationToken::new(),
                input_tx,
                10 * 1024 * 1024,
            );

            // History snapshot is reachable without locks.
            let snap = shell.history.load_full();
            assert!(snap.data.is_empty());

            // Broadcast channel accepts subscribers.
            let mut sub = shell.output_tx.subscribe();
            shell.output_tx.send(Bytes::from_static(b"hello")).unwrap();
            match sub.recv().await {
                Ok(bytes) => assert_eq!(bytes.as_ref(), b"hello"),
                Err(e) => panic!("expected hello, got {e}"),
            }

            // last_activity_ms is populated and monotonic-ish.
            let ts = shell.last_activity_ms.load(Ordering::Relaxed);
            assert!(ts > 0);
        }

        #[tokio::test]
        async fn test_write_request_data_carries_bytes() {
            let req = WriteRequest::Data(Bytes::from_static(b"abc"));
            match req {
                WriteRequest::Data(bytes) => assert_eq!(bytes.as_ref(), b"abc"),
                WriteRequest::Close => panic!("wrong variant"),
            }
        }

        #[tokio::test]
        async fn test_write_request_close_variant() {
            let req = WriteRequest::Close;
            match req {
                WriteRequest::Close => {}
                WriteRequest::Data(_) => panic!("wrong variant"),
            }
        }

        #[tokio::test]
        async fn test_input_mpsc_routes_to_writer_task() {
            let (input_tx, mut input_rx) = mpsc::channel::<WriteRequest>(4);

            input_tx
                .send(WriteRequest::Data(Bytes::from_static(b"ls\n")))
                .await
                .unwrap();
            input_tx.send(WriteRequest::Close).await.unwrap();
            drop(input_tx);

            match input_rx.recv().await {
                Some(WriteRequest::Data(bytes)) => assert_eq!(bytes.as_ref(), b"ls\n"),
                other => panic!("unexpected first frame: {other:?}"),
            }
            match input_rx.recv().await {
                Some(WriteRequest::Close) => {}
                other => panic!("unexpected second frame: {other:?}"),
            }
            assert!(input_rx.recv().await.is_none());
        }

        #[tokio::test]
        async fn test_data_notify_wakes_waiters() {
            let notify = Arc::new(Notify::new());

            let waiter_notify = Arc::clone(&notify);
            let waiter = tokio::spawn(async move {
                waiter_notify.notified().await;
            });

            // Give the waiter a moment to register.
            tokio::task::yield_now().await;
            notify.notify_waiters();
            waiter.await.unwrap();
        }
    }

    mod helpers {
        use super::*;

        #[test]
        fn test_now_ms_is_monotonically_increasing_or_equal() {
            let a = now_ms();
            let b = now_ms();
            assert!(b >= a);
        }

        #[test]
        fn test_touch_activity_updates_atomic() {
            let activity = AtomicU64::new(0);
            touch_activity(&activity);
            assert!(activity.load(Ordering::Relaxed) > 0);
        }
    }

    mod ring_buffer {
        use super::*;

        #[test]
        fn test_default_is_empty() {
            let buf = RingBuffer::default();
            assert!(buf.data.is_empty());
        }

        #[test]
        fn test_clone_shares_underlying_bytes() {
            let buf = RingBuffer {
                data: Bytes::from_static(b"hello"),
            };
            let cloned = buf.clone();
            assert_eq!(buf.data.as_ref(), cloned.data.as_ref());
        }
    }
}
