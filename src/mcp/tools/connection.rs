//! Connection-domain tools — `ssh_connect` (canary v3.0.0).
//!
//! In E3 only `ssh_connect` is migrated. `ssh_disconnect`, `ssh_list_sessions`,
//! and `ssh_disconnect_agent` are wired in E4.
//!
//! # Design
//!
//! - Args struct: [`SshConnectArgs`] derives `Deserialize + JsonSchema` so the
//!   MCP tool manifest exposes a typed schema (replacing v2.0's positional doc
//!   comments).
//! - [`ReusePolicy`](super::ReusePolicy) replaces `Option<String>` so typos
//!   become a schema validation error rather than silent fallback.
//! - Response is a plain markdown `String` wrapped in `CallToolResult` —
//!   builders from `super::super::message::builder` are reused unchanged.
//! - Helpers (`try_reuse_session`, `evaluate_identity_matches`, etc.) ported
//!   verbatim from v2.0 `commands.rs` (now parked at `commands_legacy.rs.txt`).

use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{CallToolResult, Content, ErrorData as McpError};
use russh::Disconnect;
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::super::client::{connect_to_ssh_with_retry, execute_ssh_command};
use super::super::config::{
    resolve_compression, resolve_connect_timeout, resolve_inactivity_timeout, resolve_max_retries,
    resolve_retry_delay,
};
use super::super::message::builder::{
    ConnectOkBuilder, ConnectSuggestedBuilder, SessionMatch,
};
use super::super::message::helpers::format_error;
use super::super::session::SshClientHandler;
use super::super::storage::command::COMMAND_STORAGE;
use super::super::storage::session::{SESSION_STORAGE, parse_host_port};
use super::super::storage::shell::SHELL_STORAGE;
use super::super::storage::traits::{
    CommandStorage, SessionRef, SessionStorage, ShellStorage, TransferStorage,
};
use super::super::storage::transfer::TRANSFER_STORAGE;
use super::super::types::{SessionInfo, SshCommandResponse};
use super::ReusePolicy;

/// Type alias for the SSH client handle used throughout this module.
type SshHandle = russh::client::Handle<SshClientHandler>;

/// Arguments for the `ssh_connect` MCP tool.
///
/// Field documentation appears in the JSON schema served to MCP clients
/// (rmcp inspects the doc comments via the `schemars` derive).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshConnectArgs {
    /// Optional `SESSION_ID` returned by a previous `ssh_connect`. When provided
    /// and the session is still alive (health check `echo 1`), this short-circuits
    /// the full reuse evaluation and returns the existing session as `REUSED`.
    pub session_id: Option<String>,

    /// SSH server address in the form `"host:port"` (e.g. `"192.168.1.1:22"`,
    /// `"example.com:2222"`). The port defaults to `22` if omitted.
    pub address: String,

    /// SSH username for authentication.
    pub username: String,

    /// Password for password-based authentication. Optional when `key_path`
    /// or an SSH agent (env `SSH_AUTH_SOCK`) is available.
    pub password: Option<String>,

    /// Path to a private key file for key-based authentication. Optional.
    /// Authentication chain order: key → password → agent.
    pub key_path: Option<String>,

    /// Connection timeout in seconds. Default: `30`. Env: `SSH_CONNECT_TIMEOUT`.
    pub timeout_secs: Option<u64>,

    /// Maximum retry attempts for transient connection failures. Default: `3`.
    /// Env: `SSH_MAX_RETRIES`.
    pub max_retries: Option<u32>,

    /// Initial delay between retries in milliseconds (exponential backoff,
    /// capped at 10 s). Default: `1000`. Env: `SSH_RETRY_DELAY_MS`.
    pub retry_delay_ms: Option<u64>,

    /// Enable zlib compression for the SSH transport. Default: `true`.
    /// Env: `SSH_COMPRESSION`.
    pub compress: Option<bool>,

    /// Optional human-readable name for the session (e.g.,
    /// `"production-db"`, `"staging-server"`). Surfaces in
    /// `ssh_list_sessions` to help disambiguate identical hosts.
    pub name: Option<String>,

    /// Keep the session open indefinitely (disables inactivity timeout).
    /// Default: `false`. Set `true` for long-lived backends or daemons.
    pub persistent: Option<bool>,

    /// Optional agent identifier for grouping sessions
    /// (e.g., `"claude-code-instance-abc123"`). Use `ssh_disconnect_agent`
    /// to bulk-disconnect all sessions for an agent.
    pub agent_id: Option<String>,

    /// Reuse policy applied when an existing session matches the
    /// `(host, port, username)` identity triple. See [`ReusePolicy`].
    /// Default: `Suggest`.
    pub reuse: Option<ReusePolicy>,
}

