//! MCP SSH Commands implementation.
//!
//! This module provides the main MCP tool implementations for SSH operations:
//!
//! - `ssh_connect`: Connect to an SSH server with retry logic
//! - `ssh_execute`: Execute commands asynchronously (returns command_id for polling)
//! - `ssh_get_command_output`: Get output and status of a running command
//! - `ssh_list_commands`: List all async commands
//! - `ssh_cancel_command`: Cancel a running command
//! - `ssh_forward`: Setup port forwarding (feature-gated)
//! - `ssh_disconnect`: Disconnect and cleanup a session
//! - `ssh_list_sessions`: List all active sessions

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::future::join_all;
use poem_mcpserver::{Tools, content::Text, tool::StructuredContent};
use russh::Disconnect;
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::async_command::{
    ASYNC_COMMANDS, MAX_ASYNC_COMMANDS_PER_SESSION, OutputBuffer, RunningCommand,
    count_session_commands, get_session_command_ids, register_command, unregister_command,
};
use super::client::{connect_to_ssh_with_retry, execute_ssh_command, execute_ssh_command_async};
use super::config::{
    resolve_command_timeout, resolve_compression, resolve_connect_timeout, resolve_max_retries,
    resolve_retry_delay,
};
#[cfg(feature = "port_forward")]
use super::forward::setup_port_forwarding;
use super::session::{SSH_SESSIONS, StoredSession};
use super::types::{
    AsyncCommandInfo, AsyncCommandStatus, PortForwardingResponse, SessionInfo, SessionListResponse,
    SshAsyncOutputResponse, SshCancelCommandResponse, SshConnectResponse, SshExecuteResponse,
    SshListCommandsResponse,
};

/// MCP SSH Commands tool implementation.
///
/// This struct provides all SSH-related MCP tools for connecting to servers,
/// executing commands, and managing port forwarding.
pub struct McpSSHCommands;

