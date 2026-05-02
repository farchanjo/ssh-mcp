//! Interactive PTY shell tools: `ssh_shell_open`, `ssh_shell_write`,
//! `ssh_shell_read`, `ssh_shell_close`.
//!
//! Each session can host up to [`MAX_SHELLS_PER_SESSION`] interactive PTYs.
//! Output is buffered in-memory with head-truncation past the configured
//! `max_buffer_size`.

use std::sync::Arc;

use bytes::Bytes;
use rmcp::model::{CallToolResult, Content, ErrorData as McpError};
use schemars::JsonSchema;
use serde::Deserialize;

use super::super::client::open_pty_shell;
use super::super::config::{resolve_shell_inactivity_ttl, resolve_shell_max_buffer_size};
use super::super::keys::{KeyModifiers, ShellKey};
use super::super::message::builder::{
    ShellReadBuilder, ShellReadState, render_shell_close_ok, render_shell_send_key_ok,
    render_shell_write_ok,
};
use super::super::message::helpers::{format_error, generate_nonce};
use super::super::shell::{MAX_SHELLS_PER_SESSION, RingBuffer, WriteRequest, touch_activity};
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
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,

    /// Terminal type. Default: `"xterm"`. Use `"vt100"` or `"ansi"` for
    /// SOL/IPMI/serial consoles.
    pub term: Option<String>,

    /// Terminal width in columns. Default: `80`.
    pub cols: Option<u32>,

    /// Terminal height in rows. Default: `24`.
    pub rows: Option<u32>,

    /// Inactivity TTL in seconds. The shell auto-closes if no read or write
    /// happens within this window. Default: `600`.
    /// Env: `SSH_SHELL_INACTIVITY_TTL`.
    pub inactivity_ttl: Option<u64>,

    /// Maximum output buffer size. Accepts human sizes like `"512k"`, `"10m"`,
    /// `"1g"`, `"1t"`. Default: `"10m"`. Env: `SSH_SHELL_MAX_BUFFER_SIZE`.
    /// When the buffer is full the oldest bytes are dropped first.
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
    /// `SHELL_ID` returned by `ssh_shell_open`.
    pub shell_id: String,

    /// Bytes to send to the PTY. Append `"\n"` to submit a typed command.
    /// Use control sequences directly (e.g. `"\x03"` for `Ctrl+C`,
    /// `"\x1b[A"` for arrow up).
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
    /// `SHELL_ID` returned from `ssh_shell_open`.
    pub shell_id: String,

    /// Named keystroke to send. See [`ShellKey`] for the allowed values.
    pub key: ShellKey,

    /// Apply Shift modifier. Valid on: arrows, navigation keys, F1-F12,
    /// and `tab`. Default: `false`.
    pub shift: Option<bool>,

    /// Apply Alt modifier. Valid on: arrows, navigation keys, F1-F12.
    /// Default: `false`.
    pub alt: Option<bool>,

    /// Apply Ctrl modifier. Valid on: arrows, navigation keys, F1-F12.
    /// Default: `false`.
    pub ctrl: Option<bool>,

    /// Repeat the keystroke `N` times. Default: `1`. Range: `1..=64`.
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

/// Arguments for the `ssh_shell_read` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellReadArgs {
    /// `SHELL_ID` returned by `ssh_shell_open`.
    pub shell_id: String,

    /// Drain the bytes that were rendered (head-based pagination). Default:
    /// `true`. With `false`, the buffer is preserved (peek mode) — useful for
    /// inspecting the same window multiple times.
    pub clear: Option<bool>,

    /// Maximum bytes to render. Default: `16384`. Cap: `1_048_576`. Output is
    /// rendered as the tail (most recent bytes).
    pub max_output_bytes: Option<usize>,
}

/// Implementation of `ssh_shell_read`.
pub async fn ssh_shell_read_impl(args: SshShellReadArgs) -> Result<CallToolResult, McpError> {
    let SshShellReadArgs {
        shell_id,
        clear,
        max_output_bytes,
    } = args;
    let clear = clear.unwrap_or(true);
    let max_bytes = clamp_output_bytes(max_output_bytes);

    let lookup = SHELL_STORAGE.get_direct(&shell_id).map(|shell| {
        (
            Arc::clone(&shell.history),
            shell.status_rx.clone(),
            Arc::clone(&shell.last_activity_ms),
        )
    });
    let (history, status_rx, last_activity_ms) = match lookup {
        Some(t) => t,
        None => {
            return Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_SHELL_READ",
                "SHELL_NOT_FOUND",
                "no active shell with the given ID",
                Some(&shell_id),
            ))]));
        }
    };

    let nonce = generate_nonce();
    let status = *status_rx.borrow();
    let state = match status {
        ShellStatus::Open => ShellReadState::Open,
        ShellStatus::Closed => ShellReadState::Closed,
    };

    // Lock-free snapshot: load_full returns a consistent Arc<RingBuffer> view.
    let snapshot = history.load_full();
    let markdown =
        ShellReadBuilder::new(&shell_id, state, &snapshot.data, max_bytes, &nonce).build();

    if clear {
        let shown = snapshot.data.len().min(max_bytes);
        // rcu retries on contention with the reader task: if the reader
        // stored a fresh buffer between our load and CAS, the closure runs
        // again with the latest state — so newly arrived bytes are
        // preserved while the head we already rendered is dropped.
        history.rcu(|current| {
            let head = current.data.len().min(shown);
            RingBuffer {
                data: current.data.slice(head..),
            }
        });
    }

    touch_activity(&last_activity_ms);

    Ok(CallToolResult::success(vec![Content::text(markdown)]))
}

// ---------------------------------------------------------------------------
// `ssh_shell_close`
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_shell_close` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellCloseArgs {
    /// `SHELL_ID` to close.
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
}
