//! Interactive PTY shell tools: `ssh_shell_open`, `ssh_shell_write`,
//! `ssh_shell_read`, `ssh_shell_close`.
//!
//! Each session can host up to [`MAX_SHELLS_PER_SESSION`] interactive PTYs.
//! Output is buffered in-memory with head-truncation past the configured
//! `max_buffer_size`.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use bytes::Bytes;
use rmcp::model::{CallToolResult, Content, ErrorData as McpError};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::{Notify, watch};
use tokio::time::sleep;

use super::super::client::open_pty_shell;
use super::super::config::{resolve_shell_inactivity_ttl, resolve_shell_max_buffer_size};
use super::super::keys::{KeyModifiers, ShellKey};
use super::super::message::builder::{
    ShellReadBuilder, ShellReadState, ShellWaitForBuilder, ShellWaitForState,
    render_shell_close_ok, render_shell_send_key_ok, render_shell_write_ok,
};
use super::super::message::helpers::{format_error, generate_nonce};
use super::super::shell::{
    MAX_SHELLS_PER_SESSION, RingBuffer, RunningShell, WriteRequest, touch_activity,
};
use super::super::storage::session::SESSION_STORAGE;
use super::super::storage::shell::SHELL_STORAGE;
use super::super::storage::traits::{SessionStorage, ShellStorage};
use super::super::types::ShellStatus;
use super::legacy_helpers::{clamp_output_bytes, err_session_not_found, register_shell};

/// Maximum allowed `repeat` value for `ssh_shell_send_key`.
const MAX_SEND_KEY_REPEAT: u8 = 64;

// ---------------------------------------------------------------------------
// `ssh_shell_open`
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_shell_open` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellOpenArgs {
    /// SESSION_ID returned from ssh_connect.
    pub session_id: String,

    /// Terminal type. Default: `xterm`. Use `vt100` or `ansi` for SOL/IPMI/serial consoles.
    pub term: Option<String>,

    /// Terminal width in columns. Default: 80.
    pub cols: Option<u32>,

    /// Terminal height in rows. Default: 24.
    pub rows: Option<u32>,

    /// Inactivity TTL in seconds. Shell auto-closes if no read or write happens within this window. Default: 600. Env: SSH_SHELL_INACTIVITY_TTL.
    pub inactivity_ttl: Option<u64>,

    /// Maximum output buffer size. Accepts human sizes like `512k`, `10m`, `1g`, `1t`. Default: `10m`. Env: SSH_SHELL_MAX_BUFFER_SIZE. Oldest bytes dropped first when full.
    pub max_buffer_size: Option<String>,
}

/// Implementation of `ssh_shell_open`.
pub async fn ssh_shell_open_impl(args: SshShellOpenArgs) -> Result<CallToolResult, McpError> {
    let SshShellOpenArgs {
        session_id,
        term,
        cols,
        rows,
        inactivity_ttl,
        max_buffer_size,
    } = args;
    let term = term.unwrap_or_else(|| "xterm".to_string());
    let cols = cols.unwrap_or(80);
    let rows = rows.unwrap_or(24);
    let inactivity_ttl = resolve_shell_inactivity_ttl(inactivity_ttl);
    let max_buffer_size = resolve_shell_max_buffer_size(max_buffer_size.as_deref());

    let current_count = SHELL_STORAGE.count_by_session(&session_id);
    if current_count >= MAX_SHELLS_PER_SESSION {
        return Ok(CallToolResult::error(vec![Content::text(format_error(
            "SSH_SHELL_OPEN",
            "MAX_SHELLS_EXCEEDED",
            "maximum shells per session reached",
            Some(&format!("limit={MAX_SHELLS_PER_SESSION}")),
        ))]));
    }

    let lookup = SESSION_STORAGE
        .get(&session_id)
        .map(|s| (Arc::clone(&s.handle), s.info.agent_id.clone()));
    let (handle_arc, agent_id) = match lookup {
        Some(pair) => pair,
        None => {
            return Ok(CallToolResult::error(vec![Content::text(
                err_session_not_found("SSH_SHELL_OPEN", &session_id),
            )]));
        }
    };

    let channel = match open_pty_shell(&handle_arc, &term, cols, rows).await {
        Ok(c) => c,
        Err(e) => {
            return Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_SHELL_OPEN",
                "CHANNEL_FAILED",
                &e,
                None,
            ))]));
        }
    };

    let body = register_shell(
        &session_id,
        agent_id,
        term,
        cols,
        rows,
        channel,
        inactivity_ttl,
        max_buffer_size,
    );
    Ok(CallToolResult::success(vec![Content::text(body)]))
}

// ---------------------------------------------------------------------------
// `ssh_shell_write`
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_shell_write` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellWriteArgs {
    /// SHELL_ID returned from ssh_shell_open.
    pub shell_id: String,

    /// Bytes to send to the PTY. Append `\n` to submit a typed command. Use control sequences directly (e.g. `\x03` for Ctrl+C, `\x1b[A` for arrow up). Prefer ssh_shell_send_key for named keystrokes.
    pub input: String,
}

