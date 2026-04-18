//! MCP SSH Commands implementation.
//!
//! This module provides the main MCP tool implementations for SSH operations:
//!
//! - `ssh_connect`: Connect to an SSH server with retry logic
//! - `ssh_execute`: Execute commands asynchronously (returns `command_id` for polling)
//! - `ssh_get_command_output`: Get output and status of a running command
//! - `ssh_list_commands`: List all async commands
//! - `ssh_cancel_command`: Cancel a running command
//! - `ssh_forward`: Setup port forwarding (feature-gated)
//! - `ssh_disconnect`: Disconnect and cleanup a session
//! - `ssh_list_sessions`: List all active sessions

#![allow(
    clippy::too_many_lines,
    reason = "MCP tool handlers and related helpers aggregate validation, storage lookup, builder construction, and background task spawning in one place; splitting further would hide intent"
)]
#![allow(
    clippy::needless_pass_by_value,
    reason = "the MCP #[Tools] macro requires owned parameters for the tool-call argument deserialization"
)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures::future::join_all;
use poem_mcpserver::{Tools, content::Text};
use russh::client::Msg;
use russh::{Channel, Disconnect, client};
use tokio::fs;
use tokio::sync::{Mutex, watch};
use tokio::time::{self, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::async_command::{MAX_ASYNC_COMMANDS_PER_SESSION, OutputBuffer, RunningCommand};
use super::client::{
    connect_to_ssh_with_retry, execute_ssh_command, execute_ssh_command_async,
    execute_ssh_command_async_pty, open_pty_shell,
};
use super::config::{
    resolve_command_cleanup_ttl, resolve_command_timeout, resolve_compression,
    resolve_connect_timeout, resolve_inactivity_timeout, resolve_max_retries, resolve_retry_delay,
    resolve_shell_inactivity_ttl, resolve_shell_max_buffer_size,
};
#[cfg(feature = "port_forward")]
use super::forward::setup_port_forwarding;
use super::message::builder::{
    CancelCommandCancelledBuilder, ConnectOkBuilder, ExecuteStartedBuilder,
    GetCommandOutputBuilder, GetCommandOutputState, ListCommandsBuilder, ListSessionsBuilder,
    ShellOpenBuilder, ShellReadBuilder, ShellReadState, TransferProgressBuilder,
    TransferProgressState, TransferStartDirection, TransferStartedBuilder,
    render_cancel_command_noop, render_disconnect_agent, render_disconnect_ok, render_forward_ok,
    render_shell_close_ok, render_shell_write_ok,
};
use super::message::helpers::{format_error, generate_nonce};
use super::session::SshClientHandler;
use super::sftp::{
    classify_transfer_error, open_sftp_session, resolve_local_path, sftp_download_streaming,
    sftp_upload_streaming,
};
use super::shell::{ChannelWriter, MAX_SHELLS_PER_SESSION, RunningShell};
use super::storage::command::COMMAND_STORAGE;
use super::storage::session::SESSION_STORAGE;
use super::storage::shell::SHELL_STORAGE;
use super::storage::traits::{
    CommandStorage, SessionRef, SessionStorage, ShellStorage, TransferStorage,
};
use super::storage::transfer::TRANSFER_STORAGE;
use super::transfer::{
    MAX_TRANSFERS_PER_SESSION, RunningTransfer, TransferDirection, TransferInfo, TransferStatus,
};
use super::types::{
    AsyncCommandInfo, AsyncCommandStatus, SessionInfo, ShellInfo, ShellStatus, SshCommandResponse,
};

/// Default maximum bytes to show in stdout/stderr/data blocks when the caller
/// does not provide `max_output_bytes`.
const DEFAULT_OUTPUT_MAX_BYTES: usize = 16 * 1024;
/// Hard cap on `max_output_bytes` regardless of caller request.
const OUTPUT_MAX_BYTES_CAP: usize = 1024 * 1024;
/// Default maximum items returned by list tools when no `max_items` provided.
const DEFAULT_LIST_MAX_ITEMS: usize = 500;
/// Hard cap on `max_items` to prevent abusive requests.
const LIST_MAX_ITEMS_CAP: usize = 10_000;

/// Clamp a caller-provided `max_output_bytes`.
fn clamp_output_bytes(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(DEFAULT_OUTPUT_MAX_BYTES)
        .min(OUTPUT_MAX_BYTES_CAP)
}

/// Clamp a caller-provided `max_items` for list tools.
fn clamp_list_items(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(DEFAULT_LIST_MAX_ITEMS)
        .clamp(1, LIST_MAX_ITEMS_CAP)
}

/// Map an internal session-id-not-found error to the standardized format.
fn err_session_not_found(tool: &str, session_id: &str) -> String {
    format_error(
        tool,
        "SESSION_NOT_FOUND",
        "no active SSH session with the given ID",
        Some(session_id),
    )
}

/// Type alias for the SSH client handle used throughout this module.
type SshHandle = client::Handle<SshClientHandler>;

/// MCP SSH Commands tool implementation.
///
/// This struct provides all SSH-related MCP tools for connecting to servers,
/// executing commands, and managing port forwarding.
pub struct McpSSHCommands;

// --- Free helper functions (no &self) ---

/// Try to reuse an existing session by performing a health check.
///
/// Returns the reuse response if the session is healthy, or `None` if dead.
async fn try_reuse_session(sid: &str) -> Option<Text<String>> {
    let session_ref = SESSION_STORAGE.get(sid)?;
    let health_timeout = Duration::from_secs(5);
    let now = chrono::Utc::now().to_rfc3339();

    let result = execute_ssh_command(&session_ref.handle, "echo 1", health_timeout).await;
    build_reuse_response(sid, &session_ref, now, result)
}

/// Build the reuse response based on health check result.
fn build_reuse_response(
    sid: &str,
    session_ref: &SessionRef,
    now: String,
    result: Result<SshCommandResponse, String>,
) -> Option<Text<String>> {
    match result {
        Ok(response) if !response.timed_out && response.exit_code == 0 => {
            SESSION_STORAGE.update_health(sid, now, true);
            info!("Reusing healthy session {sid}");
            let markdown =
                ConnectOkBuilder::new(sid, &session_ref.info.username, &session_ref.info.host)
                    .with_agent_id(session_ref.info.agent_id.as_deref())
                    .reused(true)
                    .build();
            Some(Text(markdown))
        }
        _ => {
            warn!("Session {sid} is dead, removing");
            SESSION_STORAGE.remove(sid);
            None
        }
    }
}

/// Cancel all transfers for a session.
fn cancel_session_transfers(session_id: &str) {
    let transfer_ids = TRANSFER_STORAGE.list_by_session(session_id);
    for xfer_id in &transfer_ids {
        if let Some(xfer) = TRANSFER_STORAGE.unregister(xfer_id) {
            xfer.cancel_token.cancel();
        }
    }
}

/// Close all interactive shells for a session.
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

/// Cancel all async commands for a session and return the count.
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

/// Wait for a watch receiver to leave the `Running` command status.
async fn wait_for_command_completion(rx: &mut watch::Receiver<AsyncCommandStatus>) {
    loop {
        let status = *rx.borrow();
        if status != AsyncCommandStatus::Running {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
}

/// Wait for a watch receiver to leave the `Running` transfer status.
async fn wait_for_transfer_completion(rx: &mut watch::Receiver<TransferStatus>) {
    loop {
        let status = *rx.borrow();
        if status != TransferStatus::Running {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
}

/// Create a `RunningCommand` with shared state.
fn create_running_command(
    command_id: &str,
    session_id: &str,
    command: &str,
    started_at: &str,
) -> RunningCommand {
    let (status_tx, status_rx) = watch::channel(AsyncCommandStatus::Running);

    RunningCommand {
        info: AsyncCommandInfo {
            command_id: command_id.to_string(),
            session_id: session_id.to_string(),
            command: command.to_string(),
            status: AsyncCommandStatus::Running,
            started_at: started_at.to_string(),
        },
        cancel_token: CancellationToken::new(),
        status_rx,
        status_tx,
        output: Arc::new(Mutex::new(OutputBuffer::with_capacity(4096, 1024))),
        exit_code: Arc::new(Mutex::new(None)),
        error: Arc::new(Mutex::new(None)),
        timed_out: Arc::new(AtomicBool::new(false)),
        output_read: Arc::new(AtomicBool::new(false)),
    }
}

/// Spawn the background command execution task.
#[allow(
    clippy::too_many_arguments,
    reason = "passes through shared state to async task"
)]
fn spawn_command_task(
    use_pty: bool,
    handle_arc: Arc<SshHandle>,
    command: String,
    cmd_timeout: Duration,
    output: Arc<Mutex<OutputBuffer>>,
    status_tx: watch::Sender<AsyncCommandStatus>,
    cancel_token: CancellationToken,
    exit_code: Arc<Mutex<Option<i32>>>,
    error: Arc<Mutex<Option<String>>>,
    timed_out: Arc<AtomicBool>,
) {
    if use_pty {
        tokio::spawn(execute_ssh_command_async_pty(
            handle_arc,
            command,
            cmd_timeout,
            output,
            status_tx,
            cancel_token,
            exit_code,
            error,
            timed_out,
        ));
    } else {
        tokio::spawn(execute_ssh_command_async(
            handle_arc,
            command,
            cmd_timeout,
            output,
            status_tx,
            cancel_token,
            exit_code,
            error,
            timed_out,
        ));
    }
}

/// Spawn the cleanup task that removes a command from storage after completion.
///
/// If the output has been read, cleanup is immediate. Otherwise, the task
/// waits up to `SSH_COMMAND_CLEANUP_TTL` (default: 60s) before removing.
fn spawn_cleanup_task(
    command_id: String,
    cleanup_rx: watch::Receiver<AsyncCommandStatus>,
    output_read: Arc<AtomicBool>,
) {
    let ttl = resolve_command_cleanup_ttl();
    tokio::spawn(async move {
        let mut rx = cleanup_rx;
        wait_for_command_completion(&mut rx).await;

        // If output already read, cleanup immediately
        if !output_read.load(Ordering::SeqCst) {
            // Wait up to TTL, checking periodically if output gets read
            let deadline = time::Instant::now() + ttl;
            let mut interval = time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                if output_read.load(Ordering::SeqCst) {
                    break;
                }
                if time::Instant::now() >= deadline {
                    info!("Cleanup: TTL expired for unread command {command_id}");
                    break;
                }
            }
        }

        COMMAND_STORAGE.unregister(&command_id);
        info!("Cleanup: removed completed command {command_id}");
    });
}

/// Spawn a background task that auto-closes a shell after inactivity.
///
/// Checks periodically whether the shell has been idle (no read/write)
/// longer than the configured TTL. When expired, cancels the shell
/// and removes it from storage.
fn spawn_shell_inactivity_task(
    shell_id: String,
    last_activity: Arc<Mutex<time::Instant>>,
    inactivity_ttl: Duration,
    cancel_token: CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;

            if cancel_token.is_cancelled() {
                break;
            }

            let elapsed = last_activity.lock().await.elapsed();
            if elapsed >= inactivity_ttl {
                info!(
                    "Shell {shell_id} inactive for {}s, auto-closing (TTL: {}s)",
                    elapsed.as_secs(),
                    inactivity_ttl.as_secs(),
                );
                if let Some(shell) = SHELL_STORAGE.unregister(&shell_id) {
                    shell.cancel_token.cancel();
                    let _ = shell.channel_writer.lock().await.close().await;
                }
                break;
            }
        }
    });
}

