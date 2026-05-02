//! Interactive PTY shell tools: `ssh_shell_open`, `ssh_shell_write`,
//! `ssh_shell_read`, `ssh_shell_close`.
//!
//! Each session can host up to [`MAX_SHELLS_PER_SESSION`] interactive PTYs.
//! Output is buffered in-memory with head-truncation past the configured
//! `max_buffer_size`.

use std::sync::Arc;

use rmcp::model::{CallToolResult, Content, ErrorData as McpError};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::time;

use super::super::client::open_pty_shell;
use super::super::config::{resolve_shell_inactivity_ttl, resolve_shell_max_buffer_size};
use super::super::message::builder::{
    ShellReadBuilder, ShellReadState, render_shell_close_ok, render_shell_write_ok,
};
use super::super::message::helpers::{format_error, generate_nonce};
use super::super::shell::MAX_SHELLS_PER_SESSION;
use super::super::storage::session::SESSION_STORAGE;
use super::super::storage::shell::SHELL_STORAGE;
use super::super::storage::traits::{SessionStorage, ShellStorage};
use super::super::types::ShellStatus;
use super::legacy_helpers::{clamp_output_bytes, err_session_not_found, register_shell};

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
    let lookup = SHELL_STORAGE.get_direct(&shell_id).map(|shell| {
        (
            Arc::clone(&shell.channel_writer),
            Arc::clone(&shell.last_activity),
        )
    });
    let (channel_writer, last_activity) = match lookup {
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

    if let Err(e) = channel_writer.lock().await.write(input.as_bytes()).await {
        return Ok(CallToolResult::error(vec![Content::text(format_error(
            "SSH_SHELL_WRITE",
            "WRITE_FAILED",
            &e,
            None,
        ))]));
    }

    *last_activity.lock().await = time::Instant::now();

    Ok(CallToolResult::success(vec![Content::text(
        render_shell_write_ok(&shell_id, input.len()),
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
            Arc::clone(&shell.output),
            shell.status_rx.clone(),
            Arc::clone(&shell.last_activity),
        )
    });
    let (output_arc, status_rx, last_activity) = match lookup {
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

    let markdown = {
        let mut guard = output_arc.lock().await;
        let markdown = ShellReadBuilder::new(&shell_id, state, &guard, max_bytes, &nonce).build();
        if clear {
            let shown = guard.len().min(max_bytes);
            guard.drain(..shown);
            if guard.capacity() > guard.len().saturating_mul(4) {
                guard.shrink_to_fit();
            }
        }
        markdown
    };

    *last_activity.lock().await = time::Instant::now();

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

    shell.cancel_token.cancel();
    let _ = shell.channel_writer.lock().await.close().await;

    tracing::info!("Closed interactive shell: {shell_id}");

    Ok(CallToolResult::success(vec![Content::text(
        render_shell_close_ok(&shell_id),
    )]))
}
