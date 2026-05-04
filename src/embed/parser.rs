//! NDJSON command parser for `ssh-mcp-tail`.
//!
//! Reads one JSON object per line from stdin (or any
//! `tokio::io::AsyncBufRead`), validates UTF-8, enforces
//! `SSH_NDJSON_LINE_MAX`, and decodes each line into the typed [`Op`]
//! enum. On parse failure the reader yields a [`ParseOutcome::Invalid`]
//! containing the corresponding [`ParseError`] so the caller can emit an
//! `INVALID_OP` error event and keep draining stdin (the daemon is not
//! killed by a single bad line — see ADR 0008).
//!
//! Every operation that mutates server state may carry an optional
//! correlation `id` field which the dispatcher echoes on the matching
//! [`crate::embed::formatter::Event`] (`ack`, `err`, `started`, ...).

use schemars::JsonSchema;
use std::io::ErrorKind;

use serde::Deserialize;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _};

use crate::domain::ids::{CommandId, SessionId, ShellId};
use crate::domain::subscription::{LagPolicy, SubId};

/// Default cap on a single NDJSON line (1 MB) — mirrors the
/// `SSH_NDJSON_LINE_MAX` env var documented in ADR 0008.
pub const DEFAULT_NDJSON_LINE_MAX: usize = 1_048_576;

/// Floor on the configurable NDJSON line cap. A pathologically small cap
/// would reject every realistic op so we refuse to drop below 1 KiB.
pub const NDJSON_LINE_MAX_MIN: usize = 1_024;

/// Hard cap on the configurable NDJSON line cap (16 MiB).
pub const NDJSON_LINE_MAX_CAP: usize = 16 * 1_048_576;

