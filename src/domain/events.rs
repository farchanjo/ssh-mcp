//! Live events fanned out by the runtime ports.
//!
//! All variants carry a monotonic `seq: u64` allocated by the
//! [`crate::ports::subscriber_registry::SubscriberRegistryPort`] so a
//! consumer that recovers from a `Lagged` error can detect gaps.

use bytes::Bytes;
use chrono::{DateTime, Utc};

/// Source channel for an [`OutputChunk`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    /// Standard output bytes.
    Stdout,
    /// Standard error bytes.
    Stderr,
}

/// Incremental output frame broadcast by command and shell aggregates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputChunk {
    /// New bytes appended on stdout / stderr.
    Data {
        /// Monotonic per-resource sequence number (starts at 1).
        seq: u64,
        /// Channel that produced the bytes.
        stream: OutputStream,
        /// Bytes appended since the previous frame.
        data: Bytes,
    },
    /// Terminal frame. `exit_code` is `None` when the producer was cancelled,
    /// timed out, or failed before yielding an exit status.
    Closed {
        /// Monotonic per-resource sequence number (starts at 1).
        seq: u64,
        /// Exit code (`None` when missing).
        exit_code: Option<i32>,
    },
}

/// Live progress event broadcast by an SFTP transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressEvent {
    /// Transfer made progress — `bytes_transferred` was just updated.
    Tick {
        /// Monotonic per-resource sequence number (starts at 1).
        seq: u64,
        /// Cumulative bytes transferred so far.
        bytes_transferred: u64,
        /// Total bytes to transfer.
        total_bytes: u64,
    },
    /// Transfer terminated successfully.
    Completed {
        /// Monotonic per-resource sequence number.
        seq: u64,
        /// Final cumulative byte count.
        bytes_transferred: u64,
    },
    /// Transfer failed (failure description lives on the transfer entity).
    Failed {
        /// Monotonic per-resource sequence number.
        seq: u64,
    },
    /// Transfer was cancelled.
    Cancelled {
        /// Monotonic per-resource sequence number.
        seq: u64,
    },
}

/// Live health event broadcast by a session's keepalive probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthEvent {
    /// Health check passed at this timestamp.
    Healthy {
        /// Monotonic per-resource sequence number.
        seq: u64,
        /// Wall-clock timestamp the probe completed.
        at: DateTime<Utc>,
    },
    /// Health check failed at this timestamp.
    Unhealthy {
        /// Monotonic per-resource sequence number.
        seq: u64,
        /// Wall-clock timestamp the probe completed.
        at: DateTime<Utc>,
    },
    /// Session was disconnected.
    Disconnected {
        /// Monotonic per-resource sequence number.
        seq: u64,
    },
}

/// Live event broadcast by an active port-forwarder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardEvent {
    /// Local connection accepted from the given remote address.
    Accept {
        /// Monotonic per-resource sequence number.
        seq: u64,
        /// Remote peer address as a printable string (`IP:port`).
        client_addr: String,
        /// Wall-clock timestamp.
        at: DateTime<Utc>,
    },
    /// Forwarded connection closed; aggregated byte counts are reported.
    Close {
        /// Monotonic per-resource sequence number.
        seq: u64,
        /// Remote peer address as a printable string.
        client_addr: String,
        /// Bytes received from the local client.
        bytes_in: u64,
        /// Bytes sent to the local client.
        bytes_out: u64,
        /// Wall-clock timestamp.
        at: DateTime<Utc>,
    },
    /// Forwarder task shut down — no further events will be emitted.
    Stopped {
        /// Monotonic per-resource sequence number.
        seq: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::{ForwardEvent, HealthEvent, OutputChunk, OutputStream, ProgressEvent};
    use bytes::Bytes;
    use chrono::Utc;

    #[test]
    fn output_chunk_data_carries_stream_and_seq() {
        let chunk = OutputChunk::Data {
            seq: 7,
            stream: OutputStream::Stderr,
            data: Bytes::from_static(b"oops"),
        };
        match chunk {
            OutputChunk::Data { seq, stream, data } => {
                assert_eq!(seq, 7);
                assert_eq!(stream, OutputStream::Stderr);
                assert_eq!(data.as_ref(), b"oops");
            }
            OutputChunk::Closed { .. } => unreachable!("wrong variant"),
        }
    }

    #[test]
    fn output_chunk_closed_can_omit_exit_code() {
        let chunk = OutputChunk::Closed {
            seq: 1,
            exit_code: None,
        };
        match chunk {
            OutputChunk::Closed { seq, exit_code } => {
                assert_eq!(seq, 1);
                assert!(exit_code.is_none());
            }
            OutputChunk::Data { .. } => unreachable!("wrong variant"),
        }
    }

    #[test]
    fn progress_event_variants_round_trip_via_match() {
        for evt in [
            ProgressEvent::Tick {
                seq: 1,
                bytes_transferred: 32,
                total_bytes: 1024,
            },
            ProgressEvent::Completed {
                seq: 2,
                bytes_transferred: 1024,
            },
            ProgressEvent::Failed { seq: 3 },
            ProgressEvent::Cancelled { seq: 4 },
        ] {
            // exhaustive match — must cover every variant explicitly.
            match evt {
                ProgressEvent::Tick { seq, .. }
                | ProgressEvent::Completed { seq, .. }
                | ProgressEvent::Failed { seq }
                | ProgressEvent::Cancelled { seq } => {
                    assert!(seq >= 1);
                }
            }
        }
    }

    #[test]
    fn health_event_carries_timestamps() {
        let now = Utc::now();
        match (
            HealthEvent::Healthy { seq: 1, at: now },
            HealthEvent::Unhealthy { seq: 2, at: now },
            HealthEvent::Disconnected { seq: 3 },
        ) {
            (
                HealthEvent::Healthy { seq: a, .. },
                HealthEvent::Unhealthy { seq: b, .. },
                HealthEvent::Disconnected { seq: c },
            ) => {
                assert_eq!((a, b, c), (1, 2, 3));
            }
            (
                HealthEvent::Healthy { .. }
                | HealthEvent::Unhealthy { .. }
                | HealthEvent::Disconnected { .. },
                HealthEvent::Healthy { .. }
                | HealthEvent::Unhealthy { .. }
                | HealthEvent::Disconnected { .. },
                HealthEvent::Healthy { .. }
                | HealthEvent::Unhealthy { .. }
                | HealthEvent::Disconnected { .. },
            ) => unreachable!("constructor sequencing should match"),
        }
    }

    #[test]
    fn forward_event_close_carries_byte_counts() {
        let now = Utc::now();
        let evt = ForwardEvent::Close {
            seq: 42,
            client_addr: "10.0.0.1:1024".to_string(),
            bytes_in: 100,
            bytes_out: 200,
            at: now,
        };
        match evt {
            ForwardEvent::Close {
                seq,
                client_addr,
                bytes_in,
                bytes_out,
                at,
            } => {
                assert_eq!(seq, 42);
                assert_eq!(client_addr, "10.0.0.1:1024");
                assert_eq!(bytes_in, 100);
                assert_eq!(bytes_out, 200);
                assert_eq!(at, now);
            }
            ForwardEvent::Accept { .. } | ForwardEvent::Stopped { .. } => {
                unreachable!("wrong variant")
            }
        }
    }
}
