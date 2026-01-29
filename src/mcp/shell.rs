//! Interactive shell session management.
//!
//! This module provides types for persistent interactive PTY shell sessions.
//! Shells maintain a continuous output buffer that can be read incrementally
//! and accept input via channel writes.
//!
//! # Architecture
//!
//! - `RunningShell`: Contains all state for an interactive shell including
//!   output buffer, channel writer, cancellation token, and status.
//! - Storage is handled by `storage::ShellStorage` trait implementations.
//!
//! # Use Cases
//!
//! - Interactive sessions (SOL/IPMI/OOB console access)
//! - Commands requiring PTY allocation (sudo, top, htop)
//! - Persistent shell sessions for multi-step workflows

use std::sync::Arc;

use russh::ChannelWriteHalf;
use russh::client;
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

use super::types::{ShellInfo, ShellStatus};

/// Write handle for sending input to a shell channel.
///
/// Wraps `russh::ChannelWriteHalf` to provide a `Send + Sync` interface
/// for writing data to the PTY channel without holding a lock on the read half.
pub struct ChannelWriter {
    pub(crate) write_half: ChannelWriteHalf<client::Msg>,
}

impl ChannelWriter {
    /// Create a new channel writer from a channel write half.
    pub fn new(write_half: ChannelWriteHalf<client::Msg>) -> Self {
        Self { write_half }
    }

    /// Send data (text, keystrokes, escape sequences) to the shell.
    pub async fn write(&self, data: &[u8]) -> Result<(), String> {
        self.write_half
            .data(data)
            .await
            .map_err(|e| format!("Failed to write to shell: {}", e))
    }

    /// Close the channel gracefully.
    pub async fn close(&self) -> Result<(), String> {
        self.write_half
            .close()
            .await
            .map_err(|e| format!("Failed to close shell channel: {}", e))
    }
}

/// State for a running interactive shell session.
pub struct RunningShell {
    /// Shell metadata
    pub info: ShellInfo,
    /// Token to cancel the background reader
    pub cancel_token: CancellationToken,
    /// Continuous PTY output buffer (single stream, no stderr separation)
    pub output: Arc<Mutex<Vec<u8>>>,
    /// Write handle for sending input to the shell
    pub channel_writer: Arc<Mutex<ChannelWriter>>,
    /// Sender for status updates (kept alive to prevent channel closure)
    #[allow(dead_code)]
    pub status_tx: watch::Sender<ShellStatus>,
    /// Receiver for status updates
    pub status_rx: watch::Receiver<ShellStatus>,
}

/// Maximum number of concurrent shells per session
pub const MAX_SHELLS_PER_SESSION: usize = 10;

#[cfg(test)]
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
        async fn test_output_buffer_concurrent_access() {
            let output = Arc::new(Mutex::new(Vec::<u8>::new()));

            let output1 = output.clone();
            let output2 = output.clone();

            let handle1 = tokio::spawn(async move {
                let mut buf = output1.lock().await;
                buf.extend_from_slice(b"from task 1");
            });

            let handle2 = tokio::spawn(async move {
                let mut buf = output2.lock().await;
                buf.extend_from_slice(b"from task 2");
            });

            handle1.await.unwrap();
            handle2.await.unwrap();

            let buf = output.lock().await;
            let content = String::from_utf8_lossy(&buf);
            assert!(content.contains("from task 1") || content.contains("from task 2"));
        }
    }
}