/// Format a `host:port` pair for display when the port is not the default 22.
fn format_host_for_display(host_lc: &str, port: u16) -> String {
    if port == 22 {
        host_lc.to_string()
    } else {
        format!("{host_lc}:{port}")
    }
}

/// Append a `REPLACED: N` line to a new-connection response when unhealthy
/// matches were purged before creating it.
fn append_replaced_line(mut response: String, replaced: usize) -> String {
    if replaced > 0 {
        response.push_str("\nREPLACED: ");
        response.push_str(&replaced.to_string());
    }
    response
}

/// Try to reuse an existing session by performing a health check.
///
/// Returns the `REUSED` markdown if the session is healthy, or `None` if the
/// stored session has died (in which case the caller should fall through to a
/// fresh connection).
async fn try_reuse_session(sid: &str) -> Option<String> {
    let session_ref = SESSION_STORAGE.get(sid)?;
    let health_timeout = Duration::from_secs(5);
    let now = chrono::Utc::now().to_rfc3339();
    let result = execute_ssh_command(&session_ref.handle, "echo 1", health_timeout).await;
    build_reuse_response(sid, &session_ref, now, result)
}

/// Build the `REUSED` response based on the health check outcome.
fn build_reuse_response(
    sid: &str,
    session_ref: &SessionRef,
    now: String,
    result: Result<SshCommandResponse, String>,
) -> Option<String> {
    match result {
        Ok(response) if !response.timed_out && response.exit_code == 0 => {
            SESSION_STORAGE.update_health(sid, now, true);
            info!("Reusing healthy session {sid}");
            Some(
                ConnectOkBuilder::new(sid, &session_ref.info.username, &session_ref.info.host)
                    .with_agent_id(session_ref.info.agent_id.as_deref())
                    .reused(true)
                    .build(),
            )
        }
        _ => {
            warn!("Session {sid} is dead, removing");
            SESSION_STORAGE.remove(sid);
            None
        }
    }
}

/// Outcome of evaluating existing sessions for a smart-reuse lookup.
struct IdentityMatches {
    /// Sessions that passed the health check, newest first.
    healthy: Vec<SessionMatch>,
    /// Number of sessions that failed the health check and were disconnected.
    replaced: usize,
}

/// Evaluate matching sessions for the given identity triple.
///
/// Runs a health check on every candidate, disconnects the unhealthy ones
/// (including removing them from storage), and returns the healthy ones
/// sorted by `connected_at` descending (most recent first).
async fn evaluate_identity_matches(host: &str, port: u16, username: &str) -> IdentityMatches {
    let candidates = SESSION_STORAGE.find_by_identity(host, port, username);
    let mut healthy: Vec<SessionMatch> = Vec::new();
    let mut replaced = 0_usize;

    for sid in candidates {
        let Some(session_ref) = SESSION_STORAGE.get(&sid) else {
            continue;
        };
        let health_timeout = Duration::from_secs(5);
        let health = execute_ssh_command(&session_ref.handle, "echo 1", health_timeout).await;
        let now = chrono::Utc::now().to_rfc3339();
        let is_healthy = matches!(&health, Ok(r) if !r.timed_out && r.exit_code == 0);
        if is_healthy {
            SESSION_STORAGE.update_health(&sid, now, true);
            healthy.push(SessionMatch {
                session_id: sid,
                host: format!("{}@{}", session_ref.info.username, session_ref.info.host),
                agent_id: session_ref.info.agent_id.clone(),
                name: session_ref.info.name.clone(),
                connected_at: session_ref.info.connected_at.clone(),
                healthy: true,
            });
        } else {
            replaced += 1;
            cancel_session_transfers(&sid);
            close_session_shells(&sid).await;
            cancel_session_commands(&sid);
            if let Some(stale) = SESSION_STORAGE.remove(&sid) {
                if let Some(agent_id) = &stale.info.agent_id {
                    SESSION_STORAGE.unregister_agent(agent_id, &sid);
                }
                let _ = stale
                    .handle
                    .disconnect(
                        Disconnect::ByApplication,
                        "Unhealthy session replaced",
                        "en",
                    )
                    .await;
            }
        }
    }

    healthy.sort_by(|a, b| b.connected_at.cmp(&a.connected_at));
    IdentityMatches { healthy, replaced }
}