/// Build the output response for a command.
///
/// # Lock ordering
///
/// This function acquires several mutexes. To prevent any deadlock risk we
/// take each lock in its own scope so only one mutex is held at a time, and
/// we render the output block via the borrow-based API (no full buffer
/// clone under the lock).
async fn build_command_output_response(
    command_id: String,
    status_rx: &watch::Receiver<AsyncCommandStatus>,
    output: &Mutex<OutputBuffer>,
    exit_code: &Mutex<Option<i32>>,
    error: &Mutex<Option<String>>,
    timed_out: &AtomicBool,
    max_output_bytes: usize,
) -> Result<Text<String>, String> {
    let status = *status_rx.borrow();
    let timed_out_val = timed_out.load(Ordering::SeqCst);
    let exit_code_val = {
        let guard = exit_code.lock().await;
        *guard
    };
    let error_val = {
        let guard = error.lock().await;
        guard.clone()
    };

    // Failed / error path shortcuts to the standardized error format.
    if matches!(status, AsyncCommandStatus::Failed) {
        let reason = error_val.as_deref().unwrap_or("command failed");
        return Ok(Text(format_error(
            "SSH_GET_COMMAND_OUTPUT",
            "COMMAND_FAILED",
            reason,
            None,
        )));
    }

    let state = if timed_out_val {
        GetCommandOutputState::Timeout
    } else {
        match status {
            AsyncCommandStatus::Running | AsyncCommandStatus::Failed => {
                GetCommandOutputState::Running
            }
            AsyncCommandStatus::Completed | AsyncCommandStatus::Cancelled => {
                GetCommandOutputState::Completed(exit_code_val.unwrap_or(0))
            }
        }
    };

    let nonce = generate_nonce();
    let markdown = {
        let guard = output.lock().await;
        GetCommandOutputBuilder::new(
            &command_id,
            state,
            &guard.stdout,
            &guard.stderr,
            max_output_bytes,
            &nonce,
        )
        .build()
    };
    Ok(Text(markdown))
}

/// Build the agent disconnect response.
fn build_agent_disconnect_response(
    agent_id: &str,
    sessions_disconnected: usize,
    commands_cancelled: usize,
) -> Text<String> {
    Text(render_disconnect_agent(
        agent_id,
        sessions_disconnected,
        commands_cancelled,
    ))
}

/// Cleanup all sessions for an agent and return total commands cancelled.
async fn cleanup_agent_sessions(session_ids: &[String]) -> usize {
    let mut total_commands_cancelled = 0;

    for session_id in session_ids {
        cancel_session_transfers(session_id);

        let shell_ids = SHELL_STORAGE.list_by_session(session_id);
        for shell_id in &shell_ids {
            if let Some(shell) = SHELL_STORAGE.unregister(shell_id) {
                shell.cancel_token.cancel();
                let _ = shell.channel_writer.lock().await.close().await;
            }
        }

        total_commands_cancelled += cancel_session_commands(session_id);

        if let Some(session_ref) = SESSION_STORAGE.remove(session_id)
            && let Err(e) = session_ref
                .handle
                .disconnect(Disconnect::ByApplication, "Agent cleanup", "en")
                .await
        {
            warn!("Error during disconnect of session {session_id}: {e}");
        }
    }

    total_commands_cancelled
}

/// Process health check results and update storage.
fn process_health_results(
    results: Vec<(
        String,
        SessionInfo,
        String,
        Result<SshCommandResponse, String>,
    )>,
    max_items: usize,
) -> Text<String> {
    let (healthy_sessions, dead_session_ids) = classify_sessions(results);

    for (id, info) in &healthy_sessions {
        if let Some(last_check) = &info.last_health_check {
            SESSION_STORAGE.update_health(id, last_check.clone(), info.healthy.unwrap_or(false));
        }
    }

    for id in &dead_session_ids {
        warn!("Removing dead session {id} from storage");
        SESSION_STORAGE.remove(id);
    }

    let mut session_infos: Vec<SessionInfo> =
        healthy_sessions.into_iter().map(|(_, info)| info).collect();
    // Deterministic order: by connected_at ascending.
    session_infos.sort_by(|a, b| a.connected_at.cmp(&b.connected_at));
    let total = session_infos.len();
    session_infos.truncate(max_items);
    Text(ListSessionsBuilder::new(&session_infos, total).build())
}

/// Classify sessions into healthy and dead based on health check results.
fn classify_sessions(
    results: Vec<(
        String,
        SessionInfo,
        String,
        Result<SshCommandResponse, String>,
    )>,
) -> (Vec<(String, SessionInfo)>, Vec<String>) {
    let mut healthy_sessions = Vec::new();
    let mut dead_session_ids = Vec::new();

    for (session_id, mut info, now, result) in results {
        match result {
            Ok(response) if !response.timed_out && response.exit_code == 0 => {
                info.last_health_check = Some(now);
                info.healthy = Some(true);
                healthy_sessions.push((session_id, info));
            }
            _ => {
                info.last_health_check = Some(now);
                info.healthy = Some(false);
                dead_session_ids.push(session_id);
            }
        }
    }

    (healthy_sessions, dead_session_ids)
}