/// Implementation of `ssh_shell_write`.
pub async fn ssh_shell_write_impl(args: SshShellWriteArgs) -> Result<CallToolResult, McpError> {
    let SshShellWriteArgs { shell_id, input } = args;
    let lookup = SHELL_STORAGE
        .get_direct(&shell_id)
        .map(|shell| (shell.input_tx.clone(), Arc::clone(&shell.last_activity_ms)));
    let (input_tx, last_activity_ms) = match lookup {
        Some(pair) => pair,
        None => {
            return Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_SHELL_WRITE",
                "SHELL_NOT_FOUND",
                "no active shell with the given ID",
                Some(&shell_id),
            ))]));
        }
    };

    let payload = Bytes::copy_from_slice(input.as_bytes());
    if let Err(e) = input_tx.send(WriteRequest::Data(payload)).await {
        return Ok(CallToolResult::error(vec![Content::text(format_error(
            "SSH_SHELL_WRITE",
            "WRITE_FAILED",
            &format!("shell writer task closed: {e}"),
            None,
        ))]));
    }

    touch_activity(&last_activity_ms);

    Ok(CallToolResult::success(vec![Content::text(
        render_shell_write_ok(&shell_id, input.len()),
    )]))
}

// ---------------------------------------------------------------------------
// `ssh_shell_send_key`
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_shell_send_key` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellSendKeyArgs {
    /// SHELL_ID returned from ssh_shell_open.
    pub shell_id: String,

    /// Named keystroke to send. Examples: `ctrl_c`, `arrow_up`, `f5`, `enter`, `tab`. See `ShellKey` for the full enum.
    pub key: ShellKey,

    /// Apply Shift modifier. Default: false. Valid on: arrows, navigation keys, F1-F12, and `tab`.
    pub shift: Option<bool>,

    /// Apply Alt modifier. Default: false. Valid on: arrows, navigation keys, F1-F12.
    pub alt: Option<bool>,

    /// Apply Ctrl modifier. Default: false. Valid on: arrows, navigation keys, F1-F12.
    pub ctrl: Option<bool>,

    /// Repeat the keystroke N times. Default: 1. Range: 1..=64.
    pub repeat: Option<u8>,
}

/// Format the active modifiers as a `+`-joined label, or `None` when no
/// modifier is set. Used by the response builder to render the
/// `MODIFIERS:` line.
fn format_modifiers_label(mods: KeyModifiers) -> Option<String> {
    if mods.is_empty() {
        return None;
    }
    let mut parts: Vec<&'static str> = Vec::with_capacity(3);
    if mods.shift {
        parts.push("shift");
    }
    if mods.alt {
        parts.push("alt");
    }
    if mods.ctrl {
        parts.push("ctrl");
    }
    Some(parts.join("+"))
}

/// Implementation of `ssh_shell_send_key`.
pub async fn ssh_shell_send_key_impl(
    args: SshShellSendKeyArgs,
) -> Result<CallToolResult, McpError> {
    let SshShellSendKeyArgs {
        shell_id,
        key,
        shift,
        alt,
        ctrl,
        repeat,
    } = args;

    let repeat = repeat.unwrap_or(1);
    if repeat == 0 || repeat > MAX_SEND_KEY_REPEAT {
        return Ok(CallToolResult::error(vec![Content::text(format_error(
            "SSH_SHELL_SEND_KEY",
            "INVALID_REPEAT",
            "repeat must be between 1 and 64 inclusive",
            Some(&format!("requested={repeat}")),
        ))]));
    }

    let mods = KeyModifiers {
        shift: shift.unwrap_or(false),
        alt: alt.unwrap_or(false),
        ctrl: ctrl.unwrap_or(false),
    };

    let payload = match key.encode(mods) {
        Ok(cow) => cow.into_owned(),
        Err(e) => match e {
            super::super::keys::EncodeError::ModifierNotAllowed {
                key_label,
                requested,
            } => {
                let detail =
                    format_modifiers_label(requested).unwrap_or_else(|| "(none)".to_string());
                return Ok(CallToolResult::error(vec![Content::text(format_error(
                    "SSH_SHELL_SEND_KEY",
                    "MODIFIER_NOT_ALLOWED",
                    &format!("key '{key_label}' rejects the requested modifier combination"),
                    Some(&format!("requested={detail}")),
                ))]));
            }
        },
    };

    let lookup = SHELL_STORAGE
        .get_direct(&shell_id)
        .map(|shell| (shell.input_tx.clone(), Arc::clone(&shell.last_activity_ms)));
    let (input_tx, last_activity_ms) = match lookup {
        Some(pair) => pair,
        None => {
            return Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_SHELL_SEND_KEY",
                "SHELL_NOT_FOUND",
                "no active shell with the given ID",
                Some(&shell_id),
            ))]));
        }
    };

    let payload_bytes = Bytes::copy_from_slice(&payload);
    let chunk_len = payload_bytes.len();
    for _ in 0..repeat {
        if let Err(e) = input_tx
            .send(WriteRequest::Data(payload_bytes.clone()))
            .await
        {
            return Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_SHELL_SEND_KEY",
                "WRITE_FAILED",
                &format!("shell writer task closed: {e}"),
                None,
            ))]));
        }
    }

    touch_activity(&last_activity_ms);

    let total_bytes = chunk_len.saturating_mul(usize::from(repeat));
    let modifiers_label = format_modifiers_label(mods);
    Ok(CallToolResult::success(vec![Content::text(
        render_shell_send_key_ok(
            &shell_id,
            key.label(),
            modifiers_label.as_deref(),
            repeat,
            total_bytes,
        ),
    )]))
}

// ---------------------------------------------------------------------------
// `ssh_shell_read`
// ---------------------------------------------------------------------------

/// Default `wait_timeout_secs` for `ssh_shell_read.wait` and
/// `ssh_shell_wait_for`.
const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 30;

/// Hard cap for `wait_timeout_secs` / `timeout_secs` (5 minutes).
const MAX_WAIT_TIMEOUT_SECS: u64 = 300;

/// Maximum number of patterns accepted by `ssh_shell_wait_for`.
const MAX_WAIT_FOR_PATTERNS: usize = 16;

/// Maximum length of any single pattern in bytes.
const MAX_WAIT_FOR_PATTERN_BYTES: usize = 1024;

/// Resolved long-poll handle pulled from a `RunningShell`.
///
/// All fields are `Arc`/`Clone` and safe to hold across await points; the
/// underlying DashMap `Ref` is dropped before the loop runs.
struct ShellLongPollHandle {
    history: Arc<ArcSwap<RingBuffer>>,
    status_rx: watch::Receiver<ShellStatus>,
    data_notify: Arc<Notify>,
    last_activity_ms: Arc<AtomicU64>,
}

impl ShellLongPollHandle {
    fn from_shell(shell: &RunningShell) -> Self {
        Self {
            history: Arc::clone(&shell.history),
            status_rx: shell.status_rx.clone(),
            data_notify: Arc::clone(&shell.data_notify),
            last_activity_ms: Arc::clone(&shell.last_activity_ms),
        }
    }
}

/// Arguments for the `ssh_shell_read` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellReadArgs {
    /// SHELL_ID returned from ssh_shell_open.
    pub shell_id: String,

    /// Drain the bytes that were rendered (head-based pagination). Default: true. With false the buffer is preserved (peek mode) for inspecting the same window multiple times.
    pub clear: Option<bool>,

    /// Maximum bytes shown in data block. Default: 16384. Cap: 1048576. Output rendered as the tail (most recent bytes). Env: SSH_MCP_OUTPUT_DEFAULT_BYTES / SSH_MCP_OUTPUT_MAX_BYTES_CAP.
    pub max_output_bytes: Option<usize>,

    /// FALLBACK long-poll. Block until min_bytes of new output arrive, shell closes, or wait_timeout_secs expires. Default: false. Prefer `resources/subscribe shell://<shell_id>/output` (realtime push) over polling.
    pub wait: Option<bool>,

    /// Maximum seconds to block when wait=true. Default: 30. Cap: 300.
    pub wait_timeout_secs: Option<u64>,

    /// Minimum new bytes to wait for before returning (only with wait=true). Default: 1 (any new byte returns). Capped at the resolved max_output_bytes. Floor: 1.
    pub min_bytes: Option<usize>,
}

