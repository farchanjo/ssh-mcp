//! Async-command tools: `ssh_execute`, `ssh_get_command_output`,
//! `ssh_list_commands`, `ssh_cancel_command`.
//!
//! These tools layer a fire-and-poll execution model on top of the russh
//! channel multiplexer:
//! - `ssh_execute` returns immediately with a `COMMAND_ID`.
//! - `ssh_get_command_output` retrieves running/final output (optionally
//!   blocking until completion).
//! - `ssh_list_commands` filters by session and/or status.
//! - `ssh_cancel_command` cooperatively cancels and reports the partial
//!   output collected so far.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rmcp::model::{CallToolResult, Content, ErrorData as McpError};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::time::{self, timeout};
use tracing::info;

use super::super::async_command::MAX_ASYNC_COMMANDS_PER_SESSION;
use super::super::config::resolve_command_timeout;
use super::super::message::builder::{
    CancelCommandCancelledBuilder, ListCommandsBuilder, render_cancel_command_noop,
};
use super::super::message::helpers::{format_error, generate_nonce};
use super::super::storage::command::COMMAND_STORAGE;
use super::super::storage::session::SESSION_STORAGE;
use super::super::storage::traits::{CommandStorage, SessionStorage};
use super::super::types::AsyncCommandStatus;
use super::CommandStatus;
use super::legacy_helpers::{
    build_command_output_response, clamp_list_items, clamp_output_bytes, err_session_not_found,
    get_running_command, register_and_spawn_command, wait_for_command_completion,
};

// ---------------------------------------------------------------------------
// `ssh_execute`
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_execute` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshExecuteArgs {
    /// SESSION_ID returned from ssh_connect.
    pub session_id: String,

    /// Shell command to execute on the remote host.
    pub command: String,

    /// Command timeout in seconds. Default: 180. Env: SSH_COMMAND_TIMEOUT.
    pub timeout_secs: Option<u64>,

    /// Allocate a pseudo-terminal (PTY) for the command. Default: false. Use for commands that require a controlling terminal (e.g. `sudo`, `top`). All output is merged into stdout in PTY mode (no stderr separation).
    pub pty: Option<bool>,
}

/// Implementation of `ssh_execute`.
#[expect(
    clippy::unused_async,
    reason = "rmcp tool entrypoints are uniformly async even when the body itself is sync"
)]
pub async fn ssh_execute_impl(args: SshExecuteArgs) -> Result<CallToolResult, McpError> {
    let SshExecuteArgs {
        session_id,
        command,
        timeout_secs,
        pty,
    } = args;
    let cmd_timeout = resolve_command_timeout(timeout_secs);

    let running_count = COMMAND_STORAGE.count_running_by_session(&session_id);
    if running_count >= MAX_ASYNC_COMMANDS_PER_SESSION {
        return Ok(CallToolResult::error(vec![Content::text(format_error(
            "SSH_EXECUTE",
            "MAX_COMMANDS_EXCEEDED",
            "maximum running async commands per session reached",
            Some(&format!("limit={MAX_ASYNC_COMMANDS_PER_SESSION}")),
        ))]));
    }

    let lookup = SESSION_STORAGE.get(&session_id).map(|s| {
        (
            Arc::clone(&s.handle),
            s.info.agent_id.clone(),
            Arc::clone(&s.channel_permits),
        )
    });
    let (handle_arc, agent_id, channel_permits) = match lookup {
        Some(triple) => triple,
        None => {
            return Ok(CallToolResult::error(vec![Content::text(
                err_session_not_found("SSH_EXECUTE", &session_id),
            )]));
        }
    };

    let body = register_and_spawn_command(
        session_id,
        command,
        agent_id,
        handle_arc,
        cmd_timeout,
        pty.unwrap_or(false),
        channel_permits,
    );
    Ok(CallToolResult::success(vec![Content::text(body)]))
}

// ---------------------------------------------------------------------------
// `ssh_get_command_output`
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_get_command_output` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshGetCommandOutputArgs {
    /// COMMAND_ID returned from ssh_execute.
    pub command_id: String,

    /// Block until completion or wait_timeout_secs expires. Default: false.
    pub wait: Option<bool>,

    /// Maximum seconds to block when wait=true. Default: 30. Cap: 300.
    pub wait_timeout_secs: Option<u64>,

    /// Maximum bytes shown in stdout/stderr. Default: 16384. Cap: 1048576. Content head-truncated; tail (most recent output) preserved. Env: SSH_MCP_OUTPUT_DEFAULT_BYTES / SSH_MCP_OUTPUT_MAX_BYTES_CAP.
    pub max_output_bytes: Option<usize>,
}