/// Spawn the background shell reader task.
fn spawn_shell_reader(
    read_half: russh::ChannelReadHalf,
    output: &Arc<Mutex<Vec<u8>>>,
    cancel_token: &CancellationToken,
    status_tx: &watch::Sender<ShellStatus>,
    max_buffer_size: &Arc<AtomicU64>,
    last_activity: &Arc<Mutex<time::Instant>>,
) {
    let args = (
        Arc::clone(output),
        cancel_token.clone(),
        status_tx.clone(),
        Arc::clone(max_buffer_size),
        Arc::clone(last_activity),
    );
    tokio::spawn(async move {
        shell_reader(read_half, args.0, args.1, args.2, args.3, args.4).await;
    });
}

/// Spawn background tasks (reader + inactivity monitor) for a shell.
fn spawn_shell_tasks(
    shell_id: &str,
    read_half: russh::ChannelReadHalf,
    shell: &RunningShell,
    inactivity_ttl: Duration,
) {
    spawn_shell_reader(
        read_half,
        &shell.output,
        &shell.cancel_token,
        &shell.status_tx,
        &shell.max_buffer_size,
        &shell.last_activity,
    );
    spawn_shell_inactivity_task(
        shell_id.to_string(),
        Arc::clone(&shell.last_activity),
        inactivity_ttl,
        shell.cancel_token.clone(),
    );
}

/// Spawn the shell reader and register the shell in storage.
fn spawn_and_register_shell(
    shell_id: &str,
    shell_info: ShellInfo,
    channel: Channel<Msg>,
    inactivity_ttl: Duration,
    max_buffer_size: u64,
) {
    let (status_tx, status_rx) = watch::channel(ShellStatus::Open);
    let (read_half, write_half) = channel.split();

    let shell = RunningShell {
        info: shell_info,
        cancel_token: CancellationToken::new(),
        output: Arc::new(Mutex::new(Vec::with_capacity(4096))),
        channel_writer: Arc::new(Mutex::new(ChannelWriter::new(write_half))),
        status_tx,
        status_rx,
        last_activity: Arc::new(Mutex::new(time::Instant::now())),
        max_buffer_size: Arc::new(AtomicU64::new(max_buffer_size)),
    };

    spawn_shell_tasks(shell_id, read_half, &shell, inactivity_ttl);
    SHELL_STORAGE.register(shell_id.to_string(), shell);
}

/// Register a new shell in storage and return the response.
#[allow(
    clippy::too_many_arguments,
    reason = "shell registration requires all PTY parameters"
)]
fn register_shell(
    session_id: &str,
    agent_id: Option<String>,
    term: String,
    cols: u32,
    rows: u32,
    channel: Channel<Msg>,
    inactivity_ttl: Duration,
    max_buffer_size: u64,
) -> Text<String> {
    let shell_id = Uuid::new_v4().to_string();

    let shell_info = ShellInfo {
        shell_id: shell_id.clone(),
        session_id: session_id.to_string(),
        term_type: term.clone(),
        cols,
        rows,
        opened_at: chrono::Utc::now().to_rfc3339(),
    };

    spawn_and_register_shell(
        &shell_id,
        shell_info,
        channel,
        inactivity_ttl,
        max_buffer_size,
    );

    info!(
        "Opened interactive shell {shell_id} on session {session_id} (term={term}, {cols}x{rows})"
    );

    Text(
        ShellOpenBuilder::new(&shell_id, session_id, &term, cols, rows)
            .with_agent_id(agent_id.as_deref())
            .build(),
    )
}

/// Shared state returned by transfer registration.
struct TransferSharedState {
    cancel_token: CancellationToken,
    bytes_transferred: Arc<AtomicU64>,
    status_tx: watch::Sender<TransferStatus>,
    error: Arc<Mutex<Option<String>>>,
}

/// Create shared state, register a transfer, and return handles for the task.
#[allow(
    clippy::too_many_arguments,
    reason = "transfer registration requires all SFTP parameters"
)]
fn create_and_register_transfer(
    transfer_id: &str,
    session_id: &str,
    direction: TransferDirection,
    local_path: &str,
    remote_path: &str,
    started_at: &str,
    total_bytes: u64,
) -> TransferSharedState {
    let (status_tx, status_rx) = watch::channel(TransferStatus::Running);
    let bytes_transferred = Arc::new(AtomicU64::new(0));
    let cancel_token = CancellationToken::new();
    let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    TRANSFER_STORAGE.register(
        transfer_id.to_string(),
        RunningTransfer {
            info: TransferInfo {
                transfer_id: transfer_id.to_string(),
                session_id: session_id.to_string(),
                direction,
                local_path: local_path.to_string(),
                remote_path: remote_path.to_string(),
                started_at: started_at.to_string(),
            },
            cancel_token: cancel_token.clone(),
            bytes_transferred: Arc::clone(&bytes_transferred),
            total_bytes: Arc::new(AtomicU64::new(total_bytes)),
            status_rx,
            status_tx: status_tx.clone(),
            error: Arc::clone(&error),
        },
    );

    TransferSharedState {
        cancel_token,
        bytes_transferred,
        status_tx,
        error,
    }
}

/// Start an upload transfer and return the response.
fn start_upload(
    session_id: String,
    agent_id: Option<String>,
    handle_arc: Arc<SshHandle>,
    resolved_path: &Path,
    remote_path: String,
    total_bytes: u64,
) -> Text<String> {
    let transfer_id = Uuid::new_v4().to_string();
    let started_at = chrono::Utc::now().to_rfc3339();
    let local_path = resolved_path.to_string_lossy().into_owned();

    let state = create_and_register_transfer(
        &transfer_id,
        &session_id,
        TransferDirection::Upload,
        &local_path,
        &remote_path,
        &started_at,
        total_bytes,
    );

    info!(
        "Starting SFTP upload {transfer_id} on session {session_id}: {local_path} -> {remote_path} ({total_bytes} bytes)"
    );
    spawn_upload_task(handle_arc, resolved_path, remote_path.clone(), state);

    Text(
        TransferStartedBuilder::new(
            TransferStartDirection::Upload,
            transfer_id,
            session_id,
            local_path,
            remote_path,
            total_bytes,
        )
        .with_agent_id(agent_id.as_deref())
        .build(),
    )
}

/// Spawn the background upload task.
fn spawn_upload_task(
    handle_arc: Arc<SshHandle>,
    resolved_path: &Path,
    remote_path: String,
    state: TransferSharedState,
) {
    tokio::spawn(sftp_upload_streaming(
        handle_arc,
        resolved_path.to_path_buf(),
        remote_path,
        state.bytes_transferred,
        state.cancel_token,
        state.status_tx,
        state.error,
    ));
}

/// Start a download transfer and return the response.
fn start_download(
    session_id: String,
    agent_id: Option<String>,
    handle_arc: Arc<SshHandle>,
    remote_path: String,
    resolved_path: &Path,
    total_bytes: u64,
) -> Text<String> {
    let transfer_id = Uuid::new_v4().to_string();
    let started_at = chrono::Utc::now().to_rfc3339();
    let local_path = resolved_path.to_string_lossy().into_owned();

    let state = create_and_register_transfer(
        &transfer_id,
        &session_id,
        TransferDirection::Download,
        &local_path,
        &remote_path,
        &started_at,
        total_bytes,
    );

    info!(
        "Starting SFTP download {transfer_id} on session {session_id}: {remote_path} -> {local_path} ({total_bytes} bytes)"
    );
    spawn_download_task(handle_arc, &remote_path, resolved_path, state);

    Text(
        TransferStartedBuilder::new(
            TransferStartDirection::Download,
            transfer_id,
            session_id,
            local_path,
            remote_path,
            total_bytes,
        )
        .with_agent_id(agent_id.as_deref())
        .build(),
    )
}