/// One stdin command in the v5 NDJSON daemon protocol.
///
/// Tagged on the `op` field per ADR 0008. Field naming is `snake_case`
/// to match the protocol shape on the wire.
#[derive(Debug, Clone, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// `connect` — open or reuse an SSH session.
    Connect {
        /// Remote host (DNS or IP).
        host: String,
        /// Remote user.
        user: String,
        /// Optional path to a private key file.
        #[serde(default)]
        key: Option<String>,
        /// Optional inline password (avoid in production).
        #[serde(default)]
        password: Option<String>,
        /// Optional remote port (defaults to 22 server-side).
        #[serde(default)]
        port: Option<u16>,
        /// Optional logical agent group for bulk-cleanup.
        #[serde(default)]
        agent_id: Option<String>,
        /// Optional reuse policy: `suggest`, `auto`, or `force_new`.
        #[serde(default)]
        reuse_policy: Option<String>,
        /// Optional correlation identifier echoed on every reply.
        #[serde(default)]
        id: Option<String>,
    },
    /// `exec` — run a one-shot async command on a session.
    Exec {
        /// Session identifier returned by a previous `connect`.
        sid: SessionId,
        /// Command line to execute remotely.
        cmd: String,
        /// Allocate a PTY for the command.
        #[serde(default)]
        pty: Option<bool>,
        /// Auto-cleanup once the last subscriber leaves.
        #[serde(default)]
        release_when_no_subs: Option<bool>,
        /// Optional correlation identifier.
        #[serde(default)]
        id: Option<String>,
    },
    /// `subscribe` — open a push channel against a resource URI.
    Subscribe {
        /// Resource URI (`shell://`, `command://`, `transfer://`,
        /// `session://`, `forward://`).
        uri: String,
        /// Subscription lifetime: `manual`, `auto-close`, `lease` —
        /// kept as a free-form string so the daemon protocol stays
        /// flat (`{"lifetime":"auto-close"}`); the dispatcher
        /// validates and translates into the domain enum.
        #[serde(default)]
        lifetime: Option<String>,
        /// Grace window (ms) when `lifetime=auto-close`.
        #[serde(default)]
        grace_ms: Option<u32>,
        /// Per-lane backpressure policy.
        #[serde(default)]
        lag_policy: Option<LagPolicy>,
        /// Optional regex/level filter applied before mpsc enqueue.
        #[serde(default)]
        filter: Option<String>,
        /// Optional replay cursor (bytes) for the lane's first drain.
        #[serde(default)]
        start_cursor: Option<u64>,
        /// Optional correlation identifier.
        #[serde(default)]
        id: Option<String>,
    },
    /// `unsubscribe` — close a push channel.
    Unsubscribe {
        /// Subscription identifier returned by `subscribe`'s `ack`.
        sub_id: SubId,
        /// Optional correlation identifier.
        #[serde(default)]
        id: Option<String>,
    },
    /// `read` — explicit `resources/read` snapshot (no subscribe needed).
    Read {
        /// Resource URI.
        uri: String,
        /// Optional cursor (byte offset).
        #[serde(default)]
        cursor: Option<u64>,
        /// Optional correlation identifier.
        #[serde(default)]
        id: Option<String>,
    },
    /// `shell_open` — allocate an interactive PTY shell.
    ShellOpen {
        /// Session identifier.
        sid: SessionId,
        /// PTY width.
        #[serde(default = "default_cols")]
        cols: u16,
        /// PTY height.
        #[serde(default = "default_rows")]
        rows: u16,
        /// Auto-cleanup once the last subscriber leaves.
        #[serde(default)]
        release_when_no_subs: Option<bool>,
        /// Idle reaper TTL (seconds).
        #[serde(default)]
        inactivity_ttl_secs: Option<u64>,
        /// Ring buffer cap (bytes).
        #[serde(default)]
        max_buffer_size: Option<u64>,
        /// Optional correlation identifier.
        #[serde(default)]
        id: Option<String>,
    },
    /// `shell_write` — forward bytes to the PTY stdin.
    ShellWrite {
        /// Shell identifier.
        shid: ShellId,
        /// Bytes payload (UTF-8 string).
        bytes: String,
        /// Optional correlation identifier.
        #[serde(default)]
        id: Option<String>,
    },
    /// `shell_key` — semantic keystroke (encoded by `domain::keys`).
    ShellKey {
        /// Shell identifier.
        shid: ShellId,
        /// Named key (`ctrl_c`, `arrow_up`, ...).
        key: String,
        /// Optional repeat count.
        #[serde(default)]
        repeat: Option<u32>,
        /// Optional correlation identifier.
        #[serde(default)]
        id: Option<String>,
    },
    /// `upload` — push a local file via SFTP.
    Upload {
        /// Session identifier.
        sid: SessionId,
        /// Local path.
        local: String,
        /// Remote destination path.
        remote: String,
        /// Auto-cleanup once the last subscriber leaves.
        #[serde(default)]
        release_when_no_subs: Option<bool>,
        /// Optional correlation identifier.
        #[serde(default)]
        id: Option<String>,
    },
    /// `download` — pull a remote file via SFTP.
    Download {
        /// Session identifier.
        sid: SessionId,
        /// Remote source path.
        remote: String,
        /// Local destination path.
        local: String,
        /// Auto-cleanup once the last subscriber leaves.
        #[serde(default)]
        release_when_no_subs: Option<bool>,
        /// Optional correlation identifier.
        #[serde(default)]
        id: Option<String>,
    },
    /// `cancel` — cancel a running async command.
    Cancel {
        /// Command identifier.
        cid: CommandId,
        /// Optional correlation identifier.
        #[serde(default)]
        id: Option<String>,
    },
    /// `disconnect` — disconnect a session (cascades shells / commands /
    /// transfers).
    Disconnect {
        /// Session identifier.
        sid: SessionId,
        /// Optional correlation identifier.
        #[serde(default)]
        id: Option<String>,
    },
    /// `shutdown` — request graceful drain.
    Shutdown {
        /// Optional correlation identifier.
        #[serde(default)]
        id: Option<String>,
    },
}

const fn default_cols() -> u16 {
    80
}

const fn default_rows() -> u16 {
    24
}

impl Op {
    /// Borrow the optional correlation `id` carried by every variant.
    /// The dispatcher echoes this string on every reply tied to the op.
    #[must_use]
    pub fn correlation_id(&self) -> Option<&str> {
        let id = match self {
            Self::Connect { id, .. }
            | Self::Exec { id, .. }
            | Self::Subscribe { id, .. }
            | Self::Unsubscribe { id, .. }
            | Self::Read { id, .. }
            | Self::ShellOpen { id, .. }
            | Self::ShellWrite { id, .. }
            | Self::ShellKey { id, .. }
            | Self::Upload { id, .. }
            | Self::Download { id, .. }
            | Self::Cancel { id, .. }
            | Self::Disconnect { id, .. }
            | Self::Shutdown { id } => id,
        };
        id.as_deref()
    }
}

/// Errors produced when decoding a single NDJSON line.
#[derive(Debug, Error)]
pub enum ParseError {
    /// The line exceeded the configured `SSH_NDJSON_LINE_MAX` cap.
    #[error("NDJSON line exceeds configured max ({0} bytes)")]
    LineTooLong(usize),
    /// The line was not valid UTF-8.
    #[error("NDJSON line is not valid UTF-8: {0}")]
    InvalidUtf8(String),
    /// `serde_json` failed to decode the line into an [`Op`].
    #[error("invalid NDJSON op: {0}")]
    InvalidJson(String),
    /// Underlying I/O failure.
    #[error("I/O error reading NDJSON: {0}")]
    Io(String),
}