#[Tools]
impl McpSSHCommands {
    /// Connect to an SSH server and store the session.
    ///
    /// Returns a session_id for subsequent commands. For long-running operations
    /// (builds, deployments, batch processing), `ssh_execute` provides non-blocking
    /// execution with progress monitoring and cancellation support.
    ///
    /// Use `persistent=true` for sessions that should remain open indefinitely.
    #[allow(clippy::too_many_arguments)]
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
    ) -> Result<StructuredContent<SshConnectResponse>, String> {
        let timeout = resolve_connect_timeout(timeout_secs);
        let max_retries_val = resolve_max_retries(max_retries);
        let retry_delay = resolve_retry_delay(retry_delay_ms);
        let compress = resolve_compression(compress);
        let persistent = persistent.unwrap_or(false);

        // Check if session_id was provided for potential reuse
        if let Some(ref sid) = session_id {
            // DashMap: use .get() which returns Option<Ref<K, V>>
            let handle_and_info = SSH_SESSIONS
                .get(sid)
                .map(|s| (s.handle.clone(), s.info.clone()));

            if let Some((handle_arc, info)) = handle_and_info {
                // Health check with 5 second timeout
                let health_timeout = Duration::from_secs(5);
                let now = chrono::Utc::now().to_rfc3339();

                match execute_ssh_command(&handle_arc, "echo 1", health_timeout).await {
                    Ok(response) if !response.timed_out && response.exit_code == 0 => {
                        // Update health status in storage
                        if let Some(mut stored) = SSH_SESSIONS.get_mut(sid) {
                            stored.info.last_health_check = Some(now);
                            stored.info.healthy = Some(true);
                        }

                        info!("Reusing healthy session {}", sid);
                        return Ok(StructuredContent(SshConnectResponse {
                            session_id: sid.clone(),
                            message: format!(
                                "Reused existing session for {}@{}{}. Use session_id '{}' for subsequent commands.",
                                info.username,
                                info.host,
                                info.name
                                    .as_ref()
                                    .map(|n| format!(" (name: '{}')", n))
                                    .unwrap_or_default(),
                                sid
                            ),
                            authenticated: true,
                            retry_attempts: 0,
                        }));
                    }
                    _ => {
                        // Session dead - remove it
                        warn!("Session {} is dead, removing", sid);
                        SSH_SESSIONS.remove(sid);
                    }
                }
            } else {
                info!("Session {} not found, creating new connection", sid);
            }
        }

        info!(
            "Attempting SSH connection to {}@{} with timeout {}s, max_retries={}, retry_delay={}ms, compress={}, persistent={}, name={:?}",
            username,
            address,
            timeout.as_secs(),
            max_retries_val,
            retry_delay.as_millis(),
            compress,
            persistent,
            name
        );

        match connect_to_ssh_with_retry(
            &address,
            &username,
            password.as_deref(),
            key_path.as_deref(),
            timeout,
            max_retries_val,
            retry_delay,
            compress,
            persistent,
        )
        .await
        {
            Ok((handle, retry_attempts)) => {
                let new_session_id = Uuid::new_v4().to_string();
                let connected_at = chrono::Utc::now().to_rfc3339();

                let session_info = SessionInfo {
                    session_id: new_session_id.clone(),
                    name: name.clone(),
                    host: address.clone(),
                    username: username.clone(),
                    connected_at,
                    default_timeout_secs: timeout.as_secs(),
                    retry_attempts,
                    compression_enabled: compress,
                    last_health_check: None,
                    healthy: None,
                };

                // DashMap: direct insert, no lock needed
                SSH_SESSIONS.insert(
                    new_session_id.clone(),
                    StoredSession {
                        info: session_info,
                        handle: Arc::new(handle),
                    },
                );

                let message = {
                    let name_part = name
                        .as_ref()
                        .map(|n| format!(" (name: '{}')", n))
                        .unwrap_or_default();
                    let retry_part = if retry_attempts > 0 {
                        format!(" after {} retry attempt(s)", retry_attempts)
                    } else {
                        String::new()
                    };
                    let persistent_part = if persistent { " [persistent]" } else { "" };

                    format!(
                        "Connected to {}@{}{}{}. Use session_id '{}' with ssh_execute to run commands.",
                        username, address, name_part, retry_part, new_session_id
                    ) + persistent_part
                };

                Ok(StructuredContent(SshConnectResponse {
                    session_id: new_session_id,
                    message,
                    authenticated: true,
                    retry_attempts,
                }))
            }
            Err(e) => {
                error!("SSH connection failed: {}", e);
                Err(e)
            }
        }
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
        info!("Disconnecting SSH session: {}", session_id);

        // Cancel all async commands for this session (sync O(1) lookup)
        let command_ids = get_session_command_ids(&session_id);
        if !command_ids.is_empty() {
            info!(
                "Cancelling {} async commands for session {}",
                command_ids.len(),
                session_id
            );

            // Cancel and remove each command using DashMap API
            for cmd_id in &command_ids {
                if let Some(cmd) = ASYNC_COMMANDS.get(cmd_id) {
                    cmd.cancel_token.cancel();
                }
            }

            // Remove cancelled commands using unregister_command
            for cmd_id in command_ids {
                unregister_command(&cmd_id);
            }
        }

        // DashMap: remove returns Option<(K, V)>
        if let Some((_, stored)) = SSH_SESSIONS.remove(&session_id) {
            // Gracefully disconnect the session
            if let Err(e) = stored
                .handle
                .disconnect(Disconnect::ByApplication, "Session closed by user", "en")
                .await
            {
                warn!("Error during disconnect: {}", e);
            }
            Ok(Text(format!(
                "Session {} disconnected successfully",
                session_id
            )))
        } else {
            Err(format!("No active SSH session with ID: {}", session_id))
        }
    }

    /// List all active SSH sessions with their metadata.
    ///
    /// Performs a health check on each session and automatically removes
    /// dead/disconnected sessions from the list. Use this to find available
    /// session_ids for command execution.
    async fn ssh_list_sessions(&self) -> StructuredContent<SessionListResponse> {
        let health_timeout = Duration::from_secs(5);

        // Get all sessions and their handles (DashMap: iterate without lock)
        let sessions_snapshot: Vec<_> = SSH_SESSIONS
            .iter()
            .map(|entry| {
                (
                    entry.key().clone(),
                    entry.value().handle.clone(),
                    entry.value().info.clone(),
                )
            })
            .collect();

        // Run health checks in PARALLEL using join_all
        let health_futures: Vec<_> = sessions_snapshot
            .into_iter()
            .map(|(session_id, handle_arc, info)| async move {
                let now = chrono::Utc::now().to_rfc3339();
                let result = execute_ssh_command(&handle_arc, "echo 1", health_timeout).await;
                (session_id, info, now, result)
            })
            .collect();

        let results = join_all(health_futures).await;

        // Process results
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

        // Update healthy sessions (DashMap: direct get_mut)
        for (id, info) in &healthy_sessions {
            if let Some(mut stored) = SSH_SESSIONS.get_mut(id) {
                stored
                    .info
                    .last_health_check
                    .clone_from(&info.last_health_check);
                stored.info.healthy = info.healthy;
            }
        }

        // Remove dead sessions (DashMap: direct remove)
        for id in &dead_session_ids {
            warn!("Removing dead session {} from storage", id);
            SSH_SESSIONS.remove(id);
        }

        let session_infos: Vec<SessionInfo> =
            healthy_sessions.into_iter().map(|(_, info)| info).collect();
        let count = session_infos.len();

        StructuredContent(SessionListResponse {
            sessions: session_infos,
            count,
        })
    }

    /// Setup port forwarding on an existing SSH session
    #[allow(unused_variables)]
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
        {
            info!(
                "Setting up port forwarding from local port {} to {}:{} using session {}",
                local_port, remote_address, remote_port, session_id
            );

            // DashMap: direct get, no lock needed
            let handle_arc = SSH_SESSIONS
                .get(&session_id)
                .map(|s| s.handle.clone())
                .ok_or_else(|| format!("No active SSH session with ID: {}", session_id))?;

            match setup_port_forwarding(handle_arc, local_port, &remote_address, remote_port).await
            {
                Ok(local_addr) => Ok(StructuredContent(PortForwardingResponse {
                    local_address: local_addr.to_string(),
                    remote_address: format!("{}:{}", remote_address, remote_port),
                    active: true,
                })),
                Err(e) => {
                    error!("Port forwarding setup failed: {}", e);
                    Err(e)
                }
            }
        }

        #[cfg(not(feature = "port_forward"))]
        {
            Err(
                "Port forwarding feature is not enabled. Rebuild with --features port_forward"
                    .to_string(),
            )
        }
    }

    /// Execute a command asynchronously on a connected SSH session.
    ///
    /// **Recommended for:** Long-running commands (builds, deployments, batch jobs,
    /// data processing) that may exceed the default timeout or benefit from
    /// progress monitoring and cancellation.
    ///
    /// **Workflow:**
    /// 1. Call `ssh_execute` - returns immediately with `command_id`
    /// 2. Poll with `ssh_get_command_output(command_id, wait=false)` to check progress
    /// 3. Or wait with `ssh_get_command_output(command_id, wait=true)` to block until done
    /// 4. Cancel anytime with `ssh_cancel_command(command_id)`
    ///
    /// **Limits:** Up to 30 concurrent async commands per session.
    ///
    /// Returns immediately with a command_id for polling or cancellation.
    async fn ssh_execute(
        &self,
        /// Session ID returned from ssh_connect
        session_id: String,
        /// Shell command to execute on the remote server
        command: String,
        /// Command execution timeout in seconds (default: 180, env: SSH_COMMAND_TIMEOUT)
        timeout_secs: Option<u64>,
    ) -> Result<StructuredContent<SshExecuteResponse>, String> {
        let timeout = resolve_command_timeout(timeout_secs);

        // Check session limit (sync O(1) lookup)
        let current_count = count_session_commands(&session_id);
        if current_count >= MAX_ASYNC_COMMANDS_PER_SESSION {
            return Err(format!(
                "Maximum async commands per session reached ({}). Cancel or wait for existing commands to complete.",
                MAX_ASYNC_COMMANDS_PER_SESSION
            ));
        }

        // DashMap: direct get, no lock needed
        let handle_arc = SSH_SESSIONS
            .get(&session_id)
            .map(|s| s.handle.clone())
            .ok_or_else(|| format!("No active SSH session with ID: {}", session_id))?;

        let command_id = Uuid::new_v4().to_string();
        let started_at = chrono::Utc::now().to_rfc3339();

        // Create shared state with pre-allocated buffers
        let (status_tx, status_rx) = watch::channel(AsyncCommandStatus::Running);
        let output = Arc::new(Mutex::new(OutputBuffer::with_capacity(4096, 1024)));
        let exit_code = Arc::new(Mutex::new(None));
        let error = Arc::new(Mutex::new(None));
        let timed_out = Arc::new(AtomicBool::new(false));
        let cancel_token = CancellationToken::new();

        // Create command info
        let info = AsyncCommandInfo {
            command_id: command_id.clone(),
            session_id: session_id.clone(),
            command: command.clone(),
            status: AsyncCommandStatus::Running,
            started_at: started_at.clone(),
        };

        // Store running command using register_command for proper indexing
        register_command(
            command_id.clone(),
            RunningCommand {
                info: info.clone(),
                cancel_token: cancel_token.clone(),
                status_rx,
                status_tx: status_tx.clone(),
                output: output.clone(),
                exit_code: exit_code.clone(),
                error: error.clone(),
                timed_out: timed_out.clone(),
            },
        );

        info!(
            "Starting async command {} on session {}: {}",
            command_id, session_id, command
        );

        // Spawn background task
        tokio::spawn(execute_ssh_command_async(
            handle_arc,
            command.clone(),
            timeout,
            output,
            status_tx,
            cancel_token,
            exit_code,
            error,
            timed_out,
        ));

        Ok(StructuredContent(SshExecuteResponse {
            command_id: command_id.clone(),
            session_id,
            command,
            started_at,
            message: format!(
                "Command started. Use ssh_get_command_output with command_id '{}' to poll for results, or ssh_cancel_command to cancel.",
                command_id
            ),
        }))
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

        // DashMap: direct get, no lock needed
        let (status_rx, output, exit_code, error, timed_out) = ASYNC_COMMANDS
            .get(&command_id)
            .map(|cmd| {
                (
                    cmd.status_rx.clone(),
                    cmd.output.clone(),
                    cmd.exit_code.clone(),
                    cmd.error.clone(),
                    cmd.timed_out.clone(),
                )
            })
            .ok_or_else(|| format!("No async command with ID: {}", command_id))?;

        // Optionally wait for completion
        if wait {
            let mut rx = status_rx.clone();
            let _ = tokio::time::timeout(wait_timeout, async {
                loop {
                    let status = *rx.borrow();
                    if status != AsyncCommandStatus::Running {
                        break;
                    }
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
        }

        // Get current state
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

    /// List all async commands, optionally filtered by session or status.
    ///
    /// Useful for monitoring multiple concurrent operations or checking
    /// which commands are still running before disconnecting a session.
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

        // DashMap: iterate without lock
        let filtered: Vec<AsyncCommandInfo> = ASYNC_COMMANDS
            .iter()
            .filter(|entry| {
                let cmd = entry.value();
                let session_match = session_id
                    .as_ref()
                    .map(|sid| cmd.info.session_id == *sid)
                    .unwrap_or(true);
                let status_match = status_filter
                    .map(|sf| *cmd.status_rx.borrow() == sf)
                    .unwrap_or(true);
                session_match && status_match
            })
            .map(|entry| {
                let cmd = entry.value();
                let mut info = cmd.info.clone();
                info.status = *cmd.status_rx.borrow();
                info
            })
            .collect();

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
        // DashMap: direct get, no lock needed
        let (cancel_token, output, status_rx) = ASYNC_COMMANDS
            .get(&command_id)
            .map(|cmd| {
                let current_status = *cmd.status_rx.borrow();
                (
                    current_status,
                    cmd.cancel_token.clone(),
                    cmd.output.clone(),
                    cmd.status_rx.clone(),
                )
            })
            .ok_or_else(|| format!("No async command with ID: {}", command_id))
            .and_then(|(current_status, cancel_token, output, status_rx)| {
                if current_status != AsyncCommandStatus::Running {
                    Err(format!(
                        "Command is not running (status: {})",
                        current_status
                    ))
                } else {
                    Ok((cancel_token, output, status_rx))
                }
            })?;

        // Signal cancellation
        cancel_token.cancel();

        // Wait briefly for cancellation to take effect
        let mut rx = status_rx;
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if *rx.borrow() != AsyncCommandStatus::Running {
                    break;
                }
                if rx.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;

        // Get final output
        let output_buf = output.lock().await;

        info!("Cancelled async command: {}", command_id);

        Ok(StructuredContent(SshCancelCommandResponse {
            command_id,
            cancelled: true,
            message: "Command cancelled successfully".to_string(),
            stdout: String::from_utf8_lossy(&output_buf.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output_buf.stderr).into_owned(),
        }))
    }
}
