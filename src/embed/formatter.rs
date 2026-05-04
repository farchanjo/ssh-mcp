//! NDJSON event formatter for `ssh-mcp-tail`.
//!
//! Serialises one [`Event`] per line on stdout. Every write is line-
//! buffered: the writer flushes after each newline so consumers (`jq`,
//! `tee`, fluentbit, vector) see events as they happen and not batched
//! at runtime exit.
//!
//! The formatter never panics: serialisation failures (JSON encode
//! errors, broken stdout) are surfaced through [`FormatError`] and the
//! caller decides whether to abort the daemon (typically only on a
//! fatal `BrokenPipe`).

use serde::Serialize;
use thiserror::Error;
use tokio::io::{AsyncWrite, AsyncWriteExt as _};

use crate::domain::ids::{CommandId, SessionId, ShellId, TransferId};
use crate::domain::subscription::SubId;

/// Stable v5.0 NDJSON protocol marker carried by `heartbeat` and
/// `daemon_stats` events.
pub const PROTOCOL_VERSION: &str = "ssh-mcp-ndjson/1";

/// One stdout event in the v5 NDJSON daemon protocol.
///
/// Tagged on the `ev` field per ADR 0008. The discriminator is
/// open-ended: consumers should ignore unknown values rather than
/// failing.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Event {
    /// Successful op confirmation.
    Ack {
        /// Echo of the op's correlation `id`.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Newly created or reused session.
        #[serde(skip_serializing_if = "Option::is_none")]
        sid: Option<SessionId>,
        /// Newly spawned async command.
        #[serde(skip_serializing_if = "Option::is_none")]
        cid: Option<CommandId>,
        /// Newly opened shell.
        #[serde(skip_serializing_if = "Option::is_none")]
        shid: Option<ShellId>,
        /// Newly registered subscription.
        #[serde(skip_serializing_if = "Option::is_none")]
        sub_id: Option<SubId>,
        /// Newly registered transfer.
        #[serde(skip_serializing_if = "Option::is_none")]
        tid: Option<TransferId>,
        /// Resource URI when the ack carries one (e.g. `subscribe`).
        #[serde(skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
    },
    /// Op failed with the v4.5+ wire taxonomy.
    Err {
        /// Echo of the op's correlation `id`.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Wire error code (see ADR 0007).
        code: String,
        /// One-sentence summary.
        reason: String,
        /// Action-oriented `DETAIL:` line.
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Async command launched.
    Started {
        /// Echo of the op's correlation `id`.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Newly spawned command.
        cid: CommandId,
        /// Owning session.
        sid: SessionId,
    },
    /// Subscriber received a push event.
    Push {
        /// Subscription identifier.
        sub_id: SubId,
        /// Source resource URI.
        uri: String,
        /// Per-`SubId` sequence number.
        seq_local: u64,
        /// Per-resource sequence number.
        seq_global: u64,
        /// Byte cursor into the resource ring buffer.
        cursor: u64,
        /// New bytes appended since the previous push.
        delta: String,
        /// RFC 3339 timestamp.
        ts: String,
    },
    /// Async command finished.
    Completed {
        /// Echo of the op's correlation `id`.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Command identifier.
        cid: CommandId,
        /// Exit code (`null` on signal-killed).
        exit: Option<i32>,
        /// `true` when the command was cancelled before completion.
        #[serde(default, skip_serializing_if = "is_false")]
        cancelled: bool,
    },
    /// SFTP transfer progress snapshot.
    TransferProgress {
        /// Transfer identifier.
        tid: TransferId,
        /// Bytes transferred so far.
        bytes: u64,
        /// Total bytes to transfer.
        total: u64,
    },
    /// Raw PTY output for the convenience `shell` subcommand.
    ShellOutput {
        /// Shell identifier.
        shid: ShellId,
        /// Bytes payload (UTF-8 string).
        bytes: String,
    },
    /// Lane recovered from overflow under `lag_policy=snapshot`.
    Snapshot {
        /// Subscription identifier.
        sub_id: SubId,
        /// Cursor advanced after the rebuild.
        cursor: u64,
        /// Reconstructed delta.
        delta: String,
    },
    /// Drop marker under `lag_policy=drop_oldest` / `drop_newest`.
    Lagged {
        /// Subscription identifier.
        sub_id: SubId,
        /// Cumulative drop count for this gap.
        dropped: u64,
    },
    /// Server-emitted advisory marker.
    Warn {
        /// Wire warning code (`SUB_LEAK_RISK`, `LAG_BACKPRESSURE`, ...).
        code: String,
        /// Resource URI when scoped.
        #[serde(skip_serializing_if = "Option::is_none")]
        resource: Option<String>,
        /// Human-readable advisory message.
        msg: String,
    },
    /// Session disconnected.
    Closed {
        /// Echo of the op's correlation `id`.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Session identifier.
        sid: SessionId,
    },
    /// Long-running resource released.
    ResourceClosed {
        /// Resource URI.
        uri: String,
        /// Lifecycle reason (`unsubscribe_grace_elapsed`, `manual_close`, ...).
        reason: String,
    },
    /// Periodic liveness signal.
    Heartbeat {
        /// RFC 3339 timestamp.
        ts: String,
        /// Protocol version marker (`ssh-mcp-ndjson/1`).
        protocol: String,
    },
    /// Periodic global stats snapshot.
    DaemonStats {
        /// Active SSH sessions.
        active_sessions: usize,
        /// Active subscriptions.
        active_subs: usize,
        /// Total ring buffer overflows.
        ring_buffer_overflows_total: u64,
        /// Total mpsc-full events across all lanes.
        mpsc_full_events_total: u64,
        /// Protocol version marker.
        protocol: String,
    },
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if calls the predicate with &T; the &bool signature is required"
)]
const fn is_false(b: &bool) -> bool {
    !*b
}

