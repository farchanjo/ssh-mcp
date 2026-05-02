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

use bytes::Bytes;
use russh::client::Msg;
use russh::{Channel, Disconnect};
use tokio::sync::{OnceCell, Semaphore, broadcast, mpsc, watch};
use tokio::time::{self};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use super::super::async_command::{OutputBuffer, RunningCommand};
use super::super::client::{
    execute_ssh_command, execute_ssh_command_async, execute_ssh_command_async_pty,
};
use super::super::config::{
    resolve_command_cleanup_ttl, resolve_list_max_items_cap, resolve_list_max_items_default,
    resolve_output_default_bytes, resolve_output_max_bytes_cap, resolve_transfer_broadcast_cap,
    resolve_transfer_cleanup_ttl,
};
use super::super::message::builder::{
    ExecuteStartedBuilder, GetCommandOutputBuilder, GetCommandOutputState, ListSessionsBuilder,
    ShellOpenBuilder, TransferProgressBuilder, TransferProgressState, TransferStartDirection,
    TransferStartedBuilder, render_disconnect_agent,
};
use super::super::message::helpers::{format_error, generate_nonce};
use super::super::session::SshClientHandler;
use super::super::sftp::{TransferShared, sftp_download_streaming, sftp_upload_streaming};
use super::super::shell::{
    ChannelWriter, RingBuffer, RunningShell, WriteRequest, now_ms, touch_activity,
};
use super::super::storage::command::COMMAND_STORAGE;
use super::super::storage::session::SESSION_STORAGE;
use super::super::storage::shell::SHELL_STORAGE;
use super::super::storage::traits::{
    CommandStorage, SessionStorage, ShellStorage, TransferStorage,
};
use super::super::storage::transfer::TRANSFER_STORAGE;
use super::super::transfer::{RunningTransfer, TransferDirection, TransferInfo, TransferStatus};
use super::super::types::{
    AsyncCommandInfo, AsyncCommandStatus, HealthEvent, SessionInfo, ShellInfo, ShellStatus,
    SshCommandResponse,
};
use arc_swap::ArcSwap;

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
        .and_then(
            |(current_status, cancel_token, output_history, status_rx)| {
                if current_status == AsyncCommandStatus::Running {
                    Ok((cancel_token, output_history, status_rx))
                } else {
                    Err(format!("Command is not running (status: {current_status})"))
                }
            },
        )
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
                let _ = shell.input_tx.send(WriteRequest::Close).await;
                shell.cancel_token.cancel();
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

