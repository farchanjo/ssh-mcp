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

use super::async_command::{MAX_ASYNC_COMMANDS_PER_SESSION, OutputBuffer, RunningCommand};
use super::client::{
    connect_to_ssh_with_retry, execute_ssh_command, execute_ssh_command_async,
    execute_ssh_command_async_pty, open_pty_shell,
};
use super::config::{
    resolve_command_timeout, resolve_compression, resolve_connect_timeout,
    resolve_inactivity_timeout, resolve_max_retries, resolve_retry_delay,
};
#[cfg(feature = "port_forward")]
use super::forward::setup_port_forwarding;
use super::message::{
    AgentDisconnectMessageBuilder, ConnectMessageBuilder, ExecuteMessageBuilder,
    ShellOpenMessageBuilder,
};
use super::shell::{ChannelWriter, MAX_SHELLS_PER_SESSION, RunningShell};
use super::storage::{
    COMMAND_STORAGE, CommandStorage, SESSION_STORAGE, SHELL_STORAGE, SessionStorage, ShellStorage,
};
use super::types::{
    AgentDisconnectResponse, AsyncCommandInfo, AsyncCommandStatus, PortForwardingResponse,
    SessionInfo, SessionListResponse, ShellInfo, ShellStatus, SshAsyncOutputResponse,
    SshCancelCommandResponse, SshConnectResponse, SshExecuteResponse, SshListCommandsResponse,
    SshShellCloseResponse, SshShellOpenResponse, SshShellReadResponse,
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
    /// Returns session_id and optional agent_id that you MUST remember for subsequent commands.
    ///
    /// **Important identifiers in response:**
    /// - `session_id`: Use with ssh_execute, ssh_disconnect
    /// - `agent_id`: Use with ssh_list_sessions (filter), ssh_disconnect_agent (cleanup)
    ///
    /// For long-running operations (builds, deployments, batch processing),
    /// `ssh_execute` provides non-blocking execution with progress monitoring.
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
        /// Optional agent identifier for grouping sessions (e.g., "claude-code-instance-abc123"). Use ssh_disconnect_agent to disconnect all sessions for an agent.
        agent_id: Option<String>,
    ) -> Result<StructuredContent<SshConnectResponse>, String> {
        let timeout = resolve_connect_timeout(timeout_secs);
        let inactivity_timeout = resolve_inactivity_timeout();
        let max_retries_val = resolve_max_retries(max_retries);
        let retry_delay = resolve_retry_delay(retry_delay_ms);
        let compress = resolve_compression(compress);
        let persistent = persistent.unwrap_or(false);

        // Check if session_id was provided for potential reuse
        if let Some(ref sid) = session_id {
            if let Some(session_ref) = SESSION_STORAGE.get(sid) {
                // Health check with 5 second timeout
                let health_timeout = Duration::from_secs(5);
                let now = chrono::Utc::now().to_rfc3339();

                match execute_ssh_command(&session_ref.handle, "echo 1", health_timeout).await {
                    Ok(response) if !response.timed_out && response.exit_code == 0 => {
                        // Update health status in storage
                        SESSION_STORAGE.update_health(sid, now, true);

                        info!("Reusing healthy session {}", sid);
                        let reuse_agent_id = session_ref.info.agent_id.clone();
                        let message = ConnectMessageBuilder::new(
                            sid,
                            &session_ref.info.username,
                            &session_ref.info.host,
                        )
                        .with_agent_id(reuse_agent_id.as_deref())
                        .with_name(session_ref.info.name.as_deref())
                        .reused(true)
                        .build();
                        return Ok(StructuredContent(SshConnectResponse {
                            session_id: sid.clone(),
                            agent_id: reuse_agent_id,
                            message,
                            authenticated: true,
                            retry_attempts: 0,
                        }));
                    }
                    _ => {
                        // Session dead - remove it
                        warn!("Session {} is dead, removing", sid);
                        SESSION_STORAGE.remove(sid);
                    }
                }
            } else {
                info!("Session {} not found, creating new connection", sid);
            }
        }

        info!(
            "Attempting SSH connection to {}@{} with timeout {}s, max_retries={}, retry_delay={}ms, compress={}, persistent={}, name={:?}, agent_id={:?}",
            username,
            address,
            timeout.as_secs(),
            max_retries_val,
            retry_delay.as_millis(),
            compress,
            persistent,
            name,
            agent_id
        );

        match connect_to_ssh_with_retry(
            &address,
            &username,
            password.as_deref(),
            key_path.as_deref(),
            timeout,
            inactivity_timeout,
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
                    agent_id: agent_id.clone(),
                    host: address.clone(),
                    username: username.clone(),
                    connected_at,
                    default_timeout_secs: timeout.as_secs(),
                    retry_attempts,
                    compression_enabled: compress,
                    last_health_check: None,
                    healthy: None,
                };

                // Insert session using storage abstraction
                SESSION_STORAGE.insert(new_session_id.clone(), session_info, Arc::new(handle));

                // Register in agent index if agent_id is provided
                if let Some(ref aid) = agent_id {
                    SESSION_STORAGE.register_agent(aid, &new_session_id);
                }

                let message = ConnectMessageBuilder::new(&new_session_id, &username, &address)
                    .with_agent_id(agent_id.as_deref())
                    .with_name(name.as_deref())
                    .with_retry_attempts(retry_attempts)
                    .with_persistent(persistent)
                    .build();

                Ok(StructuredContent(SshConnectResponse {
                    session_id: new_session_id,
                    agent_id,
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

        // Close all interactive shells for this session
        let shell_ids = SHELL_STORAGE.list_by_session(&session_id);
        if !shell_ids.is_empty() {
            info!(
                "Closing {} interactive shells for session {}",
                shell_ids.len(),
                session_id
            );
            for shell_id in &shell_ids {
                if let Some(shell) = SHELL_STORAGE.unregister(shell_id) {
                    shell.cancel_token.cancel();
                    let writer = shell.channel_writer.lock().await;
                    let _ = writer.close().await;
                }
            }
        }

        // Cancel all async commands for this session (sync O(1) lookup)
        let command_ids = COMMAND_STORAGE.list_by_session(&session_id);
        if !command_ids.is_empty() {
            info!(
                "Cancelling {} async commands for session {}",
                command_ids.len(),
                session_id
            );

            // Cancel and remove each command
            for cmd_id in &command_ids {
                if let Some(cmd_ref) = COMMAND_STORAGE.get_ref(cmd_id) {
                    cmd_ref.running.cancel_token.cancel();
                }
            }

            // Remove cancelled commands
            for cmd_id in command_ids {
                COMMAND_STORAGE.unregister(&cmd_id);
            }
        }

        // Remove session from storage
        if let Some(session_ref) = SESSION_STORAGE.remove(&session_id) {
            // Unregister from agent index if agent_id exists
            if let Some(ref agent_id) = session_ref.info.agent_id {
                SESSION_STORAGE.unregister_agent(agent_id, &session_id);
            }

            // Gracefully disconnect the session
            if let Err(e) = session_ref
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
    ///
    /// **Filtering by agent_id:** When provided, only sessions belonging to that
    /// agent are returned. This is useful when multiple agents share an MCP server.
    async fn ssh_list_sessions(
        &self,
        /// Filter by agent ID to list only sessions for a specific agent
        agent_id: Option<String>,
    ) -> StructuredContent<SessionListResponse> {
        let health_timeout = Duration::from_secs(5);

        // Get session IDs to check (either all or filtered by agent)
        let session_ids_to_check: Vec<String> = if let Some(ref aid) = agent_id {
            SESSION_STORAGE.get_agent_sessions(aid)
        } else {
            SESSION_STORAGE.session_ids()
        };

        // Get sessions and their handles for health checks
        let sessions_snapshot: Vec<_> = session_ids_to_check
            .into_iter()
            .filter_map(|session_id| {
                SESSION_STORAGE.get(&session_id).map(|session_ref| {
                    (
                        session_id,
                        session_ref.handle.clone(),
                        session_ref.info.clone(),
                    )
                })
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

        // Update healthy sessions using storage abstraction
        for (id, info) in &healthy_sessions {
            if let Some(ref last_check) = info.last_health_check {
                SESSION_STORAGE.update_health(
                    id,
                    last_check.clone(),
                    info.healthy.unwrap_or(false),
                );
            }
        }

        // Remove dead sessions using storage abstraction
        for id in &dead_session_ids {
            warn!("Removing dead session {} from storage", id);
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

            // Get session handle using storage abstraction
            let handle_arc = SESSION_STORAGE
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
    /// Returns command_id, session_id, and agent_id that you MUST remember.
    ///
    /// **Important identifiers in response:**
    /// - `command_id`: Use with ssh_get_command_output (poll), ssh_cancel_command (cancel)
    /// - `session_id`: The session running this command
    /// - `agent_id`: The agent that owns this session (if set)
    ///
    /// **Workflow:**
    /// 1. ssh_execute → get command_id
    /// 2. ssh_get_command_output(command_id, wait=true) → get result
    ///
    /// **Limits:** Up to 100 concurrent multiplexed commands per session.
    /// When the limit is reached, you must wait for existing commands to complete
    /// or cancel them using ssh_cancel_command before starting new ones.
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
        /// Allocate a pseudo-terminal (PTY) for the command. Use for commands requiring a terminal (sudo, top). All output goes to stdout in PTY mode (no stderr separation).
        pty: Option<bool>,
    ) -> Result<StructuredContent<SshExecuteResponse>, String> {
        let timeout = resolve_command_timeout(timeout_secs);

        // Check session limit (sync O(1) lookup)
        let current_count = COMMAND_STORAGE.count_by_session(&session_id);
        if current_count >= MAX_ASYNC_COMMANDS_PER_SESSION {
            return Err(format!(
                "Maximum async commands per session reached ({}). Cancel or wait for existing commands to complete.",
                MAX_ASYNC_COMMANDS_PER_SESSION
            ));
        }

        // Get session handle and agent_id using storage abstraction
        let (handle_arc, agent_id) = SESSION_STORAGE
            .get(&session_id)
            .map(|s| (s.handle.clone(), s.info.agent_id.clone()))
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
        let cmd_info = AsyncCommandInfo {
            command_id: command_id.clone(),
            session_id: session_id.clone(),
            command: command.clone(),
            status: AsyncCommandStatus::Running,
            started_at: started_at.clone(),
        };

        // Store running command using storage abstraction
        COMMAND_STORAGE.register(
            command_id.clone(),
            RunningCommand {
                info: cmd_info,
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

        // Spawn background task (with or without PTY)
        if pty.unwrap_or(false) {
            tokio::spawn(execute_ssh_command_async_pty(
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
        } else {
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
        }

        let message = ExecuteMessageBuilder::new(&command_id, &session_id, &command)
            .with_agent_id(agent_id.as_deref())
            .build();

        Ok(StructuredContent(SshExecuteResponse {
            command_id,
            session_id,
            agent_id,
            command,
            started_at,
            message,
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

        // Get command using storage abstraction
        let (status_rx, output, exit_code, error, timed_out) = COMMAND_STORAGE
            .get_direct(&command_id)
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

        // Use storage trait method for filtered listing (LSP compliance)
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
        // Get command using storage abstraction
        let (cancel_token, output, status_rx) = COMMAND_STORAGE
            .get_direct(&command_id)
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

    /// Disconnect ALL sessions for a specific agent.
    ///
    /// Use this for cleanup when an agent is done. This will:
    /// - Cancel all running commands for the agent's sessions
    /// - Disconnect all sessions owned by the agent
    /// - Other agents' sessions are NOT affected
    ///
    /// **Required identifier:**
    /// - `agent_id`: The agent identifier from ssh_connect
    async fn ssh_disconnect_agent(
        &self,
        /// The agent identifier to disconnect all sessions for
        agent_id: String,
    ) -> Result<StructuredContent<AgentDisconnectResponse>, String> {
        info!("Disconnecting all sessions for agent: {}", agent_id);

        // Get and remove all session IDs for this agent atomically
        let session_ids = SESSION_STORAGE.remove_agent_sessions(&agent_id);

        if session_ids.is_empty() {
            let message = AgentDisconnectMessageBuilder::new(&agent_id)
                .with_sessions_disconnected(0)
                .with_commands_cancelled(0)
                .build();
            return Ok(StructuredContent(AgentDisconnectResponse {
                agent_id: agent_id.clone(),
                sessions_disconnected: 0,
                commands_cancelled: 0,
                message,
            }));
        }

        let mut total_commands_cancelled = 0;

        let mut total_shells_closed = 0;

        // Process each session
        for session_id in &session_ids {
            // Close all interactive shells for this session
            let shell_ids = SHELL_STORAGE.list_by_session(session_id);
            for shell_id in &shell_ids {
                if let Some(shell) = SHELL_STORAGE.unregister(shell_id) {
                    shell.cancel_token.cancel();
                    let writer = shell.channel_writer.lock().await;
                    let _ = writer.close().await;
                }
            }
            total_shells_closed += shell_ids.len();

            // Cancel all async commands for this session
            let command_ids = COMMAND_STORAGE.list_by_session(session_id);
            for cmd_id in &command_ids {
                if let Some(cmd_ref) = COMMAND_STORAGE.get_ref(cmd_id) {
                    cmd_ref.running.cancel_token.cancel();
                }
            }
            total_commands_cancelled += command_ids.len();

            // Remove cancelled commands
            for cmd_id in command_ids {
                COMMAND_STORAGE.unregister(&cmd_id);
            }

            // Disconnect the session
            if let Some(session_ref) = SESSION_STORAGE.remove(session_id)
                && let Err(e) = session_ref
                    .handle
                    .disconnect(Disconnect::ByApplication, "Agent cleanup", "en")
                    .await
            {
                warn!("Error during disconnect of session {}: {}", session_id, e);
            }
        }

        let sessions_disconnected = session_ids.len();

        info!(
            "Disconnected {} sessions, cancelled {} commands, and closed {} shells for agent {}",
            sessions_disconnected, total_commands_cancelled, total_shells_closed, agent_id
        );

        let message = AgentDisconnectMessageBuilder::new(&agent_id)
            .with_sessions_disconnected(sessions_disconnected)
            .with_commands_cancelled(total_commands_cancelled)
            .build();

        Ok(StructuredContent(AgentDisconnectResponse {
            agent_id: agent_id.clone(),
            sessions_disconnected,
            commands_cancelled: total_commands_cancelled,
            message,
        }))
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

        // Check shell limit
        let current_count = SHELL_STORAGE.count_by_session(&session_id);
        if current_count >= MAX_SHELLS_PER_SESSION {
            return Err(format!(
                "Maximum shells per session reached ({}). Close existing shells first.",
                MAX_SHELLS_PER_SESSION
            ));
        }

        // Get session handle and agent_id
        let (handle_arc, agent_id) = SESSION_STORAGE
            .get(&session_id)
            .map(|s| (s.handle.clone(), s.info.agent_id.clone()))
            .ok_or_else(|| format!("No active SSH session with ID: {}", session_id))?;

        // Open PTY channel with shell
        let channel = open_pty_shell(&handle_arc, &term, cols, rows).await?;

        let shell_id = Uuid::new_v4().to_string();
        let opened_at = chrono::Utc::now().to_rfc3339();

        let shell_info = ShellInfo {
            shell_id: shell_id.clone(),
            session_id: session_id.clone(),
            term_type: term.clone(),
            cols,
            rows,
            opened_at,
        };

        // Create shared state
        let (status_tx, status_rx) = watch::channel(ShellStatus::Open);
        let output = Arc::new(Mutex::new(Vec::with_capacity(4096)));
        let cancel_token = CancellationToken::new();

        // Split channel into independent read/write halves to avoid mutex contention.
        // The reader exclusively owns the read half, while writes go through the write half.
        let (read_half, write_half) = channel.split();
        let writer = ChannelWriter::new(write_half);
        let channel_writer = Arc::new(Mutex::new(writer));

        // Spawn background reader task (owns read_half exclusively, no mutex needed)
        let reader_output = output.clone();
        let reader_cancel = cancel_token.clone();
        let reader_status_tx = status_tx.clone();

        tokio::spawn(async move {
            shell_reader(read_half, reader_output, reader_cancel, reader_status_tx).await;
        });

        // Store running shell
        SHELL_STORAGE.register(
            shell_id.clone(),
            RunningShell {
                info: shell_info,
                cancel_token,
                output,
                channel_writer,
                status_tx,
                status_rx,
            },
        );

        info!(
            "Opened interactive shell {} on session {} (term={}, {}x{})",
            shell_id, session_id, term, cols, rows
        );

        let message = ShellOpenMessageBuilder::new(&shell_id, &session_id, &term, cols, rows)
            .with_agent_id(agent_id.as_deref())
            .build();

        Ok(StructuredContent(SshShellOpenResponse {
            shell_id,
            session_id,
            agent_id,
            term_type: term,
            message,
        }))
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
            .map(|shell| shell.channel_writer.clone())
            .ok_or_else(|| format!("No active shell with ID: {}", shell_id))?;

        let writer = channel_writer.lock().await;
        writer.write(input.as_bytes()).await?;

        Ok(Text(format!(
            "Sent {} bytes to shell '{}'",
            input.len(),
            shell_id
        )))
    }

    /// Read accumulated output from an interactive shell.
    ///
    /// Returns all output buffered since the last read (when `clear=true`)
    /// or all output since shell open (when `clear=false`).
    ///
    /// **Recommended workflow:**
    /// 1. ssh_shell_write to send a command
    /// 2. Wait briefly (shell needs time to produce output)
    /// 3. ssh_shell_read with clear=true to get new output
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
            .map(|shell| (shell.output.clone(), shell.status_rx.clone()))
            .ok_or_else(|| format!("No active shell with ID: {}", shell_id))?;

        let data = if clear {
            let mut buf = output_arc.lock().await;
            let data = std::mem::take(&mut *buf);
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
            .ok_or_else(|| format!("No active shell with ID: {}", shell_id))?;

        // Cancel the background reader
        shell.cancel_token.cancel();

        // Close the channel
        let writer = shell.channel_writer.lock().await;
        let _ = writer.close().await;

        info!("Closed interactive shell: {}", shell_id);

        Ok(StructuredContent(SshShellCloseResponse {
            shell_id,
            closed: true,
            message: "Shell closed successfully".to_string(),
        }))
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

            _ = cancel_token.cancelled() => {
                break;
            }

            msg = read_half.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        let mut buf = output.lock().await;
                        buf.extend_from_slice(&data);
                    }
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        let mut buf = output.lock().await;
                        buf.extend_from_slice(&data);
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                        break;
                    }
                    Some(_) => {}
                }
            }
        }
    }

    let _ = status_tx.send(ShellStatus::Closed);
}
