//! Markdown response builders for the MCP SSH tools.
//!
//! Each builder produces the full response text for a tool call, using the
//! shared helpers in `super::helpers` for nonce generation, UTF-8 safe
//! truncation, value sanitization, and standardized error formatting.
//!
//! # Format overview
//!
//! - First line: `TOOL_NAME: STATUS` (where status is uppercase
//!   `SCREAMING_SNAKE_CASE` — `OK`, `REUSED`, `SUGGESTED`, `STARTED`,
//!   `RUNNING`, `COMPLETED`, `FAILED`, `TIMEOUT`, `CANCELLED`, `NOOP`,
//!   `OPEN`, `CLOSED`, `MATCHED`, `ERROR`).
//! - Block layout (uniform): `KEY: value` per line, one field per line.
//!   The inline `KEY: value | KEY: value` separator was removed in v3.0
//!   (E6) to keep responses uniform and easier to parse line-by-line.
//! - Identifiers use the `_ID` suffix (`SESSION_ID`, `COMMAND_ID`,
//!   `SHELL_ID`, `TRANSFER_ID`).

use super::helpers::{format_bytes_human, render_output_block, sanitize_value};
use crate::mcp::types::{AsyncCommandInfo, AsyncCommandStatus, SessionInfo};

// ---------------------------------------------------------------------------
// ssh_connect — OK / REUSED
// ---------------------------------------------------------------------------

/// Builder for `ssh_connect` in OK or REUSED scenarios.
#[must_use]
pub struct ConnectOkBuilder {
    session_id: String,
    username: String,
    host: String,
    agent_id: Option<String>,
    retry_attempts: u32,
    persistent: bool,
    reused: bool,
    replaced: usize,
}

impl ConnectOkBuilder {
    /// Create a new builder with the required identifiers.
    pub fn new(
        session_id: impl Into<String>,
        username: impl Into<String>,
        host: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            username: username.into(),
            host: host.into(),
            agent_id: None,
            retry_attempts: 0,
            persistent: false,
            reused: false,
            replaced: 0,
        }
    }

    /// Attach an agent identifier.
    pub fn with_agent_id(mut self, agent_id: Option<impl Into<String>>) -> Self {
        self.agent_id = agent_id.map(Into::into);
        self
    }

    /// Set the retry attempts counter.
    pub const fn with_retry_attempts(mut self, retry: u32) -> Self {
        self.retry_attempts = retry;
        self
    }

    /// Mark whether the session uses persistent keepalive.
    pub const fn with_persistent(mut self, persistent: bool) -> Self {
        self.persistent = persistent;
        self
    }

    /// Mark whether this response is a REUSED (already existed) case.
    pub const fn reused(mut self, reused: bool) -> Self {
        self.reused = reused;
        self
    }

    /// Record how many unhealthy sessions were disconnected before creating this one.
    pub const fn with_replaced(mut self, replaced: usize) -> Self {
        self.replaced = replaced;
        self
    }

    /// Render the markdown response.
    #[must_use]
    pub fn build(&self) -> String {
        let status = if self.reused { "REUSED" } else { "OK" };
        let mut out = String::with_capacity(192);
        out.push_str("SSH_CONNECT: ");
        out.push_str(status);
        out.push('\n');
        out.push_str("SESSION_ID: ");
        out.push_str(&self.session_id);
        out.push_str("\nHOST: ");
        out.push_str(&self.username);
        out.push('@');
        out.push_str(&self.host);
        if let Some(aid) = &self.agent_id {
            out.push_str("\nAGENT: ");
            out.push_str(&sanitize_value(aid));
        }
        // REUSED omits RETRY / PERSISTENT — the original connect set those.
        if !self.reused {
            out.push_str("\nRETRY: ");
            out.push_str(&self.retry_attempts.to_string());
            out.push_str("\nPERSISTENT: ");
            out.push_str(if self.persistent { "true" } else { "false" });
            if self.replaced > 0 {
                out.push_str("\nREPLACED: ");
                out.push_str(&self.replaced.to_string());
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// ssh_connect — SUGGESTED (reuse detection)
// ---------------------------------------------------------------------------

/// Information about an existing session that matches a connect request.
#[derive(Debug, Clone)]
pub struct SessionMatch {
    pub session_id: String,
    /// Formatted as `"user@host:port"`.
    pub host: String,
    pub agent_id: Option<String>,
    pub name: Option<String>,
    /// Connection timestamp in RFC3339 form.
    pub connected_at: String,
    pub healthy: bool,
}

/// Builder for `ssh_connect` in SUGGESTED scenario (one or more matching sessions).
#[must_use]
pub struct ConnectSuggestedBuilder {
    matches: Vec<SessionMatch>,
}

impl ConnectSuggestedBuilder {
    /// Create a new suggested builder from a non-empty match list.
    pub const fn new(matches: Vec<SessionMatch>) -> Self {
        Self { matches }
    }

    /// Render the markdown response.
    #[must_use]
    pub fn build(&self) -> String {
        if self.matches.len() == 1 {
            self.render_single()
        } else {
            self.render_multi()
        }
    }

    fn render_single(&self) -> String {
        let Some(m) = self.matches.first() else {
            return String::from("SSH_CONNECT: SUGGESTED\nMATCHES: 0");
        };
        let mut out = String::with_capacity(256);
        out.push_str("SSH_CONNECT: SUGGESTED\nEXISTING_SESSION_ID: ");
        out.push_str(&m.session_id);
        out.push_str("\nHOST: ");
        out.push_str(&m.host);
        if let Some(a) = &m.agent_id {
            out.push_str("\nAGENT: ");
            out.push_str(&sanitize_value(a));
        }
        if let Some(n) = &m.name {
            out.push_str("\nNAME: ");
            out.push_str(&sanitize_value(n));
        }
        out.push_str("\nCONNECTED_AT: ");
        out.push_str(&m.connected_at);
        out.push_str("\nHEALTHY: ");
        out.push_str(if m.healthy { "true" } else { "false" });
        out.push_str("\nHINT: use existing SESSION_ID, or retry with reuse=\"force_new\"");
        out
    }

    fn render_multi(&self) -> String {
        let mut out = String::with_capacity(64 + self.matches.len() * 128);
        out.push_str("SSH_CONNECT: SUGGESTED\nMATCHES: ");
        out.push_str(&self.matches.len().to_string());
        for m in &self.matches {
            out.push_str("\n- ");
            out.push_str(&m.session_id);
            out.push(' ');
            out.push_str(&m.host);
            append_match_decorations(&mut out, m);
        }
        out.push_str("\nHINT: pick an existing SESSION_ID, or retry with reuse=\"force_new\"");
        out
    }
}

#[allow(
    clippy::useless_let_if_seq,
    reason = "accumulator tracking multiple optional fields is clearer than nested if/else"
)]
fn append_match_decorations(out: &mut String, m: &SessionMatch) {
    out.push_str(" [");
    let mut wrote_field = false;
    if let Some(a) = &m.agent_id {
        out.push_str("agent: ");
        out.push_str(&sanitize_value(a));
        wrote_field = true;
    }
    if let Some(n) = &m.name {
        append_separator_if(out, wrote_field);
        out.push_str("name: ");
        out.push_str(&sanitize_value(n));
        wrote_field = true;
    }
    append_separator_if(out, wrote_field);
    out.push_str("connected: ");
    out.push_str(extract_time(&m.connected_at));
    out.push_str(", ");
    out.push_str(if m.healthy { "healthy" } else { "unhealthy" });
    out.push(']');
}

/// Extract the `HH:MM:SS` portion from an RFC3339 timestamp, or return the
/// entire input if it does not contain a `T` separator.
fn extract_time(ts: &str) -> &str {
    if let Some((_, rest)) = ts.split_once('T') {
        // Strip timezone suffix like "Z" or "+00:00".
        let end = rest.find(['Z', '+', '-']).unwrap_or(rest.len());
        &rest[..end]
    } else {
        ts
    }
}

// ---------------------------------------------------------------------------
// ssh_disconnect
// ---------------------------------------------------------------------------

/// Block response for `ssh_disconnect: OK`.
#[must_use]
pub fn render_disconnect_ok(session_id: &str) -> String {
    let mut out = String::with_capacity(48);
    out.push_str("SSH_DISCONNECT: OK\nSESSION_ID: ");
    out.push_str(session_id);
    out
}

// ---------------------------------------------------------------------------
// ssh_list_sessions
// ---------------------------------------------------------------------------

/// Builder for `ssh_list_sessions`.
#[must_use]
pub struct ListSessionsBuilder<'a> {
    sessions: &'a [SessionInfo],
    total: usize,
}