/// Current wall-clock time as milliseconds since the Unix epoch.
///
/// Falls back to `0` when the system clock is set before 1970.
fn health_event_now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
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
        if let Some(session_ref) = SESSION_STORAGE.get(id) {
            let _ = session_ref.health_tx.send(HealthEvent::Healthy {
                at_ms: health_event_now_ms(),
            });
        }
    }

    for id in &dead_session_ids {
        warn!("Removing dead session {id} from storage");
        if let Some(session_ref) = SESSION_STORAGE.get(id) {
            let _ = session_ref.health_tx.send(HealthEvent::Unhealthy {
                at_ms: health_event_now_ms(),
            });
        }
        // remove() already broadcasts `HealthEvent::Disconnected`.
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
// Shell lifecycle (lock-free — E8)
// ---------------------------------------------------------------------------

/// Flush threshold for batched shell output (8 KB).
const SHELL_FLUSH_THRESHOLD: usize = 8192;

/// Publish the just-arrived `chunk` through `history` and broadcast the
/// delta. The history update is performed via `ArcSwap::rcu` so concurrent
/// `ssh_shell_read` callers (which clear the head with their own rcu) cannot
/// clobber freshly appended bytes — and vice versa.
///
/// Errors on `output_tx.send` are swallowed: a missing subscriber is the
/// steady state until `ssh_shell_subscribe` lands.
fn publish_shell_chunk(
    history: &ArcSwap<RingBuffer>,
    output_tx: &broadcast::Sender<Bytes>,
    chunk: Bytes,
    max_buffer_size: usize,
) {
    history.rcu(|current| {
        let mut combined = Vec::with_capacity(current.data.len() + chunk.len());
        combined.extend_from_slice(&current.data);
        combined.extend_from_slice(&chunk);
        if max_buffer_size > 0 && combined.len() > max_buffer_size {
            let excess = combined.len() - max_buffer_size;
            combined.drain(..excess);
        }
        RingBuffer {
            data: Bytes::from(combined),
        }
    });
    let _ = output_tx.send(chunk);
}

/// Background reader that exclusively owns the channel read half. Each
/// flushed batch is published through `history` (via `ArcSwap::rcu`) and
/// broadcast as a `Bytes` chunk via `output_tx`. Activity is tracked
/// atomically through `last_activity_ms`.
async fn shell_reader(
    mut read_half: russh::ChannelReadHalf,
    history: Arc<ArcSwap<RingBuffer>>,
    output_tx: broadcast::Sender<Bytes>,
    data_notify: Arc<tokio::sync::Notify>,
    cancel_token: CancellationToken,
    status_tx: watch::Sender<ShellStatus>,
    max_buffer_size: Arc<AtomicU64>,
    last_activity_ms: Arc<AtomicU64>,
) {
    use russh::ChannelMsg;

    let mut local: Vec<u8> = Vec::with_capacity(4096);

    loop {
        tokio::select! {
            biased;

            () = cancel_token.cancelled() => break,

            msg = read_half.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                        local.extend_from_slice(&data);
                        touch_activity(&last_activity_ms);
                        if local.len() >= SHELL_FLUSH_THRESHOLD {
                            flush_shell_local(
                                &history,
                                &output_tx,
                                &data_notify,
                                &mut local,
                                &max_buffer_size,
                            );
                        }
                    }
                    Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
                    Some(_) => {}
                }
            }
        }
    }

    if !local.is_empty() {
        flush_shell_local(
            &history,
            &output_tx,
            &data_notify,
            &mut local,
            &max_buffer_size,
        );
    }

    let _ = status_tx.send(ShellStatus::Closed);
}

/// Flush `local` into the lock-free history and broadcast the delta.
fn flush_shell_local(
    history: &ArcSwap<RingBuffer>,
    output_tx: &broadcast::Sender<Bytes>,
    data_notify: &tokio::sync::Notify,
    local: &mut Vec<u8>,
    max_buffer_size: &AtomicU64,
) {
    let chunk = Bytes::copy_from_slice(local);
    local.clear();
    let max_size = usize::try_from(max_buffer_size.load(Ordering::Relaxed)).unwrap_or(usize::MAX);
    publish_shell_chunk(history, output_tx, chunk, max_size);
    data_notify.notify_waiters();
}

/// Spawn the background shell reader task.
fn spawn_shell_reader(read_half: russh::ChannelReadHalf, shell: &RunningShell) {
    let history = Arc::clone(&shell.history);
    let output_tx = shell.output_tx.clone();
    let data_notify = Arc::clone(&shell.data_notify);
    let cancel_token = shell.cancel_token.clone();
    let status_tx = shell.status_tx.clone();
    let max_buffer_size = Arc::clone(&shell.max_buffer_size);
    let last_activity_ms = Arc::clone(&shell.last_activity_ms);
    tokio::spawn(async move {
        shell_reader(
            read_half,
            history,
            output_tx,
            data_notify,
            cancel_token,
            status_tx,
            max_buffer_size,
            last_activity_ms,
        )
        .await;
    });
}