/// Errors produced by the formatter.
#[derive(Debug, Error)]
pub enum FormatError {
    /// `serde_json` failed to encode the event.
    #[error("event serialisation failed: {0}")]
    Encode(String),
    /// Underlying I/O failure (typically `BrokenPipe` on stdout).
    #[error("stdout write failed: {0}")]
    Io(String),
}

/// Serialise a single event to its NDJSON line representation
/// (without trailing newline). Useful for tests and bridges.
///
/// # Errors
/// Returns [`FormatError::Encode`] if `serde_json` cannot encode the
/// event (e.g. invalid UTF-8 inside a string field — practically
/// unreachable given our typed fields, but we surface it rather than
/// panic).
pub fn encode_line(event: &Event) -> Result<String, FormatError> {
    serde_json::to_string(event).map_err(|err| FormatError::Encode(err.to_string()))
}

/// Line-buffered NDJSON writer. Flushes after every event so consumers
/// see updates with sub-millisecond latency.
#[derive(Debug)]
pub struct NdjsonWriter<W> {
    inner: W,
}

impl<W> NdjsonWriter<W>
where
    W: AsyncWrite + Unpin,
{
    /// Wrap an async writer (typically `tokio::io::stdout()`).
    #[must_use]
    pub const fn new(inner: W) -> Self {
        Self { inner }
    }

    /// Encode and write one event followed by `\n`. Flushes the
    /// underlying writer so the line is immediately visible.
    ///
    /// # Errors
    /// Surfaces [`FormatError::Encode`] when `serde_json` rejects the
    /// event and [`FormatError::Io`] on broken stdout.
    pub async fn write(&mut self, event: &Event) -> Result<(), FormatError> {
        let line = encode_line(event)?;
        self.inner
            .write_all(line.as_bytes())
            .await
            .map_err(|err| FormatError::Io(err.to_string()))?;
        self.inner
            .write_all(b"\n")
            .await
            .map_err(|err| FormatError::Io(err.to_string()))?;
        self.inner
            .flush()
            .await
            .map_err(|err| FormatError::Io(err.to_string()))?;
        Ok(())
    }

    /// Drain any buffered bytes (useful before shutdown).
    ///
    /// # Errors
    /// Surfaces [`FormatError::Io`] on broken stdout.
    pub async fn shutdown(&mut self) -> Result<(), FormatError> {
        self.inner
            .flush()
            .await
            .map_err(|err| FormatError::Io(err.to_string()))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test-only assertions are deliberately direct"
)]
mod tests {
    use super::*;