impl<'a> ListSessionsBuilder<'a> {
    /// Create a new list sessions builder. `total` is the count before
    /// pagination; `sessions.len()` may be less if `max_items` was applied.
    pub const fn new(sessions: &'a [SessionInfo], total: usize) -> Self {
        Self { sessions, total }
    }

    /// Render the markdown response.
    #[must_use]
    pub fn build(&self) -> String {
        if self.sessions.is_empty() && self.total == 0 {
            return String::from("SSH_LIST_SESSIONS: OK\nCOUNT: 0");
        }
        let mut out = String::with_capacity(64 + self.sessions.len() * 128);
        out.push_str("SSH_LIST_SESSIONS: OK\nCOUNT: ");
        out.push_str(&self.sessions.len().to_string());
        if self.total > self.sessions.len() {
            out.push_str(" (showing ");
            out.push_str(&self.sessions.len().to_string());
            out.push_str(" of ");
            out.push_str(&self.total.to_string());
            out.push(')');
        }
        for s in self.sessions {
            out.push_str("\n- ");
            append_session_item(&mut out, s);
        }
        out
    }
}

fn append_session_item(out: &mut String, s: &SessionInfo) {
    out.push_str(&s.session_id);
    out.push(' ');
    out.push_str(&sanitize_value(&s.username));
    out.push('@');
    out.push_str(&sanitize_value(&s.host));
    append_session_flags(out, s);
}

#[allow(
    clippy::useless_let_if_seq,
    clippy::too_many_lines,
    reason = "accumulator tracking multiple optional fields; restructure would hurt readability"
)]
fn append_session_flags(out: &mut String, s: &SessionInfo) {
    let show_compression = !s.compression_enabled;
    let health_label = match s.healthy {
        Some(true) => Some("healthy"),
        Some(false) => Some("unhealthy"),
        None => None,
    };
    if s.agent_id.is_none() && s.name.is_none() && !show_compression && health_label.is_none() {
        return;
    }
    out.push_str(" [");
    let mut wrote = false;
    if let Some(a) = &s.agent_id {
        out.push_str("agent: ");
        out.push_str(&sanitize_value(a));
        wrote = true;
    }
    if let Some(n) = &s.name {
        append_separator_if(out, wrote);
        out.push_str("name: ");
        out.push_str(&sanitize_value(n));
        wrote = true;
    }
    if show_compression {
        append_separator_if(out, wrote);
        out.push_str("compression: off");
        wrote = true;
    }
    if let Some(label) = health_label {
        append_separator_if(out, wrote);
        out.push_str(label);
    }
    out.push(']');
}

fn append_separator_if(out: &mut String, yes: bool) {
    if yes {
        out.push_str(", ");
    }
}

// ---------------------------------------------------------------------------
// ssh_disconnect_agent
// ---------------------------------------------------------------------------

/// Block response for `ssh_disconnect_agent: OK`.
#[must_use]
pub fn render_disconnect_agent(agent_id: &str, sessions: usize, commands: usize) -> String {
    let mut out = String::with_capacity(96);
    out.push_str("SSH_DISCONNECT_AGENT: OK\nAGENT: ");
    out.push_str(&sanitize_value(agent_id));
    out.push_str("\nSESSIONS: ");
    out.push_str(&sessions.to_string());
    out.push_str("\nCOMMANDS: ");
    out.push_str(&commands.to_string());
    out
}

// ---------------------------------------------------------------------------
// ssh_execute (STARTED)
// ---------------------------------------------------------------------------

/// Builder for the `ssh_execute: STARTED` response.
#[must_use]
#[allow(
    clippy::struct_field_names,
    reason = "identifier fields naturally share the _id suffix for the MCP contract"
)]
pub struct ExecuteStartedBuilder {
    command_id: String,
    session_id: String,
    agent_id: Option<String>,
}

impl ExecuteStartedBuilder {
    /// Create a new builder.
    pub fn new(command_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            command_id: command_id.into(),
            session_id: session_id.into(),
            agent_id: None,
        }
    }

    /// Attach an agent identifier.
    pub fn with_agent_id(mut self, agent_id: Option<impl Into<String>>) -> Self {
        self.agent_id = agent_id.map(Into::into);
        self
    }

    /// Render the markdown response.
    #[must_use]
    pub fn build(&self) -> String {
        let mut out = String::with_capacity(128);
        out.push_str("SSH_EXECUTE: STARTED\nCOMMAND_ID: ");
        out.push_str(&self.command_id);
        out.push_str("\nSESSION_ID: ");
        out.push_str(&self.session_id);
        if let Some(a) = &self.agent_id {
            out.push_str("\nAGENT: ");
            out.push_str(&sanitize_value(a));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// ssh_get_command_output
// ---------------------------------------------------------------------------

/// State of an async command for `ssh_get_command_output`.
#[derive(Debug, Clone, Copy)]
pub enum GetCommandOutputState {
    /// The command is still running. Content is marked `(partial)`.
    Running,
    /// The command completed with the given exit code.
    Completed(i32),
    /// The command timed out. Content is marked `(partial)`.
    Timeout,
}

/// Builder for `ssh_get_command_output`.
#[must_use]
pub struct GetCommandOutputBuilder<'a> {
    command_id: &'a str,
    state: GetCommandOutputState,
    stdout: &'a [u8],
    stderr: &'a [u8],
    max_output_bytes: usize,
    nonce: &'a str,
}

impl<'a> GetCommandOutputBuilder<'a> {
    /// Create a new builder. `max_output_bytes` governs both stdout and stderr tails.
    pub const fn new(
        command_id: &'a str,
        state: GetCommandOutputState,
        stdout: &'a [u8],
        stderr: &'a [u8],
        max_output_bytes: usize,
        nonce: &'a str,
    ) -> Self {
        Self {
            command_id,
            state,
            stdout,
            stderr,
            max_output_bytes,
            nonce,
        }
    }

    /// Render the markdown response.
    #[must_use]
    pub fn build(&self) -> String {
        let (status, hint, exit) = classify_command_output_state(self.state);
        let stdout_block = render_output_block(
            "stdout",
            self.nonce,
            self.stdout,
            self.max_output_bytes,
            hint,
        );
        let stderr_block = render_output_block(
            "stderr",
            self.nonce,
            self.stderr,
            self.max_output_bytes,
            hint,
        );
        let mut out = String::with_capacity(128 + stdout_block.len() + stderr_block.len());
        out.push_str("SSH_GET_COMMAND_OUTPUT: ");
        out.push_str(status);
        out.push_str("\nCOMMAND_ID: ");
        out.push_str(self.command_id);
        if let Some(code) = exit {
            out.push_str("\nEXIT: ");
            out.push_str(&code.to_string());
        }
        out.push('\n');
        out.push_str(&stdout_block);
        out.push('\n');
        out.push_str(&stderr_block);
        out
    }
}

const fn classify_command_output_state(
    state: GetCommandOutputState,
) -> (&'static str, Option<&'static str>, Option<i32>) {
    match state {
        GetCommandOutputState::Running => ("RUNNING", Some("partial"), None),
        GetCommandOutputState::Completed(code) => ("COMPLETED", None, Some(code)),
        GetCommandOutputState::Timeout => ("TIMEOUT", Some("partial"), None),
    }
}

// ---------------------------------------------------------------------------
// ssh_list_commands
// ---------------------------------------------------------------------------

/// Builder for `ssh_list_commands`.
#[must_use]
pub struct ListCommandsBuilder<'a> {
    commands: &'a [AsyncCommandInfo],
    total: usize,
}

