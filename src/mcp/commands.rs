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

use std::mem;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures::future::join_all;
use poem_mcpserver::{Tools, content::Text, tool::StructuredContent};
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
};
#[cfg(feature = "port_forward")]
use super::forward::setup_port_forwarding;
use super::message::builder::{
    AgentDisconnectMessageBuilder, ConnectMessageBuilder, DownloadMessageBuilder,
    ExecuteMessageBuilder, ShellOpenMessageBuilder, TransferProgressMessageBuilder,
    UploadMessageBuilder,
};
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
    AgentDisconnectResponse, AsyncCommandInfo, AsyncCommandStatus, PortForwardingResponse,
    SessionInfo, SessionListResponse, ShellInfo, ShellStatus, SshAsyncOutputResponse,
    SshCancelCommandResponse, SshCommandResponse, SshConnectResponse, SshDownloadResponse,
    SshExecuteResponse, SshListCommandsResponse, SshShellCloseResponse, SshShellOpenResponse,
    SshShellReadResponse, SshTransferProgressResponse, SshUploadResponse,
};

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
/// Returns the reuse response if the session is healthy, or `None` if
/// dead or not found.
async fn try_reuse_session(sid: &str) -> Option<StructuredContent<SshConnectResponse>> {
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
) -> Option<StructuredContent<SshConnectResponse>> {
    match result {
        Ok(response) if !response.timed_out && response.exit_code == 0 => {
            SESSION_STORAGE.update_health(sid, now, true);
            info!("Reusing healthy session {sid}");
            let reuse_agent_id = session_ref.info.agent_id.clone();
            let message =
                ConnectMessageBuilder::new(sid, &session_ref.info.username, &session_ref.info.host)
                    .with_agent_id(reuse_agent_id.as_deref())
                    .with_name(session_ref.info.name.as_deref())
                    .reused(true)
                    .build();
            Some(StructuredContent(SshConnectResponse {
                session_id: sid.to_string(),
                agent_id: reuse_agent_id,
                message,
                authenticated: true,
                retry_attempts: 0,
            }))
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

/// Get a session handle and agent ID, or return a formatted error.
fn get_session_handle_and_agent(
    session_id: &str,
) -> Result<(Arc<SshHandle>, Option<String>), String> {
    SESSION_STORAGE
        .get(session_id)
        .map(|s| (Arc::clone(&s.handle), s.info.agent_id.clone()))
        .ok_or_else(|| format!("No active SSH session with ID: {session_id}"))
}

/// Compute transfer progress as a percentage (0-100).
fn compute_progress_percent(transferred: u64, total: u64) -> u8 {
    if total > 0 {
        let percent = (u128::from(transferred) * 100) / u128::from(total);
        u8::try_from(percent.min(100)).unwrap_or(100)
    } else {
        0
    }
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

/// Build the output response for a command.
async fn build_command_output_response(
    command_id: String,
    status_rx: &watch::Receiver<AsyncCommandStatus>,
    output: &Mutex<OutputBuffer>,
    exit_code: &Mutex<Option<i32>>,
    error: &Mutex<Option<String>>,
    timed_out: &AtomicBool,
) -> Result<StructuredContent<SshAsyncOutputResponse>, String> {
    let status = *status_rx.borrow();
    let output_buf = output.lock().await;
    let exit_code_val = *exit_code.lock().await;
    let error_val = error.lock().await.clone();
    let timed_out_val = timed_out.load(Ordering::SeqCst);

    Ok(StructuredContent(SshAsyncOutputResponse {
        command_id,
        status,
        stdout: String::from_utf8_lossy(&output_buf.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output_buf.stderr).into_owned(),
        exit_code: exit_code_val,
        error: error_val,
        timed_out: timed_out_val,
    }))
}

/// Build the agent disconnect response.
fn build_agent_disconnect_response(
    agent_id: &str,
    sessions_disconnected: usize,
    commands_cancelled: usize,
) -> StructuredContent<AgentDisconnectResponse> {
    let message = AgentDisconnectMessageBuilder::new(agent_id)
        .with_sessions_disconnected(sessions_disconnected)
        .with_commands_cancelled(commands_cancelled)
        .build();

    StructuredContent(AgentDisconnectResponse {
        agent_id: agent_id.to_string(),
        sessions_disconnected,
        commands_cancelled,
        message,
    })
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
) -> StructuredContent<SessionListResponse> {
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

    let session_infos: Vec<SessionInfo> =
        healthy_sessions.into_iter().map(|(_, info)| info).collect();
    let count = session_infos.len();

    StructuredContent(SessionListResponse {
        sessions: session_infos,
        count,
    })
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

/// Spawn the shell reader and register the shell in storage.
fn spawn_and_register_shell(shell_id: &str, shell_info: ShellInfo, channel: Channel<Msg>) {
    let (status_tx, status_rx) = watch::channel(ShellStatus::Open);
    let output = Arc::new(Mutex::new(Vec::with_capacity(4096)));
    let cancel_token = CancellationToken::new();

    let (read_half, write_half) = channel.split();
    let channel_writer = Arc::new(Mutex::new(ChannelWriter::new(write_half)));

    let reader_output = Arc::clone(&output);
    let reader_cancel = cancel_token.clone();
    let reader_status_tx = status_tx.clone();

    tokio::spawn(async move {
        shell_reader(read_half, reader_output, reader_cancel, reader_status_tx).await;
    });

    SHELL_STORAGE.register(
        shell_id.to_string(),
        RunningShell {
            info: shell_info,
            cancel_token,
            output,
            channel_writer,
            status_tx,
            status_rx,
        },
    );
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
) -> StructuredContent<SshShellOpenResponse> {
    let shell_id = Uuid::new_v4().to_string();

    let shell_info = ShellInfo {
        shell_id: shell_id.clone(),
        session_id: session_id.to_string(),
        term_type: term.clone(),
        cols,
        rows,
        opened_at: chrono::Utc::now().to_rfc3339(),
    };

    spawn_and_register_shell(&shell_id, shell_info, channel);

    info!(
        "Opened interactive shell {shell_id} on session {session_id} (term={term}, {cols}x{rows})"
    );

    let message = ShellOpenMessageBuilder::new(&shell_id, session_id, &term, cols, rows)
        .with_agent_id(agent_id.as_deref())
        .build();

    StructuredContent(SshShellOpenResponse {
        shell_id,
        session_id: session_id.to_string(),
        agent_id,
        term_type: term,
        message,
    })
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
) -> StructuredContent<SshUploadResponse> {
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

    build_upload_response(
        transfer_id,
        session_id,
        agent_id,
        local_path,
        remote_path,
        total_bytes,
    )
}

/// Build the upload response with message.
fn build_upload_response(
    transfer_id: String,
    session_id: String,
    agent_id: Option<String>,
    local_path: String,
    remote_path: String,
    total_bytes: u64,
) -> StructuredContent<SshUploadResponse> {
    let message = UploadMessageBuilder::new(
        &transfer_id,
        &session_id,
        &local_path,
        &remote_path,
        total_bytes,
    )
    .with_agent_id(agent_id.as_deref())
    .build();

    StructuredContent(SshUploadResponse {
        transfer_id,
        session_id,
        agent_id,
        local_path,
        remote_path,
        total_bytes,
        message,
    })
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
) -> StructuredContent<SshDownloadResponse> {
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

    build_download_response(
        transfer_id,
        session_id,
        agent_id,
        remote_path,
        local_path,
        total_bytes,
    )
}

/// Build the download response with message.
fn build_download_response(
    transfer_id: String,
    session_id: String,
    agent_id: Option<String>,
    remote_path: String,
    local_path: String,
    total_bytes: u64,
) -> StructuredContent<SshDownloadResponse> {
    let message = DownloadMessageBuilder::new(
        &transfer_id,
        &session_id,
        &remote_path,
        &local_path,
        total_bytes,
    )
    .with_agent_id(agent_id.as_deref())
    .build();

    StructuredContent(SshDownloadResponse {
        transfer_id,
        session_id,
        agent_id,
        remote_path,
        local_path,
        total_bytes,
        message,
    })
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
) -> Result<StructuredContent<SshTransferProgressResponse>, String> {
    let status = *status_rx.borrow();
    let transferred = bytes_transferred.load(Ordering::SeqCst);
    let total = total_bytes_arc.load(Ordering::SeqCst);
    let error_val = error.lock().await.clone();
    let progress_percent = compute_progress_percent(transferred, total);

    let direction_str = info.direction.to_string();
    let status_str = status.to_string();

    let message = TransferProgressMessageBuilder::new(
        &transfer_id,
        &direction_str,
        &status_str,
        transferred,
        total,
    )
    .build();

    Ok(StructuredContent(SshTransferProgressResponse {
        transfer_id,
        session_id: info.session_id.clone(),
        direction: direction_str,
        local_path: info.local_path.clone(),
        remote_path: info.remote_path.clone(),
        status: status_str,
        bytes_transferred: transferred,
        total_bytes: total,
        progress_percent,
        error: error_val,
        message,
    }))
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
) -> Result<StructuredContent<SshConnectResponse>, String> {
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

    let (handle, retries) =
        attempt_connection(&address, &username, password, key_path, &cfg).await?;

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
) -> StructuredContent<SshConnectResponse> {
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

    let message = ConnectMessageBuilder::new(&new_session_id, username, address)
        .with_agent_id(agent_id.as_deref())
        .with_name(name)
        .with_retry_attempts(retry_attempts)
        .with_persistent(persistent)
        .build();

    StructuredContent(SshConnectResponse {
        session_id: new_session_id,
        agent_id,
        message,
        authenticated: true,
        retry_attempts,
    })
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
) -> StructuredContent<SshExecuteResponse> {
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

    let message = ExecuteMessageBuilder::new(&command_id, &session_id, &command)
        .with_agent_id(agent_id.as_deref())
        .build();

    StructuredContent(SshExecuteResponse {
        command_id,
        session_id,
        agent_id,
        command,
        started_at,
        message,
    })
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
) -> Result<StructuredContent<PortForwardingResponse>, String> {
    info!(
        "Setting up port forwarding from local port {} to {}:{} using session {}",
        local_port, remote_address, remote_port, session_id
    );

    let handle_arc = SESSION_STORAGE
        .get(&session_id)
        .map(|s| Arc::clone(&s.handle))
        .ok_or_else(|| format!("No active SSH session with ID: {session_id}"))?;

    setup_port_forwarding(handle_arc, local_port, &remote_address, remote_port)
        .await
        .map(|local_addr| {
            StructuredContent(PortForwardingResponse {
                local_address: local_addr.to_string(),
                remote_address: format!("{remote_address}:{remote_port}"),
                active: true,
            })
        })
        .map_err(|e| {
            error!("Port forwarding setup failed: {e}");
            e
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
    ) -> Result<StructuredContent<SshConnectResponse>, String> {
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
            Ok(Text(format!(
                "Session {session_id} disconnected successfully"
            )))
        } else {
            Err(format!("No active SSH session with ID: {session_id}"))
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
    ) -> StructuredContent<SessionListResponse> {
        let health_timeout = Duration::from_secs(5);

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
        process_health_results(results)
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
    ) -> Result<StructuredContent<PortForwardingResponse>, String> {
        #[cfg(feature = "port_forward")]
        return forward_impl(session_id, local_port, remote_address, remote_port).await;

        #[cfg(not(feature = "port_forward"))]
        Err(
            "Port forwarding feature is not enabled. Rebuild with --features port_forward"
                .to_string(),
        )
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
    ) -> Result<StructuredContent<SshExecuteResponse>, String> {
        let cmd_timeout = resolve_command_timeout(timeout_secs);

        let running_count = COMMAND_STORAGE.count_running_by_session(&session_id);
        if running_count >= MAX_ASYNC_COMMANDS_PER_SESSION {
            return Err(format!(
                "Maximum running async commands per session reached ({MAX_ASYNC_COMMANDS_PER_SESSION}). Cancel or wait for existing commands to complete."
            ));
        }

        let (handle_arc, agent_id) = get_session_handle_and_agent(&session_id)?;

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
    ) -> Result<StructuredContent<SshAsyncOutputResponse>, String> {
        let wait = wait.unwrap_or(false);
        let wait_timeout = Duration::from_secs(wait_timeout_secs.unwrap_or(30).min(300));

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
            .ok_or_else(|| format!("No async command with ID: {command_id}"))?;

        if wait {
            let mut rx = status_rx.clone();
            let _ = timeout(wait_timeout, wait_for_command_completion(&mut rx)).await;
        }

        // Mark output as read so the cleanup task can release immediately
        output_read.store(true, Ordering::SeqCst);

        build_command_output_response(
            command_id, &status_rx, &output, &exit_code, &error, &timed_out,
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
    ) -> StructuredContent<SshListCommandsResponse> {
        let status_filter: Option<AsyncCommandStatus> = status.and_then(|s| match s.as_str() {
            "running" => Some(AsyncCommandStatus::Running),
            "completed" => Some(AsyncCommandStatus::Completed),
            "cancelled" => Some(AsyncCommandStatus::Cancelled),
            "failed" => Some(AsyncCommandStatus::Failed),
            _ => None,
        });

        let filtered = COMMAND_STORAGE.list_filtered(session_id.as_deref(), status_filter);
        let count = filtered.len();

        StructuredContent(SshListCommandsResponse {
            commands: filtered,
            count,
        })
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
    ) -> Result<StructuredContent<SshCancelCommandResponse>, String> {
        let (cancel_token, output, status_rx) = get_running_command(&command_id)?;

        cancel_token.cancel();

        let mut rx = status_rx;
        let _ = timeout(Duration::from_secs(2), wait_for_command_completion(&mut rx)).await;

        let output_buf = output.lock().await;
        info!("Cancelled async command: {command_id}");

        Ok(StructuredContent(SshCancelCommandResponse {
            command_id,
            cancelled: true,
            message: "Command cancelled successfully".to_string(),
            stdout: String::from_utf8_lossy(&output_buf.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output_buf.stderr).into_owned(),
        }))
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
    ) -> Result<StructuredContent<AgentDisconnectResponse>, String> {
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
    ) -> Result<StructuredContent<SshShellOpenResponse>, String> {
        let term = term.unwrap_or_else(|| "xterm".to_string());
        let cols = cols.unwrap_or(80);
        let rows = rows.unwrap_or(24);

        let current_count = SHELL_STORAGE.count_by_session(&session_id);
        if current_count >= MAX_SHELLS_PER_SESSION {
            return Err(format!(
                "Maximum shells per session reached ({MAX_SHELLS_PER_SESSION}). Close existing shells first."
            ));
        }

        let (handle_arc, agent_id) = get_session_handle_and_agent(&session_id)?;
        let channel = open_pty_shell(&handle_arc, &term, cols, rows).await?;

        Ok(register_shell(
            &session_id,
            agent_id,
            term,
            cols,
            rows,
            channel,
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
        let channel_writer = SHELL_STORAGE
            .get_direct(&shell_id)
            .map(|shell| Arc::clone(&shell.channel_writer))
            .ok_or_else(|| format!("No active shell with ID: {shell_id}"))?;

        channel_writer.lock().await.write(input.as_bytes()).await?;

        Ok(Text(format!(
            "Sent {} bytes to shell '{shell_id}'",
            input.len(),
        )))
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
        /// Clear the output buffer after reading (default: true). Set to false to peek without consuming.
        clear: Option<bool>,
    ) -> Result<StructuredContent<SshShellReadResponse>, String> {
        let clear = clear.unwrap_or(true);

        let (output_arc, status_rx) = SHELL_STORAGE
            .get_direct(&shell_id)
            .map(|shell| (Arc::clone(&shell.output), shell.status_rx.clone()))
            .ok_or_else(|| format!("No active shell with ID: {shell_id}"))?;

        let data = if clear {
            let data = mem::take(&mut *output_arc.lock().await);
            String::from_utf8_lossy(&data).into_owned()
        } else {
            let buf = output_arc.lock().await;
            String::from_utf8_lossy(&buf).into_owned()
        };

        let status = *status_rx.borrow();

        Ok(StructuredContent(SshShellReadResponse {
            shell_id,
            data,
            status,
        }))
    }

    /// Close an interactive shell session.
    ///
    /// Stops the background reader and closes the PTY channel.
    /// Any buffered output is discarded.
    async fn ssh_shell_close(
        &self,
        /// Shell ID to close
        shell_id: String,
    ) -> Result<StructuredContent<SshShellCloseResponse>, String> {
        let shell = SHELL_STORAGE
            .unregister(&shell_id)
            .ok_or_else(|| format!("No active shell with ID: {shell_id}"))?;

        shell.cancel_token.cancel();
        let _ = shell.channel_writer.lock().await.close().await;

        info!("Closed interactive shell: {shell_id}");

        Ok(StructuredContent(SshShellCloseResponse {
            shell_id,
            closed: true,
            message: "Shell closed successfully".to_string(),
        }))
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
    ) -> Result<StructuredContent<SshUploadResponse>, String> {
        let current_count = TRANSFER_STORAGE.count_by_session(&session_id);
        if current_count >= MAX_TRANSFERS_PER_SESSION {
            return Err(format!(
                "Maximum transfers per session reached ({MAX_TRANSFERS_PER_SESSION}). Wait for existing transfers to complete."
            ));
        }

        let (handle_arc, agent_id) = get_session_handle_and_agent(&session_id)?;

        let resolved_path = resolve_local_path(&local_path);
        let metadata = fs::metadata(&resolved_path).await.map_err(|e| {
            classify_transfer_error(
                &format!("access local file '{}'", resolved_path.display()),
                &e.to_string(),
            )
        })?;

        if !metadata.is_file() {
            return Err(format!(
                "'{}' is not a regular file",
                resolved_path.display()
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
    ) -> Result<StructuredContent<SshDownloadResponse>, String> {
        let current_count = TRANSFER_STORAGE.count_by_session(&session_id);
        if current_count >= MAX_TRANSFERS_PER_SESSION {
            return Err(format!(
                "Maximum transfers per session reached ({MAX_TRANSFERS_PER_SESSION}). Wait for existing transfers to complete."
            ));
        }

        let (handle_arc, agent_id) = get_session_handle_and_agent(&session_id)?;
        let resolved_path = resolve_local_path(&local_path);

        let sftp = open_sftp_session(&handle_arc).await?;
        let remote_metadata = sftp.metadata(&remote_path).await.map_err(|e| {
            classify_transfer_error(
                &format!("get remote file metadata for '{remote_path}'"),
                &e.to_string(),
            )
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
    ) -> Result<StructuredContent<SshTransferProgressResponse>, String> {
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
                .ok_or_else(|| format!("No transfer with ID: {transfer_id}"))?;

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
async fn shell_reader(
    mut read_half: russh::ChannelReadHalf,
    output: Arc<Mutex<Vec<u8>>>,
    cancel_token: CancellationToken,
    status_tx: watch::Sender<ShellStatus>,
) {
    use russh::ChannelMsg;

    loop {
        tokio::select! {
            biased;

            () = cancel_token.cancelled() => {
                break;
            }

            msg = read_half.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                        let mut buf = output.lock().await;
                        buf.extend_from_slice(&data);
                    }
                    Some(ChannelMsg::Eof | ChannelMsg::Close) | None => {
                        break;
                    }
                    Some(_) => {}
                }
            }
        }
    }

    let _ = status_tx.send(ShellStatus::Closed);
}
