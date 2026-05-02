//! Free helper functions ported verbatim from `commands_legacy.rs.txt`
//! (the parked v2.0.1 monolithic file).
//!
//! Each helper is `pub(crate)` so the per-domain tool modules
//! (`execute`, `shell`, `sftp`, `forward`) can reuse the same lifecycle
//! primitives that `commands.rs` had as private fns. The `connection`
//! module keeps its own copies of session-cleanup helpers (a small
//! duplication that tightly scopes the closure of imports).
//!
//! All return types previously wrapped in `poem_mcpserver::content::Text<String>`
//! now return a plain `String`. The rmcp tool layer wraps those into
//! `CallToolResult::success(vec![Content::text(s)])` at the call site.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use russh::client::Msg;
use russh::{Channel, Disconnect};
use tokio::sync::{Mutex, Semaphore, watch};
use tokio::time::{self};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use super::super::async_command::{OutputBuffer, RunningCommand};
use super::super::client::{
    execute_ssh_command, execute_ssh_command_async, execute_ssh_command_async_pty,
};
use arc_swap::ArcSwap;
use super::super::config::{
    resolve_command_cleanup_ttl, resolve_list_max_items_cap, resolve_list_max_items_default,
    resolve_output_default_bytes, resolve_output_max_bytes_cap, resolve_transfer_cleanup_ttl,
};
use super::super::message::builder::{
    ExecuteStartedBuilder, GetCommandOutputBuilder, GetCommandOutputState, ListSessionsBuilder,
    ShellOpenBuilder, TransferProgressBuilder, TransferProgressState, TransferStartDirection,
    TransferStartedBuilder, render_disconnect_agent,
};
use super::super::message::helpers::{format_error, generate_nonce};
use super::super::session::SshClientHandler;
use super::super::sftp::{sftp_download_streaming, sftp_upload_streaming};
use super::super::shell::{ChannelWriter, RunningShell};
use super::super::storage::command::COMMAND_STORAGE;
use super::super::storage::session::SESSION_STORAGE;
use super::super::storage::shell::SHELL_STORAGE;
use super::super::storage::traits::{
    CommandStorage, SessionStorage, ShellStorage, TransferStorage,
};
use super::super::storage::transfer::TRANSFER_STORAGE;
use super::super::transfer::{RunningTransfer, TransferDirection, TransferInfo, TransferStatus};
use super::super::types::{
    AsyncCommandInfo, AsyncCommandStatus, SessionInfo, ShellInfo, ShellStatus, SshCommandResponse,
};

/// Type alias for the SSH client handle used throughout legacy helpers.
pub(crate) type SshHandle = russh::client::Handle<SshClientHandler>;

// ---------------------------------------------------------------------------
// Bounds & validation
// ---------------------------------------------------------------------------

/// Clamp a caller-provided `max_output_bytes` (caller -> env -> default,
/// then capped to [`resolve_output_max_bytes_cap`]).
pub(crate) fn clamp_output_bytes(requested: Option<usize>) -> usize {
    let default = resolve_output_default_bytes();
    let cap = resolve_output_max_bytes_cap();
    requested.unwrap_or(default).min(cap)
}

/// Clamp a caller-provided `max_items` for list tools.
pub(crate) fn clamp_list_items(requested: Option<usize>) -> usize {
    let default = resolve_list_max_items_default();
    let cap = resolve_list_max_items_cap();
    requested.unwrap_or(default).clamp(1, cap)
}

/// Standardized `SESSION_NOT_FOUND` error for any tool referencing a session.
pub(crate) fn err_session_not_found(tool: &str, session_id: &str) -> String {
    format_error(
        tool,
        "SESSION_NOT_FOUND",
        "no active SSH session with the given ID",
        Some(session_id),
    )
}

// ---------------------------------------------------------------------------
// Session cleanup primitives
// ---------------------------------------------------------------------------

/// Cancel all in-flight transfers for a session.
pub(crate) fn cancel_session_transfers(session_id: &str) {
    let transfer_ids = TRANSFER_STORAGE.list_by_session(session_id);
    for xfer_id in &transfer_ids {
        if let Some(xfer) = TRANSFER_STORAGE.unregister(xfer_id) {
            xfer.cancel_token.cancel();
        }
    }
}