impl<'a> ListCommandsBuilder<'a> {
    /// Create a new builder. `total` is the count before pagination.
    pub const fn new(commands: &'a [AsyncCommandInfo], total: usize) -> Self {
        Self { commands, total }
    }

    /// Render the markdown response.
    #[must_use]
    pub fn build(&self) -> String {
        if self.commands.is_empty() && self.total == 0 {
            return String::from("SSH_LIST_COMMANDS: OK\nCOUNT: 0");
        }
        let mut out = String::with_capacity(64 + self.commands.len() * 128);
        out.push_str("SSH_LIST_COMMANDS: OK\nCOUNT: ");
        out.push_str(&self.commands.len().to_string());
        if self.total > self.commands.len() {
            out.push_str(" (showing ");
            out.push_str(&self.commands.len().to_string());
            out.push_str(" of ");
            out.push_str(&self.total.to_string());
            out.push(')');
        }
        for c in self.commands {
            out.push_str("\n- ");
            append_command_item(&mut out, c);
        }
        out
    }
}

fn append_command_item(out: &mut String, c: &AsyncCommandInfo) {
    out.push_str(&c.command_id);
    out.push_str(" [");
    out.push_str(status_name_upper(c.status));
    out.push_str("] ");
    out.push_str(&c.session_id);
    out.push_str(": ");
    out.push_str(&sanitize_value(&c.command));
    out.push_str(" (");
    out.push_str(extract_time(&c.started_at));
    out.push(')');
}

const fn status_name_upper(s: AsyncCommandStatus) -> &'static str {
    match s {
        AsyncCommandStatus::Running => "RUNNING",
        AsyncCommandStatus::Completed => "COMPLETED",
        AsyncCommandStatus::Cancelled => "CANCELLED",
        AsyncCommandStatus::Failed => "FAILED",
    }
}

// ---------------------------------------------------------------------------
// ssh_cancel_command
// ---------------------------------------------------------------------------

/// Builder for `ssh_cancel_command` in the CANCELLED scenario (with output).
#[must_use]
pub struct CancelCommandCancelledBuilder<'a> {
    command_id: &'a str,
    stdout: &'a [u8],
    stderr: &'a [u8],
    max_output_bytes: usize,
    nonce: &'a str,
}

impl<'a> CancelCommandCancelledBuilder<'a> {
    /// Create a new builder.
    pub const fn new(
        command_id: &'a str,
        stdout: &'a [u8],
        stderr: &'a [u8],
        max_output_bytes: usize,
        nonce: &'a str,
    ) -> Self {
        Self {
            command_id,
            stdout,
            stderr,
            max_output_bytes,
            nonce,
        }
    }

    /// Render the markdown response.
    #[must_use]
    pub fn build(&self) -> String {
        let stdout_block = render_output_block(
            "stdout",
            self.nonce,
            self.stdout,
            self.max_output_bytes,
            Some("partial"),
        );
        let stderr_block = render_output_block(
            "stderr",
            self.nonce,
            self.stderr,
            self.max_output_bytes,
            Some("partial"),
        );
        let mut out = String::with_capacity(96 + stdout_block.len() + stderr_block.len());
        out.push_str("SSH_CANCEL_COMMAND: CANCELLED\nCOMMAND_ID: ");
        out.push_str(self.command_id);
        out.push('\n');
        out.push_str(&stdout_block);
        out.push('\n');
        out.push_str(&stderr_block);
        out
    }
}

/// Block response for `ssh_cancel_command: NOOP` (command not running).
#[must_use]
pub fn render_cancel_command_noop(command_id: &str, reason: &str) -> String {
    let mut out = String::with_capacity(96);
    out.push_str("SSH_CANCEL_COMMAND: NOOP\nCOMMAND_ID: ");
    out.push_str(command_id);
    out.push_str("\nREASON: ");
    out.push_str(&sanitize_value(reason));
    out
}

// ---------------------------------------------------------------------------
// ssh_shell_open
// ---------------------------------------------------------------------------

/// Builder for `ssh_shell_open: OK`.
#[must_use]
pub struct ShellOpenBuilder {
    shell_id: String,
    session_id: String,
    term: String,
    cols: u32,
    rows: u32,
    agent_id: Option<String>,
}

impl ShellOpenBuilder {
    /// Create a new builder.
    pub fn new(
        shell_id: impl Into<String>,
        session_id: impl Into<String>,
        term: impl Into<String>,
        cols: u32,
        rows: u32,
    ) -> Self {
        Self {
            shell_id: shell_id.into(),
            session_id: session_id.into(),
            term: term.into(),
            cols,
            rows,
            agent_id: None,
        }
    }

    /// Attach an agent identifier.
    pub fn with_agent_id(mut self, agent_id: Option<impl Into<String>>) -> Self {
        self.agent_id = agent_id.map(Into::into);
        self
    }