/// Outcome of reading the next NDJSON line from stdin. A single
/// malformed line produces [`ParseOutcome::Invalid`] (the daemon
/// continues); EOF / I/O closure produces [`ParseOutcome::Eof`].
#[derive(Debug)]
pub enum ParseOutcome {
    /// Successfully decoded one op.
    Op(Op),
    /// Decoding failed but the reader is still alive.
    Invalid(ParseError),
    /// Stdin has reached EOF.
    Eof,
}

/// Streaming NDJSON line reader. Backed by an `AsyncBufRead` so the
/// daemon can drain stdin without blocking the runtime.
#[derive(Debug)]
pub struct NdjsonReader<R> {
    inner: R,
    line_max: usize,
    buf: String,
}

impl<R> NdjsonReader<R>
where
    R: AsyncBufRead + Unpin,
{
    /// Build a reader with an explicit per-line cap.
    #[must_use]
    pub const fn with_line_max(inner: R, line_max: usize) -> Self {
        Self {
            inner,
            line_max,
            buf: String::new(),
        }
    }

    /// Build a reader with the default per-line cap.
    #[must_use]
    pub const fn new(inner: R) -> Self {
        Self::with_line_max(inner, DEFAULT_NDJSON_LINE_MAX)
    }

    /// Read the next NDJSON line. Returns [`ParseOutcome::Eof`] on stdin
    /// closure and [`ParseOutcome::Invalid`] on a single malformed line.
    pub async fn next(&mut self) -> ParseOutcome {
        self.buf.clear();
        match self.inner.read_line(&mut self.buf).await {
            Ok(0) => ParseOutcome::Eof,
            Ok(_) => {
                if self.buf.len() > self.line_max {
                    let len = self.buf.len();
                    return ParseOutcome::Invalid(ParseError::LineTooLong(len));
                }
                let trimmed = self.buf.trim();
                if trimmed.is_empty() {
                    // Empty / whitespace-only line — treat as a benign skip.
                    return Box::pin(self.next()).await;
                }
                match serde_json::from_str::<Op>(trimmed) {
                    Ok(op) => ParseOutcome::Op(op),
                    Err(err) => ParseOutcome::Invalid(ParseError::InvalidJson(err.to_string())),
                }
            }
            Err(err) => {
                let kind = err.kind();
                if matches!(kind, ErrorKind::InvalidData) {
                    ParseOutcome::Invalid(ParseError::InvalidUtf8(err.to_string()))
                } else {
                    ParseOutcome::Invalid(ParseError::Io(err.to_string()))
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test-only assertions are deliberately direct"
)]
mod tests {
    use std::io::Cursor;

    use tokio::io::BufReader;

    use super::*;

    fn reader(payload: &str) -> NdjsonReader<BufReader<Cursor<Vec<u8>>>> {
        NdjsonReader::new(BufReader::new(Cursor::new(payload.as_bytes().to_vec())))
    }

    #[tokio::test]
    async fn parses_connect_op() {
        let mut r =
            reader("{\"op\":\"connect\",\"host\":\"h\",\"user\":\"u\",\"id\":\"corr-1\"}\n");
        let outcome = r.next().await;
        match outcome {
            ParseOutcome::Op(Op::Connect { host, user, id, .. }) => {
                assert_eq!(host, "h");
                assert_eq!(user, "u");
                assert_eq!(id.as_deref(), Some("corr-1"));
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn parses_every_op_variant() {
        let lines = vec![
            "{\"op\":\"connect\",\"host\":\"h\",\"user\":\"u\"}",
            "{\"op\":\"exec\",\"sid\":\"s\",\"cmd\":\"ls\"}",
            "{\"op\":\"subscribe\",\"uri\":\"command://x/output\"}",
            "{\"op\":\"unsubscribe\",\"sub_id\":\"sub-1\"}",
            "{\"op\":\"read\",\"uri\":\"shell://x/output\",\"cursor\":42}",
            "{\"op\":\"shell_open\",\"sid\":\"s\",\"cols\":80,\"rows\":24}",
            "{\"op\":\"shell_write\",\"shid\":\"sh\",\"bytes\":\"ls\\n\"}",
            "{\"op\":\"shell_key\",\"shid\":\"sh\",\"key\":\"ctrl_c\"}",
            "{\"op\":\"upload\",\"sid\":\"s\",\"local\":\"/tmp/a\",\"remote\":\"/srv/a\"}",
            "{\"op\":\"download\",\"sid\":\"s\",\"remote\":\"/srv/a\",\"local\":\"/tmp/a\"}",
            "{\"op\":\"cancel\",\"cid\":\"c\"}",
            "{\"op\":\"disconnect\",\"sid\":\"s\"}",
            "{\"op\":\"shutdown\"}",
        ];
        let payload = format!("{}\n", lines.join("\n"));
        let mut r = reader(&payload);
        let mut count = 0_usize;
        loop {
            match r.next().await {
                ParseOutcome::Op(_) => count += 1,
                ParseOutcome::Eof => break,
                ParseOutcome::Invalid(err) => panic!("unexpected parse error: {err:?}"),
            }
        }
        assert_eq!(count, lines.len());
    }

    #[tokio::test]
    async fn invalid_json_does_not_kill_reader() {
        let mut r = reader("{not-json}\n{\"op\":\"shutdown\"}\n");
        assert!(matches!(r.next().await, ParseOutcome::Invalid(_)));
        assert!(matches!(
            r.next().await,
            ParseOutcome::Op(Op::Shutdown { .. })
        ));
        assert!(matches!(r.next().await, ParseOutcome::Eof));
    }

    #[tokio::test]
    async fn unknown_op_is_invalid_op() {
        let mut r = reader("{\"op\":\"frobnicate\"}\n");
        match r.next().await {
            ParseOutcome::Invalid(ParseError::InvalidJson(msg)) => {
                assert!(msg.to_lowercase().contains("frobnicate") || msg.contains("variant"));
            }
            other => panic!("expected InvalidJson, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn line_too_long_yields_error() {
        let big = "x".repeat(2_048);
        let payload = format!("{big}\n");
        let inner = BufReader::new(Cursor::new(payload.as_bytes().to_vec()));
        let mut r = NdjsonReader::with_line_max(inner, 1_024);
        match r.next().await {
            ParseOutcome::Invalid(ParseError::LineTooLong(_)) => {}
            other => panic!("expected LineTooLong, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn blank_lines_are_skipped() {
        let mut r = reader("\n\n   \n{\"op\":\"shutdown\"}\n");
        assert!(matches!(
            r.next().await,
            ParseOutcome::Op(Op::Shutdown { .. })
        ));
        assert!(matches!(r.next().await, ParseOutcome::Eof));
    }

    #[tokio::test]
    async fn correlation_id_round_trips() {
        let mut r = reader("{\"op\":\"shutdown\",\"id\":\"x\"}\n");
        match r.next().await {
            ParseOutcome::Op(op) => {
                assert_eq!(op.correlation_id(), Some("x"));
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn correlation_id_absent_yields_none() {
        let mut r = reader("{\"op\":\"shutdown\"}\n");
        match r.next().await {
            ParseOutcome::Op(op) => {
                assert_eq!(op.correlation_id(), None);
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn subscribe_with_full_payload() {
        let mut r = reader(
            "{\"op\":\"subscribe\",\"uri\":\"command://x/output\",\"lifetime\":\"auto-close\",\"grace_ms\":2000,\"lag_policy\":\"snapshot\",\"filter\":\"ERROR\",\"start_cursor\":42,\"id\":\"s1\"}\n",
        );
        match r.next().await {
            ParseOutcome::Op(Op::Subscribe {
                uri,
                grace_ms,
                filter,
                start_cursor,
                ..
            }) => {
                assert_eq!(uri, "command://x/output");
                assert_eq!(grace_ms, Some(2_000));
                assert_eq!(filter.as_deref(), Some("ERROR"));
                assert_eq!(start_cursor, Some(42));
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn shell_open_defaults_apply() {
        let mut r = reader("{\"op\":\"shell_open\",\"sid\":\"s\"}\n");
        match r.next().await {
            ParseOutcome::Op(Op::ShellOpen { cols, rows, .. }) => {
                assert_eq!(cols, 80);
                assert_eq!(rows, 24);
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn eof_returns_eof() {
        let mut r = reader("");
        assert!(matches!(r.next().await, ParseOutcome::Eof));
    }

    #[tokio::test]
    async fn cr_lf_terminator_supported() {
        let mut r = reader("{\"op\":\"shutdown\"}\r\n");
        assert!(matches!(
            r.next().await,
            ParseOutcome::Op(Op::Shutdown { .. })
        ));
    }
}