    fn ack_session(sid: &str, id: &str) -> Event {
        Event::Ack {
            id: Some(id.to_string()),
            sid: Some(SessionId::new(sid.to_string())),
            cid: None,
            shid: None,
            sub_id: None,
            tid: None,
            uri: None,
        }
    }

    #[test]
    fn encode_ack_session_round_trip() {
        let line = encode_line(&ack_session("s1", "corr-1")).unwrap();
        assert!(line.contains("\"ev\":\"ack\""));
        assert!(line.contains("\"sid\":\"s1\""));
        assert!(line.contains("\"id\":\"corr-1\""));
    }

    #[test]
    fn encode_omits_none_fields() {
        let line = encode_line(&ack_session("s1", "corr-1")).unwrap();
        assert!(!line.contains("\"cid\""));
        assert!(!line.contains("\"shid\""));
    }

    #[test]
    fn encode_err_carries_code_and_reason() {
        let event = Event::Err {
            id: Some("c1".to_string()),
            code: "AUTH_FAILED".to_string(),
            reason: "Authentication failed".to_string(),
            detail: Some("Check credentials.".to_string()),
        };
        let line = encode_line(&event).unwrap();
        assert!(line.contains("\"ev\":\"err\""));
        assert!(line.contains("\"code\":\"AUTH_FAILED\""));
        assert!(line.contains("\"reason\":\"Authentication failed\""));
        assert!(line.contains("\"detail\":\"Check credentials.\""));
    }

    #[test]
    fn encode_started_event() {
        let event = Event::Started {
            id: Some("c1".to_string()),
            cid: CommandId::new("cmd1".to_string()),
            sid: SessionId::new("s1".to_string()),
        };
        let line = encode_line(&event).unwrap();
        assert!(line.contains("\"ev\":\"started\""));
        assert!(line.contains("\"cid\":\"cmd1\""));
    }

    #[test]
    fn encode_push_event() {
        let event = Event::Push {
            sub_id: SubId::new("sub1".to_string()),
            uri: "command://cmd1/output".to_string(),
            seq_local: 1,
            seq_global: 1893,
            cursor: 102_400,
            delta: "hello".to_string(),
            ts: "2026-05-04T12:34:56.789Z".to_string(),
        };
        let line = encode_line(&event).unwrap();
        assert!(line.contains("\"ev\":\"push\""));
        assert!(line.contains("\"seq_local\":1"));
        assert!(line.contains("\"seq_global\":1893"));
        assert!(line.contains("\"cursor\":102400"));
    }

    #[test]
    fn encode_completed_omits_cancelled_when_false() {
        let event = Event::Completed {
            id: None,
            cid: CommandId::new("c1".to_string()),
            exit: Some(0),
            cancelled: false,
        };
        let line = encode_line(&event).unwrap();
        assert!(!line.contains("\"cancelled\""));
    }

    #[test]
    fn encode_completed_includes_cancelled_when_true() {
        let event = Event::Completed {
            id: None,
            cid: CommandId::new("c1".to_string()),
            exit: None,
            cancelled: true,
        };
        let line = encode_line(&event).unwrap();
        assert!(line.contains("\"cancelled\":true"));
    }

    #[test]
    fn encode_transfer_progress() {
        let event = Event::TransferProgress {
            tid: TransferId::new("t1".to_string()),
            bytes: 1024,
            total: 4096,
        };
        let line = encode_line(&event).unwrap();
        assert!(line.contains("\"ev\":\"transfer_progress\""));
        assert!(line.contains("\"bytes\":1024"));
    }

    #[test]
    fn encode_shell_output() {
        let event = Event::ShellOutput {
            shid: ShellId::new("sh1".to_string()),
            bytes: "ok\n".to_string(),
        };
        let line = encode_line(&event).unwrap();
        assert!(line.contains("\"ev\":\"shell_output\""));
        assert!(line.contains("\"bytes\":\"ok\\n\""));
    }