    /// Render the markdown response.
    #[must_use]
    pub fn build(&self) -> String {
        let mut out = String::with_capacity(192);
        out.push_str("SSH_SHELL_OPEN: OK\nSHELL_ID: ");
        out.push_str(&self.shell_id);
        out.push_str("\nSESSION_ID: ");
        out.push_str(&self.session_id);
        out.push_str("\nTERM: ");
        out.push_str(&sanitize_value(&self.term));
        out.push(' ');
        out.push_str(&self.cols.to_string());
        out.push('x');
        out.push_str(&self.rows.to_string());
        if let Some(a) = &self.agent_id {
            out.push_str("\nAGENT: ");
            out.push_str(&sanitize_value(a));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// ssh_shell_write / ssh_shell_close
// ---------------------------------------------------------------------------

/// Block response for `ssh_shell_write: OK`.
#[must_use]
pub fn render_shell_write_ok(shell_id: &str, bytes: usize) -> String {
    let mut out = String::with_capacity(80);
    out.push_str("SSH_SHELL_WRITE: OK\nSHELL_ID: ");
    out.push_str(shell_id);
    out.push_str("\nBYTES_SENT: ");
    out.push_str(&bytes.to_string());
    out
}

/// Block response for `ssh_shell_close: OK`.
#[must_use]
pub fn render_shell_close_ok(shell_id: &str) -> String {
    let mut out = String::with_capacity(64);
    out.push_str("SSH_SHELL_CLOSE: OK\nSHELL_ID: ");
    out.push_str(shell_id);
    out
}

/// Block response for `ssh_shell_send_key: OK`.
///
/// `modifiers_label` is rendered verbatim (already formatted as
/// `"shift+ctrl"` or similar). Pass `None` to omit the line entirely.
#[must_use]
pub fn render_shell_send_key_ok(
    shell_id: &str,
    key_label: &str,
    modifiers_label: Option<&str>,
    repeat: u8,
    bytes_sent: usize,
) -> String {
    let mut out = String::with_capacity(128);
    out.push_str("SSH_SHELL_SEND_KEY: OK\nSHELL_ID: ");
    out.push_str(shell_id);
    out.push_str("\nKEY: ");
    out.push_str(key_label);
    if let Some(label) = modifiers_label {
        out.push_str("\nMODIFIERS: ");
        out.push_str(label);
    }
    out.push_str("\nREPEAT: ");
    out.push_str(&repeat.to_string());
    out.push_str("\nBYTES_SENT: ");
    out.push_str(&bytes_sent.to_string());
    out
}

// ---------------------------------------------------------------------------
// ssh_shell_read
// ---------------------------------------------------------------------------

/// Shell state reported by `ssh_shell_read`.
#[derive(Debug, Clone, Copy)]
pub enum ShellReadState {
    /// Shell is still open.
    Open,
    /// Shell has been closed (background reader ended).
    Closed,
    /// Long-poll deadline expired before `min_bytes` of new output arrived.
    /// The shell remains open; the response carries whatever is currently
    /// buffered.
    Timeout,
}

/// Builder for `ssh_shell_read`.
#[must_use]
pub struct ShellReadBuilder<'a> {
    shell_id: &'a str,
    state: ShellReadState,
    data: &'a [u8],
    max_output_bytes: usize,
    nonce: &'a str,
}

impl<'a> ShellReadBuilder<'a> {
    /// Create a new builder.
    pub const fn new(
        shell_id: &'a str,
        state: ShellReadState,
        data: &'a [u8],
        max_output_bytes: usize,
        nonce: &'a str,
    ) -> Self {
        Self {
            shell_id,
            state,
            data,
            max_output_bytes,
            nonce,
        }
    }

    /// Render the markdown response.
    #[must_use]
    pub fn build(&self) -> String {
        let status = match self.state {
            ShellReadState::Open => "OPEN",
            ShellReadState::Closed => "CLOSED",
            ShellReadState::Timeout => "TIMEOUT",
        };
        let data_block =
            render_output_block("data", self.nonce, self.data, self.max_output_bytes, None);
        let mut out = String::with_capacity(64 + data_block.len());
        out.push_str("SSH_SHELL_READ: ");
        out.push_str(status);
        out.push_str("\nSHELL_ID: ");
        out.push_str(self.shell_id);
        out.push('\n');
        out.push_str(&data_block);
        out
    }
}

// ---------------------------------------------------------------------------
// ssh_shell_wait_for
// ---------------------------------------------------------------------------

/// Outcome reported by `ssh_shell_wait_for`.
#[derive(Debug, Clone)]
pub enum ShellWaitForState {
    /// At least one pattern was found in the buffer. The matched pattern
    /// label is included in the response.
    Matched { pattern: String },
    /// Deadline expired without any pattern matching. The current buffer
    /// is included verbatim (subject to `max_output_bytes`).
    Timeout,
    /// The shell closed during the wait. Response carries whatever was
    /// buffered.
    Closed,
}

/// Builder for `ssh_shell_wait_for`.
#[must_use]
pub struct ShellWaitForBuilder<'a> {
    shell_id: &'a str,
    state: ShellWaitForState,
    data: &'a [u8],
    max_output_bytes: usize,
    nonce: &'a str,
}

impl<'a> ShellWaitForBuilder<'a> {
    /// Create a new builder.
    pub const fn new(
        shell_id: &'a str,
        state: ShellWaitForState,
        data: &'a [u8],
        max_output_bytes: usize,
        nonce: &'a str,
    ) -> Self {
        Self {
            shell_id,
            state,
            data,
            max_output_bytes,
            nonce,
        }
    }

    /// Render the markdown response.
    #[must_use]
    pub fn build(&self) -> String {
        let (status, matched_pattern) = match &self.state {
            ShellWaitForState::Matched { pattern } => ("MATCHED", Some(pattern.as_str())),
            ShellWaitForState::Timeout => ("TIMEOUT", None),
            ShellWaitForState::Closed => ("CLOSED", None),
        };
        let data_block =
            render_output_block("data", self.nonce, self.data, self.max_output_bytes, None);
        let bytes_returned = self.data.len().min(self.max_output_bytes);
        let mut out = String::with_capacity(128 + data_block.len());
        out.push_str("SSH_SHELL_WAIT_FOR: ");
        out.push_str(status);
        out.push_str("\nSHELL_ID: ");
        out.push_str(self.shell_id);
        if let Some(pattern) = matched_pattern {
            out.push_str("\nMATCHED_PATTERN: ");
            out.push_str(&sanitize_value(pattern));
        }
        out.push_str("\nBYTES_RETURNED: ");
        out.push_str(&bytes_returned.to_string());
        out.push('\n');
        out.push_str(&data_block);
        out
    }
}

// ---------------------------------------------------------------------------
// ssh_upload / ssh_download (STARTED)
// ---------------------------------------------------------------------------

/// Direction marker for transfer-start responses.
#[derive(Debug, Clone, Copy)]
pub enum TransferStartDirection {
    Upload,
    Download,
}

/// Builder for `ssh_upload: STARTED` or `ssh_download: STARTED`.
#[must_use]
pub struct TransferStartedBuilder {
    direction: TransferStartDirection,
    transfer_id: String,
    session_id: String,
    local_path: String,
    remote_path: String,
    total_bytes: u64,
    agent_id: Option<String>,
}

impl TransferStartedBuilder {
    /// Create a new builder.
    pub fn new(
        direction: TransferStartDirection,
        transfer_id: impl Into<String>,
        session_id: impl Into<String>,
        local_path: impl Into<String>,
        remote_path: impl Into<String>,
        total_bytes: u64,
    ) -> Self {
        Self {
            direction,
            transfer_id: transfer_id.into(),
            session_id: session_id.into(),
            local_path: local_path.into(),
            remote_path: remote_path.into(),
            total_bytes,
            agent_id: None,
        }
    }

    /// Attach an agent identifier.
    pub fn with_agent_id(mut self, agent_id: Option<impl Into<String>>) -> Self {
        self.agent_id = agent_id.map(Into::into);
        self
    }