/// Implementation of `ssh_shell_read`.
pub async fn ssh_shell_read_impl(args: SshShellReadArgs) -> Result<CallToolResult, McpError> {
    let SshShellReadArgs {
        shell_id,
        clear,
        max_output_bytes,
        wait,
        wait_timeout_secs,
        min_bytes,
    } = args;
    let clear = clear.unwrap_or(true);
    let max_bytes = clamp_output_bytes(max_output_bytes);
    let wait = wait.unwrap_or(false);

    let lookup = SHELL_STORAGE
        .get_direct(&shell_id)
        .map(|shell| ShellLongPollHandle::from_shell(&shell));
    let handle = match lookup {
        Some(h) => h,
        None => {
            return Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_SHELL_READ",
                "SHELL_NOT_FOUND",
                "no active shell with the given ID",
                Some(&shell_id),
            ))]));
        }
    };

    let (state, snapshot) = if wait {
        long_poll_shell_read(&handle, wait_timeout_secs, min_bytes, max_bytes).await
    } else {
        snapshot_shell_read(&handle)
    };

    let nonce = generate_nonce();
    let markdown =
        ShellReadBuilder::new(&shell_id, state, &snapshot.data, max_bytes, &nonce).build();

    if clear {
        let shown = snapshot.data.len().min(max_bytes);
        // rcu retries on contention with the reader task: if the reader
        // stored a fresh buffer between our load and CAS, the closure runs
        // again with the latest state — so newly arrived bytes are
        // preserved while the head we already rendered is dropped.
        handle.history.rcu(|current| {
            let head = current.data.len().min(shown);
            RingBuffer {
                data: current.data.slice(head..),
            }
        });
    }

    touch_activity(&handle.last_activity_ms);

    Ok(CallToolResult::success(vec![Content::text(markdown)]))
}