/// Spawn the background download task.
fn spawn_download_task(
    handle_arc: Arc<SshHandle>,
    remote_path: &str,
    resolved_path: &Path,
    state: TransferSharedState,
) {
    tokio::spawn(sftp_download_streaming(
        handle_arc,
        remote_path.to_string(),
        resolved_path.to_path_buf(),
        state.bytes_transferred,
        state.cancel_token,
        state.status_tx,
        state.error,
    ));
}

/// Build the transfer progress response.
async fn build_transfer_progress_response(
    transfer_id: String,
    status_rx: &watch::Receiver<TransferStatus>,
    bytes_transferred: &AtomicU64,
    total_bytes_arc: &AtomicU64,
    error: &Mutex<Option<String>>,
    info: &TransferInfo,
) -> Result<Text<String>, String> {
    let status = *status_rx.borrow();
    let transferred = bytes_transferred.load(Ordering::SeqCst);
    let total = total_bytes_arc.load(Ordering::SeqCst);
    let error_val = {
        let guard = error.lock().await;
        guard.clone()
    };

    let direction = match info.direction {
        TransferDirection::Upload => TransferStartDirection::Upload,
        TransferDirection::Download => TransferStartDirection::Download,
    };

    let state = match status {
        TransferStatus::Running => TransferProgressState::Running,
        TransferStatus::Completed => TransferProgressState::Completed,
        TransferStatus::Failed => {
            let reason = error_val.as_deref().unwrap_or("transfer failed");
            return Ok(Text(
                TransferProgressBuilder::new(
                    &transfer_id,
                    direction,
                    transferred,
                    total,
                    TransferProgressState::Failed(reason),
                )
                .build(),
            ));
        }
        TransferStatus::Cancelled => {
            return Ok(Text(
                TransferProgressBuilder::new(
                    &transfer_id,
                    direction,
                    transferred,
                    total,
                    TransferProgressState::Failed("transfer cancelled"),
                )
                .build(),
            ));
        }
    };

    Ok(Text(
        TransferProgressBuilder::new(&transfer_id, direction, transferred, total, state).build(),
    ))
}

/// Create a new SSH connection (non-reuse path).
#[allow(
    clippy::too_many_arguments,
    reason = "delegates from ssh_connect which has many SSH parameters"
)]
async fn create_new_connection(
    address: String,
    username: String,
    password: Option<String>,
    key_path: Option<String>,
    timeout_secs: Option<u64>,
    max_retries: Option<u32>,
    retry_delay_ms: Option<u64>,
    compress: Option<bool>,
    name: Option<String>,
    persistent: Option<bool>,
    agent_id: Option<String>,
) -> Result<Text<String>, String> {
    let cfg = resolve_connection_config(
        timeout_secs,
        max_retries,
        retry_delay_ms,
        compress,
        persistent,
    );

    log_new_connection(
        &username,
        &address,
        &cfg,
        name.as_deref(),
        agent_id.as_deref(),
    );

    let (handle, retries) = attempt_connection(&address, &username, password, key_path, &cfg)
        .await
        .map_err(|e| format_error("SSH_CONNECT", "CONNECTION_FAILED", &e, None))?;

    Ok(build_connect_success(
        handle,
        retries,
        cfg.connect_timeout,
        &address,
        &username,
        name.as_deref(),
        agent_id,
        cfg.compress,
        cfg.persistent,
    ))
}

/// Attempt the SSH connection with retry logic.
async fn attempt_connection(
    address: &str,
    username: &str,
    password: Option<String>,
    key_path: Option<String>,
    cfg: &ConnectionConfig,
) -> Result<(SshHandle, u32), String> {
    connect_to_ssh_with_retry(
        address,
        username,
        password.as_deref(),
        key_path.as_deref(),
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
        e
    })
}

/// Resolved connection configuration.
struct ConnectionConfig {
    connect_timeout: Duration,
    inactivity_timeout: Duration,
    max_retries: u32,
    retry_delay: Duration,
    compress: bool,
    persistent: bool,
}

/// Resolve all connection parameters from options/env/defaults.
fn resolve_connection_config(
    timeout_secs: Option<u64>,
    max_retries: Option<u32>,
    retry_delay_ms: Option<u64>,
    compress: Option<bool>,
    persistent: Option<bool>,
) -> ConnectionConfig {
    ConnectionConfig {
        connect_timeout: resolve_connect_timeout(timeout_secs),
        inactivity_timeout: resolve_inactivity_timeout(),
        max_retries: resolve_max_retries(max_retries),
        retry_delay: resolve_retry_delay(retry_delay_ms),
        compress: resolve_compression(compress),
        persistent: persistent.unwrap_or(false),
    }
}

/// Log details of a new SSH connection attempt.
fn log_new_connection(
    username: &str,
    address: &str,
    cfg: &ConnectionConfig,
    name: Option<&str>,
    agent_id: Option<&str>,
) {
    info!(
        "Attempting SSH to {username}@{address} timeout={}s retries={} delay={}ms compress={} persistent={} name={name:?} agent={agent_id:?}",
        cfg.connect_timeout.as_secs(),
        cfg.max_retries,
        cfg.retry_delay.as_millis(),
        cfg.compress,
        cfg.persistent,
    );
}

/// Store the new session and register agent if applicable.
fn store_new_session(
    session_id: &str,
    handle: SshHandle,
    info: SessionInfo,
    agent_id: Option<&str>,
) {
    SESSION_STORAGE.insert(session_id.to_string(), info, Arc::new(handle));
    if let Some(aid) = agent_id {
        SESSION_STORAGE.register_agent(aid, session_id);
    }
}

/// Build the success response for a new SSH connection.
#[allow(
    clippy::too_many_arguments,
    reason = "constructs response from all connection parameters"
)]
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
) -> Text<String> {
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

    store_new_session(&new_session_id, handle, session_info, agent_id.as_deref());

    Text(
        ConnectOkBuilder::new(&new_session_id, username, address)
            .with_agent_id(agent_id.as_deref())
            .with_retry_attempts(retry_attempts)
            .with_persistent(persistent)
            .build(),
    )
}

/// State needed to cancel a running command and retrieve its output.
type CancelState = (
    CancellationToken,
    Arc<Mutex<OutputBuffer>>,
    watch::Receiver<AsyncCommandStatus>,
);

/// Get a running command's cancellation handles, or error if not running.
fn get_running_command(command_id: &str) -> Result<CancelState, String> {
    COMMAND_STORAGE
        .get_direct(command_id)
        .map(|cmd| {
            let current_status = *cmd.status_rx.borrow();
            (
                current_status,
                cmd.cancel_token.clone(),
                Arc::clone(&cmd.output),
                cmd.status_rx.clone(),
            )
        })
        .ok_or_else(|| format!("No async command with ID: {command_id}"))
        .and_then(|(current_status, cancel_token, output, status_rx)| {
            if current_status == AsyncCommandStatus::Running {
                Ok((cancel_token, output, status_rx))
            } else {
                Err(format!("Command is not running (status: {current_status})"))
            }
        })
}

/// Register and spawn an async command, returning the response.
#[allow(
    clippy::too_many_arguments,
    reason = "coordinates command lifecycle across shared state"
)]
fn register_and_spawn_command(
    session_id: String,
    command: String,
    agent_id: Option<String>,
    handle_arc: Arc<SshHandle>,
    cmd_timeout: Duration,
    use_pty: bool,
) -> Text<String> {
    let command_id = Uuid::new_v4().to_string();
    let started_at = chrono::Utc::now().to_rfc3339();

    let running_cmd = create_running_command(&command_id, &session_id, &command, &started_at);
    let cleanup_rx = running_cmd.status_rx.clone();
    let output_read = Arc::clone(&running_cmd.output_read);

    register_and_spawn(
        &command_id,
        &session_id,
        running_cmd,
        use_pty,
        handle_arc,
        &command,
        cmd_timeout,
    );
    spawn_cleanup_task(command_id.clone(), cleanup_rx, output_read);

    Text(
        ExecuteStartedBuilder::new(&command_id, &session_id)
            .with_agent_id(agent_id.as_deref())
            .build(),
    )
}