    /// Render the markdown response.
    #[must_use]
    pub fn build(&self) -> String {
        let (tool, from, to) = match self.direction {
            TransferStartDirection::Upload => ("SSH_UPLOAD", &self.local_path, &self.remote_path),
            TransferStartDirection::Download => {
                ("SSH_DOWNLOAD", &self.remote_path, &self.local_path)
            }
        };
        let mut out = String::with_capacity(256);
        out.push_str(tool);
        out.push_str(": STARTED\nTRANSFER_ID: ");
        out.push_str(&self.transfer_id);
        out.push_str("\nSESSION_ID: ");
        out.push_str(&self.session_id);
        if let Some(a) = &self.agent_id {
            out.push_str("\nAGENT: ");
            out.push_str(&sanitize_value(a));
        }
        out.push_str("\nFROM: ");
        out.push_str(&sanitize_value(from));
        out.push_str("\nTO: ");
        out.push_str(&sanitize_value(to));
        out.push_str("\nSIZE: ");
        out.push_str(&format_bytes_human(self.total_bytes));
        out.push_str(" (");
        out.push_str(&self.total_bytes.to_string());
        out.push_str(" bytes)\nBYTES: ");
        out.push_str(&self.total_bytes.to_string());
        out
    }
}

// ---------------------------------------------------------------------------
// ssh_get_transfer_progress
// ---------------------------------------------------------------------------

/// Progress state for `ssh_get_transfer_progress`.
#[derive(Debug, Clone, Copy)]
pub enum TransferProgressState<'a> {
    Running,
    Completed,
    /// Carries the structured REASON text (no DETAIL lines are emitted here;
    /// full error formatting lives in `format_error`).
    Failed(&'a str),
}

/// Builder for `ssh_get_transfer_progress`.
#[must_use]
pub struct TransferProgressBuilder<'a> {
    transfer_id: &'a str,
    direction: TransferStartDirection,
    bytes_transferred: u64,
    total_bytes: u64,
    state: TransferProgressState<'a>,
}

impl<'a> TransferProgressBuilder<'a> {
    /// Create a new builder.
    pub const fn new(
        transfer_id: &'a str,
        direction: TransferStartDirection,
        bytes_transferred: u64,
        total_bytes: u64,
        state: TransferProgressState<'a>,
    ) -> Self {
        Self {
            transfer_id,
            direction,
            bytes_transferred,
            total_bytes,
            state,
        }
    }

    /// Render the markdown response.
    #[must_use]
    pub fn build(&self) -> String {
        let dir_upper = match self.direction {
            TransferStartDirection::Upload => "UPLOAD",
            TransferStartDirection::Download => "DOWNLOAD",
        };
        let percent = compute_percent(self.bytes_transferred, self.total_bytes);
        match self.state {
            TransferProgressState::Running => self.render_block("RUNNING", dir_upper, percent),
            TransferProgressState::Completed => self.render_block("COMPLETED", dir_upper, percent),
            TransferProgressState::Failed(reason) => self.render_failed(dir_upper, percent, reason),
        }
    }

    fn render_block(&self, status: &str, dir: &str, percent: u8) -> String {
        let mut out = String::with_capacity(160);
        out.push_str("SSH_GET_TRANSFER_PROGRESS: ");
        out.push_str(status);
        out.push_str("\nTRANSFER_ID: ");
        out.push_str(self.transfer_id);
        out.push_str("\nDIRECTION: ");
        out.push_str(dir);
        out.push_str("\nPROGRESS: ");
        out.push_str(&percent.to_string());
        out.push_str("% (");
        out.push_str(&self.bytes_transferred.to_string());
        out.push('/');
        out.push_str(&self.total_bytes.to_string());
        out.push_str(" bytes)");
        out
    }