/// Snapshot-mode read (no waiting). Returns the current state and buffer.
fn snapshot_shell_read(handle: &ShellLongPollHandle) -> (ShellReadState, Arc<RingBuffer>) {
    let snapshot = handle.history.load_full();
    let state = match *handle.status_rx.borrow() {
        ShellStatus::Open => ShellReadState::Open,
        ShellStatus::Closed => ShellReadState::Closed,
    };
    (state, snapshot)
}

/// Long-poll-mode read. Blocks until `min_bytes` new bytes arrive, the shell
/// closes, or the deadline expires.
async fn long_poll_shell_read(
    handle: &ShellLongPollHandle,
    wait_timeout_secs: Option<u64>,
    min_bytes: Option<usize>,
    max_bytes: usize,
) -> (ShellReadState, Arc<RingBuffer>) {
    let timeout_secs = wait_timeout_secs
        .unwrap_or(DEFAULT_WAIT_TIMEOUT_SECS)
        .min(MAX_WAIT_TIMEOUT_SECS);
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let target_min = min_bytes.unwrap_or(1).clamp(1, max_bytes.max(1));

    let initial_snapshot = handle.history.load_full();
    let initial_len = initial_snapshot.data.len();
    let mut status_rx = handle.status_rx.clone();

    loop {
        let snapshot = handle.history.load_full();
        let new_bytes = snapshot.data.len().saturating_sub(initial_len);
        if new_bytes >= target_min {
            return (ShellReadState::Open, snapshot);
        }

        let status_now = current_status(&status_rx);
        match status_now {
            ShellStatus::Closed => return (ShellReadState::Closed, snapshot),
            ShellStatus::Open => {}
        }

        let now = Instant::now();
        if now >= deadline {
            return (ShellReadState::Timeout, snapshot);
        }
        let remaining = deadline - now;
        let notified = handle.data_notify.notified();
        tokio::select! {
            () = notified => {},
            () = sleep(remaining) => {
                let final_snapshot = handle.history.load_full();
                let final_new = final_snapshot.data.len().saturating_sub(initial_len);
                if final_new >= target_min {
                    return (ShellReadState::Open, final_snapshot);
                }
                // Status may have flipped to Closed between checks.
                let final_state = match current_status(&status_rx) {
                    ShellStatus::Closed => ShellReadState::Closed,
                    ShellStatus::Open => ShellReadState::Timeout,
                };
                return (final_state, final_snapshot);
            },
            res = status_rx.changed() => {
                // status changed; loop will re-check at the top.
                let _ = res;
            }
        }
    }
}

/// Read the current shell status by Copy to avoid holding the watch
/// `Ref` guard across await points.
fn current_status(rx: &watch::Receiver<ShellStatus>) -> ShellStatus {
    let guard = rx.borrow();
    *guard
}

// ---------------------------------------------------------------------------
// `ssh_shell_wait_for`
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_shell_wait_for` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellWaitForArgs {
    /// SHELL_ID returned from ssh_shell_open.
    pub shell_id: String,

    /// 1..=16 substring patterns. First match returns immediately as MATCHED_PATTERN. Each pattern up to 1024 bytes. Prefer `resources/subscribe shell://<shell_id>/output` (realtime push) over polling — use this for single-shot prompt gating.
    pub patterns: Vec<String>,

    /// Maximum seconds to wait. Default: 30. Cap: 300.
    pub timeout_secs: Option<u64>,

    /// Maximum bytes shown when matched. Default: 16384. Cap: 1048576. Env: SSH_MCP_OUTPUT_DEFAULT_BYTES / SSH_MCP_OUTPUT_MAX_BYTES_CAP.
    pub max_output_bytes: Option<usize>,

    /// Drain matched output from the shell history (head) after returning so subsequent reads start fresh. Default: true.
    pub clear: Option<bool>,
}

/// Outcome of a single buffer scan.
struct PatternMatch<'a> {
    /// Index into the original `patterns` slice.
    pattern: &'a str,
    /// Absolute end offset (exclusive) of the match in the buffer.
    end_offset: usize,
}

/// Scan `buffer` for the earliest occurrence of any pattern in `patterns`.
/// Ties are broken by the order of the `patterns` slice (first wins).
fn scan_for_first_match<'a>(buffer: &[u8], patterns: &'a [String]) -> Option<PatternMatch<'a>> {
    let mut best: Option<(usize, &'a str)> = None;
    for pattern in patterns {
        let needle = pattern.as_bytes();
        if needle.is_empty() {
            continue;
        }
        let Some(pos) = find_subslice(buffer, needle) else {
            continue;
        };
        match best {
            Some((cur, _)) if cur <= pos => {}
            _ => best = Some((pos, pattern.as_str())),
        }
    }
    best.map(|(pos, pattern)| PatternMatch {
        pattern,
        end_offset: pos.saturating_add(pattern.len()),
    })
}