/// Implementation of `ssh_get_command_output`.
pub async fn ssh_get_command_output_impl(
    args: SshGetCommandOutputArgs,
) -> Result<CallToolResult, McpError> {
    let SshGetCommandOutputArgs {
        command_id,
        wait,
        wait_timeout_secs,
        max_output_bytes,
    } = args;
    let wait = wait.unwrap_or(false);
    let wait_timeout = Duration::from_secs(wait_timeout_secs.unwrap_or(30).min(300));
    let max_bytes = clamp_output_bytes(max_output_bytes);

    let lookup = COMMAND_STORAGE.get_direct(&command_id).map(|cmd| {
        (
            cmd.status_rx.clone(),
            Arc::clone(&cmd.output_history),
            Arc::clone(&cmd.exit_code),
            Arc::clone(&cmd.error),
            Arc::clone(&cmd.timed_out),
            Arc::clone(&cmd.output_read),
        )
    });
    let (status_rx, output_history, exit_code, error, timed_out, output_read) = match lookup {
        Some(t) => t,
        None => {
            return Ok(CallToolResult::error(vec![Content::text(format_error(
                "SSH_GET_COMMAND_OUTPUT",
                "COMMAND_NOT_FOUND",
                "no async command with the given ID",
                Some(&command_id),
            ))]));
        }
    };

    if wait {
        let mut rx = status_rx.clone();
        let _ = timeout(wait_timeout, wait_for_command_completion(&mut rx)).await;
    }

    output_read.store(true, Ordering::SeqCst);

    let body = build_command_output_response(
        command_id,
        &status_rx,
        &output_history,
        &exit_code,
        &error,
        &timed_out,
        max_bytes,
    );
    Ok(CallToolResult::success(vec![Content::text(body)]))
}

// ---------------------------------------------------------------------------
// `ssh_list_commands`
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_list_commands` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshListCommandsArgs {
    /// SESSION_ID returned from ssh_connect. Optional filter; when omitted returns commands across all sessions.
    pub session_id: Option<String>,

    /// Filter by command status. Values: `running`, `completed`, `cancelled`, `failed`. When omitted returns every status.
    pub status: Option<CommandStatus>,

    /// Maximum entries returned. Default: 500. Cap: 10000. Env: SSH_MCP_LIST_MAX_ITEMS / SSH_MCP_LIST_MAX_ITEMS_CAP.
    pub max_items: Option<usize>,
}

/// Implementation of `ssh_list_commands`.
#[expect(
    clippy::unused_async,
    reason = "rmcp tool entrypoints are uniformly async even when the body itself is sync"
)]
pub async fn ssh_list_commands_impl(args: SshListCommandsArgs) -> Result<CallToolResult, McpError> {
    let SshListCommandsArgs {
        session_id,
        status,
        max_items,
    } = args;
    let status_filter: Option<AsyncCommandStatus> = status.map(CommandStatus::into_legacy);

    let max = clamp_list_items(max_items);
    let mut filtered = COMMAND_STORAGE.list_filtered(session_id.as_deref(), status_filter);
    filtered.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    let total = filtered.len();
    filtered.truncate(max);
    let body = ListCommandsBuilder::new(&filtered, total).build();
    Ok(CallToolResult::success(vec![Content::text(body)]))
}

// ---------------------------------------------------------------------------
// `ssh_cancel_command`
// ---------------------------------------------------------------------------

/// Arguments for the `ssh_cancel_command` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshCancelCommandArgs {
    /// COMMAND_ID returned from ssh_execute.
    pub command_id: String,

    /// Maximum bytes shown in stdout/stderr. Default: 16384. Cap: 1048576. Content head-truncated; tail (most recent output) preserved. Env: SSH_MCP_OUTPUT_DEFAULT_BYTES / SSH_MCP_OUTPUT_MAX_BYTES_CAP.
    pub max_output_bytes: Option<usize>,
}

/// Implementation of `ssh_cancel_command`.
pub async fn ssh_cancel_command_impl(
    args: SshCancelCommandArgs,
) -> Result<CallToolResult, McpError> {
    let SshCancelCommandArgs {
        command_id,
        max_output_bytes,
    } = args;
    let max_bytes = clamp_output_bytes(max_output_bytes);
    let (cancel_token, output_history, status_rx) = match get_running_command(&command_id) {
        Ok(x) => x,
        Err(e) => {
            if e.contains("No async command") {
                return Ok(CallToolResult::error(vec![Content::text(format_error(
                    "SSH_CANCEL_COMMAND",
                    "COMMAND_NOT_FOUND",
                    "no async command with the given ID",
                    Some(&command_id),
                ))]));
            }
            return Ok(CallToolResult::success(vec![Content::text(
                render_cancel_command_noop(&command_id, "not running"),
            )]));
        }
    };

    cancel_token.cancel();

    let mut rx = status_rx;
    let _ = timeout(Duration::from_secs(5), wait_for_command_completion(&mut rx)).await;

    time::sleep(Duration::from_millis(100)).await;

    info!("Cancelled async command: {command_id}");
    let nonce = generate_nonce();
    let snapshot = output_history.load_full();
    let body = CancelCommandCancelledBuilder::new(
        &command_id,
        &snapshot.stdout,
        &snapshot.stderr,
        max_bytes,
        &nonce,
    )
    .build();
    Ok(CallToolResult::success(vec![Content::text(body)]))
}
