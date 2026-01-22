use std::env;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use backon::{ExponentialBuilder, Retryable};
use once_cell::sync::Lazy;
use poem_mcpserver::{Tools, content::Text, tool::StructuredContent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ssh2::Session;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Session metadata for tracking connection information
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionInfo {
    pub session_id: String,
    pub host: String,
    pub username: String,
    pub connected_at: String,
    /// Default timeout in seconds used for this session's connection
    pub default_timeout_secs: u64,
    /// Number of retry attempts needed to establish the connection
    pub retry_attempts: u32,
}

/// Stored session data combining metadata with the actual session
struct StoredSession {
    info: SessionInfo,
    session: Arc<Mutex<Session>>,
}

// Global storage for active SSH sessions with metadata
static SSH_SESSIONS: Lazy<Mutex<std::collections::HashMap<String, StoredSession>>> =
    Lazy::new(|| Mutex::new(std::collections::HashMap::new()));

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SshConnectResponse {
    session_id: String,
    message: String,
    authenticated: bool,
    /// Number of retry attempts needed to establish the connection
    retry_attempts: u32,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SshCommandResponse {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PortForwardingResponse {
    local_address: String,
    remote_address: String,
    active: bool,
}

pub struct McpSSHCommands;

#[Tools]
impl McpSSHCommands {
    /// Connect to an SSH server and store the session
    #[allow(clippy::too_many_arguments)]
    async fn ssh_connect(
        &self,
        address: String,
        username: String,
        password: Option<String>,
        key_path: Option<String>,
        timeout_secs: Option<u64>,
        max_retries: Option<u32>,
        retry_delay_ms: Option<u64>,
    ) -> Result<StructuredContent<SshConnectResponse>, String> {
        let timeout = resolve_connect_timeout(timeout_secs);
        let max_retries = resolve_max_retries(max_retries);
        let retry_delay_ms = resolve_retry_delay_ms(retry_delay_ms);

        info!(
            "Attempting SSH connection to {}@{} with timeout {}s, max_retries={}, retry_delay_ms={}",
            username, address, timeout, max_retries, retry_delay_ms
        );

        match connect_to_ssh_with_retry(
            &address,
            &username,
            password.as_deref(),
            key_path.as_deref(),
            timeout,
            max_retries,
            retry_delay_ms,
        )
        .await
        {
            Ok((session, retry_attempts)) => {
                let session_id = Uuid::new_v4().to_string();
                let connected_at = chrono::Utc::now().to_rfc3339();

                let session_info = SessionInfo {
                    session_id: session_id.clone(),
                    host: address.clone(),
                    username: username.clone(),
                    connected_at,
                    default_timeout_secs: timeout,
                    retry_attempts,
                };

                // Minimize lock scope: only hold the lock while inserting
                {
                    let mut sessions = SSH_SESSIONS.lock().await;
                    sessions.insert(
                        session_id.clone(),
                        StoredSession {
                            info: session_info,
                            session: Arc::new(Mutex::new(session)),
                        },
                    );
                }

                let message = if retry_attempts > 0 {
                    format!(
                        "Successfully connected to {}@{} after {} retry attempt(s)",
                        username, address, retry_attempts
                    )
                } else {
                    format!("Successfully connected to {}@{}", username, address)
                };

                Ok(StructuredContent(SshConnectResponse {
                    session_id,
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

    /// Execute a command on a connected SSH session
    async fn ssh_execute(
        &self,
        session_id: String,
        command: String,
        timeout_secs: Option<u64>,
    ) -> Result<StructuredContent<SshCommandResponse>, String> {
        let timeout = resolve_command_timeout(timeout_secs);
        info!(
            "Executing command on SSH session {} with timeout {}s: {}",
            session_id, timeout, command
        );

        // Clone Arc and release global lock immediately to avoid double mutex contention
        let session_arc = {
            let sessions = SSH_SESSIONS.lock().await;
            sessions
                .get(&session_id)
                .map(|s| s.session.clone())
                .ok_or_else(|| format!("No active SSH session with ID: {}", session_id))?
        };

        // Now hold only the session-specific lock
        let session = session_arc.lock().await;

        // Wrap command execution with timeout
        match tokio::time::timeout(
            Duration::from_secs(timeout),
            execute_ssh_command(&session, &command),
        )
        .await
        {
            Ok(result) => result.map(StructuredContent).map_err(|e| {
                error!("Command execution failed: {}", e);
                e
            }),
            Err(_) => {
                error!(
                    "Command execution timed out after {}s: {}",
                    timeout, command
                );
                Err(format!(
                    "Command execution timed out after {} seconds",
                    timeout
                ))
            }
        }
    }

    /// Setup port forwarding on an existing SSH session
    #[cfg(feature = "port_forward")]
    async fn ssh_forward(
        &self,
        session_id: String,
        local_port: u16,
        remote_address: String,
        remote_port: u16,
    ) -> Result<StructuredContent<PortForwardingResponse>, String> {
        info!(
            "Setting up port forwarding from local port {} to {}:{} using session {}",
            local_port, remote_address, remote_port, session_id
        );

        // Clone Arc and release global lock immediately
        let session_arc = {
            let sessions = SSH_SESSIONS.lock().await;
            sessions
                .get(&session_id)
                .map(|s| s.session.clone())
                .ok_or_else(|| format!("No active SSH session with ID: {}", session_id))?
        };

        let session = session_arc.lock().await;
        match setup_port_forwarding(&session, local_port, &remote_address, remote_port).await {
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

    /// Disconnect an SSH session
    async fn ssh_disconnect(&self, session_id: String) -> Result<Text<String>, String> {
        info!("Disconnecting SSH session: {}", session_id);

        let mut sessions = SSH_SESSIONS.lock().await;
        if sessions.remove(&session_id).is_some() {
            Ok(Text(format!(
                "Session {} disconnected successfully",
                session_id
            )))
        } else {
            Err(format!("No active SSH session with ID: {}", session_id))
        }
    }

    /// List all active SSH sessions
    async fn ssh_list_sessions(&self) -> StructuredContent<Vec<SessionInfo>> {
        let sessions = SSH_SESSIONS.lock().await;
        let session_infos: Vec<SessionInfo> = sessions
            .values()
            .map(|stored| stored.info.clone())
            .collect();

        StructuredContent(session_infos)
    }
}

/// Default SSH connection timeout in seconds
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;

/// Default SSH command execution timeout in seconds
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 60;

/// Default maximum retry attempts for SSH connection
const DEFAULT_MAX_RETRIES: u32 = 3;

/// Default retry delay in milliseconds
const DEFAULT_RETRY_DELAY_MS: u64 = 1000;

/// Maximum retry delay cap in seconds (10 seconds)
const MAX_RETRY_DELAY_SECS: u64 = 10;

/// Environment variable name for SSH connection timeout
const CONNECT_TIMEOUT_ENV_VAR: &str = "SSH_CONNECT_TIMEOUT";

/// Environment variable name for SSH command execution timeout
const COMMAND_TIMEOUT_ENV_VAR: &str = "SSH_COMMAND_TIMEOUT";

/// Environment variable name for SSH max retries
const MAX_RETRIES_ENV_VAR: &str = "SSH_MAX_RETRIES";

/// Environment variable name for SSH retry delay in milliseconds
const RETRY_DELAY_MS_ENV_VAR: &str = "SSH_RETRY_DELAY_MS";

/// Resolve the connection timeout value with priority: parameter -> env var -> default
fn resolve_connect_timeout(timeout_param: Option<u64>) -> u64 {
    // Priority 1: Use parameter if provided
    if let Some(timeout) = timeout_param {
        return timeout;
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_timeout) = env::var(CONNECT_TIMEOUT_ENV_VAR)
        && let Ok(timeout) = env_timeout.parse::<u64>()
    {
        return timeout;
    }

    // Priority 3: Default value
    DEFAULT_CONNECT_TIMEOUT_SECS
}

/// Resolve the command execution timeout value with priority: parameter -> env var -> default
fn resolve_command_timeout(timeout_param: Option<u64>) -> u64 {
    // Priority 1: Use parameter if provided
    if let Some(timeout) = timeout_param {
        return timeout;
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_timeout) = env::var(COMMAND_TIMEOUT_ENV_VAR)
        && let Ok(timeout) = env_timeout.parse::<u64>()
    {
        return timeout;
    }

    // Priority 3: Default value
    DEFAULT_COMMAND_TIMEOUT_SECS
}

/// Resolve the max retries value with priority: parameter -> env var -> default
fn resolve_max_retries(max_retries_param: Option<u32>) -> u32 {
    // Priority 1: Use parameter if provided
    if let Some(max_retries) = max_retries_param {
        return max_retries;
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_retries) = env::var(MAX_RETRIES_ENV_VAR)
        && let Ok(retries) = env_retries.parse::<u32>()
    {
        return retries;
    }

    // Priority 3: Default value
    DEFAULT_MAX_RETRIES
}

/// Resolve the retry delay value with priority: parameter -> env var -> default
fn resolve_retry_delay_ms(retry_delay_param: Option<u64>) -> u64 {
    // Priority 1: Use parameter if provided
    if let Some(delay) = retry_delay_param {
        return delay;
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_delay) = env::var(RETRY_DELAY_MS_ENV_VAR)
        && let Ok(delay) = env_delay.parse::<u64>()
    {
        return delay;
    }

    // Priority 3: Default value
    DEFAULT_RETRY_DELAY_MS
}

/// Check if an error is retryable (transient connection errors, not authentication failures)
fn is_retryable_error(error: &str) -> bool {
    let error_lower = error.to_lowercase();

    // Authentication failures are NOT retryable
    let auth_errors = [
        "authentication failed",
        "password authentication failed",
        "key authentication failed",
        "agent authentication failed",
        "permission denied",
        "publickey",
        "auth fail",
    ];

    for auth_err in &auth_errors {
        if error_lower.contains(auth_err) {
            return false;
        }
    }

    // Connection errors ARE retryable
    let retryable_errors = [
        "connection refused",
        "connection reset",
        "connection timed out",
        "timeout",
        "network is unreachable",
        "no route to host",
        "host is down",
        "temporary failure",
        "resource temporarily unavailable",
        "handshake failed",
        "failed to connect",
    ];

    for retryable_err in &retryable_errors {
        if error_lower.contains(retryable_err) {
            return true;
        }
    }

    // Default: retry on unknown errors (conservative approach for transient issues)
    // But if it looks like an SSH protocol error, don't retry
    !error_lower.contains("ssh")
        || error_lower.contains("timeout")
        || error_lower.contains("connect")
}

// Implementation functions for SSH operations

/// Connect to SSH with retry logic using exponential backoff with jitter.
/// Returns the session and the number of retry attempts that were made.
async fn connect_to_ssh_with_retry(
    address: &str,
    username: &str,
    password: Option<&str>,
    key_path: Option<&str>,
    timeout_secs: u64,
    max_retries: u32,
    min_delay_ms: u64,
) -> Result<(Session, u32), String> {
    // Track retry attempts using atomic counter
    let attempt_counter = AtomicU32::new(0);

    // Clone values for the retry closure
    let address = address.to_string();
    let username = username.to_string();
    let password = password.map(|s| s.to_string());
    let key_path = key_path.map(|s| s.to_string());

    let backoff = ExponentialBuilder::default()
        .with_min_delay(Duration::from_millis(min_delay_ms))
        .with_max_delay(Duration::from_secs(MAX_RETRY_DELAY_SECS))
        .with_max_times(max_retries as usize)
        .with_jitter();

    let result = (|| async {
        let current_attempt = attempt_counter.fetch_add(1, Ordering::SeqCst);

        if current_attempt > 0 {
            warn!(
                "SSH connection retry attempt {} to {}@{}",
                current_attempt, username, address
            );
        }

        connect_to_ssh(
            &address,
            &username,
            password.as_deref(),
            key_path.as_deref(),
            timeout_secs,
        )
        .await
    })
    .retry(backoff)
    .when(|e| {
        let retryable = is_retryable_error(e);
        if !retryable {
            warn!(
                "SSH connection to {}@{} failed with non-retryable error: {}",
                username, address, e
            );
        }
        retryable
    })
    .notify(|err, dur| {
        warn!(
            "SSH connection failed: {}. Retrying in {:?}",
            err, dur
        );
    })
    .await;

    let total_attempts = attempt_counter.load(Ordering::SeqCst);
    let retry_count = total_attempts.saturating_sub(1);

    match result {
        Ok(session) => {
            if retry_count > 0 {
                info!(
                    "SSH connection to {}@{} succeeded after {} retry attempt(s)",
                    username, address, retry_count
                );
            }
            Ok((session, retry_count))
        }
        Err(e) => {
            error!(
                "SSH connection to {}@{} failed after {} attempt(s). Last error: {}",
                username, address, total_attempts, e
            );
            Err(format!(
                "SSH connection failed after {} attempt(s). Last error: {}",
                total_attempts, e
            ))
        }
    }
}

async fn connect_to_ssh(
    address: &str,
    username: &str,
    password: Option<&str>,
    key_path: Option<&str>,
    timeout_secs: u64,
) -> Result<Session, String> {
    // Clone all inputs for moving into spawn_blocking
    let address = address.to_string();
    let username = username.to_string();
    let password = password.map(|s| s.to_string());
    let key_path = key_path.map(|s| s.to_string());

    // Wrap all blocking ssh2 operations in spawn_blocking
    tokio::task::spawn_blocking(move || {
        // Parse the address and connect with timeout
        let socket_addr = address
            .to_socket_addrs()
            .map_err(|e| format!("Failed to parse address: {}", e))?
            .next()
            .ok_or_else(|| format!("No valid socket address for: {}", address))?;

        let tcp = TcpStream::connect_timeout(&socket_addr, Duration::from_secs(timeout_secs))
            .map_err(|e| format!("Failed to connect (timeout {}s): {}", timeout_secs, e))?;

        let mut sess =
            Session::new().map_err(|e| format!("Failed to create SSH session: {}", e))?;

        sess.set_tcp_stream(tcp);
        sess.handshake()
            .map_err(|e| format!("SSH handshake failed: {}", e))?;

        // Authenticate with either password or key
        if let Some(password) = password {
            sess.userauth_password(&username, &password)
                .map_err(|e| format!("Password authentication failed: {}", e))?;
        } else if let Some(key_path) = key_path {
            sess.userauth_pubkey_file(&username, None, Path::new(&key_path), None)
                .map_err(|e| format!("Key authentication failed: {}", e))?;
        } else {
            // Try agent authentication
            sess.userauth_agent(&username)
                .map_err(|e| format!("Agent authentication failed: {}", e))?;
        }

        if !sess.authenticated() {
            return Err("Authentication failed".to_string());
        }

        Ok(sess)
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

async fn execute_ssh_command(sess: &Session, command: &str) -> Result<SshCommandResponse, String> {
    // Clone the session for use in spawn_blocking
    // ssh2::Session implements Clone (shares underlying state)
    let sess = sess.clone();
    let command = command.to_string();

    tokio::task::spawn_blocking(move || {
        let mut channel = sess
            .channel_session()
            .map_err(|e| format!("Failed to open channel: {}", e))?;

        channel
            .exec(&command)
            .map_err(|e| format!("Failed to execute command: {}", e))?;

        let mut stdout = String::new();
        channel
            .read_to_string(&mut stdout)
            .map_err(|e| format!("Failed to read stdout: {}", e))?;

        let mut stderr = String::new();
        channel
            .stderr()
            .read_to_string(&mut stderr)
            .map_err(|e| format!("Failed to read stderr: {}", e))?;

        let exit_code = channel
            .exit_status()
            .map_err(|e| format!("Failed to get exit status: {}", e))?;

        channel
            .wait_close()
            .map_err(|e| format!("Failed to close channel: {}", e))?;

        Ok(SshCommandResponse {
            stdout,
            stderr,
            exit_code,
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

#[cfg(feature = "port_forward")]
async fn setup_port_forwarding(
    sess: &Session,
    local_port: u16,
    remote_address: &str,
    remote_port: u16,
) -> Result<SocketAddr, String> {
    // Create a TCP listener for the local port
    let listener_addr = format!("127.0.0.1:{}", local_port);
    let listener = std::net::TcpListener::bind(&listener_addr)
        .map_err(|e| format!("Failed to bind to local port {}: {}", local_port, e))?;

    let local_addr = listener
        .local_addr()
        .map_err(|e| format!("Failed to get local address: {}", e))?;

    // Clone session for the spawned thread (ssh2::Session implements Clone)
    let sess_clone = sess.clone();
    let remote_addr_clone = remote_address.to_string();

    // Start a separate OS thread to handle port forwarding connections
    // We use std::thread because ssh2::Session is !Send
    std::thread::spawn(move || {
        debug!("Port forwarding active on {}", local_addr);

        // Set session to non-blocking mode for bidirectional I/O
        sess_clone.set_blocking(false);

        for stream in listener.incoming() {
            match stream {
                Ok(local_stream) => {
                    let client_addr = match local_stream.peer_addr() {
                        Ok(addr) => addr,
                        Err(_) => continue,
                    };

                    debug!("New connection from {} to forwarded port", client_addr);

                    // Create a channel to the remote destination
                    match sess_clone.channel_direct_tcpip(&remote_addr_clone, remote_port, None) {
                        Ok(remote_channel) => {
                            // Handle bidirectional forwarding in a separate thread
                            handle_bidirectional_forwarding(local_stream, remote_channel);
                        }
                        Err(e) => {
                            error!("Failed to create direct channel: {}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("Error accepting connection: {}", e);
                    break;
                }
            }
        }
    });

    Ok(local_addr)
}

/// Handle bidirectional forwarding between local TCP stream and remote SSH channel.
///
/// Since ssh2::Channel doesn't implement Clone and cannot be split into separate
/// read/write handles, we use a single-threaded polling approach with non-blocking I/O.
/// The session must already be set to non-blocking mode before calling this function.
#[cfg(feature = "port_forward")]
fn handle_bidirectional_forwarding(local_stream: TcpStream, mut remote_channel: ssh2::Channel) {
    std::thread::spawn(move || {
        // Set local stream to non-blocking for polling
        if let Err(e) = local_stream.set_nonblocking(true) {
            error!("Failed to set local stream to non-blocking: {}", e);
            return;
        }

        let mut local_stream = local_stream;
        let mut local_buf = [0u8; 8192];
        let mut remote_buf = [0u8; 8192];

        loop {
            let mut did_work = false;

            // Try to read from local and write to remote
            match local_stream.read(&mut local_buf) {
                Ok(0) => {
                    debug!("Local connection closed (EOF)");
                    break;
                }
                Ok(n) => {
                    did_work = true;
                    // Write to remote channel (may need multiple attempts in non-blocking mode)
                    let mut written = 0;
                    while written < n {
                        match remote_channel.write(&local_buf[written..n]) {
                            Ok(w) => written += w,
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                // Channel not ready, will retry next iteration
                                break;
                            }
                            Err(e) => {
                                debug!("Error writing to remote channel: {}", e);
                                let _ = remote_channel.close();
                                return;
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No data available from local, continue
                }
                Err(e) => {
                    debug!("Error reading from local stream: {}", e);
                    break;
                }
            }

            // Try to read from remote and write to local
            match remote_channel.read(&mut remote_buf) {
                Ok(0) => {
                    // Check if channel is at EOF
                    if remote_channel.eof() {
                        debug!("Remote channel closed (EOF)");
                        break;
                    }
                }
                Ok(n) => {
                    did_work = true;
                    // Write to local stream
                    let mut written = 0;
                    while written < n {
                        match local_stream.write(&remote_buf[written..n]) {
                            Ok(w) => written += w,
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                // Local not ready, will retry next iteration
                                break;
                            }
                            Err(e) => {
                                debug!("Error writing to local stream: {}", e);
                                let _ = remote_channel.close();
                                return;
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No data available from remote, continue
                }
                Err(e) => {
                    debug!("Error reading from remote channel: {}", e);
                    break;
                }
            }

            // If no work was done, sleep briefly to avoid busy-waiting
            if !did_work {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            // Check if remote channel is closed
            if remote_channel.eof() {
                debug!("Remote channel EOF detected");
                break;
            }
        }

        // Cleanup
        let _ = local_stream.shutdown(std::net::Shutdown::Both);
        let _ = remote_channel.close();
        let _ = remote_channel.wait_close();
        debug!("Port forwarding connection closed");
    });
}