/// Background writer task. Exclusively owns `ChannelWriter` (no `Mutex`) and
/// drains `WriteRequest` frames from the input mpsc channel until either a
/// `Close` is received, the channel is dropped, or the cancel token fires.
async fn shell_writer(
    writer: ChannelWriter,
    mut input_rx: mpsc::Receiver<WriteRequest>,
    cancel_token: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            () = cancel_token.cancelled() => {
                let _ = writer.close().await;
                break;
            }
            req = input_rx.recv() => {
                match req {
                    Some(WriteRequest::Data(bytes)) => {
                        if let Err(e) = writer.write(&bytes).await {
                            warn!("Shell writer task: {e}");
                            break;
                        }
                    }
                    Some(WriteRequest::Close) => {
                        let _ = writer.close().await;
                        break;
                    }
                    None => {
                        // mpsc senders dropped — graceful shutdown.
                        let _ = writer.close().await;
                        break;
                    }
                }
            }
        }
    }
}

/// Spawn the dedicated writer task that owns the `ChannelWriter` exclusively.
fn spawn_shell_writer(
    write_half: russh::ChannelWriteHalf<Msg>,
    input_rx: mpsc::Receiver<WriteRequest>,
    cancel_token: CancellationToken,
) {
    let writer = ChannelWriter::new(write_half);
    tokio::spawn(async move {
        shell_writer(writer, input_rx, cancel_token).await;
    });
}

/// Auto-close a shell after the configured inactivity TTL.
///
/// Uses an atomic epoch-millis counter — no mutex acquisition on the polling
/// loop.
fn spawn_shell_inactivity_task(
    shell_id: String,
    last_activity_ms: Arc<AtomicU64>,
    inactivity_ttl: Duration,
    cancel_token: CancellationToken,
) {
    let ttl_ms = u64::try_from(inactivity_ttl.as_millis()).unwrap_or(u64::MAX);
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;

            if cancel_token.is_cancelled() {
                break;
            }

            let last = last_activity_ms.load(Ordering::Relaxed);
            let elapsed_ms = now_ms().saturating_sub(last);
            if elapsed_ms >= ttl_ms {
                info!(
                    "Shell {shell_id} inactive for {}s, auto-closing (TTL: {}s)",
                    elapsed_ms / 1000,
                    inactivity_ttl.as_secs(),
                );
                if let Some(shell) = SHELL_STORAGE.unregister(&shell_id) {
                    let _ = shell.input_tx.send(WriteRequest::Close).await;
                    shell.cancel_token.cancel();
                }
                break;
            }
        }
    });
}

/// Spawn background tasks (reader + writer + inactivity monitor) for a shell.
fn spawn_shell_tasks(
    shell_id: &str,
    read_half: russh::ChannelReadHalf,
    write_half: russh::ChannelWriteHalf<Msg>,
    input_rx: mpsc::Receiver<WriteRequest>,
    shell: &RunningShell,
    inactivity_ttl: Duration,
) {
    spawn_shell_reader(read_half, shell);
    spawn_shell_writer(write_half, input_rx, shell.cancel_token.clone());
    spawn_shell_inactivity_task(
        shell_id.to_string(),
        Arc::clone(&shell.last_activity_ms),
        inactivity_ttl,
        shell.cancel_token.clone(),
    );
}

/// Capacity for the per-shell input mpsc queue. The dedicated writer task
/// drains it; with one tool call per write request a small backlog is enough.
const SHELL_INPUT_CHANNEL_CAP: usize = 64;