    #[test]
    fn encode_snapshot_event() {
        let event = Event::Snapshot {
            sub_id: SubId::new("s1".to_string()),
            cursor: 42,
            delta: "rebuild".to_string(),
        };
        let line = encode_line(&event).unwrap();
        assert!(line.contains("\"ev\":\"snapshot\""));
    }

    #[test]
    fn encode_lagged_event() {
        let event = Event::Lagged {
            sub_id: SubId::new("s1".to_string()),
            dropped: 42,
        };
        let line = encode_line(&event).unwrap();
        assert!(line.contains("\"ev\":\"lagged\""));
        assert!(line.contains("\"dropped\":42"));
    }

    #[test]
    fn encode_warn_event() {
        let event = Event::Warn {
            code: "SUB_LEAK_RISK".to_string(),
            resource: Some("shell://abc/output".to_string()),
            msg: "Resource owned > 2s".to_string(),
        };
        let line = encode_line(&event).unwrap();
        assert!(line.contains("\"ev\":\"warn\""));
        assert!(line.contains("\"resource\":\"shell://abc/output\""));
    }

    #[test]
    fn encode_closed_event() {
        let event = Event::Closed {
            id: None,
            sid: SessionId::new("s1".to_string()),
        };
        let line = encode_line(&event).unwrap();
        assert!(line.contains("\"ev\":\"closed\""));
    }

    #[test]
    fn encode_resource_closed_event() {
        let event = Event::ResourceClosed {
            uri: "command://x/output".to_string(),
            reason: "manual_close".to_string(),
        };
        let line = encode_line(&event).unwrap();
        assert!(line.contains("\"ev\":\"resource_closed\""));
        assert!(line.contains("\"reason\":\"manual_close\""));
    }

    #[test]
    fn encode_heartbeat_event() {
        let event = Event::Heartbeat {
            ts: "2026-05-04T00:00:00Z".to_string(),
            protocol: PROTOCOL_VERSION.to_string(),
        };
        let line = encode_line(&event).unwrap();
        assert!(line.contains("\"ev\":\"heartbeat\""));
        assert!(line.contains("ssh-mcp-ndjson/1"));
    }

    #[test]
    fn encode_daemon_stats_event() {
        let event = Event::DaemonStats {
            active_sessions: 3,
            active_subs: 7,
            ring_buffer_overflows_total: 0,
            mpsc_full_events_total: 0,
            protocol: PROTOCOL_VERSION.to_string(),
        };
        let line = encode_line(&event).unwrap();
        assert!(line.contains("\"ev\":\"daemon_stats\""));
        assert!(line.contains("\"active_sessions\":3"));
    }

    #[tokio::test]
    async fn writer_appends_newline_and_flushes() {
        let buf: Vec<u8> = Vec::new();
        let mut w = NdjsonWriter::new(buf);
        w.write(&ack_session("s1", "c1")).await.unwrap();
        let bytes = w.inner;
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.ends_with('\n'));
        assert!(s.contains("\"sid\":\"s1\""));
    }

    #[tokio::test]
    async fn writer_concatenates_multiple_events() {
        let buf: Vec<u8> = Vec::new();
        let mut w = NdjsonWriter::new(buf);
        w.write(&ack_session("s1", "c1")).await.unwrap();
        w.write(&ack_session("s2", "c2")).await.unwrap();
        let s = std::str::from_utf8(&w.inner).unwrap();
        let lines: Vec<&str> = s.trim_end_matches('\n').split('\n').collect();
        assert_eq!(lines.len(), 2);
    }

    #[tokio::test]
    async fn writer_shutdown_is_idempotent() {
        let buf: Vec<u8> = Vec::new();
        let mut w = NdjsonWriter::new(buf);
        w.write(&ack_session("s1", "c1")).await.unwrap();
        w.shutdown().await.unwrap();
        // No second write needed; just confirm the call returns.
        w.shutdown().await.unwrap();
    }
}
