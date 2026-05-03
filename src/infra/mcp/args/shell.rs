//! Interactive PTY shell argument types.
//!
//! Mirrors v3 `src/mcp/tools/shell.rs::Ssh*Args` exactly.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::domain::keys::ShellKey;

/// Arguments for the `ssh_shell_open` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellOpenArgs {
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,

    /// Terminal type. Default: `xterm`. Use `vt100` or `ansi` for
    /// SOL/IPMI/serial consoles.
    pub term: Option<String>,

    /// Terminal width in columns. Default: 80.
    pub cols: Option<u32>,

    /// Terminal height in rows. Default: 24.
    pub rows: Option<u32>,

    /// Inactivity TTL in seconds. Shell auto-closes if no read or write
    /// happens within this window. Default: 600. Env:
    /// `SSH_SHELL_INACTIVITY_TTL`.
    pub inactivity_ttl: Option<u64>,

    /// Maximum output buffer size. Accepts human sizes like `512k`,
    /// `10m`, `1g`, `1t`. Default: `10m`. Env: `SSH_SHELL_MAX_BUFFER_SIZE`.
    /// Oldest bytes dropped first when full.
    pub max_buffer_size: Option<String>,
}

/// Arguments for the `ssh_shell_write` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellWriteArgs {
    /// `SHELL_ID` returned from `ssh_shell_open`.
    pub shell_id: String,

    /// Bytes to send to the PTY. Append `\n` to submit a typed command.
    /// Use control sequences directly (e.g. `\x03` for Ctrl+C, `\x1b[A`
    /// for arrow up). Prefer `ssh_shell_send_key` for named keystrokes.
    pub input: String,
}

/// Arguments for the `ssh_shell_send_key` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellSendKeyArgs {
    /// `SHELL_ID` returned from `ssh_shell_open`.
    pub shell_id: String,

    /// Named keystroke to send. Examples: `ctrl_c`, `arrow_up`, `f5`,
    /// `enter`, `tab`. See `ShellKey` for the full enum.
    pub key: ShellKey,

    /// Apply Shift modifier. Default: false. Valid on: arrows,
    /// navigation keys, F1-F12, and `tab`.
    pub shift: Option<bool>,

    /// Apply Alt modifier. Default: false. Valid on: arrows, navigation
    /// keys, F1-F12.
    pub alt: Option<bool>,

    /// Apply Ctrl modifier. Default: false. Valid on: arrows, navigation
    /// keys, F1-F12.
    pub ctrl: Option<bool>,

    /// Repeat the keystroke N times. Default: 1. Range: 1..=64.
    pub repeat: Option<u8>,
}

/// Arguments for the `ssh_shell_read` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellReadArgs {
    /// `SHELL_ID` returned from `ssh_shell_open`.
    pub shell_id: String,

    /// Drain the bytes that were rendered (head-based pagination).
    /// Default: true. With false the buffer is preserved (peek mode) for
    /// inspecting the same window multiple times.
    pub clear: Option<bool>,

    /// Maximum bytes shown in data block. Default: 16384. Cap: 1048576.
    /// Output rendered as the tail (most recent bytes). Env:
    /// `SSH_MCP_OUTPUT_DEFAULT_BYTES` / `SSH_MCP_OUTPUT_MAX_BYTES_CAP`.
    pub max_output_bytes: Option<usize>,

    /// FALLBACK long-poll. Block until `min_bytes` of new output arrive,
    /// the shell closes, or `wait_timeout_secs` expires. Default: false.
    /// Prefer `resources/subscribe shell://<shell_id>/output` (realtime
    /// push) over polling.
    pub wait: Option<bool>,

    /// Maximum seconds to block when `wait=true`. Default: 30. Cap: 300.
    pub wait_timeout_secs: Option<u64>,

    /// Minimum new bytes to wait for before returning (only with
    /// `wait=true`). Default: 1 (any new byte returns). Capped at the
    /// resolved `max_output_bytes`. Floor: 1.
    pub min_bytes: Option<usize>,
}

/// Arguments for the `ssh_shell_wait_for` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellWaitForArgs {
    /// `SHELL_ID` returned from `ssh_shell_open`.
    pub shell_id: String,

    /// 1..=16 substring patterns. First match returns immediately as
    /// `MATCHED_PATTERN`. Each pattern up to 1024 bytes. Prefer
    /// `resources/subscribe shell://<shell_id>/output` (realtime push)
    /// over polling — use this for single-shot prompt gating.
    pub patterns: Vec<String>,

    /// Maximum seconds to wait. Default: 30. Cap: 300.
    pub timeout_secs: Option<u64>,

    /// Maximum bytes shown when matched. Default: 16384. Cap: 1048576.
    /// Env: `SSH_MCP_OUTPUT_DEFAULT_BYTES` /
    /// `SSH_MCP_OUTPUT_MAX_BYTES_CAP`.
    pub max_output_bytes: Option<usize>,

    /// Drain matched output from the shell history (head) after
    /// returning so subsequent reads start fresh. Default: true.
    pub clear: Option<bool>,
}

/// Arguments for the `ssh_shell_close` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshShellCloseArgs {
    /// `SHELL_ID` returned from `ssh_shell_open`.
    pub shell_id: String,
}