/// Spawn the shell reader/writer and register the shell in storage.
fn spawn_and_register_shell(
    shell_id: &str,
    shell_info: ShellInfo,
    channel: Channel<Msg>,
    inactivity_ttl: Duration,
    max_buffer_size: u64,
) {
    let (read_half, write_half) = channel.split();
    let (input_tx, input_rx) = mpsc::channel::<WriteRequest>(SHELL_INPUT_CHANNEL_CAP);
    let cancel_token = CancellationToken::new();

    let shell = RunningShell::new(shell_info, cancel_token, input_tx, max_buffer_size);

    spawn_shell_tasks(
        shell_id,
        read_half,
        write_half,
        input_rx,
        &shell,
        inactivity_ttl,
    );
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

/// Create a transfer with lock-free shared state, register it in storage,
/// and return a `TransferShared` ready to plumb into the streaming task.
fn create_and_register_transfer(
    transfer_id: &str,
    session_id: &str,
    direction: TransferDirection,
    local_path: &str,
    remote_path: &str,
    started_at: &str,
    total_bytes: u64,
) -> TransferShared {
    let info = TransferInfo {
        transfer_id: transfer_id.to_string(),
        session_id: session_id.to_string(),
        direction,
        local_path: local_path.to_string(),
        remote_path: remote_path.to_string(),
        started_at: started_at.to_string(),
    };
    let cap = resolve_transfer_broadcast_cap();
    let xfer = RunningTransfer::new(info, total_bytes, cap);

    let shared = TransferShared {
        bytes_transferred: Arc::clone(&xfer.bytes_transferred),
        total_bytes: Arc::clone(&xfer.total_bytes),
        progress_tx: xfer.progress_tx.clone(),
        data_notify: Arc::clone(&xfer.data_notify),
        cancel_token: xfer.cancel_token.clone(),
        status_tx: xfer.status_tx.clone(),
        error: Arc::clone(&xfer.error),
    };
    let cleanup_rx = xfer.status_rx.clone();

    TRANSFER_STORAGE.register(transfer_id.to_string(), xfer);

    spawn_transfer_cleanup_task(transfer_id.to_string(), cleanup_rx);

    shared
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
    shared: TransferShared,
) {
    tokio::spawn(sftp_upload_streaming(
        handle_arc,
        resolved_path.to_path_buf(),
        remote_path,
        shared,
    ));
}

/// Spawn the background download task.
fn spawn_download_task(
    handle_arc: Arc<SshHandle>,
    remote_path: &str,
    resolved_path: &Path,
    shared: TransferShared,
) {
    tokio::spawn(sftp_download_streaming(
        handle_arc,
        remote_path.to_string(),
        resolved_path.to_path_buf(),
        shared,
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

    let shared = create_and_register_transfer(
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
    spawn_upload_task(handle_arc, resolved_path, remote_path.clone(), shared);

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

    let shared = create_and_register_transfer(
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
    spawn_download_task(handle_arc, &remote_path, resolved_path, shared);

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
///
/// Reads the lock-free state without taking any mutex: `error` is fetched
/// via `OnceCell::get`, status via `watch::Receiver::borrow`, and the
/// progress counters via atomic loads.
pub(crate) fn build_transfer_progress_response(
    transfer_id: &str,
    status_rx: &watch::Receiver<TransferStatus>,
    bytes_transferred: &AtomicU64,
    total_bytes_arc: &AtomicU64,
    error: &OnceCell<String>,
    info: &TransferInfo,
) -> String {
    let status = *status_rx.borrow();
    let transferred = bytes_transferred.load(Ordering::SeqCst);
    let total = total_bytes_arc.load(Ordering::SeqCst);
    let error_val: Option<&str> = error.get().map(String::as_str);

    let direction = match info.direction {
        TransferDirection::Upload => TransferStartDirection::Upload,
        TransferDirection::Download => TransferStartDirection::Download,
    };

    let state = match status {
        TransferStatus::Running => TransferProgressState::Running,
        TransferStatus::Completed => TransferProgressState::Completed,
        TransferStatus::Failed => {
            let reason = error_val.unwrap_or("transfer failed");
            return TransferProgressBuilder::new(
                transfer_id,
                direction,
                transferred,
                total,
                TransferProgressState::Failed(reason),
            )
            .build();
        }
        TransferStatus::Cancelled => {
            return TransferProgressBuilder::new(
                transfer_id,
                direction,
                transferred,
                total,
                TransferProgressState::Failed("transfer cancelled"),
            )
            .build();
        }
    };

    TransferProgressBuilder::new(transfer_id, direction, transferred, total, state).build()
}