/// Register the command in storage and spawn the execution task.
fn register_and_spawn(
    command_id: &str,
    session_id: &str,
    running_cmd: RunningCommand,
    use_pty: bool,
    handle_arc: Arc<SshHandle>,
    command: &str,
    cmd_timeout: Duration,
) {
    let status_tx = running_cmd.status_tx.clone();
    let output = Arc::clone(&running_cmd.output);
    let exit_code = Arc::clone(&running_cmd.exit_code);
    let error = Arc::clone(&running_cmd.error);
    let timed_out = Arc::clone(&running_cmd.timed_out);
    let cancel_token = running_cmd.cancel_token.clone();

    COMMAND_STORAGE.register(command_id.to_string(), running_cmd);
    info!("Starting async command {command_id} on session {session_id}: {command}");

    spawn_command_task(
        use_pty,
        handle_arc,
        command.to_string(),
        cmd_timeout,
        output,
        status_tx,
        cancel_token,
        exit_code,
        error,
        timed_out,
    );
}

/// Port forwarding implementation (feature-gated).
#[cfg(feature = "port_forward")]
async fn forward_impl(
    session_id: String,
    local_port: u16,
    remote_address: String,
    remote_port: u16,
) -> Result<Text<String>, String> {
    info!(
        "Setting up port forwarding from local port {} to {}:{} using session {}",
        local_port, remote_address, remote_port, session_id
    );

    let handle_arc = SESSION_STORAGE
        .get(&session_id)
        .map(|s| Arc::clone(&s.handle))
        .ok_or_else(|| err_session_not_found("SSH_FORWARD", &session_id))?;

    setup_port_forwarding(handle_arc, local_port, &remote_address, remote_port)
        .await
        .map(|local_addr| {
            Text(render_forward_ok(
                &local_addr.to_string(),
                &format!("{remote_address}:{remote_port}"),
                true,
            ))
        })
        .map_err(|e| {
            error!("Port forwarding setup failed: {e}");
            format_error("SSH_FORWARD", "FORWARD_FAILED", &e, None)
        })
}

// --- MCP Tool implementations ---