/// Cancel all in-flight transfers for a session.
fn cancel_session_transfers(session_id: &str) {
    let transfer_ids = TRANSFER_STORAGE.list_by_session(session_id);
    for xfer_id in &transfer_ids {
        if let Some(xfer) = TRANSFER_STORAGE.unregister(xfer_id) {
            xfer.cancel_token.cancel();
        }
    }
}

/// Close all interactive PTY shells for a session.
async fn close_session_shells(session_id: &str) {
    let shell_ids = SHELL_STORAGE.list_by_session(session_id);
    if !shell_ids.is_empty() {
        info!(
            "Closing {} interactive shells for session {session_id}",
            shell_ids.len(),
        );
        for shell_id in &shell_ids {
            if let Some(shell) = SHELL_STORAGE.unregister(shell_id) {
                shell.cancel_token.cancel();
                let _ = shell.channel_writer.lock().await.close().await;
            }
        }
    }
}

/// Cancel all async commands for a session.
fn cancel_session_commands(session_id: &str) -> usize {
    let command_ids = COMMAND_STORAGE.list_by_session(session_id);
    if command_ids.is_empty() {
        return 0;
    }
    info!(
        "Cancelling {} async commands for session {session_id}",
        command_ids.len(),
    );
    for cmd_id in &command_ids {
        if let Some(cmd_ref) = COMMAND_STORAGE.get_ref(cmd_id) {
            cmd_ref.running.cancel_token.cancel();
        }
    }
    let count = command_ids.len();
    for cmd_id in command_ids {
        COMMAND_STORAGE.unregister(&cmd_id);
    }
    count
}

/// Resolved connection configuration from arguments + env.
struct ConnectionConfig {
    connect_timeout: Duration,
    inactivity_timeout: Duration,
    max_retries: u32,
    retry_delay: Duration,
    compress: bool,
    persistent: bool,
}

/// Resolve all connection parameters following Param -> Env -> Default precedence.
fn resolve_connection_config(args: &SshConnectArgs) -> ConnectionConfig {
    ConnectionConfig {
        connect_timeout: resolve_connect_timeout(args.timeout_secs),
        inactivity_timeout: resolve_inactivity_timeout(),
        max_retries: resolve_max_retries(args.max_retries),
        retry_delay: resolve_retry_delay(args.retry_delay_ms),
        compress: resolve_compression(args.compress),
        persistent: args.persistent.unwrap_or(false),
    }
}

/// Establish a fresh SSH connection (with retry) and build the success response.
async fn create_new_connection(args: SshConnectArgs) -> Result<String, String> {
    let cfg = resolve_connection_config(&args);

    info!(
        "Attempting SSH to {}@{} timeout={}s retries={} delay={}ms compress={} persistent={} name={:?} agent={:?}",
        args.username,
        args.address,
        cfg.connect_timeout.as_secs(),
        cfg.max_retries,
        cfg.retry_delay.as_millis(),
        cfg.compress,
        cfg.persistent,
        args.name,
        args.agent_id,
    );

    let (handle, retries) = connect_to_ssh_with_retry(
        &args.address,
        &args.username,
        args.password.as_deref(),
        args.key_path.as_deref(),
        cfg.connect_timeout,
        cfg.inactivity_timeout,
        cfg.max_retries,
        cfg.retry_delay,
        cfg.compress,
        cfg.persistent,
    )
    .await
    .map_err(|e| {
        error!("SSH connection failed: {e}");
        format_error("SSH_CONNECT", "CONNECTION_FAILED", &e, None)
    })?;

    Ok(build_connect_success(
        handle,
        retries,
        cfg.connect_timeout,
        &args.address,
        &args.username,
        args.name.as_deref(),
        args.agent_id,
        cfg.compress,
        cfg.persistent,
    ))
}

/// Build the success response for a new SSH connection (and persist the session).
fn build_connect_success(
    handle: SshHandle,
    retry_attempts: u32,
    connect_timeout: Duration,
    address: &str,
    username: &str,
    name: Option<&str>,
    agent_id: Option<String>,
    compress: bool,
    persistent: bool,
) -> String {
    let new_session_id = Uuid::new_v4().to_string();

    let session_info = SessionInfo {
        session_id: new_session_id.clone(),
        name: name.map(String::from),
        agent_id: agent_id.clone(),
        host: address.to_string(),
        username: username.to_string(),
        connected_at: chrono::Utc::now().to_rfc3339(),
        default_timeout_secs: connect_timeout.as_secs(),
        retry_attempts,
        compression_enabled: compress,
        last_health_check: None,
        healthy: None,
    };

    SESSION_STORAGE.insert(new_session_id.clone(), session_info, Arc::new(handle));
    if let Some(aid) = agent_id.as_deref() {
        SESSION_STORAGE.register_agent(aid, &new_session_id);
    }

    ConnectOkBuilder::new(&new_session_id, username, address)
        .with_agent_id(agent_id.as_deref())
        .with_retry_attempts(retry_attempts)
        .with_persistent(persistent)
        .build()
}