/// Find the first occurrence of `needle` in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Validate the `patterns` argument. Returns the formatted error string if
/// invalid, otherwise `None`.
fn validate_wait_for_patterns(patterns: &[String]) -> Option<String> {
    if patterns.is_empty() {
        return Some(format_error(
            "SSH_SHELL_WAIT_FOR",
            "EMPTY_PATTERNS",
            "patterns must contain at least one entry",
            None,
        ));
    }
    if patterns.len() > MAX_WAIT_FOR_PATTERNS {
        return Some(format_error(
            "SSH_SHELL_WAIT_FOR",
            "TOO_MANY_PATTERNS",
            "patterns exceeds maximum of 16",
            Some(&format!("count={}", patterns.len())),
        ));
    }
    for pattern in patterns {
        if pattern.len() > MAX_WAIT_FOR_PATTERN_BYTES {
            return Some(format_error(
                "SSH_SHELL_WAIT_FOR",
                "PATTERN_TOO_LONG",
                "individual pattern exceeds 1024 bytes",
                Some(&format!("len={}", pattern.len())),
            ));
        }
    }
    None
}

/// Implementation of `ssh_shell_wait_for`.
pub async fn ssh_shell_wait_for_impl(
    args: SshShellWaitForArgs,
) -> Result<CallToolResult, McpError> {
    let SshShellWaitForArgs {
        shell_id,
        patterns,
        timeout_secs,
        max_output_bytes,
        clear,
    } = args;

    if let Some(err) = validate_wait_for_patterns(&patterns) {
        return Ok(CallToolResult::error(vec![Content::text(err)]));
    }

    let max_bytes = clamp_output_bytes(max_output_bytes);
    let clear = clear.unwrap_or(true);
    let timeout_secs = timeout_secs
        .unwrap_or(DEFAULT_WAIT_TIMEOUT_SECS)
        .min(MAX_WAIT_TIMEOUT_SECS);
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);

    let lookup = SHELL_STORAGE
        .get_direct(&shell_id)
        .map(|shell| ShellLongPollHandle::from_shell(&shell));
    let handle = match lookup {
        Some(h) => h,
        None => {
            return Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_SHELL_WAIT_FOR",
                "SHELL_NOT_FOUND",
                "no active shell with the given ID",
                Some(&shell_id),
            ))]));
        }
    };

    let (state, snapshot, drain_up_to) = wait_for_pattern_loop(&handle, &patterns, deadline).await;

    let nonce = generate_nonce();
    let data_slice: &[u8] = &snapshot.data;
    let markdown =
        ShellWaitForBuilder::new(&shell_id, state, data_slice, max_bytes, &nonce).build();

    if clear {
        if let Some(head_len) = drain_up_to {
            handle.history.rcu(|current| {
                let head = current.data.len().min(head_len);
                RingBuffer {
                    data: current.data.slice(head..),
                }
            });
        }
    }

    touch_activity(&handle.last_activity_ms);

    Ok(CallToolResult::success(vec![Content::text(markdown)]))
}

/// Loop body for `ssh_shell_wait_for`. Returns the final state, a snapshot
/// of the buffer that should be rendered, and an optional drain-up-to offset
/// (used only on MATCHED to chop the matched prefix off the head).
async fn wait_for_pattern_loop(
    handle: &ShellLongPollHandle,
    patterns: &[String],
    deadline: Instant,
) -> (ShellWaitForState, Arc<RingBuffer>, Option<usize>) {
    let max_pat_len = patterns.iter().map(String::len).max().unwrap_or(0);
    let mut last_scanned_offset: usize = 0;
    let mut status_rx = handle.status_rx.clone();

    loop {
        let snapshot = handle.history.load_full();
        if let Some(found) = scan_window(&snapshot.data, last_scanned_offset, max_pat_len, patterns)
        {
            return (
                ShellWaitForState::Matched {
                    pattern: found.pattern,
                },
                snapshot,
                Some(found.abs_end),
            );
        }
        last_scanned_offset = snapshot.data.len();

        let status_now = current_status(&status_rx);
        match status_now {
            ShellStatus::Closed => return (ShellWaitForState::Closed, snapshot, None),
            ShellStatus::Open => {}
        }

        let now = Instant::now();
        if now >= deadline {
            return (ShellWaitForState::Timeout, snapshot, None);
        }
        let remaining = deadline - now;
        let notified = handle.data_notify.notified();
        tokio::select! {
            () = notified => {},
            () = sleep(remaining) => {
                let final_snapshot = handle.history.load_full();
                if let Some(found) = scan_window(
                    &final_snapshot.data,
                    last_scanned_offset,
                    max_pat_len,
                    patterns,
                ) {
                    return (
                        ShellWaitForState::Matched { pattern: found.pattern },
                        final_snapshot,
                        Some(found.abs_end),
                    );
                }
                let final_state = match current_status(&status_rx) {
                    ShellStatus::Closed => ShellWaitForState::Closed,
                    ShellStatus::Open => ShellWaitForState::Timeout,
                };
                return (final_state, final_snapshot, None);
            },
            res = status_rx.changed() => {
                let _ = res;
            }
        }
    }
}