/// Cancel all async commands for a session and return the count cancelled.
pub(crate) fn cancel_session_commands(session_id: &str) -> usize {
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

// ---------------------------------------------------------------------------
// Watch-receiver waits
// ---------------------------------------------------------------------------

/// Wait for a watch receiver to leave the `Running` command status.
pub(crate) async fn wait_for_command_completion(rx: &mut watch::Receiver<AsyncCommandStatus>) {
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
pub(crate) async fn wait_for_transfer_completion(rx: &mut watch::Receiver<TransferStatus>) {
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

// ---------------------------------------------------------------------------
// Async command lifecycle
// ---------------------------------------------------------------------------

/// Create a `RunningCommand` with lock-free shared state.
fn create_running_command(
    command_id: &str,
    session_id: &str,
    command: &str,
    started_at: &str,
) -> RunningCommand {
    RunningCommand::new(AsyncCommandInfo {
        command_id: command_id.to_string(),
        session_id: session_id.to_string(),
        command: command.to_string(),
        status: AsyncCommandStatus::Running,
        started_at: started_at.to_string(),
    })
}

/// Spawn the background command execution task.
fn spawn_command_task(
    use_pty: bool,
    handle_arc: Arc<SshHandle>,
    command_text: String,
    cmd_timeout: Duration,
    command: Arc<RunningCommand>,
    channel_permits: Arc<Semaphore>,
) {
    tokio::spawn(async move {
        // Hold permit for the entire channel lifecycle.
        let Ok(_permit) = channel_permits.acquire_owned().await else {
            let _ = command.error.set(String::from(
                "Failed to acquire channel permit (session semaphore closed)",
            ));
            let _ = command.status_tx.send(AsyncCommandStatus::Failed);
            return;
        };
        if use_pty {
            execute_ssh_command_async_pty(handle_arc, command_text, cmd_timeout, command).await;
        } else {
            execute_ssh_command_async(handle_arc, command_text, cmd_timeout, command).await;
        }
        // Permit dropped here -> slot freed.
    });
}

/// Spawn the cleanup task that removes a command from storage after completion.
fn spawn_cleanup_task(
    command_id: String,
    cleanup_rx: watch::Receiver<AsyncCommandStatus>,
    output_read: Arc<AtomicBool>,
) {
    /// Minimum delay between marking a command as read and removing it from
    /// storage. Lets the caller issue a follow-up `ssh_list_commands` or a
    /// double-poll without seeing `COMMAND_NOT_FOUND` immediately.
    const POST_READ_GRACE: Duration = Duration::from_millis(1_000);
    let ttl = resolve_command_cleanup_ttl();
    tokio::spawn(async move {
        let mut rx = cleanup_rx;
        wait_for_command_completion(&mut rx).await;

        if !output_read.load(Ordering::SeqCst) {
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

        time::sleep(POST_READ_GRACE).await;

        COMMAND_STORAGE.unregister(&command_id);
        info!("Cleanup: removed completed command {command_id}");
    });
}

/// Build a shallow clone of a `RunningCommand` that shares all underlying
/// state (Arc/`broadcast::Sender`/watch/`CancellationToken`) with the
/// original. Used when both the storage entry and the background spawn task
/// must reference the same command without locks.
fn shallow_clone_command(cmd: &RunningCommand) -> RunningCommand {
    RunningCommand {
        info: cmd.info.clone(),
        cancel_token: cmd.cancel_token.clone(),
        status_rx: cmd.status_rx.clone(),
        status_tx: cmd.status_tx.clone(),
        output_history: Arc::clone(&cmd.output_history),
        output_tx: cmd.output_tx.clone(),
        exit_code: Arc::clone(&cmd.exit_code),
        error: Arc::clone(&cmd.error),
        timed_out: Arc::clone(&cmd.timed_out),
        output_read: Arc::clone(&cmd.output_read),
    }
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
    channel_permits: Arc<Semaphore>,
) {
    let task_handle = Arc::new(shallow_clone_command(&running_cmd));

    COMMAND_STORAGE.register(command_id.to_string(), running_cmd);
    info!("Starting async command {command_id} on session {session_id}: {command}");

    spawn_command_task(
        use_pty,
        handle_arc,
        command.to_string(),
        cmd_timeout,
        task_handle,
        channel_permits,
    );
}

/// Register and spawn an async command, returning the response markdown.
pub(crate) fn register_and_spawn_command(
    session_id: String,
    command: String,
    agent_id: Option<String>,
    handle_arc: Arc<SshHandle>,
    cmd_timeout: Duration,
    use_pty: bool,
    channel_permits: Arc<Semaphore>,
) -> String {
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
        channel_permits,
    );
    spawn_cleanup_task(command_id.clone(), cleanup_rx, output_read);

    ExecuteStartedBuilder::new(&command_id, &session_id)
        .with_agent_id(agent_id.as_deref())
        .build()
}

/// Build the output response markdown for a command.
///
/// Reads the lock-free state without taking any mutex. The output snapshot
/// comes from `ArcSwap::load_full`, while terminal `exit_code` / `error`
/// values come from `OnceCell::get`.
pub(crate) fn build_command_output_response(
    command_id: String,
    status_rx: &watch::Receiver<AsyncCommandStatus>,
    output_history: &ArcSwap<OutputBuffer>,
    exit_code: &tokio::sync::OnceCell<i32>,
    error: &tokio::sync::OnceCell<String>,
    timed_out: &AtomicBool,
    max_output_bytes: usize,
) -> String {
    let status = *status_rx.borrow();
    let timed_out_val = timed_out.load(Ordering::SeqCst);
    let exit_code_val: Option<i32> = exit_code.get().copied();
    let error_val: Option<String> = error.get().cloned();

    if matches!(status, AsyncCommandStatus::Failed) {
        let reason = error_val.as_deref().unwrap_or("command failed");
        return format_error("SSH_GET_COMMAND_OUTPUT", "COMMAND_FAILED", reason, None);
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
    let snapshot = output_history.load_full();
    GetCommandOutputBuilder::new(
        &command_id,
        state,
        &snapshot.stdout,
        &snapshot.stderr,
        max_output_bytes,
        &nonce,
    )
    .build()
}

/// State needed to cancel a running command and retrieve its output.
pub(crate) type CancelState = (
    CancellationToken,
    Arc<ArcSwap<OutputBuffer>>,
    watch::Receiver<AsyncCommandStatus>,
);

/// Get a running command's cancellation handles, or error if not running.
pub(crate) fn get_running_command(command_id: &str) -> Result<CancelState, String> {
    COMMAND_STORAGE
        .get_direct(command_id)
        .map(|cmd| {
            let current_status = *cmd.status_rx.borrow();
            (
                current_status,
                cmd.cancel_token.clone(),
                Arc::clone(&cmd.output_history),
                cmd.status_rx.clone(),
            )
        })
        .ok_or_else(|| format!("No async command with ID: {command_id}"))
        .and_then(|(current_status, cancel_token, output_history, status_rx)| {
            if current_status == AsyncCommandStatus::Running {
                Ok((cancel_token, output_history, status_rx))
            } else {
                Err(format!("Command is not running (status: {current_status})"))
            }
        })
}

// ---------------------------------------------------------------------------
// Agent disconnect
// ---------------------------------------------------------------------------

/// Build the agent disconnect response markdown.
pub(crate) fn build_agent_disconnect_response(
    agent_id: &str,
    sessions_disconnected: usize,
    commands_cancelled: usize,
) -> String {
    render_disconnect_agent(agent_id, sessions_disconnected, commands_cancelled)
}

/// Cleanup all sessions for an agent and return total commands cancelled.
pub(crate) async fn cleanup_agent_sessions(session_ids: &[String]) -> usize {
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

// ---------------------------------------------------------------------------
// Health-check classification (`ssh_list_sessions`)
// ---------------------------------------------------------------------------

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
            Ok(_) | Err(_) => {
                info.last_health_check = Some(now);
                info.healthy = Some(false);
                dead_session_ids.push(session_id);
            }
        }
    }

    (healthy_sessions, dead_session_ids)
}

/// Process health check results and update storage, returning the
/// `SSH_LIST_SESSIONS` markdown.
pub(crate) fn process_health_results(
    results: Vec<(
        String,
        SessionInfo,
        String,
        Result<SshCommandResponse, String>,
    )>,
    max_items: usize,
) -> String {
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
    session_infos.sort_by(|a, b| a.connected_at.cmp(&b.connected_at));
    let total = session_infos.len();
    session_infos.truncate(max_items);
    ListSessionsBuilder::new(&session_infos, total).build()
}

/// Convenience wrapper used by `ssh_list_sessions`: run a `echo 1` health
/// probe with a 5-second timeout against `handle`.
pub(crate) async fn health_probe(handle: &Arc<SshHandle>) -> Result<SshCommandResponse, String> {
    let health_timeout = Duration::from_secs(5);
    execute_ssh_command(handle, "echo 1", health_timeout).await
}

// ---------------------------------------------------------------------------
// Shell lifecycle
// ---------------------------------------------------------------------------

/// Append data to the shell buffer, truncating oldest bytes if over the max
/// size. Bounded so long sessions don't unbounded-grow the heap.
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

/// Background reader that exclusively owns the channel read half. Reads
/// from the channel without any mutex contention, allowing concurrent
/// writes through the separate write half.
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

/// Auto-close a shell after the configured inactivity TTL.
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

/// Register a new shell in storage and return the response markdown.
#[expect(
    clippy::too_many_arguments,
    reason = "shell registration requires all PTY parameters plus session linkage"
)]
pub(crate) fn register_shell(
    session_id: &str,
    agent_id: Option<String>,
    term: String,
    cols: u32,
    rows: u32,
    channel: Channel<Msg>,
    inactivity_ttl: Duration,
    max_buffer_size: u64,
) -> String {
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

    ShellOpenBuilder::new(&shell_id, session_id, &term, cols, rows)
        .with_agent_id(agent_id.as_deref())
        .build()
}

