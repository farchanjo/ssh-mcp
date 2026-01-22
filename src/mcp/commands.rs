//! MCP SSH Commands implementation.
//!
//! This module provides the main MCP tool implementations for SSH operations:
//!
//! - `ssh_connect`: Connect to an SSH server with retry logic
//! - `ssh_execute`: Execute commands on a connected SSH session
//! - `ssh_forward`: Setup port forwarding (feature-gated)
//! - `ssh_disconnect`: Disconnect and cleanup a session
//! - `ssh_list_sessions`: List all active sessions

use std::sync::Arc;
use std::time::Duration;

use poem_mcpserver::{Tools, content::Text, tool::StructuredContent};
use russh::Disconnect;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::client::{connect_to_ssh_with_retry, execute_ssh_command};
use super::config::{
    resolve_command_timeout, resolve_compression, resolve_connect_timeout, resolve_max_retries,
    resolve_retry_delay,
};
#[cfg(feature = "port_forward")]
use super::forward::setup_port_forwarding;
use super::session::{SSH_SESSIONS, StoredSession};
use super::types::{
    PortForwardingResponse, SessionInfo, SessionListResponse, SshCommandResponse,
    SshConnectResponse,
};
use tokio::sync::Mutex;

/// MCP SSH Commands tool implementation.
///
/// This struct provides all SSH-related MCP tools for connecting to servers,
/// executing commands, and managing port forwarding.
pub struct McpSSHCommands;

#[Tools]
impl McpSSHCommands {
    /// Connect to an SSH server and store the session
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
            let handle_and_info = {
                let sessions = SSH_SESSIONS.lock().await;
                sessions
                    .get(sid)
                    .map(|s| (s.handle.clone(), s.info.clone()))
            };

            if let Some((handle_arc, info)) = handle_and_info {
                // Health check with 5 second timeout
                let health_timeout = Duration::from_secs(5);
                let now = chrono::Utc::now().to_rfc3339();

                match execute_ssh_command(&handle_arc, "echo 1", health_timeout).await {
                    Ok(response) if !response.timed_out && response.exit_code == 0 => {
                        // Update health status in storage
                        {
                            let mut sessions = SSH_SESSIONS.lock().await;
                            if let Some(stored) = sessions.get_mut(sid) {
                                stored.info.last_health_check = Some(now);
                                stored.info.healthy = Some(true);
                            }
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
                        let mut sessions = SSH_SESSIONS.lock().await;
                        sessions.remove(sid);
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

                // Minimize lock scope: only hold the lock while inserting
                {
                    let mut sessions = SSH_SESSIONS.lock().await;
                    sessions.insert(
                        new_session_id.clone(),
                        StoredSession {
                            info: session_info,
                            handle: Arc::new(Mutex::new(handle)),
                        },
                    );
                }

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
                        "Connected to {}@{}{}{}. Use session_id '{}' for subsequent commands.",
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

    /// Execute a command on a connected SSH session.
    ///
    /// On timeout, returns partial output with `timed_out: true` instead of an error.
    /// The session remains active and can be reused for subsequent commands.
    async fn ssh_execute(
        &self,
        /// Session ID returned from ssh_connect
        session_id: String,
        /// Shell command to execute on the remote server
        command: String,
        /// Command execution timeout in seconds (default: 60, env: SSH_COMMAND_TIMEOUT)
        timeout_secs: Option<u64>,
    ) -> Result<StructuredContent<SshCommandResponse>, String> {
        let timeout = resolve_command_timeout(timeout_secs);
        info!(
            "Executing command on SSH session {} with timeout {}s: {}",
            session_id,
            timeout.as_secs(),
            command
        );

        // Clone Arc and release global lock immediately
        let handle_arc = {
            let sessions = SSH_SESSIONS.lock().await;
            sessions
                .get(&session_id)
                .map(|s| s.handle.clone())
                .ok_or_else(|| format!("No active SSH session with ID: {}", session_id))?
        };

        // Execute command with timeout - timeout returns partial output, not an error
        match execute_ssh_command(&handle_arc, &command, timeout).await {
            Ok(response) => {
                if response.timed_out {
                    warn!(
                        "Command timed out after {}s but returning partial output: {}",
                        timeout.as_secs(),
                        command
                    );
                }
                Ok(StructuredContent(response))
            }
            Err(e) => {
                error!("Command execution failed: {}", e);
                Err(e)
            }
        }
    }

    /// Disconnect an SSH session and release resources
    async fn ssh_disconnect(
        &self,
        /// Session ID to disconnect
        session_id: String,
    ) -> Result<Text<String>, String> {
        info!("Disconnecting SSH session: {}", session_id);

        let mut sessions = SSH_SESSIONS.lock().await;
        if let Some(stored) = sessions.remove(&session_id) {
            // Gracefully disconnect the session
            let handle = stored.handle.lock().await;
            if let Err(e) = handle
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

    /// List all active SSH sessions with their metadata
    async fn ssh_list_sessions(&self) -> StructuredContent<SessionListResponse> {
        let health_timeout = Duration::from_secs(5);
        let mut healthy_sessions = Vec::new();
        let mut dead_session_ids = Vec::new();

        // Get all sessions and their handles
        let sessions_snapshot: Vec<_> = {
            let sessions = SSH_SESSIONS.lock().await;
            sessions
                .iter()
                .map(|(id, s)| (id.clone(), s.handle.clone(), s.info.clone()))
                .collect()
        };

        // Health check each session
        for (session_id, handle_arc, mut info) in sessions_snapshot {
            let now = chrono::Utc::now().to_rfc3339();

            match execute_ssh_command(&handle_arc, "echo 1", health_timeout).await {
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

        // Update healthy sessions and remove dead ones
        {
            let mut sessions = SSH_SESSIONS.lock().await;
            for (id, info) in &healthy_sessions {
                if let Some(stored) = sessions.get_mut(id) {
                    stored
                        .info
                        .last_health_check
                        .clone_from(&info.last_health_check);
                    stored.info.healthy = info.healthy;
                }
            }
            for id in &dead_session_ids {
                warn!("Removing dead session {} from storage", id);
                sessions.remove(id);
            }
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

            // Clone Arc and release global lock immediately
            let handle_arc = {
                let sessions = SSH_SESSIONS.lock().await;
                sessions
                    .get(&session_id)
                    .map(|s| s.handle.clone())
                    .ok_or_else(|| format!("No active SSH session with ID: {}", session_id))?
            };

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
}