/// Resolved match within the buffer, including absolute end offset.
struct ResolvedMatch {
    pattern: String,
    abs_end: usize,
}

/// Scan the unscanned tail of `data` for any pattern. The window starts at
/// `last_scanned_offset - (max_pat_len - 1)` to catch matches that straddle
/// the previous scan boundary.
fn scan_window(
    data: &[u8],
    last_scanned_offset: usize,
    max_pat_len: usize,
    patterns: &[String],
) -> Option<ResolvedMatch> {
    let scan_start = last_scanned_offset.saturating_sub(max_pat_len.saturating_sub(1));
    if scan_start >= data.len() {
        return None;
    }
    let found = scan_for_first_match(&data[scan_start..], patterns)?;
    let abs_end = scan_start.saturating_add(found.end_offset);
    Some(ResolvedMatch {
        pattern: found.pattern.to_string(),
        abs_end,
    })
}

// ---------------------------------------------------------------------------
// `ssh_shell_close`
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_shell_close` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellCloseArgs {
    /// SHELL_ID returned from ssh_shell_open.
    pub shell_id: String,
}

/// Implementation of `ssh_shell_close`.
pub async fn ssh_shell_close_impl(args: SshShellCloseArgs) -> Result<CallToolResult, McpError> {
    let SshShellCloseArgs { shell_id } = args;
    let shell = match SHELL_STORAGE.unregister(&shell_id) {
        Some(s) => s,
        None => {
            return Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_SHELL_CLOSE",
                "SHELL_NOT_FOUND",
                "no active shell with the given ID",
                Some(&shell_id),
            ))]));
        }
    };

    let _ = shell.input_tx.send(WriteRequest::Close).await;
    shell.cancel_token.cancel();

    tracing::info!("Closed interactive shell: {shell_id}");

    Ok(CallToolResult::success(vec![Content::text(
        render_shell_close_ok(&shell_id),
    )]))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    mod format_modifiers_label {
        use super::*;

        #[test]
        fn empty_returns_none() {
            let mods = KeyModifiers::default();
            assert_eq!(format_modifiers_label(mods), None);
        }

        #[test]
        fn shift_only() {
            let mods = KeyModifiers {
                shift: true,
                alt: false,
                ctrl: false,
            };
            assert_eq!(format_modifiers_label(mods).as_deref(), Some("shift"));
        }

        #[test]
        fn shift_and_ctrl_joined_with_plus() {
            let mods = KeyModifiers {
                shift: true,
                alt: false,
                ctrl: true,
            };
            assert_eq!(format_modifiers_label(mods).as_deref(), Some("shift+ctrl"));
        }

        #[test]
        fn all_three_modifiers_in_canonical_order() {
            let mods = KeyModifiers {
                shift: true,
                alt: true,
                ctrl: true,
            };
            assert_eq!(
                format_modifiers_label(mods).as_deref(),
                Some("shift+alt+ctrl")
            );
        }

        #[test]
        fn alt_and_ctrl_joined() {
            let mods = KeyModifiers {
                shift: false,
                alt: true,
                ctrl: true,
            };
            assert_eq!(format_modifiers_label(mods).as_deref(), Some("alt+ctrl"));
        }
    }

    mod send_key_impl_validation {
        use super::*;

        async fn assert_invalid_repeat(repeat: u8) {
            let args = SshShellSendKeyArgs {
                shell_id: "missing".to_string(),
                key: ShellKey::CtrlC,
                shift: None,
                alt: None,
                ctrl: None,
                repeat: Some(repeat),
            };
            let result = ssh_shell_send_key_impl(args).await;
            let call = match result {
                Ok(c) => c,
                Err(e) => panic!("expected Ok with error CallToolResult, got {e:?}"),
            };
            // CallToolResult on error embeds isError=true and the formatted text.
            let body = format!("{call:?}");
            assert!(
                body.contains("INVALID_REPEAT"),
                "expected INVALID_REPEAT, got {body}"
            );
        }

        #[tokio::test]
        async fn rejects_repeat_zero() {
            assert_invalid_repeat(0).await;
        }

        #[tokio::test]
        async fn rejects_repeat_above_cap() {
            assert_invalid_repeat(65).await;
        }

        #[tokio::test]
        async fn rejects_modifier_on_ctrl_c() {
            let args = SshShellSendKeyArgs {
                shell_id: "missing".to_string(),
                key: ShellKey::CtrlC,
                shift: Some(true),
                alt: None,
                ctrl: None,
                repeat: Some(1),
            };
            let result = ssh_shell_send_key_impl(args).await;
            let call = match result {
                Ok(c) => c,
                Err(e) => panic!("expected Ok with error CallToolResult, got {e:?}"),
            };
            let body = format!("{call:?}");
            assert!(
                body.contains("MODIFIER_NOT_ALLOWED"),
                "expected MODIFIER_NOT_ALLOWED, got {body}"
            );
        }

        #[tokio::test]
        async fn missing_shell_returns_shell_not_found() {
            let args = SshShellSendKeyArgs {
                shell_id: "definitely-missing-shell-id".to_string(),
                key: ShellKey::ArrowUp,
                shift: None,
                alt: None,
                ctrl: None,
                repeat: Some(1),
            };
            let result = ssh_shell_send_key_impl(args).await;
            let call = match result {
                Ok(c) => c,
                Err(e) => panic!("expected Ok with error CallToolResult, got {e:?}"),
            };
            let body = format!("{call:?}");
            assert!(
                body.contains("SHELL_NOT_FOUND"),
                "expected SHELL_NOT_FOUND, got {body}"
            );
        }
    }

    mod max_send_key_repeat {
        use super::*;

        #[test]
        fn cap_is_64() {
            assert_eq!(MAX_SEND_KEY_REPEAT, 64);
        }
    }

    mod shell_read_long_poll_args {
        use super::*;

        #[tokio::test]
        async fn missing_shell_returns_shell_not_found_with_wait() {
            let args = SshShellReadArgs {
                shell_id: "definitely-missing-shell-id".to_string(),
                clear: None,
                max_output_bytes: None,
                wait: Some(true),
                wait_timeout_secs: Some(1),
                min_bytes: Some(1),
            };
            let call = match ssh_shell_read_impl(args).await {
                Ok(c) => c,
                Err(e) => panic!("expected Ok, got {e:?}"),
            };
            let body = format!("{call:?}");
            assert!(
                body.contains("SHELL_NOT_FOUND"),
                "expected SHELL_NOT_FOUND, got {body}"
            );
        }

        #[tokio::test]
        async fn deserialize_long_poll_fields() {
            let json = serde_json::json!({
                "shell_id": "shell-1",
                "wait": true,
                "wait_timeout_secs": 10,
                "min_bytes": 64,
            });
            let parsed: SshShellReadArgs =
                serde_json::from_value(json).expect("deserializes valid args");
            assert_eq!(parsed.shell_id, "shell-1");
            assert_eq!(parsed.wait, Some(true));
            assert_eq!(parsed.wait_timeout_secs, Some(10));
            assert_eq!(parsed.min_bytes, Some(64));
        }
    }

    mod scan_for_first_match {
        use super::*;

        #[test]
        fn returns_none_for_no_match() {
            let patterns = vec!["foo".to_string(), "bar".to_string()];
            let m = scan_for_first_match(b"hello world", &patterns);
            assert!(m.is_none());
        }

        #[test]
        fn finds_earliest_position_across_patterns() {
            // "$ " appears at offset 6, "x" appears at offset 0.
            let patterns = vec!["$ ".to_string(), "x".to_string()];
            let buf = b"x foo $ ready";
            let m = scan_for_first_match(buf, &patterns).expect("must match");
            assert_eq!(m.pattern, "x");
            assert_eq!(m.end_offset, 1);
        }

        #[test]
        fn ties_break_to_first_pattern_in_list() {
            // Both patterns start at offset 0; first one in `patterns` wins.
            let patterns = vec!["abc".to_string(), "ab".to_string()];
            let m = scan_for_first_match(b"abcdef", &patterns).expect("must match");
            assert_eq!(m.pattern, "abc");
            assert_eq!(m.end_offset, 3);
        }

        #[test]
        fn skips_empty_pattern() {
            let patterns = vec![String::new(), "x".to_string()];
            let m = scan_for_first_match(b"yyx", &patterns).expect("must match");
            assert_eq!(m.pattern, "x");
            assert_eq!(m.end_offset, 3);
        }

        #[test]
        fn earliest_position_wins_over_pattern_order() {
            // "second" wins because it appears earlier at offset 0, even
            // though "first" comes first in the patterns list.
            let patterns = vec!["first".to_string(), "second".to_string()];
            let m = scan_for_first_match(b"second first", &patterns).expect("must match");
            assert_eq!(m.pattern, "second");
            assert_eq!(m.end_offset, 6);
        }
    }

    mod scan_window {
        use super::*;

        #[test]
        fn straddling_boundary_is_caught() {
            // Buffer initial scan only saw "passwo".
            // Next buffer is "password:" — a naive scan from
            // last_scanned_offset=6 would miss the match.
            let patterns = vec!["password:".to_string()];
            let max_pat_len = 9;
            let m = scan_window(b"password:", 6, max_pat_len, &patterns)
                .expect("must catch boundary match");
            assert_eq!(m.pattern, "password:");
            assert_eq!(m.abs_end, 9);
        }

        #[test]
        fn returns_none_when_scan_start_past_buffer() {
            let patterns = vec!["x".to_string()];
            let m = scan_window(b"abc", 100, 1, &patterns);
            assert!(m.is_none());
        }
    }

    mod validate_wait_for_patterns {
        use super::*;

        #[test]
        fn empty_patterns_rejected() {
            let err = validate_wait_for_patterns(&[]).expect("must error");
            assert!(err.contains("EMPTY_PATTERNS"), "got {err}");
        }

        #[test]
        fn too_many_patterns_rejected() {
            let patterns: Vec<String> = (0..17).map(|i| format!("p{i}")).collect();
            let err = validate_wait_for_patterns(&patterns).expect("must error");
            assert!(err.contains("TOO_MANY_PATTERNS"), "got {err}");
            assert!(err.contains("count=17"), "got {err}");
        }

        #[test]
        fn pattern_too_long_rejected() {
            let patterns = vec!["x".repeat(MAX_WAIT_FOR_PATTERN_BYTES + 1)];
            let err = validate_wait_for_patterns(&patterns).expect("must error");
            assert!(err.contains("PATTERN_TOO_LONG"), "got {err}");
        }

        #[test]
        fn pattern_at_limit_accepted() {
            let patterns = vec!["x".repeat(MAX_WAIT_FOR_PATTERN_BYTES)];
            assert!(validate_wait_for_patterns(&patterns).is_none());
        }

        #[test]
        fn sixteen_patterns_accepted() {
            let patterns: Vec<String> = (0..16).map(|i| format!("p{i}")).collect();
            assert!(validate_wait_for_patterns(&patterns).is_none());
        }
    }

    mod wait_for_impl_validation {
        use super::*;

        #[tokio::test]
        async fn empty_patterns_returns_error() {
            let args = SshShellWaitForArgs {
                shell_id: "missing".to_string(),
                patterns: vec![],
                timeout_secs: Some(1),
                max_output_bytes: None,
                clear: None,
            };
            let call = match ssh_shell_wait_for_impl(args).await {
                Ok(c) => c,
                Err(e) => panic!("expected Ok, got {e:?}"),
            };
            let body = format!("{call:?}");
            assert!(body.contains("EMPTY_PATTERNS"), "got {body}");
        }

        #[tokio::test]
        async fn too_many_patterns_returns_error() {
            let patterns: Vec<String> = (0..20).map(|i| format!("p{i}")).collect();
            let args = SshShellWaitForArgs {
                shell_id: "missing".to_string(),
                patterns,
                timeout_secs: Some(1),
                max_output_bytes: None,
                clear: None,
            };
            let call = match ssh_shell_wait_for_impl(args).await {
                Ok(c) => c,
                Err(e) => panic!("expected Ok, got {e:?}"),
            };
            let body = format!("{call:?}");
            assert!(body.contains("TOO_MANY_PATTERNS"), "got {body}");
        }

        #[tokio::test]
        async fn pattern_too_long_returns_error() {
            let args = SshShellWaitForArgs {
                shell_id: "missing".to_string(),
                patterns: vec!["x".repeat(MAX_WAIT_FOR_PATTERN_BYTES + 1)],
                timeout_secs: Some(1),
                max_output_bytes: None,
                clear: None,
            };
            let call = match ssh_shell_wait_for_impl(args).await {
                Ok(c) => c,
                Err(e) => panic!("expected Ok, got {e:?}"),
            };
            let body = format!("{call:?}");
            assert!(body.contains("PATTERN_TOO_LONG"), "got {body}");
        }

        #[tokio::test]
        async fn missing_shell_returns_shell_not_found() {
            let args = SshShellWaitForArgs {
                shell_id: "definitely-missing-shell-id".to_string(),
                patterns: vec!["$ ".to_string()],
                timeout_secs: Some(1),
                max_output_bytes: None,
                clear: None,
            };
            let call = match ssh_shell_wait_for_impl(args).await {
                Ok(c) => c,
                Err(e) => panic!("expected Ok, got {e:?}"),
            };
            let body = format!("{call:?}");
            assert!(body.contains("SHELL_NOT_FOUND"), "got {body}");
        }
    }

    mod wait_for_args_serde {
        use super::*;

        #[test]
        fn deserialize_required_fields_only() {
            let json = serde_json::json!({
                "shell_id": "shell-1",
                "patterns": ["$ ", "password:"],
            });
            let parsed: SshShellWaitForArgs = serde_json::from_value(json).expect("valid args");
            assert_eq!(parsed.shell_id, "shell-1");
            assert_eq!(parsed.patterns, vec!["$ ", "password:"]);
            assert!(parsed.timeout_secs.is_none());
            assert!(parsed.max_output_bytes.is_none());
            assert!(parsed.clear.is_none());
        }

        #[test]
        fn deserialize_full_payload() {
            let json = serde_json::json!({
                "shell_id": "shell-1",
                "patterns": ["$ "],
                "timeout_secs": 60,
                "max_output_bytes": 4096,
                "clear": false,
            });
            let parsed: SshShellWaitForArgs = serde_json::from_value(json).expect("valid args");
            assert_eq!(parsed.timeout_secs, Some(60));
            assert_eq!(parsed.max_output_bytes, Some(4096));
            assert_eq!(parsed.clear, Some(false));
        }
    }
}