// ---------------------------------------------------------------------------
// SFTP transfer lifecycle
// ---------------------------------------------------------------------------

/// Shared state returned by transfer registration.
struct TransferSharedState {
    cancel_token: CancellationToken,
    bytes_transferred: Arc<AtomicU64>,
    status_tx: watch::Sender<TransferStatus>,
    error: Arc<Mutex<Option<String>>>,
}

/// Create shared state, register a transfer, and return handles for the task.
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
            status_rx: status_rx.clone(),
            status_tx: status_tx.clone(),
            error: Arc::clone(&error),
        },
    );

    spawn_transfer_cleanup_task(transfer_id.to_string(), status_rx);

    TransferSharedState {
        cancel_token,
        bytes_transferred,
        status_tx,
        error,
    }
}

/// Remove a terminated transfer from storage after `SSH_TRANSFER_CLEANUP_TTL`.
fn spawn_transfer_cleanup_task(
    transfer_id: String,
    mut status_rx: watch::Receiver<TransferStatus>,
) {
    let ttl = resolve_transfer_cleanup_ttl();
    tokio::spawn(async move {
        wait_for_transfer_completion(&mut status_rx).await;
        time::sleep(ttl).await;
        if TRANSFER_STORAGE.unregister(&transfer_id).is_some() {
            info!("Cleanup: removed terminal transfer {transfer_id} (TTL expired)");
        }
    });
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

/// Start an upload transfer and return the response markdown.
pub(crate) fn start_upload(
    session_id: String,
    agent_id: Option<String>,
    handle_arc: Arc<SshHandle>,
    resolved_path: &Path,
    remote_path: String,
    total_bytes: u64,
) -> String {
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

    TransferStartedBuilder::new(
        TransferStartDirection::Upload,
        transfer_id,
        session_id,
        local_path,
        remote_path,
        total_bytes,
    )
    .with_agent_id(agent_id.as_deref())
    .build()
}

/// Start a download transfer and return the response markdown.
pub(crate) fn start_download(
    session_id: String,
    agent_id: Option<String>,
    handle_arc: Arc<SshHandle>,
    remote_path: String,
    resolved_path: &Path,
    total_bytes: u64,
) -> String {
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

    TransferStartedBuilder::new(
        TransferStartDirection::Download,
        transfer_id,
        session_id,
        local_path,
        remote_path,
        total_bytes,
    )
    .with_agent_id(agent_id.as_deref())
    .build()
}

/// Build the transfer progress response markdown.
pub(crate) async fn build_transfer_progress_response(
    transfer_id: String,
    status_rx: &watch::Receiver<TransferStatus>,
    bytes_transferred: &AtomicU64,
    total_bytes_arc: &AtomicU64,
    error: &Mutex<Option<String>>,
    info: &TransferInfo,
) -> String {
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
            return TransferProgressBuilder::new(
                &transfer_id,
                direction,
                transferred,
                total,
                TransferProgressState::Failed(reason),
            )
            .build();
        }
        TransferStatus::Cancelled => {
            return TransferProgressBuilder::new(
                &transfer_id,
                direction,
                transferred,
                total,
                TransferProgressState::Failed("transfer cancelled"),
            )
            .build();
        }
    };

    TransferProgressBuilder::new(&transfer_id, direction, transferred, total, state).build()
}