#[Tools]
impl McpSSHCommands {
    /// Connect to an SSH server and store the session.
    ///
    /// Returns `session_id` and optional `agent_id` that you MUST remember for subsequent commands.
    ///
    /// **Important identifiers in response:**
    /// - `session_id`: Use with `ssh_execute`, `ssh_disconnect`
    /// - `agent_id`: Use with `ssh_list_sessions` (filter), `ssh_disconnect_agent` (cleanup)
    ///
    /// For long-running operations (builds, deployments, batch processing),
    /// `ssh_execute` provides non-blocking execution with progress monitoring.
    ///
    /// Use `persistent=true` for sessions that should remain open indefinitely.
    #[allow(
        clippy::too_many_arguments,
        reason = "MCP tool requires many SSH connection parameters"
    )]
    async fn ssh_connect(
        &self,
        /// Optional session ID to reuse - if provided and still connected, returns existing session
        session_id: Option<String>,
        /// SSH server address in format "host:port" (e.g., "192.168.1.1:22")
        address: String,
        /// SSH username for authentication
        username: String,
        /// Password for password-based authentication (optional if using key or agent)
        password: Option<String>,
        /// Path to private key file for key-based authentication (optional)
        key_path: Option<String>,
        /// Connection timeout in seconds (default: 30, env: SSH_CONNECT_TIMEOUT)
        timeout_secs: Option<u64>,
        /// Maximum retry attempts for transient connection failures (default: 3, env: SSH_MAX_RETRIES)
        max_retries: Option<u32>,
        /// Initial delay between retries in milliseconds, uses exponential backoff (default: 1000, env: SSH_RETRY_DELAY_MS)
        retry_delay_ms: Option<u64>,
        /// Enable zlib compression for the SSH connection (default: true, env: SSH_COMPRESSION)
        compress: Option<bool>,
        /// Optional human-readable name for the session (helps identify sessions, e.g., "production-db", "staging-server")
        name: Option<String>,
        /// Keep session open indefinitely until explicitly disconnected (disables inactivity timeout, default: false)
        persistent: Option<bool>,
        /// Optional agent identifier for grouping sessions (e.g., "claude-code-instance-abc123"). Use ssh_disconnect_agent to disconnect all sessions for an agent.
        agent_id: Option<String>,
    ) -> Result<Text<String>, String> {
        if let Some(sid) = &session_id {
            if let Some(response) = try_reuse_session(sid).await {
                return Ok(response);
            }
            info!("Session {sid} not found or dead, creating new connection");
        }

        create_new_connection(
            address,
            username,
            password,
            key_path,
            timeout_secs,
            max_retries,
            retry_delay_ms,
            compress,
            name,
            persistent,
            agent_id,
        )
        .await
    }

    /// Disconnect an SSH session and release resources.
    ///
    /// **Important:** This automatically cancels all running async commands
    /// associated with the session. Check `ssh_list_commands` first if you
    /// need to preserve running operations.
    async fn ssh_disconnect(
        &self,
        /// Session ID to disconnect
        session_id: String,
    ) -> Result<Text<String>, String> {
        info!("Disconnecting SSH session: {session_id}");

        cancel_session_transfers(&session_id);
        close_session_shells(&session_id).await;
        cancel_session_commands(&session_id);

        if let Some(session_ref) = SESSION_STORAGE.remove(&session_id) {
            if let Some(agent_id) = &session_ref.info.agent_id {
                SESSION_STORAGE.unregister_agent(agent_id, &session_id);
            }

            if let Err(e) = session_ref
                .handle
                .disconnect(Disconnect::ByApplication, "Session closed by user", "en")
                .await
            {
                warn!("Error during disconnect: {e}");
            }
            Ok(Text(render_disconnect_ok(&session_id)))
        } else {
            Err(err_session_not_found("SSH_DISCONNECT", &session_id))
        }
    }

    /// List all active SSH sessions with their metadata.
    ///
    /// Performs a health check on each session and automatically removes
    /// dead/disconnected sessions from the list. Use this to find available
    /// `session_ids` for command execution.
    ///
    /// **Filtering by `agent_id`:** When provided, only sessions belonging to that
    /// agent are returned. This is useful when multiple agents share an MCP server.
    async fn ssh_list_sessions(
        &self,
        /// Filter by agent ID to list only sessions for a specific agent
        agent_id: Option<String>,
        /// Maximum number of sessions to return (default: 500, cap: 10000)
        max_items: Option<usize>,
    ) -> Text<String> {
        let health_timeout = Duration::from_secs(5);
        let max = clamp_list_items(max_items);

        let session_ids_to_check: Vec<String> = agent_id.as_ref().map_or_else(
            || SESSION_STORAGE.session_ids(),
            |aid| SESSION_STORAGE.get_agent_sessions(aid),
        );

        let health_futures: Vec<_> = session_ids_to_check
            .into_iter()
            .filter_map(|session_id| {
                SESSION_STORAGE
                    .get(&session_id)
                    .map(|sr| (session_id, Arc::clone(&sr.handle), sr.info))
            })
            .map(|(session_id, handle_arc, info)| async move {
                let now = chrono::Utc::now().to_rfc3339();
                let result = execute_ssh_command(&handle_arc, "echo 1", health_timeout).await;
                (session_id, info, now, result)
            })
            .collect();

        let results = join_all(health_futures).await;
        process_health_results(results, max)
    }

    /// Setup port forwarding on an existing SSH session
    #[allow(
        unused_variables,
        reason = "parameters used only when port_forward feature is enabled"
    )]
    async fn ssh_forward(
        &self,
        /// Session ID returned from ssh_connect
        session_id: String,
        /// Local port to listen on (e.g., 8080)
        local_port: u16,
        /// Remote host to forward to (e.g., "localhost" or "10.0.0.1")
        remote_address: String,
        /// Remote port to forward to (e.g., 3306 for MySQL)
        remote_port: u16,
    ) -> Result<Text<String>, String> {
        #[cfg(feature = "port_forward")]
        return forward_impl(session_id, local_port, remote_address, remote_port).await;

        #[cfg(not(feature = "port_forward"))]
        Err(format_error(
            "SSH_FORWARD",
            "FEATURE_DISABLED",
            "port forwarding feature is not enabled",
            Some("rebuild with --features port_forward"),
        ))
    }

    /// Execute a command asynchronously on a connected SSH session.
    ///
    /// **Recommended for:** Long-running commands (builds, deployments, batch jobs,
    /// data processing) that may exceed the default timeout or benefit from
    /// progress monitoring and cancellation.
    ///
    /// Returns `command_id`, `session_id`, and `agent_id` that you MUST remember.
    ///
    /// **Important identifiers in response:**
    /// - `command_id`: Use with `ssh_get_command_output` (poll), `ssh_cancel_command` (cancel)
    /// - `session_id`: The session running this command
    /// - `agent_id`: The agent that owns this session (if set)
    ///
    /// **Workflow:**
    /// 1. `ssh_execute` -> get `command_id`
    /// 2. `ssh_get_command_output(command_id, wait=true)` -> get result
    ///
    /// **Limits:** Up to 100 concurrent multiplexed commands per session.
    /// When the limit is reached, you must wait for existing commands to complete
    /// or cancel them using `ssh_cancel_command` before starting new ones.
    ///
    /// Returns immediately with a `command_id` for polling or cancellation.
    #[allow(
        clippy::unused_async,
        reason = "MCP tool macro requires async signature"
    )]
    async fn ssh_execute(
        &self,
        /// Session ID returned from ssh_connect
        session_id: String,
        /// Shell command to execute on the remote server
        command: String,
        /// Command execution timeout in seconds (default: 180, env: SSH_COMMAND_TIMEOUT)
        timeout_secs: Option<u64>,
        /// Allocate a pseudo-terminal (PTY) for the command. Use for commands requiring a terminal (sudo, top). All output goes to stdout in PTY mode (no stderr separation).
        pty: Option<bool>,
    ) -> Result<Text<String>, String> {
        let cmd_timeout = resolve_command_timeout(timeout_secs);

        let running_count = COMMAND_STORAGE.count_running_by_session(&session_id);
        if running_count >= MAX_ASYNC_COMMANDS_PER_SESSION {
            return Err(format_error(
                "SSH_EXECUTE",
                "MAX_COMMANDS_EXCEEDED",
                "maximum running async commands per session reached",
                Some(&format!("limit={MAX_ASYNC_COMMANDS_PER_SESSION}")),
            ));
        }

        let (handle_arc, agent_id) = SESSION_STORAGE
            .get(&session_id)
            .map(|s| (Arc::clone(&s.handle), s.info.agent_id.clone()))
            .ok_or_else(|| err_session_not_found("SSH_EXECUTE", &session_id))?;

        Ok(register_and_spawn_command(
            session_id,
            command,
            agent_id,
            handle_arc,
            cmd_timeout,
            pty.unwrap_or(false),
        ))
    }

    /// Get the current output and status of an async command.
    ///
    /// **Polling mode** (`wait=false`): Returns immediately with current status and partial output.
    /// Use this for progress monitoring or checking if a command is still running.
    ///
    /// **Blocking mode** (`wait=true`): Waits until the command completes or timeout expires.
    /// Use this when you need the final result and can wait.
    ///
    /// **Status values:** `running`, `completed`, `cancelled`, `failed`
    async fn ssh_get_command_output(
        &self,
        /// Command ID returned from ssh_execute_async
        command_id: String,
        /// If true, block until command completes or wait_timeout_secs expires
        wait: Option<bool>,
        /// Max seconds to wait when wait=true (default: 30, max: 300)
        wait_timeout_secs: Option<u64>,
        /// Maximum bytes to show for stdout/stderr (default: 16384, cap: 1048576).
        /// Content is truncated head-side; the tail (most recent output) is preserved.
        max_output_bytes: Option<usize>,
    ) -> Result<Text<String>, String> {
        let wait = wait.unwrap_or(false);
        let wait_timeout = Duration::from_secs(wait_timeout_secs.unwrap_or(30).min(300));
        let max_bytes = clamp_output_bytes(max_output_bytes);

        let (status_rx, output, exit_code, error, timed_out, output_read) = COMMAND_STORAGE
            .get_direct(&command_id)
            .map(|cmd| {
                (
                    cmd.status_rx.clone(),
                    Arc::clone(&cmd.output),
                    Arc::clone(&cmd.exit_code),
                    Arc::clone(&cmd.error),
                    Arc::clone(&cmd.timed_out),
                    Arc::clone(&cmd.output_read),
                )
            })
            .ok_or_else(|| {
                format_error(
                    "SSH_GET_COMMAND_OUTPUT",
                    "COMMAND_NOT_FOUND",
                    "no async command with the given ID",
                    Some(&command_id),
                )
            })?;

        if wait {
            let mut rx = status_rx.clone();
            let _ = timeout(wait_timeout, wait_for_command_completion(&mut rx)).await;
        }

        // Mark output as read so the cleanup task can release immediately.
        output_read.store(true, Ordering::SeqCst);

        build_command_output_response(
            command_id, &status_rx, &output, &exit_code, &error, &timed_out, max_bytes,
        )
        .await
    }

    /// List all async commands, optionally filtered by session or status.
    ///
    /// Useful for monitoring multiple concurrent operations or checking
    /// which commands are still running before disconnecting a session.
    #[allow(
        clippy::unused_async,
        reason = "MCP tool macro requires async signature"
    )]
    async fn ssh_list_commands(
        &self,
        /// Filter by session ID
        session_id: Option<String>,
        /// Filter by status: "running", "completed", "cancelled", "failed"
        status: Option<String>,
        /// Maximum number of commands to return (default: 500, cap: 10000)
        max_items: Option<usize>,
    ) -> Text<String> {
        let status_filter: Option<AsyncCommandStatus> = status.and_then(|s| match s.as_str() {
            "running" => Some(AsyncCommandStatus::Running),
            "completed" => Some(AsyncCommandStatus::Completed),
            "cancelled" => Some(AsyncCommandStatus::Cancelled),
            "failed" => Some(AsyncCommandStatus::Failed),
            _ => None,
        });

        let max = clamp_list_items(max_items);
        let mut filtered = COMMAND_STORAGE.list_filtered(session_id.as_deref(), status_filter);
        filtered.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        let total = filtered.len();
        filtered.truncate(max);
        Text(ListCommandsBuilder::new(&filtered, total).build())
    }

    /// Cancel a running async command.
    ///
    /// Returns the output collected so far. Use this to stop long-running commands
    /// that are no longer needed, or to abort commands that are taking too long.
    ///
    /// Note: Only running commands can be cancelled. Completed/failed commands
    /// will return an error.
    async fn ssh_cancel_command(
        &self,
        /// Command ID to cancel
        command_id: String,
        /// Maximum bytes to show for stdout/stderr (default: 16384, cap: 1048576).
        max_output_bytes: Option<usize>,
    ) -> Result<Text<String>, String> {
        let max_bytes = clamp_output_bytes(max_output_bytes);
        let (cancel_token, output, status_rx) = match get_running_command(&command_id) {
            Ok(x) => x,
            Err(e) => {
                // Distinguish "not found" from "not running" (noop).
                if e.contains("No async command") {
                    return Err(format_error(
                        "SSH_CANCEL_COMMAND",
                        "COMMAND_NOT_FOUND",
                        "no async command with the given ID",
                        Some(&command_id),
                    ));
                }
                return Ok(Text(render_cancel_command_noop(&command_id, "not running")));
            }
        };

        cancel_token.cancel();

        let mut rx = status_rx;
        let _ = timeout(Duration::from_secs(2), wait_for_command_completion(&mut rx)).await;

        info!("Cancelled async command: {command_id}");
        let nonce = generate_nonce();
        let markdown = {
            let guard = output.lock().await;
            CancelCommandCancelledBuilder::new(
                &command_id,
                &guard.stdout,
                &guard.stderr,
                max_bytes,
                &nonce,
            )
            .build()
        };
        Ok(Text(markdown))
    }

    /// Disconnect ALL sessions for a specific agent.
    ///
    /// Use this for cleanup when an agent is done. This will:
    /// - Cancel all running commands for the agent's sessions
    /// - Disconnect all sessions owned by the agent
    /// - Other agents' sessions are NOT affected
    ///
    /// **Required identifier:**
    /// - `agent_id`: The agent identifier from `ssh_connect`
    async fn ssh_disconnect_agent(
        &self,
        /// The agent identifier to disconnect all sessions for
        agent_id: String,
    ) -> Result<Text<String>, String> {
        info!("Disconnecting all sessions for agent: {agent_id}");

        let session_ids = SESSION_STORAGE.remove_agent_sessions(&agent_id);

        if session_ids.is_empty() {
            return Ok(build_agent_disconnect_response(&agent_id, 0, 0));
        }

        let total_commands_cancelled = cleanup_agent_sessions(&session_ids).await;
        let sessions_disconnected = session_ids.len();

        info!(
            "Disconnected {sessions_disconnected} sessions, cancelled {total_commands_cancelled} commands for agent {agent_id}"
        );

        Ok(build_agent_disconnect_response(
            &agent_id,
            sessions_disconnected,
            total_commands_cancelled,
        ))
    }

    /// Open an interactive PTY shell on a connected SSH session.
    ///
    /// Allocates a pseudo-terminal and starts a shell for interactive use.
    /// Output is continuously buffered and can be read with `ssh_shell_read`.
    /// Input can be sent with `ssh_shell_write`.
    ///
    /// **Use cases:**
    /// - Interactive sessions (SOL/IPMI/OOB console access)
    /// - Multi-step workflows requiring persistent shell state
    /// - Commands requiring terminal interaction
    ///
    /// For Serial Over LAN (SOL) / IPMI / OOB access, use `term="vt100"` with `cols=80`, `rows=24`.
    ///
    /// **Limits:** Up to 10 concurrent shells per session.
    async fn ssh_shell_open(
        &self,
        /// Session ID returned from ssh_connect
        session_id: String,
        /// Terminal type (default: "xterm"). Use "vt100" or "ansi" for SOL/IPMI/serial consoles.
        term: Option<String>,
        /// Terminal width in columns (default: 80)
        cols: Option<u32>,
        /// Terminal height in rows (default: 24)
        rows: Option<u32>,
        /// Shell inactivity TTL in seconds. Auto-closes the shell if no read/write occurs within this duration. Default: 600s (env: SSH_SHELL_INACTIVITY_TTL).
        inactivity_ttl: Option<u64>,
        /// Maximum output buffer size. Accepts human-readable sizes: "512k", "10m", "1g", "1t". When exceeded, oldest output is truncated. Default: "10m" (env: SSH_SHELL_MAX_BUFFER_SIZE).
        max_buffer_size: Option<String>,
    ) -> Result<Text<String>, String> {
        let term = term.unwrap_or_else(|| "xterm".to_string());
        let cols = cols.unwrap_or(80);
        let rows = rows.unwrap_or(24);
        let inactivity_ttl = resolve_shell_inactivity_ttl(inactivity_ttl);
        let max_buffer_size = resolve_shell_max_buffer_size(max_buffer_size.as_deref());

        let current_count = SHELL_STORAGE.count_by_session(&session_id);
        if current_count >= MAX_SHELLS_PER_SESSION {
            return Err(format_error(
                "SSH_SHELL_OPEN",
                "MAX_SHELLS_EXCEEDED",
                "maximum shells per session reached",
                Some(&format!("limit={MAX_SHELLS_PER_SESSION}")),
            ));
        }

        let (handle_arc, agent_id) = SESSION_STORAGE
            .get(&session_id)
            .map(|s| (Arc::clone(&s.handle), s.info.agent_id.clone()))
            .ok_or_else(|| err_session_not_found("SSH_SHELL_OPEN", &session_id))?;
        let channel = open_pty_shell(&handle_arc, &term, cols, rows)
            .await
            .map_err(|e| format_error("SSH_SHELL_OPEN", "CHANNEL_FAILED", &e, None))?;

        Ok(register_shell(
            &session_id,
            agent_id,
            term,
            cols,
            rows,
            channel,
            inactivity_ttl,
            max_buffer_size,
        ))
    }

    /// Send input (text, keystrokes, escape sequences) to an interactive shell.
    ///
    /// Sends raw input to the shell's PTY channel. Use this for:
    /// - Typing commands (append `\n` for Enter)
    /// - Sending control characters (`\x03` for Ctrl+C, `\x04` for Ctrl+D)
    /// - Sending escape sequences (`\x1b[A` for arrow up)
    async fn ssh_shell_write(
        &self,
        /// Shell ID returned from ssh_shell_open
        shell_id: String,
        /// Input to send to the shell (text, control chars, escape sequences). Append \n for Enter.
        input: String,
    ) -> Result<Text<String>, String> {
        let (channel_writer, last_activity) = SHELL_STORAGE
            .get_direct(&shell_id)
            .map(|shell| {
                (
                    Arc::clone(&shell.channel_writer),
                    Arc::clone(&shell.last_activity),
                )
            })
            .ok_or_else(|| {
                format_error(
                    "SSH_SHELL_WRITE",
                    "SHELL_NOT_FOUND",
                    "no active shell with the given ID",
                    Some(&shell_id),
                )
            })?;

        channel_writer
            .lock()
            .await
            .write(input.as_bytes())
            .await
            .map_err(|e| format_error("SSH_SHELL_WRITE", "WRITE_FAILED", &e, None))?;

        // Reset inactivity timer on write.
        *last_activity.lock().await = time::Instant::now();

        Ok(Text(render_shell_write_ok(&shell_id, input.len())))
    }

    /// Read accumulated output from an interactive shell.
    ///
    /// Returns all output buffered since the last read (when `clear=true`)
    /// or all output since shell open (when `clear=false`).
    ///
    /// **Recommended workflow:**
    /// 1. `ssh_shell_write` to send a command
    /// 2. Wait briefly (shell needs time to produce output)
    /// 3. `ssh_shell_read` with clear=true to get new output
    async fn ssh_shell_read(
        &self,
        /// Shell ID returned from ssh_shell_open
        shell_id: String,
        /// Clear the output buffer after reading (default: true). When true, only
        /// the bytes actually shown in this response are removed from the buffer
        /// (head-based pagination). The rest stays available for the next call.
        clear: Option<bool>,
        /// Maximum bytes to show (default: 16384, cap: 1048576). Content is
        /// rendered as the tail (most recent output).
        max_output_bytes: Option<usize>,
    ) -> Result<Text<String>, String> {
        let clear = clear.unwrap_or(true);
        let max_bytes = clamp_output_bytes(max_output_bytes);

        let (output_arc, status_rx, last_activity) = SHELL_STORAGE
            .get_direct(&shell_id)
            .map(|shell| {
                (
                    Arc::clone(&shell.output),
                    shell.status_rx.clone(),
                    Arc::clone(&shell.last_activity),
                )
            })
            .ok_or_else(|| {
                format_error(
                    "SSH_SHELL_READ",
                    "SHELL_NOT_FOUND",
                    "no active shell with the given ID",
                    Some(&shell_id),
                )
            })?;

        let nonce = generate_nonce();
        let status = *status_rx.borrow();
        let state = match status {
            ShellStatus::Open => ShellReadState::Open,
            ShellStatus::Closed => ShellReadState::Closed,
        };

        // Acquire the buffer lock in a narrow scope: either render + drain_head
        // (head-based pagination) or render without mutation (peek mode).
        let markdown = {
            let mut guard = output_arc.lock().await;
            let markdown =
                ShellReadBuilder::new(&shell_id, state, &guard, max_bytes, &nonce).build();
            if clear {
                // Remove exactly the bytes shown (head-based pagination).
                let shown = guard.len().min(max_bytes);
                guard.drain(..shown);
                // Release memory when capacity dwarfs remaining content.
                if guard.capacity() > guard.len().saturating_mul(4) {
                    guard.shrink_to_fit();
                }
            }
            markdown
        };

        // Reset inactivity timer on read.
        *last_activity.lock().await = time::Instant::now();

        Ok(Text(markdown))
    }

    /// Close an interactive shell session.
    ///
    /// Stops the background reader and closes the PTY channel.
    /// Any buffered output is discarded.
    async fn ssh_shell_close(
        &self,
        /// Shell ID to close
        shell_id: String,
    ) -> Result<Text<String>, String> {
        let shell = SHELL_STORAGE.unregister(&shell_id).ok_or_else(|| {
            format_error(
                "SSH_SHELL_CLOSE",
                "SHELL_NOT_FOUND",
                "no active shell with the given ID",
                Some(&shell_id),
            )
        })?;

        shell.cancel_token.cancel();
        let _ = shell.channel_writer.lock().await.close().await;

        info!("Closed interactive shell: {shell_id}");

        Ok(Text(render_shell_close_ok(&shell_id)))
    }

    /// Upload a local file to a remote path via SFTP.
    ///
    /// Starts an asynchronous file upload. Returns immediately with a
    /// `transfer_id` for progress tracking.
    ///
    /// **Workflow:**
    /// 1. `ssh_upload` -> get `transfer_id`
    /// 2. `ssh_get_transfer_progress(transfer_id, wait=true)` -> get result
    ///
    /// Streams the file in 32KB chunks with minimal memory usage.
    ///
    /// **Limits:** Up to 10 concurrent transfers per session.
    async fn ssh_upload(
        &self,
        /// Session ID returned from ssh_connect
        session_id: String,
        /// Local file path to upload (relative paths resolve to home directory)
        local_path: String,
        /// Remote destination path on the SSH server
        remote_path: String,
    ) -> Result<Text<String>, String> {
        let current_count = TRANSFER_STORAGE.count_by_session(&session_id);
        if current_count >= MAX_TRANSFERS_PER_SESSION {
            return Err(format_error(
                "SSH_UPLOAD",
                "MAX_TRANSFERS_EXCEEDED",
                "maximum transfers per session reached",
                Some(&format!("limit={MAX_TRANSFERS_PER_SESSION}")),
            ));
        }

        let (handle_arc, agent_id) = SESSION_STORAGE
            .get(&session_id)
            .map(|s| (Arc::clone(&s.handle), s.info.agent_id.clone()))
            .ok_or_else(|| err_session_not_found("SSH_UPLOAD", &session_id))?;

        let resolved_path = resolve_local_path(&local_path);
        let metadata = fs::metadata(&resolved_path).await.map_err(|e| {
            let reason = classify_transfer_error(
                &format!("access local file '{}'", resolved_path.display()),
                &e.to_string(),
            );
            format_error("SSH_UPLOAD", "LOCAL_FILE_ERROR", &reason, None)
        })?;

        if !metadata.is_file() {
            return Err(format_error(
                "SSH_UPLOAD",
                "LOCAL_NOT_FILE",
                "path is not a regular file",
                Some(&resolved_path.display().to_string()),
            ));
        }

        Ok(start_upload(
            session_id,
            agent_id,
            handle_arc,
            &resolved_path,
            remote_path,
            metadata.len(),
        ))
    }

    /// Download a remote file to a local path via SFTP.
    ///
    /// Starts an asynchronous file download. Returns immediately with a
    /// `transfer_id` for progress tracking.
    ///
    /// **Workflow:**
    /// 1. `ssh_download` -> get `transfer_id`
    /// 2. `ssh_get_transfer_progress(transfer_id, wait=true)` -> get result
    ///
    /// Streams the file in 32KB chunks with minimal memory usage.
    ///
    /// **Limits:** Up to 10 concurrent transfers per session.
    async fn ssh_download(
        &self,
        /// Session ID returned from ssh_connect
        session_id: String,
        /// Remote file path to download from the SSH server
        remote_path: String,
        /// Local destination path (relative paths resolve to home directory)
        local_path: String,
    ) -> Result<Text<String>, String> {
        let current_count = TRANSFER_STORAGE.count_by_session(&session_id);
        if current_count >= MAX_TRANSFERS_PER_SESSION {
            return Err(format_error(
                "SSH_DOWNLOAD",
                "MAX_TRANSFERS_EXCEEDED",
                "maximum transfers per session reached",
                Some(&format!("limit={MAX_TRANSFERS_PER_SESSION}")),
            ));
        }

        let (handle_arc, agent_id) = SESSION_STORAGE
            .get(&session_id)
            .map(|s| (Arc::clone(&s.handle), s.info.agent_id.clone()))
            .ok_or_else(|| err_session_not_found("SSH_DOWNLOAD", &session_id))?;
        let resolved_path = resolve_local_path(&local_path);

        let sftp = open_sftp_session(&handle_arc)
            .await
            .map_err(|e| format_error("SSH_DOWNLOAD", "SFTP_OPEN_FAILED", &e, None))?;
        let remote_metadata = sftp.metadata(&remote_path).await.map_err(|e| {
            let reason = classify_transfer_error(
                &format!("get remote file metadata for '{remote_path}'"),
                &e.to_string(),
            );
            format_error("SSH_DOWNLOAD", "REMOTE_METADATA_ERROR", &reason, None)
        })?;

        let total_bytes = remote_metadata.size.unwrap_or(0);

        Ok(start_download(
            session_id,
            agent_id,
            handle_arc,
            remote_path,
            &resolved_path,
            total_bytes,
        ))
    }

    /// Get the current progress of a file transfer.
    ///
    /// **Polling mode** (`wait=false`): Returns immediately with current progress.
    ///
    /// **Blocking mode** (`wait=true`): Waits until the transfer completes or timeout expires.
    ///
    /// **Status values:** `running`, `completed`, `failed`, `cancelled`
    async fn ssh_get_transfer_progress(
        &self,
        /// Transfer ID returned from ssh_upload or ssh_download
        transfer_id: String,
        /// If true, block until transfer completes or wait_timeout_secs expires
        wait: Option<bool>,
        /// Max seconds to wait when wait=true (default: 30, max: 300)
        wait_timeout_secs: Option<u64>,
    ) -> Result<Text<String>, String> {
        let wait = wait.unwrap_or(false);
        let wait_timeout = Duration::from_secs(wait_timeout_secs.unwrap_or(30).min(300));

        let (status_rx, bytes_transferred, total_bytes_arc, error, transfer_info) =
            TRANSFER_STORAGE
                .get_direct(&transfer_id)
                .map(|t| {
                    (
                        t.status_rx.clone(),
                        Arc::clone(&t.bytes_transferred),
                        Arc::clone(&t.total_bytes),
                        Arc::clone(&t.error),
                        t.info.clone(),
                    )
                })
                .ok_or_else(|| {
                    format_error(
                        "SSH_GET_TRANSFER_PROGRESS",
                        "TRANSFER_NOT_FOUND",
                        "no transfer with the given ID",
                        Some(&transfer_id),
                    )
                })?;

        if wait {
            let mut rx = status_rx.clone();
            let _ = timeout(wait_timeout, wait_for_transfer_completion(&mut rx)).await;
        }

        build_transfer_progress_response(
            transfer_id,
            &status_rx,
            &bytes_transferred,
            &total_bytes_arc,
            &error,
            &transfer_info,
        )
        .await
    }
}