/// Implementation of `ssh_connect` — kept as a free async fn so it can be
/// wired into the `#[tool_router]` macro from `server.rs`.
///
/// Returns either a `CallToolResult::success` carrying the markdown response,
/// or a `CallToolResult::error` for connection failures (also carrying
/// markdown so the LLM sees the standardized `REASON: [CODE]` line).
pub async fn ssh_connect_impl(args: SshConnectArgs) -> Result<CallToolResult, McpError> {
    if let Some(sid) = args.session_id.as_deref() {
        if let Some(response) = try_reuse_session(sid).await {
            return Ok(CallToolResult::success(vec![Content::text(response)]));
        }
        info!("Session {sid} not found or dead, creating new connection");
    }

    let policy = args.reuse.unwrap_or(ReusePolicy::Suggest);
    let (host_lc, port) = parse_host_port(&args.address);

    let matches = if matches!(policy, ReusePolicy::ForceNew) {
        IdentityMatches {
            healthy: Vec::new(),
            replaced: 0,
        }
    } else {
        evaluate_identity_matches(&host_lc, port, &args.username).await
    };

    if !matches.healthy.is_empty() {
        match policy {
            ReusePolicy::Auto => {
                if let Some(m) = matches.healthy.first() {
                    let body = ConnectOkBuilder::new(
                        &m.session_id,
                        &args.username,
                        format_host_for_display(&host_lc, port),
                    )
                    .with_agent_id(m.agent_id.as_deref())
                    .reused(true)
                    .build();
                    return Ok(CallToolResult::success(vec![Content::text(body)]));
                }
            }
            ReusePolicy::Suggest => {
                let body = ConnectSuggestedBuilder::new(matches.healthy).build();
                return Ok(CallToolResult::success(vec![Content::text(body)]));
            }
            ReusePolicy::ForceNew => {
                // Unreachable: ForceNew skips the lookup above.
            }
        }
    }

    let response = match create_new_connection(args).await {
        Ok(body) => append_replaced_line(body, matches.replaced),
        Err(err_markdown) => {
            return Ok(CallToolResult::error(vec![Content::text(err_markdown)]));
        }
    };
    Ok(CallToolResult::success(vec![Content::text(response)]))
}

/// Tool description (en-US) shared with the rmcp `#[tool]` macro registration.
pub const SSH_CONNECT_DESCRIPTION: &str = "Connect to an SSH server and store the session.

When to use:
- Establishing a new SSH connection to run commands, open shells, or transfer files.
- Reusing an already-connected session by passing its `session_id`.

Important identifiers in response:
- `SESSION_ID`: passed to ssh_execute, ssh_shell_open, ssh_upload, ssh_download,
  ssh_disconnect, ssh_forward.
- `AGENT_ID`: optional grouping; passed to ssh_list_sessions (filter) and
  ssh_disconnect_agent (cleanup).

Workflow:
1. Call ssh_connect once per remote host.
2. Use the returned SESSION_ID for subsequent tool calls.
3. Call ssh_disconnect (or ssh_disconnect_agent) when done.

Status values: OK, REUSED, SUGGESTED.

Errors: CONNECTION_FAILED.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_host_omits_default_port() {
        assert_eq!(format_host_for_display("example.com", 22), "example.com");
    }

    #[test]
    fn format_host_includes_non_default_port() {
        assert_eq!(format_host_for_display("example.com", 2222), "example.com:2222");
    }

    #[test]
    fn append_replaced_line_skips_when_zero() {
        let body = "SSH_CONNECT: OK\nSESSION_ID: abc".to_string();
        let out = append_replaced_line(body.clone(), 0);
        assert_eq!(out, body);
    }

    #[test]
    fn append_replaced_line_appends_when_positive() {
        let body = "SSH_CONNECT: OK\nSESSION_ID: abc".to_string();
        let out = append_replaced_line(body, 2);
        assert!(out.ends_with("\nREPLACED: 2"));
    }
}