    fn render_failed(&self, dir: &str, percent: u8, reason: &str) -> String {
        let mut out = String::with_capacity(192);
        out.push_str("SSH_GET_TRANSFER_PROGRESS: FAILED\nTRANSFER_ID: ");
        out.push_str(self.transfer_id);
        out.push_str("\nDIRECTION: ");
        out.push_str(dir);
        out.push_str("\nPROGRESS: ");
        out.push_str(&percent.to_string());
        out.push_str("% (");
        out.push_str(&self.bytes_transferred.to_string());
        out.push('/');
        out.push_str(&self.total_bytes.to_string());
        out.push_str(" bytes)\nREASON: ");
        out.push_str(&sanitize_value(reason));
        out
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "percentage calculation where precision loss is acceptable and result is always 0..=100"
)]
fn compute_percent(transferred: u64, total: u64) -> u8 {
    if total > 0 {
        (transferred as f64 / total as f64 * 100.0) as u8
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// ssh_forward
// ---------------------------------------------------------------------------

/// Block response for `ssh_forward: OK`.
#[must_use]
pub fn render_forward_ok(local: &str, remote: &str, active: bool) -> String {
    let mut out = String::with_capacity(96);
    out.push_str("SSH_FORWARD: OK\nLOCAL: ");
    out.push_str(&sanitize_value(local));
    out.push_str("\nREMOTE: ");
    out.push_str(&sanitize_value(remote));
    out.push_str("\nACTIVE: ");
    out.push_str(if active { "true" } else { "false" });
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    mod connect_ok_builder {
        use super::*;

        #[test]
        fn ok_minimal() {
            let m = ConnectOkBuilder::new("sess-abc", "user", "host:22").build();
            assert!(m.starts_with("SSH_CONNECT: OK\n"));
            assert!(m.contains("SESSION_ID: sess-abc"));
            assert!(m.contains("HOST: user@host:22"));
            assert!(m.contains("RETRY: 0"));
            assert!(m.contains("PERSISTENT: false"));
        }

        #[test]
        fn ok_with_agent_and_retry() {
            let m = ConnectOkBuilder::new("sess-abc", "user", "host:22")
                .with_agent_id(Some("my-agent"))
                .with_retry_attempts(3)
                .with_persistent(true)
                .build();
            assert!(m.contains("AGENT: my-agent"));
            assert!(m.contains("RETRY: 3"));
            assert!(m.contains("PERSISTENT: true"));
        }

        #[test]
        fn reused_omits_retry_and_persistent() {
            let m = ConnectOkBuilder::new("sess-abc", "user", "host:22")
                .reused(true)
                .with_retry_attempts(5)
                .with_persistent(true)
                .build();
            assert!(m.starts_with("SSH_CONNECT: REUSED\n"));
            assert!(!m.contains("RETRY"));
            assert!(!m.contains("PERSISTENT"));
        }

        #[test]
        fn ok_with_replaced() {
            let m = ConnectOkBuilder::new("sess-abc", "user", "host:22")
                .with_replaced(2)
                .build();
            assert!(m.contains("REPLACED: 2"));
        }

        #[test]
        fn replaced_zero_omitted() {
            let m = ConnectOkBuilder::new("sess-abc", "user", "host:22")
                .with_replaced(0)
                .build();
            assert!(!m.contains("REPLACED"));
        }

        #[test]
        fn agent_id_sanitized() {
            let m = ConnectOkBuilder::new("s", "u", "h")
                .with_agent_id(Some("my\nagent"))
                .build();
            assert!(m.contains("AGENT: my\\nagent"));
        }
    }

    mod connect_suggested_builder {
        use super::*;

        fn sample_match() -> SessionMatch {
            SessionMatch {
                session_id: "sess-abc".to_string(),
                host: "user@host:22".to_string(),
                agent_id: Some("my-agent".to_string()),
                name: Some("prod-db".to_string()),
                connected_at: "2026-04-18T10:30:00Z".to_string(),
                healthy: true,
            }
        }

        #[test]
        fn single_match_full_fields() {
            let m = ConnectSuggestedBuilder::new(vec![sample_match()]).build();
            assert!(m.contains("SSH_CONNECT: SUGGESTED"));
            assert!(m.contains("EXISTING_SESSION_ID: sess-abc"));
            assert!(m.contains("HOST: user@host:22"));
            assert!(m.contains("AGENT: my-agent"));
            assert!(m.contains("NAME: prod-db"));
            assert!(m.contains("CONNECTED_AT: 2026-04-18T10:30:00Z"));
            assert!(m.contains("HEALTHY: true"));
            assert!(m.contains("HINT: use existing SESSION_ID"));
        }

        #[test]
        fn single_match_unhealthy_shown() {
            let mut s = sample_match();
            s.healthy = false;
            let m = ConnectSuggestedBuilder::new(vec![s]).build();
            assert!(m.contains("HEALTHY: false"));
        }

        #[test]
        fn multi_match_list_format() {
            let matches = vec![
                sample_match(),
                SessionMatch {
                    session_id: "sess-def".to_string(),
                    host: "user@host:22".to_string(),
                    agent_id: None,
                    name: None,
                    connected_at: "2026-04-18T09:15:00Z".to_string(),
                    healthy: true,
                },
            ];
            let m = ConnectSuggestedBuilder::new(matches).build();
            assert!(m.contains("MATCHES: 2"));
            assert!(m.contains("- sess-abc user@host:22 [agent: my-agent"));
            assert!(m.contains("connected: 10:30:00"));
            assert!(m.contains("healthy"));
            assert!(m.contains("- sess-def user@host:22 [connected: 09:15:00"));
            assert!(m.contains("HINT: pick an existing SESSION_ID"));
        }

        #[test]
        fn empty_matches_shows_zero() {
            let m = ConnectSuggestedBuilder::new(vec![]).build();
            assert!(m.contains("MATCHES: 0"));
        }
    }

    mod disconnect {
        use super::*;

        #[test]
        fn block_format() {
            let m = render_disconnect_ok("sess-abc");
            assert_eq!(m, "SSH_DISCONNECT: OK\nSESSION_ID: sess-abc");
        }
    }

    mod list_sessions_builder {
        use super::*;

        fn sample_session(id: &str, healthy: Option<bool>, agent: Option<&str>) -> SessionInfo {
            SessionInfo {
                session_id: id.to_string(),
                name: None,
                agent_id: agent.map(str::to_string),
                host: "host:22".to_string(),
                username: "user".to_string(),
                connected_at: "2026-04-18T10:30:00Z".to_string(),
                default_timeout_secs: 30,
                retry_attempts: 0,
                compression_enabled: true,
                last_health_check: None,
                healthy,
            }
        }

        #[test]
        fn empty_list_block() {
            let m = ListSessionsBuilder::new(&[], 0).build();
            assert_eq!(m, "SSH_LIST_SESSIONS: OK\nCOUNT: 0");
        }

        #[test]
        fn multi_session_rendered_as_bullets() {
            let sessions = vec![
                sample_session("sess-1", Some(true), Some("my-agent")),
                sample_session("sess-2", None, None),
            ];
            let m = ListSessionsBuilder::new(&sessions, 2).build();
            assert!(m.contains("COUNT: 2"));
            assert!(m.contains("- sess-1 user@host:22 [agent: my-agent, healthy]"));
            assert!(m.contains("- sess-2 user@host:22"));
        }

        #[test]
        fn pagination_marker_when_truncated() {
            let sessions = vec![sample_session("sess-1", Some(true), None)];
            let m = ListSessionsBuilder::new(&sessions, 10).build();
            assert!(m.contains("COUNT: 1 (showing 1 of 10)"));
        }

        #[test]
        fn compression_off_shown() {
            let mut s = sample_session("sess-1", None, None);
            s.compression_enabled = false;
            let sessions = vec![s];
            let m = ListSessionsBuilder::new(&sessions, 1).build();
            assert!(m.contains("[compression: off]"));
        }

        #[test]
        fn unhealthy_marker() {
            let sessions = vec![sample_session("sess-1", Some(false), None)];
            let m = ListSessionsBuilder::new(&sessions, 1).build();
            assert!(m.contains("[unhealthy]"));
        }
    }

    mod disconnect_agent {
        use super::*;

        #[test]
        fn block_format() {
            let m = render_disconnect_agent("my-agent", 3, 5);
            assert_eq!(
                m,
                "SSH_DISCONNECT_AGENT: OK\nAGENT: my-agent\nSESSIONS: 3\nCOMMANDS: 5"
            );
        }

        #[test]
        fn zero_counts_still_rendered() {
            let m = render_disconnect_agent("agent", 0, 0);
            assert!(m.contains("\nSESSIONS: 0"));
            assert!(m.contains("\nCOMMANDS: 0"));
        }
    }

    mod execute_started_builder {
        use super::*;

        #[test]
        fn without_agent() {
            let m = ExecuteStartedBuilder::new("cmd-1", "sess-1").build();
            assert!(m.contains("SSH_EXECUTE: STARTED"));
            assert!(m.contains("COMMAND_ID: cmd-1"));
            assert!(m.contains("SESSION_ID: sess-1"));
            assert!(!m.contains("AGENT"));
        }

        #[test]
        fn with_agent() {
            let m = ExecuteStartedBuilder::new("cmd-1", "sess-1")
                .with_agent_id(Some("my-agent"))
                .build();
            assert!(m.contains("AGENT: my-agent"));
        }

        #[test]
        fn no_command_text_in_output() {
            // Explicit: execute STARTED must NOT echo the command line.
            let m = ExecuteStartedBuilder::new("cmd-1", "sess-1").build();
            assert!(!m.contains("CMD"));
            assert!(!m.contains("RUNNING:"));
        }
    }

    mod get_command_output_builder {
        use super::*;

        #[test]
        fn running_with_partial_annotation() {
            let m = GetCommandOutputBuilder::new(
                "cmd-1",
                GetCommandOutputState::Running,
                b"line1\nline2",
                b"",
                1024,
                "nonce001",
            )
            .build();
            assert!(m.starts_with("SSH_GET_COMMAND_OUTPUT: RUNNING\n"));
            assert!(m.contains("COMMAND_ID: cmd-1"));
            assert!(m.contains("--- stdout [nonce001] (partial) ---"));
            assert!(m.contains("line1\nline2"));
            assert!(m.contains("--- stderr [nonce001] (partial, empty) ---"));
            assert!(!m.contains("EXIT:"));
        }

        #[test]
        fn completed_with_exit_code() {
            let m = GetCommandOutputBuilder::new(
                "cmd-2",
                GetCommandOutputState::Completed(0),
                b"ok",
                b"",
                1024,
                "nonce002",
            )
            .build();
            assert!(m.contains("SSH_GET_COMMAND_OUTPUT: COMPLETED"));
            assert!(m.contains("EXIT: 0"));
            assert!(m.contains("--- stdout [nonce002] ---"));
            assert!(m.contains("--- stderr [nonce002] (empty) ---"));
        }

        #[test]
        fn completed_negative_exit_code() {
            let m = GetCommandOutputBuilder::new(
                "cmd-3",
                GetCommandOutputState::Completed(-1),
                b"",
                b"err",
                1024,
                "n",
            )
            .build();
            assert!(m.contains("EXIT: -1"));
        }

        #[test]
        fn timeout_shows_partial() {
            let m = GetCommandOutputBuilder::new(
                "cmd-4",
                GetCommandOutputState::Timeout,
                b"partial",
                b"",
                1024,
                "n",
            )
            .build();
            assert!(m.contains("SSH_GET_COMMAND_OUTPUT: TIMEOUT"));
            assert!(m.contains("(partial)"));
            assert!(!m.contains("EXIT:"));
        }

        #[test]
        fn truncation_propagates_to_block() {
            let big = vec![b'x'; 1024 * 1024];
            let m = GetCommandOutputBuilder::new(
                "cmd-5",
                GetCommandOutputState::Completed(0),
                &big,
                b"",
                256,
                "n",
            )
            .build();
            assert!(m.contains("truncated: showing 256B of 1.0MB"));
        }
    }

    mod list_commands_builder {
        use super::*;

        fn sample_cmd(id: &str, status: AsyncCommandStatus, cmd: &str) -> AsyncCommandInfo {
            AsyncCommandInfo {
                command_id: id.to_string(),
                session_id: "sess-1".to_string(),
                command: cmd.to_string(),
                status,
                started_at: "2026-04-18T10:30:00Z".to_string(),
            }
        }

        #[test]
        fn empty_block() {
            let m = ListCommandsBuilder::new(&[], 0).build();
            assert_eq!(m, "SSH_LIST_COMMANDS: OK\nCOUNT: 0");
        }

        #[test]
        fn multi_command_list() {
            let cmds = vec![
                sample_cmd("cmd-1", AsyncCommandStatus::Running, "sleep 10"),
                sample_cmd("cmd-2", AsyncCommandStatus::Completed, "ls -la"),
            ];
            let m = ListCommandsBuilder::new(&cmds, 2).build();
            assert!(m.contains("COUNT: 2"));
            assert!(m.contains("- cmd-1 [RUNNING] sess-1: sleep 10 (10:30:00)"));
            assert!(m.contains("- cmd-2 [COMPLETED] sess-1: ls -la (10:30:00)"));
        }

        #[test]
        fn command_text_sanitized() {
            let cmds = vec![sample_cmd(
                "cmd-1",
                AsyncCommandStatus::Running,
                "echo a\nb",
            )];
            let m = ListCommandsBuilder::new(&cmds, 1).build();
            assert!(m.contains("echo a\\nb"));
        }

        #[test]
        fn pagination_marker() {
            let cmds = vec![sample_cmd("c1", AsyncCommandStatus::Running, "x")];
            let m = ListCommandsBuilder::new(&cmds, 50).build();
            assert!(m.contains("COUNT: 1 (showing 1 of 50)"));
        }
    }

    mod cancel_command {
        use super::*;

        #[test]
        fn cancelled_with_output_blocks() {
            let m =
                CancelCommandCancelledBuilder::new("cmd-1", b"line1", b"", 1024, "nonce").build();
            assert!(m.contains("SSH_CANCEL_COMMAND: CANCELLED"));
            assert!(m.contains("COMMAND_ID: cmd-1"));
            assert!(m.contains("--- stdout [nonce] (partial) ---"));
            assert!(m.contains("line1"));
            assert!(m.contains("--- stderr [nonce] (partial, empty) ---"));
        }

        #[test]
        fn noop_block() {
            let m = render_cancel_command_noop("cmd-1", "not running");
            assert_eq!(
                m,
                "SSH_CANCEL_COMMAND: NOOP\nCOMMAND_ID: cmd-1\nREASON: not running"
            );
        }

        #[test]
        fn noop_reason_sanitized() {
            let m = render_cancel_command_noop("cmd-1", "line1\nline2");
            assert!(m.contains("line1\\nline2"));
        }
    }

    mod shell_open_builder {
        use super::*;

        #[test]
        fn basic_message() {
            let m = ShellOpenBuilder::new("shell-1", "sess-1", "xterm", 80, 24).build();
            assert!(m.contains("SSH_SHELL_OPEN: OK"));
            assert!(m.contains("SHELL_ID: shell-1"));
            assert!(m.contains("SESSION_ID: sess-1"));
            assert!(m.contains("TERM: xterm 80x24"));
        }

        #[test]
        fn with_agent() {
            let m = ShellOpenBuilder::new("shell-1", "sess-1", "vt100", 80, 24)
                .with_agent_id(Some("my-agent"))
                .build();
            assert!(m.contains("AGENT: my-agent"));
            assert!(m.contains("TERM: vt100 80x24"));
        }

        #[test]
        fn custom_dimensions() {
            let m = ShellOpenBuilder::new("shell-1", "sess-1", "xterm", 132, 43).build();
            assert!(m.contains("TERM: xterm 132x43"));
        }
    }

    mod shell_write_close {
        use super::*;

        #[test]
        fn write_block() {
            let m = render_shell_write_ok("shell-1", 42);
            assert_eq!(
                m,
                "SSH_SHELL_WRITE: OK\nSHELL_ID: shell-1\nBYTES_SENT: 42"
            );
        }

        #[test]
        fn close_block() {
            let m = render_shell_close_ok("shell-1");
            assert_eq!(m, "SSH_SHELL_CLOSE: OK\nSHELL_ID: shell-1");
        }

        #[test]
        fn send_key_no_modifiers_omits_line() {
            let m = render_shell_send_key_ok("shell-1", "ctrl_c", None, 1, 1);
            assert!(m.starts_with("SSH_SHELL_SEND_KEY: OK\n"));
            assert!(m.contains("SHELL_ID: shell-1"));
            assert!(m.contains("KEY: ctrl_c"));
            assert!(!m.contains("MODIFIERS"));
            assert!(m.contains("REPEAT: 1"));
            assert!(m.contains("BYTES_SENT: 1"));
        }

        #[test]
        fn send_key_with_modifiers_includes_line() {
            let m = render_shell_send_key_ok("shell-1", "arrow_up", Some("shift+ctrl"), 3, 18);
            assert!(m.contains("MODIFIERS: shift+ctrl"));
            assert!(m.contains("REPEAT: 3"));
            assert!(m.contains("BYTES_SENT: 18"));
        }
    }

    mod shell_read_builder {
        use super::*;

        #[test]
        fn open_with_data() {
            let m =
                ShellReadBuilder::new("shell-1", ShellReadState::Open, b"$ ls\nfile1", 1024, "n")
                    .build();
            assert!(m.starts_with("SSH_SHELL_READ: OPEN\n"));
            assert!(m.contains("SHELL_ID: shell-1"));
            assert!(m.contains("--- data [n] ---"));
            assert!(m.contains("$ ls\nfile1"));
        }

        #[test]
        fn closed_with_empty_data() {
            let m =
                ShellReadBuilder::new("shell-1", ShellReadState::Closed, b"", 1024, "n").build();
            assert!(m.contains("SSH_SHELL_READ: CLOSED"));
            assert!(m.contains("--- data [n] (empty) ---"));
        }

        #[test]
        fn timeout_with_data() {
            let m = ShellReadBuilder::new(
                "shell-1",
                ShellReadState::Timeout,
                b"partial output",
                1024,
                "n",
            )
            .build();
            assert!(m.starts_with("SSH_SHELL_READ: TIMEOUT\n"));
            assert!(m.contains("SHELL_ID: shell-1"));
            assert!(m.contains("--- data [n] ---"));
            assert!(m.contains("partial output"));
        }

        #[test]
        fn timeout_with_empty_data() {
            let m =
                ShellReadBuilder::new("shell-1", ShellReadState::Timeout, b"", 1024, "n").build();
            assert!(m.contains("SSH_SHELL_READ: TIMEOUT"));
            assert!(m.contains("--- data [n] (empty) ---"));
        }
    }

    mod shell_wait_for_builder {
        use super::*;

        #[test]
        fn matched_includes_pattern_and_data() {
            let m = ShellWaitForBuilder::new(
                "shell-1",
                ShellWaitForState::Matched {
                    pattern: "$ ".to_string(),
                },
                b"login\nuser@host:~$ ",
                1024,
                "nonce",
            )
            .build();
            assert!(m.starts_with("SSH_SHELL_WAIT_FOR: MATCHED\n"));
            assert!(m.contains("SHELL_ID: shell-1"));
            assert!(m.contains("MATCHED_PATTERN: $ "));
            assert!(m.contains("BYTES_RETURNED: 19"));
            assert!(m.contains("--- data [nonce] ---"));
            assert!(m.contains("user@host:~$ "));
        }

        #[test]
        fn timeout_omits_matched_pattern() {
            let m = ShellWaitForBuilder::new(
                "shell-1",
                ShellWaitForState::Timeout,
                b"still loading",
                1024,
                "nonce",
            )
            .build();
            assert!(m.starts_with("SSH_SHELL_WAIT_FOR: TIMEOUT\n"));
            assert!(!m.contains("MATCHED_PATTERN"));
            assert!(m.contains("BYTES_RETURNED: 13"));
        }

        #[test]
        fn closed_omits_matched_pattern() {
            let m =
                ShellWaitForBuilder::new("shell-1", ShellWaitForState::Closed, b"", 1024, "nonce")
                    .build();
            assert!(m.starts_with("SSH_SHELL_WAIT_FOR: CLOSED\n"));
            assert!(!m.contains("MATCHED_PATTERN"));
            assert!(m.contains("BYTES_RETURNED: 0"));
            assert!(m.contains("--- data [nonce] (empty) ---"));
        }

        #[test]
        fn matched_pattern_sanitized() {
            let m = ShellWaitForBuilder::new(
                "shell-1",
                ShellWaitForState::Matched {
                    pattern: "line1\nline2".to_string(),
                },
                b"data",
                1024,
                "n",
            )
            .build();
            assert!(m.contains("MATCHED_PATTERN: line1\\nline2"));
        }

        #[test]
        fn bytes_returned_capped_by_max_output_bytes() {
            let buf = vec![b'x'; 500];
            let m = ShellWaitForBuilder::new("shell-1", ShellWaitForState::Timeout, &buf, 100, "n")
                .build();
            assert!(m.contains("BYTES_RETURNED: 100"));
        }
    }

    mod transfer_started_builder {
        use super::*;

        #[test]
        fn upload_direction() {
            let m = TransferStartedBuilder::new(
                TransferStartDirection::Upload,
                "xfer-1",
                "sess-1",
                "/tmp/f.txt",
                "/home/user/f.txt",
                1_048_576,
            )
            .build();
            assert!(m.contains("SSH_UPLOAD: STARTED"));
            assert!(m.contains("FROM: /tmp/f.txt"));
            assert!(m.contains("TO: /home/user/f.txt"));
            assert!(m.contains("SIZE: 1.0MB (1048576 bytes)"));
            assert!(m.contains("BYTES: 1048576"));
        }

        #[test]
        fn download_swaps_from_to() {
            let m = TransferStartedBuilder::new(
                TransferStartDirection::Download,
                "xfer-2",
                "sess-1",
                "/tmp/local",
                "/remote",
                100,
            )
            .build();
            assert!(m.contains("SSH_DOWNLOAD: STARTED"));
            assert!(m.contains("FROM: /remote"));
            assert!(m.contains("TO: /tmp/local"));
        }

        #[test]
        fn with_agent_id() {
            let m =
                TransferStartedBuilder::new(TransferStartDirection::Upload, "x", "s", "a", "b", 0)
                    .with_agent_id(Some("my-agent"))
                    .build();
            assert!(m.contains("AGENT: my-agent"));
        }
    }

    mod transfer_progress_builder {
        use super::*;

        #[test]
        fn running_block() {
            let m = TransferProgressBuilder::new(
                "xfer-1",
                TransferStartDirection::Upload,
                524_288,
                1_048_576,
                TransferProgressState::Running,
            )
            .build();
            assert_eq!(
                m,
                "SSH_GET_TRANSFER_PROGRESS: RUNNING\nTRANSFER_ID: xfer-1\nDIRECTION: UPLOAD\nPROGRESS: 50% (524288/1048576 bytes)"
            );
        }

        #[test]
        fn completed_block_100_percent() {
            let m = TransferProgressBuilder::new(
                "xfer-1",
                TransferStartDirection::Download,
                1_048_576,
                1_048_576,
                TransferProgressState::Completed,
            )
            .build();
            assert!(m.starts_with("SSH_GET_TRANSFER_PROGRESS: COMPLETED\n"));
            assert!(m.contains("DIRECTION: DOWNLOAD"));
            assert!(m.contains("PROGRESS: 100%"));
        }

        #[test]
        fn failed_block_format() {
            let m = TransferProgressBuilder::new(
                "xfer-1",
                TransferStartDirection::Download,
                100,
                1000,
                TransferProgressState::Failed("connection lost"),
            )
            .build();
            assert!(m.starts_with("SSH_GET_TRANSFER_PROGRESS: FAILED\n"));
            assert!(m.contains("TRANSFER_ID: xfer-1"));
            assert!(m.contains("DIRECTION: DOWNLOAD"));
            assert!(m.contains("PROGRESS: 10% (100/1000 bytes)"));
            assert!(m.contains("REASON: connection lost"));
        }

        #[test]
        fn zero_total_shows_zero_percent() {
            let m = TransferProgressBuilder::new(
                "xfer-1",
                TransferStartDirection::Upload,
                0,
                0,
                TransferProgressState::Running,
            )
            .build();
            assert!(m.contains("0%"));
        }
    }

    mod forward {
        use super::*;

        #[test]
        fn active_block() {
            let m = render_forward_ok("127.0.0.1:8080", "localhost:3306", true);
            assert_eq!(
                m,
                "SSH_FORWARD: OK\nLOCAL: 127.0.0.1:8080\nREMOTE: localhost:3306\nACTIVE: true"
            );
        }

        #[test]
        fn inactive_renders_false() {
            let m = render_forward_ok("a", "b", false);
            assert!(m.contains("\nACTIVE: false"));
        }
    }

    mod extract_time {
        use super::*;

        #[test]
        fn zulu_timestamp() {
            assert_eq!(extract_time("2026-04-18T10:30:00Z"), "10:30:00");
        }

        #[test]
        fn offset_timestamp() {
            assert_eq!(extract_time("2026-04-18T10:30:00+03:00"), "10:30:00");
        }

        #[test]
        fn no_t_separator_falls_back() {
            assert_eq!(extract_time("whatever"), "whatever");
        }

        #[test]
        fn fractional_seconds() {
            assert_eq!(extract_time("2026-04-18T10:30:00.123Z"), "10:30:00.123");
        }
    }
}