/// Background reader that exclusively owns the channel read half.
///
/// Reads from the channel without any mutex contention, allowing
/// concurrent writes through the separate write half.
/// Append data to the shell buffer, truncating oldest bytes if over the max size.
async fn append_shell_output(output: &Mutex<Vec<u8>>, data: &[u8], max_buffer_size: &AtomicU64) {
    let mut buf = output.lock().await;
    buf.extend_from_slice(data);

    let max_size = max_buffer_size.load(Ordering::Relaxed);
    let target = usize::try_from(max_size).unwrap_or(usize::MAX);
    if max_size > 0 && buf.len() > target {
        let excess = buf.len().saturating_sub(target);
        buf.drain(..excess);
    }
}

async fn shell_reader(
    mut read_half: russh::ChannelReadHalf,
    output: Arc<Mutex<Vec<u8>>>,
    cancel_token: CancellationToken,
    status_tx: watch::Sender<ShellStatus>,
    max_buffer_size: Arc<AtomicU64>,
    last_activity: Arc<Mutex<time::Instant>>,
) {
    use russh::ChannelMsg;

    loop {
        tokio::select! {
            biased;

            () = cancel_token.cancelled() => break,

            msg = read_half.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                        append_shell_output(&output, &data, &max_buffer_size).await;
                        *last_activity.lock().await = time::Instant::now();
                    }
                    Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
                    Some(_) => {}
                }
            }
        }
    }

    let _ = status_tx.send(ShellStatus::Closed);
}
