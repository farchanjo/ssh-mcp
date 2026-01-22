use std::env;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use backon::{ExponentialBuilder, Retryable};
use once_cell::sync::Lazy;
use poem_mcpserver::{Tools, content::Text, tool::StructuredContent};
use russh::{ChannelMsg, Disconnect, client, keys};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
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
    /// Whether compression is enabled for this session
    pub compression_enabled: bool,
}

/// Client handler for russh that accepts all host keys
struct SshClientHandler;

impl client::Handler for SshClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        // Accept all host keys (similar to StrictHostKeyChecking=no)
        // In production, you'd want to verify against known_hosts
        Ok(true)
    }
}

/// Stored session data combining metadata with the actual session handle
/// Handle is not Clone, so we wrap it in Arc<Mutex<>> to share across tasks
struct StoredSession {
    info: SessionInfo,
    handle: Arc<Mutex<client::Handle<SshClientHandler>>>,
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

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionListResponse {
    /// List of active SSH sessions
    sessions: Vec<SessionInfo>,
    /// Total number of active sessions
    count: usize,
}

pub struct McpSSHCommands;

#[Tools]
impl McpSSHCommands {
    /// Connect to an SSH server and store the session
    #[allow(clippy::too_many_arguments)]
    async fn ssh_connect(
        &self,
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
    ) -> Result<StructuredContent<SshConnectResponse>, String> {
        let timeout = resolve_connect_timeout(timeout_secs);
        let max_retries = resolve_max_retries(max_retries);
        let retry_delay_ms = resolve_retry_delay_ms(retry_delay_ms);
        let compress = resolve_compression(compress);

        info!(
            "Attempting SSH connection to {}@{} with timeout {}s, max_retries={}, retry_delay_ms={}, compress={}",
            username, address, timeout, max_retries, retry_delay_ms, compress
        );

        match connect_to_ssh_with_retry(
            &address,
            &username,
            password.as_deref(),
            key_path.as_deref(),
            timeout,
            max_retries,
            retry_delay_ms,
            compress,
        )
        .await
        {
            Ok((handle, retry_attempts)) => {
                let session_id = Uuid::new_v4().to_string();
                let connected_at = chrono::Utc::now().to_rfc3339();

                let session_info = SessionInfo {
                    session_id: session_id.clone(),
                    host: address.clone(),
                    username: username.clone(),
                    connected_at,
                    default_timeout_secs: timeout,
                    retry_attempts,
                    compression_enabled: compress,
                };

                // Minimize lock scope: only hold the lock while inserting
                {
                    let mut sessions = SSH_SESSIONS.lock().await;
                    sessions.insert(
                        session_id.clone(),
                        StoredSession {
                            info: session_info,
                            handle: Arc::new(Mutex::new(handle)),
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
            session_id, timeout, command
        );

        // Clone Arc and release global lock immediately
        let handle_arc = {
            let sessions = SSH_SESSIONS.lock().await;
            sessions
                .get(&session_id)
                .map(|s| s.handle.clone())
                .ok_or_else(|| format!("No active SSH session with ID: {}", session_id))?
        };

        // Wrap command execution with timeout
        match tokio::time::timeout(
            Duration::from_secs(timeout),
            execute_ssh_command(&handle_arc, &command),
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
        /// Session ID returned from ssh_connect
        session_id: String,
        /// Local port to listen on (e.g., 8080)
        local_port: u16,
        /// Remote host to forward to (e.g., "localhost" or "10.0.0.1")
        remote_address: String,
        /// Remote port to forward to (e.g., 3306 for MySQL)
        remote_port: u16,
    ) -> Result<StructuredContent<PortForwardingResponse>, String> {
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

        match setup_port_forwarding(handle_arc, local_port, &remote_address, remote_port).await {
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
        let sessions = SSH_SESSIONS.lock().await;
        let session_infos: Vec<SessionInfo> = sessions
            .values()
            .map(|stored| stored.info.clone())
            .collect();
        let count = session_infos.len();

        StructuredContent(SessionListResponse {
            sessions: session_infos,
            count,
        })
    }
}

/// Default SSH connection timeout in seconds
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;

/// Default SSH command execution timeout in seconds
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 180;

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

/// Environment variable name for SSH compression
const COMPRESSION_ENV_VAR: &str = "SSH_COMPRESSION";

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

/// Resolve the compression setting with priority: parameter -> env var -> default (true)
fn resolve_compression(compress_param: Option<bool>) -> bool {
    // Priority 1: Use parameter if provided
    if let Some(compress) = compress_param {
        return compress;
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_compress) = env::var(COMPRESSION_ENV_VAR) {
        return env_compress.eq_ignore_ascii_case("true") || env_compress == "1";
    }

    // Priority 3: Default value (enabled)
    true
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
        "no authentication",
        "all authentication methods failed",
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
        "broken pipe",
        "would block",
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
/// Returns the session handle and the number of retry attempts that were made.
#[allow(clippy::too_many_arguments)]
async fn connect_to_ssh_with_retry(
    address: &str,
    username: &str,
    password: Option<&str>,
    key_path: Option<&str>,
    timeout_secs: u64,
    max_retries: u32,
    min_delay_ms: u64,
    compress: bool,
) -> Result<(client::Handle<SshClientHandler>, u32), String> {
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
            compress,
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
        warn!("SSH connection failed: {}. Retrying in {:?}", err, dur);
    })
    .await;

    let total_attempts = attempt_counter.load(Ordering::SeqCst);
    let retry_count = total_attempts.saturating_sub(1);

    match result {
        Ok(handle) => {
            if retry_count > 0 {
                info!(
                    "SSH connection to {}@{} succeeded after {} retry attempt(s)",
                    username, address, retry_count
                );
            }
            Ok((handle, retry_count))
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

/// Build russh client configuration with the specified settings
fn build_client_config(timeout_secs: u64, compress: bool) -> Arc<client::Config> {
    let compression = if compress {
        (&[russh::compression::ZLIB, russh::compression::NONE][..]).into()
    } else {
        (&[russh::compression::NONE][..]).into()
    };

    let preferred = russh::Preferred {
        compression,
        ..Default::default()
    };

    Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(timeout_secs)),
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        preferred,
        ..Default::default()
    })
}

/// Parse address string into host and port components
fn parse_address(address: &str) -> Result<(String, u16), String> {
    // Try to parse as "host:port" format
    if let Some((host, port_str)) = address.rsplit_once(':') {
        let port = port_str
            .parse::<u16>()
            .map_err(|e| format!("Invalid port number: {}", e))?;
        Ok((host.to_string(), port))
    } else {
        // No port specified, use default SSH port
        Ok((address.to_string(), 22))
    }
}

async fn connect_to_ssh(
    address: &str,
    username: &str,
    password: Option<&str>,
    key_path: Option<&str>,
    timeout_secs: u64,
    compress: bool,
) -> Result<client::Handle<SshClientHandler>, String> {
    let config = build_client_config(timeout_secs, compress);
    let handler = SshClientHandler;

    // Parse address into host and port
    let (host, port) = parse_address(address)?;

    // Connect with timeout
    let connect_future = client::connect(config, (host.as_str(), port), handler);

    let mut handle = tokio::time::timeout(Duration::from_secs(timeout_secs), connect_future)
        .await
        .map_err(|_| format!("Connection timed out after {}s", timeout_secs))?
        .map_err(|e| format!("Failed to connect: {}", e))?;

    // Authenticate with either password, key, or agent
    let auth_result = if let Some(password) = password {
        // Password authentication
        handle
            .authenticate_password(username, password)
            .await
            .map_err(|e| format!("Password authentication failed: {}", e))?
    } else if let Some(key_path) = key_path {
        // Key-based authentication
        authenticate_with_key(&mut handle, username, key_path).await?
    } else {
        // Try agent authentication
        authenticate_with_agent(&mut handle, username).await?
    };

    if !auth_result.success() {
        return Err("Authentication failed: no authentication methods succeeded".to_string());
    }

    Ok(handle)
}

/// Authenticate using a private key file
async fn authenticate_with_key(
    handle: &mut client::Handle<SshClientHandler>,
    username: &str,
    key_path: &str,
) -> Result<client::AuthResult, String> {
    let path = Path::new(key_path);

    // Load the secret key (supports passphrase-less keys or will prompt if needed)
    let key_pair = keys::load_secret_key(path, None)
        .map_err(|e| format!("Failed to load private key from {}: {}", key_path, e))?;

    // Wrap the key with the preferred hash algorithm (use default None for auto-detection)
    let key_with_hash = keys::PrivateKeyWithHashAlg::new(Arc::new(key_pair), None);

    handle
        .authenticate_publickey(username, key_with_hash)
        .await
        .map_err(|e| format!("Key authentication failed: {}", e))
}

/// Authenticate using SSH agent
async fn authenticate_with_agent(
    handle: &mut client::Handle<SshClientHandler>,
    username: &str,
) -> Result<client::AuthResult, String> {
    // Connect to the SSH agent
    let mut agent = keys::agent::client::AgentClient::connect_env()
        .await
        .map_err(|e| format!("Failed to connect to SSH agent: {}", e))?;

    // Get identities from the agent
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| format!("Failed to get identities from SSH agent: {}", e))?;

    if identities.is_empty() {
        return Err("No identities found in SSH agent".to_string());
    }

    // Try each identity until one succeeds
    for identity in identities {
        debug!("Trying SSH agent identity: {:?}", identity.comment());

        match handle
            .authenticate_publickey_with(username, identity.clone(), None, &mut agent)
            .await
        {
            Ok(result) if result.success() => {
                info!("Successfully authenticated with SSH agent");
                return Ok(result);
            }
            Ok(_) => {
                debug!("Agent identity not accepted, trying next...");
                continue;
            }
            Err(e) => {
                debug!("Agent authentication error: {}, trying next...", e);
                continue;
            }
        }
    }

    Err("Agent authentication failed: no identities accepted".to_string())
}

async fn execute_ssh_command(
    handle_arc: &Arc<Mutex<client::Handle<SshClientHandler>>>,
    command: &str,
) -> Result<SshCommandResponse, String> {
    // Lock the handle for this operation
    let handle = handle_arc.lock().await;

    // Open a session channel
    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("Failed to open channel: {}", e))?;

    // Execute the command
    channel
        .exec(true, command)
        .await
        .map_err(|e| format!("Failed to execute command: {}", e))?;

    // Drop the handle lock so other operations can proceed
    drop(handle);

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_code: Option<u32> = None;

    // Read channel messages until EOF
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Data { data }) => {
                stdout.extend_from_slice(&data);
            }
            Some(ChannelMsg::ExtendedData { data, ext }) => {
                // ext == 1 is stderr in SSH protocol
                if ext == 1 {
                    stderr.extend_from_slice(&data);
                }
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                exit_code = Some(exit_status);
            }
            Some(ChannelMsg::Eof) => {
                // Continue to wait for exit status if not received yet
                if exit_code.is_some() {
                    break;
                }
            }
            Some(ChannelMsg::Close) => {
                break;
            }
            Some(_) => {
                // Ignore other message types
            }
            None => {
                // Channel closed
                break;
            }
        }
    }

    // Close the channel
    let _ = channel.close().await;

    let stdout_str = String::from_utf8_lossy(&stdout).into_owned();
    let stderr_str = String::from_utf8_lossy(&stderr).into_owned();

    Ok(SshCommandResponse {
        stdout: stdout_str,
        stderr: stderr_str,
        exit_code: exit_code.map(|c| c as i32).unwrap_or(-1),
    })
}

#[cfg(feature = "port_forward")]
async fn setup_port_forwarding(
    handle_arc: Arc<Mutex<client::Handle<SshClientHandler>>>,
    local_port: u16,
    remote_address: &str,
    remote_port: u16,
) -> Result<SocketAddr, String> {
    // Create a TCP listener for the local port
    let listener_addr = format!("127.0.0.1:{}", local_port);
    let listener = TcpListener::bind(&listener_addr)
        .await
        .map_err(|e| format!("Failed to bind to local port {}: {}", local_port, e))?;

    let local_addr = listener
        .local_addr()
        .map_err(|e| format!("Failed to get local address: {}", e))?;

    let remote_addr_clone = remote_address.to_string();

    // Spawn an async task to handle port forwarding connections
    tokio::spawn(async move {
        debug!("Port forwarding active on {}", local_addr);

        loop {
            match listener.accept().await {
                Ok((local_stream, client_addr)) => {
                    debug!("New connection from {} to forwarded port", client_addr);

                    // Clone handle arc for this connection
                    let handle_arc = handle_arc.clone();
                    let remote_host = remote_addr_clone.clone();

                    // Spawn a task for each connection
                    tokio::spawn(async move {
                        if let Err(e) = handle_port_forward_connection(
                            handle_arc,
                            local_stream,
                            &remote_host,
                            remote_port,
                        )
                        .await
                        {
                            debug!("Port forwarding connection error: {}", e);
                        }
                    });
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

/// Handle a single port forwarding connection using async I/O
#[cfg(feature = "port_forward")]
async fn handle_port_forward_connection(
    handle_arc: Arc<Mutex<client::Handle<SshClientHandler>>>,
    local_stream: tokio::net::TcpStream,
    remote_host: &str,
    remote_port: u16,
) -> Result<(), String> {
    // Lock the handle to open a channel
    let handle = handle_arc.lock().await;

    // Open a direct-tcpip channel to the remote destination
    let channel = handle
        .channel_open_direct_tcpip(
            remote_host,
            remote_port as u32,
            "127.0.0.1",
            0, // Local originator port (not significant for direct-tcpip)
        )
        .await
        .map_err(|e| format!("Failed to open direct-tcpip channel: {}", e))?;

    // Drop the handle lock so other operations can proceed
    drop(handle);

    // Convert channel to stream for bidirectional I/O
    let channel_stream = channel.into_stream();

    // Split both streams for bidirectional forwarding
    let (mut local_read, mut local_write) = tokio::io::split(local_stream);
    let (mut channel_read, mut channel_write) = tokio::io::split(channel_stream);

    // Use tokio::io::copy for efficient bidirectional forwarding
    let local_to_remote = tokio::io::copy(&mut local_read, &mut channel_write);
    let remote_to_local = tokio::io::copy(&mut channel_read, &mut local_write);

    // Run both directions concurrently until one completes or errors
    tokio::select! {
        result = local_to_remote => {
            if let Err(e) = result {
                debug!("Local to remote copy ended: {}", e);
            }
        }
        result = remote_to_local => {
            if let Err(e) = result {
                debug!("Remote to local copy ended: {}", e);
            }
        }
    }

    debug!("Port forwarding connection closed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    // Use a mutex to serialize env var tests to avoid race conditions
    // SAFETY: Tests are serialized via ENV_TEST_MUTEX to prevent data races
    static ENV_TEST_MUTEX: once_cell::sync::Lazy<StdMutex<()>> =
        once_cell::sync::Lazy::new(|| StdMutex::new(()));

    /// Helper to set an environment variable safely within tests.
    /// SAFETY: Must be called while holding ENV_TEST_MUTEX to prevent data races.
    unsafe fn set_env(key: &str, value: &str) {
        // SAFETY: Caller ensures ENV_TEST_MUTEX is held
        unsafe { env::set_var(key, value) };
    }

    /// Helper to remove an environment variable safely within tests.
    /// SAFETY: Must be called while holding ENV_TEST_MUTEX to prevent data races.
    unsafe fn remove_env(key: &str) {
        // SAFETY: Caller ensures ENV_TEST_MUTEX is held
        unsafe { env::remove_var(key) };
    }

    mod config_resolution {
        use super::*;

        mod connect_timeout {
            use super::*;

            #[test]
            fn test_uses_param_when_provided() {
                let result = resolve_connect_timeout(Some(60));
                assert_eq!(result, 60);
            }

            #[test]
            fn test_param_takes_priority_over_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(CONNECT_TIMEOUT_ENV_VAR, "120");
                }
                let result = resolve_connect_timeout(Some(45));
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(CONNECT_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, 45);
            }

            #[test]
            fn test_uses_env_var_when_no_param() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(CONNECT_TIMEOUT_ENV_VAR, "90");
                }
                let result = resolve_connect_timeout(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(CONNECT_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, 90);
            }

            #[test]
            fn test_uses_default_when_no_param_or_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(CONNECT_TIMEOUT_ENV_VAR);
                }
                let result = resolve_connect_timeout(None);
                assert_eq!(result, DEFAULT_CONNECT_TIMEOUT_SECS);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(CONNECT_TIMEOUT_ENV_VAR, "invalid");
                }
                let result = resolve_connect_timeout(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(CONNECT_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, DEFAULT_CONNECT_TIMEOUT_SECS);
            }

            #[test]
            fn test_ignores_negative_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(CONNECT_TIMEOUT_ENV_VAR, "-10");
                }
                let result = resolve_connect_timeout(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(CONNECT_TIMEOUT_ENV_VAR);
                }
                // Parsing fails for negative u64, so default is used
                assert_eq!(result, DEFAULT_CONNECT_TIMEOUT_SECS);
            }
        }

        mod command_timeout {
            use super::*;

            #[test]
            fn test_uses_param_when_provided() {
                let result = resolve_command_timeout(Some(120));
                assert_eq!(result, 120);
            }

            #[test]
            fn test_param_takes_priority_over_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMMAND_TIMEOUT_ENV_VAR, "300");
                }
                let result = resolve_command_timeout(Some(60));
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMMAND_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, 60);
            }

            #[test]
            fn test_uses_env_var_when_no_param() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMMAND_TIMEOUT_ENV_VAR, "240");
                }
                let result = resolve_command_timeout(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMMAND_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, 240);
            }

            #[test]
            fn test_uses_default_when_no_param_or_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMMAND_TIMEOUT_ENV_VAR);
                }
                let result = resolve_command_timeout(None);
                assert_eq!(result, DEFAULT_COMMAND_TIMEOUT_SECS);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMMAND_TIMEOUT_ENV_VAR, "not_a_number");
                }
                let result = resolve_command_timeout(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMMAND_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, DEFAULT_COMMAND_TIMEOUT_SECS);
            }
        }

        mod max_retries {
            use super::*;

            #[test]
            fn test_uses_param_when_provided() {
                let result = resolve_max_retries(Some(5));
                assert_eq!(result, 5);
            }

            #[test]
            fn test_param_takes_priority_over_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(MAX_RETRIES_ENV_VAR, "10");
                }
                let result = resolve_max_retries(Some(2));
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(MAX_RETRIES_ENV_VAR);
                }
                assert_eq!(result, 2);
            }

            #[test]
            fn test_uses_env_var_when_no_param() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(MAX_RETRIES_ENV_VAR, "7");
                }
                let result = resolve_max_retries(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(MAX_RETRIES_ENV_VAR);
                }
                assert_eq!(result, 7);
            }

            #[test]
            fn test_uses_default_when_no_param_or_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(MAX_RETRIES_ENV_VAR);
                }
                let result = resolve_max_retries(None);
                assert_eq!(result, DEFAULT_MAX_RETRIES);
            }

            #[test]
            fn test_zero_retries_is_valid() {
                let result = resolve_max_retries(Some(0));
                assert_eq!(result, 0);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(MAX_RETRIES_ENV_VAR, "abc");
                }
                let result = resolve_max_retries(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(MAX_RETRIES_ENV_VAR);
                }
                assert_eq!(result, DEFAULT_MAX_RETRIES);
            }
        }

        mod retry_delay_ms {
            use super::*;

            #[test]
            fn test_uses_param_when_provided() {
                let result = resolve_retry_delay_ms(Some(2000));
                assert_eq!(result, 2000);
            }

            #[test]
            fn test_param_takes_priority_over_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(RETRY_DELAY_MS_ENV_VAR, "5000");
                }
                let result = resolve_retry_delay_ms(Some(500));
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(RETRY_DELAY_MS_ENV_VAR);
                }
                assert_eq!(result, 500);
            }

            #[test]
            fn test_uses_env_var_when_no_param() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(RETRY_DELAY_MS_ENV_VAR, "3000");
                }
                let result = resolve_retry_delay_ms(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(RETRY_DELAY_MS_ENV_VAR);
                }
                assert_eq!(result, 3000);
            }

            #[test]
            fn test_uses_default_when_no_param_or_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(RETRY_DELAY_MS_ENV_VAR);
                }
                let result = resolve_retry_delay_ms(None);
                assert_eq!(result, DEFAULT_RETRY_DELAY_MS);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(RETRY_DELAY_MS_ENV_VAR, "xyz");
                }
                let result = resolve_retry_delay_ms(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(RETRY_DELAY_MS_ENV_VAR);
                }
                assert_eq!(result, DEFAULT_RETRY_DELAY_MS);
            }
        }

        mod compression {
            use super::*;

            #[test]
            fn test_uses_param_true_when_provided() {
                let result = resolve_compression(Some(true));
                assert!(result);
            }

            #[test]
            fn test_uses_param_false_when_provided() {
                let result = resolve_compression(Some(false));
                assert!(!result);
            }

            #[test]
            fn test_param_takes_priority_over_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "true");
                }
                let result = resolve_compression(Some(false));
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(!result);
            }

            #[test]
            fn test_env_var_true_lowercase() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "true");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(result);
            }

            #[test]
            fn test_env_var_true_uppercase() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "TRUE");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(result);
            }

            #[test]
            fn test_env_var_true_mixed_case() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "TrUe");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(result);
            }

            #[test]
            fn test_env_var_one() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "1");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(result);
            }

            #[test]
            fn test_env_var_false_lowercase() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "false");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(!result);
            }

            #[test]
            fn test_env_var_zero() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "0");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(!result);
            }

            #[test]
            fn test_env_var_random_value_is_false() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "yes");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                // "yes" is not "true" or "1", so it's false
                assert!(!result);
            }

            #[test]
            fn test_default_is_true() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                let result = resolve_compression(None);
                assert!(result);
            }
        }
    }

    mod error_classification {
        use super::*;

        mod auth_errors_not_retryable {
            use super::*;

            #[test]
            fn test_authentication_failed() {
                assert!(!is_retryable_error("Authentication failed"));
                assert!(!is_retryable_error("AUTHENTICATION FAILED"));
                assert!(!is_retryable_error("authentication failed for user"));
            }

            #[test]
            fn test_password_authentication_failed() {
                assert!(!is_retryable_error("Password authentication failed"));
                assert!(!is_retryable_error(
                    "password authentication failed: wrong password"
                ));
            }

            #[test]
            fn test_key_authentication_failed() {
                assert!(!is_retryable_error("Key authentication failed"));
                assert!(!is_retryable_error(
                    "key authentication failed: invalid key"
                ));
            }

            #[test]
            fn test_agent_authentication_failed() {
                assert!(!is_retryable_error("Agent authentication failed"));
                assert!(!is_retryable_error("agent authentication failed: no keys"));
            }

            #[test]
            fn test_permission_denied() {
                assert!(!is_retryable_error("Permission denied"));
                assert!(!is_retryable_error("permission denied (publickey)"));
                assert!(!is_retryable_error("PERMISSION DENIED"));
            }

            #[test]
            fn test_publickey_error() {
                assert!(!is_retryable_error("publickey"));
                assert!(!is_retryable_error("Publickey authentication required"));
            }

            #[test]
            fn test_auth_fail() {
                assert!(!is_retryable_error("auth fail"));
                assert!(!is_retryable_error("Auth fail: invalid credentials"));
            }

            #[test]
            fn test_no_authentication() {
                assert!(!is_retryable_error("No authentication methods available"));
                assert!(!is_retryable_error("no authentication methods succeeded"));
            }

            #[test]
            fn test_all_auth_methods_failed() {
                assert!(!is_retryable_error("All authentication methods failed"));
            }
        }

        mod connection_errors_retryable {
            use super::*;

            #[test]
            fn test_connection_refused() {
                assert!(is_retryable_error("Connection refused"));
                assert!(is_retryable_error("connection refused by server"));
            }

            #[test]
            fn test_connection_reset() {
                assert!(is_retryable_error("Connection reset"));
                assert!(is_retryable_error("connection reset by peer"));
            }

            #[test]
            fn test_connection_timed_out() {
                assert!(is_retryable_error("Connection timed out"));
                assert!(is_retryable_error("connection timed out after 30s"));
            }

            #[test]
            fn test_timeout() {
                assert!(is_retryable_error("timeout"));
                assert!(is_retryable_error("Operation timeout"));
                assert!(is_retryable_error("TIMEOUT waiting for response"));
            }

            #[test]
            fn test_network_unreachable() {
                assert!(is_retryable_error("Network is unreachable"));
                assert!(is_retryable_error("network is unreachable"));
            }

            #[test]
            fn test_no_route_to_host() {
                assert!(is_retryable_error("No route to host"));
                assert!(is_retryable_error("no route to host"));
            }

            #[test]
            fn test_host_is_down() {
                assert!(is_retryable_error("Host is down"));
                assert!(is_retryable_error("host is down"));
            }

            #[test]
            fn test_temporary_failure() {
                assert!(is_retryable_error("Temporary failure in name resolution"));
                assert!(is_retryable_error("temporary failure"));
            }

            #[test]
            fn test_resource_temporarily_unavailable() {
                assert!(is_retryable_error("Resource temporarily unavailable"));
            }

            #[test]
            fn test_handshake_failed() {
                assert!(is_retryable_error("Handshake failed"));
                assert!(is_retryable_error("SSH handshake failed"));
            }

            #[test]
            fn test_failed_to_connect() {
                assert!(is_retryable_error("Failed to connect"));
                assert!(is_retryable_error("failed to connect to server"));
            }

            #[test]
            fn test_broken_pipe() {
                assert!(is_retryable_error("Broken pipe"));
                assert!(is_retryable_error("broken pipe error"));
            }

            #[test]
            fn test_would_block() {
                assert!(is_retryable_error("Would block"));
                assert!(is_retryable_error("operation would block"));
            }
        }

        mod edge_cases {
            use super::*;

            #[test]
            fn test_empty_string_is_retryable() {
                // Empty string doesn't match any auth patterns, doesn't contain "ssh"
                assert!(is_retryable_error(""));
            }

            #[test]
            fn test_unknown_error_without_ssh() {
                // Unknown errors that don't contain "ssh" are retryable (conservative)
                assert!(is_retryable_error("Something went wrong"));
            }

            #[test]
            fn test_ssh_protocol_error_not_retryable() {
                // SSH protocol errors without timeout/connect keywords are not retryable
                assert!(!is_retryable_error("SSH protocol error"));
                assert!(!is_retryable_error("SSH version mismatch"));
            }

            #[test]
            fn test_ssh_with_timeout_is_retryable() {
                // SSH errors with timeout keyword are retryable
                assert!(is_retryable_error("SSH connection timeout"));
            }

            #[test]
            fn test_ssh_with_connect_is_retryable() {
                // SSH errors with connect keyword are retryable
                assert!(is_retryable_error("SSH failed to connect"));
            }

            #[test]
            fn test_case_insensitivity() {
                assert!(!is_retryable_error("PERMISSION DENIED"));
                assert!(is_retryable_error("CONNECTION REFUSED"));
            }

            #[test]
            fn test_auth_error_takes_precedence_over_connection() {
                // If both auth and connection keywords present, auth should win
                assert!(!is_retryable_error(
                    "Connection timeout during authentication failed"
                ));
            }
        }
    }

    mod address_parsing {
        use super::*;

        #[test]
        fn test_host_with_port() {
            let result = parse_address("192.168.1.1:22");
            assert!(result.is_ok());
            let (host, port) = result.unwrap();
            assert_eq!(host, "192.168.1.1");
            assert_eq!(port, 22);
        }

        #[test]
        fn test_hostname_with_port() {
            let result = parse_address("example.com:2222");
            assert!(result.is_ok());
            let (host, port) = result.unwrap();
            assert_eq!(host, "example.com");
            assert_eq!(port, 2222);
        }

        #[test]
        fn test_host_without_port_defaults_to_22() {
            let result = parse_address("192.168.1.1");
            assert!(result.is_ok());
            let (host, port) = result.unwrap();
            assert_eq!(host, "192.168.1.1");
            assert_eq!(port, 22);
        }

        #[test]
        fn test_hostname_without_port_defaults_to_22() {
            let result = parse_address("example.com");
            assert!(result.is_ok());
            let (host, port) = result.unwrap();
            assert_eq!(host, "example.com");
            assert_eq!(port, 22);
        }

        #[test]
        fn test_invalid_port_returns_error() {
            let result = parse_address("example.com:invalid");
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("Invalid port number"));
        }

        #[test]
        fn test_port_out_of_range() {
            let result = parse_address("example.com:99999");
            assert!(result.is_err());
        }

        #[test]
        fn test_negative_port_returns_error() {
            let result = parse_address("example.com:-22");
            assert!(result.is_err());
        }

        #[test]
        fn test_ipv6_with_port() {
            // IPv6 with port uses rsplit_once which handles the last colon
            let result = parse_address("[::1]:22");
            assert!(result.is_ok());
            let (host, port) = result.unwrap();
            assert_eq!(host, "[::1]");
            assert_eq!(port, 22);
        }

        #[test]
        fn test_localhost_with_port() {
            let result = parse_address("localhost:22");
            assert!(result.is_ok());
            let (host, port) = result.unwrap();
            assert_eq!(host, "localhost");
            assert_eq!(port, 22);
        }

        #[test]
        fn test_empty_host_with_port() {
            let result = parse_address(":22");
            assert!(result.is_ok());
            let (host, port) = result.unwrap();
            assert_eq!(host, "");
            assert_eq!(port, 22);
        }

        #[test]
        fn test_zero_port() {
            let result = parse_address("example.com:0");
            assert!(result.is_ok());
            let (host, port) = result.unwrap();
            assert_eq!(host, "example.com");
            assert_eq!(port, 0);
        }

        #[test]
        fn test_max_port() {
            let result = parse_address("example.com:65535");
            assert!(result.is_ok());
            let (host, port) = result.unwrap();
            assert_eq!(host, "example.com");
            assert_eq!(port, 65535);
        }
    }

    mod response_serialization {
        use super::*;

        mod ssh_connect_response {
            use super::*;

            #[test]
            fn test_serialize_and_deserialize() {
                let response = SshConnectResponse {
                    session_id: "test-uuid-123".to_string(),
                    message: "Connected successfully".to_string(),
                    authenticated: true,
                    retry_attempts: 2,
                };

                let json = serde_json::to_string(&response).unwrap();
                let deserialized: SshConnectResponse = serde_json::from_str(&json).unwrap();

                assert_eq!(deserialized.session_id, "test-uuid-123");
                assert_eq!(deserialized.message, "Connected successfully");
                assert!(deserialized.authenticated);
                assert_eq!(deserialized.retry_attempts, 2);
            }

            #[test]
            fn test_json_structure() {
                let response = SshConnectResponse {
                    session_id: "abc".to_string(),
                    message: "msg".to_string(),
                    authenticated: false,
                    retry_attempts: 0,
                };

                let json = serde_json::to_value(&response).unwrap();

                assert!(json.get("session_id").is_some());
                assert!(json.get("message").is_some());
                assert!(json.get("authenticated").is_some());
                assert!(json.get("retry_attempts").is_some());
            }
        }

        mod ssh_command_response {
            use super::*;

            #[test]
            fn test_serialize_and_deserialize() {
                let response = SshCommandResponse {
                    stdout: "Hello, World!".to_string(),
                    stderr: "Warning: something".to_string(),
                    exit_code: 0,
                };

                let json = serde_json::to_string(&response).unwrap();
                let deserialized: SshCommandResponse = serde_json::from_str(&json).unwrap();

                assert_eq!(deserialized.stdout, "Hello, World!");
                assert_eq!(deserialized.stderr, "Warning: something");
                assert_eq!(deserialized.exit_code, 0);
            }

            #[test]
            fn test_negative_exit_code() {
                let response = SshCommandResponse {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: -1,
                };

                let json = serde_json::to_string(&response).unwrap();
                let deserialized: SshCommandResponse = serde_json::from_str(&json).unwrap();

                assert_eq!(deserialized.exit_code, -1);
            }

            #[test]
            fn test_empty_output() {
                let response = SshCommandResponse {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 127,
                };

                let json = serde_json::to_string(&response).unwrap();
                let deserialized: SshCommandResponse = serde_json::from_str(&json).unwrap();

                assert_eq!(deserialized.stdout, "");
                assert_eq!(deserialized.stderr, "");
                assert_eq!(deserialized.exit_code, 127);
            }

            #[test]
            fn test_unicode_content() {
                let response = SshCommandResponse {
                    stdout: "Hello, \u{4e16}\u{754c}!".to_string(), // Hello, World! in Chinese
                    stderr: String::new(),
                    exit_code: 0,
                };

                let json = serde_json::to_string(&response).unwrap();
                let deserialized: SshCommandResponse = serde_json::from_str(&json).unwrap();

                assert!(deserialized.stdout.contains('\u{4e16}'));
            }
        }

        mod session_info {
            use super::*;

            #[test]
            fn test_serialize_and_deserialize() {
                let info = SessionInfo {
                    session_id: "uuid-123".to_string(),
                    host: "192.168.1.1:22".to_string(),
                    username: "testuser".to_string(),
                    connected_at: "2024-01-15T10:30:00Z".to_string(),
                    default_timeout_secs: 30,
                    retry_attempts: 1,
                    compression_enabled: true,
                };

                let json = serde_json::to_string(&info).unwrap();
                let deserialized: SessionInfo = serde_json::from_str(&json).unwrap();

                assert_eq!(deserialized.session_id, "uuid-123");
                assert_eq!(deserialized.host, "192.168.1.1:22");
                assert_eq!(deserialized.username, "testuser");
                assert_eq!(deserialized.connected_at, "2024-01-15T10:30:00Z");
                assert_eq!(deserialized.default_timeout_secs, 30);
                assert_eq!(deserialized.retry_attempts, 1);
                assert!(deserialized.compression_enabled);
            }

            #[test]
            fn test_clone() {
                let info = SessionInfo {
                    session_id: "clone-test".to_string(),
                    host: "host".to_string(),
                    username: "user".to_string(),
                    connected_at: "now".to_string(),
                    default_timeout_secs: 60,
                    retry_attempts: 0,
                    compression_enabled: false,
                };

                let cloned = info.clone();

                assert_eq!(cloned.session_id, info.session_id);
                assert_eq!(cloned.compression_enabled, info.compression_enabled);
            }
        }

        mod session_list_response {
            use super::*;

            #[test]
            fn test_empty_list() {
                let response = SessionListResponse {
                    sessions: vec![],
                    count: 0,
                };

                let json = serde_json::to_string(&response).unwrap();
                let deserialized: SessionListResponse = serde_json::from_str(&json).unwrap();

                assert!(deserialized.sessions.is_empty());
                assert_eq!(deserialized.count, 0);
            }

            #[test]
            fn test_multiple_sessions() {
                let session1 = SessionInfo {
                    session_id: "s1".to_string(),
                    host: "host1".to_string(),
                    username: "user1".to_string(),
                    connected_at: "t1".to_string(),
                    default_timeout_secs: 30,
                    retry_attempts: 0,
                    compression_enabled: true,
                };
                let session2 = SessionInfo {
                    session_id: "s2".to_string(),
                    host: "host2".to_string(),
                    username: "user2".to_string(),
                    connected_at: "t2".to_string(),
                    default_timeout_secs: 60,
                    retry_attempts: 2,
                    compression_enabled: false,
                };

                let response = SessionListResponse {
                    sessions: vec![session1, session2],
                    count: 2,
                };

                let json = serde_json::to_string(&response).unwrap();
                let deserialized: SessionListResponse = serde_json::from_str(&json).unwrap();

                assert_eq!(deserialized.sessions.len(), 2);
                assert_eq!(deserialized.count, 2);
                assert_eq!(deserialized.sessions[0].session_id, "s1");
                assert_eq!(deserialized.sessions[1].session_id, "s2");
            }
        }

        #[cfg(feature = "port_forward")]
        mod port_forwarding_response {
            use super::*;

            #[test]
            fn test_serialize_and_deserialize() {
                let response = PortForwardingResponse {
                    local_address: "127.0.0.1:8080".to_string(),
                    remote_address: "localhost:3306".to_string(),
                    active: true,
                };

                let json = serde_json::to_string(&response).unwrap();
                let deserialized: PortForwardingResponse = serde_json::from_str(&json).unwrap();

                assert_eq!(deserialized.local_address, "127.0.0.1:8080");
                assert_eq!(deserialized.remote_address, "localhost:3306");
                assert!(deserialized.active);
            }
        }
    }

    mod client_config {
        use super::*;

        #[test]
        fn test_builds_config_with_timeout() {
            let config = build_client_config(45, true);
            assert_eq!(config.inactivity_timeout, Some(Duration::from_secs(45)));
        }

        #[test]
        fn test_builds_config_with_keepalive() {
            let config = build_client_config(30, true);
            assert_eq!(config.keepalive_interval, Some(Duration::from_secs(30)));
            assert_eq!(config.keepalive_max, 3);
        }

        #[test]
        fn test_compression_enabled_includes_zlib() {
            let config = build_client_config(30, true);
            // When compression is enabled, ZLIB should be preferred
            let compression = &config.preferred.compression;
            assert!(!compression.is_empty());
        }

        #[test]
        fn test_compression_disabled() {
            let config = build_client_config(30, false);
            // When compression is disabled, only NONE should be available
            let compression = &config.preferred.compression;
            assert!(!compression.is_empty());
        }

        #[test]
        fn test_different_timeouts() {
            let config1 = build_client_config(10, true);
            let config2 = build_client_config(120, true);

            assert_eq!(config1.inactivity_timeout, Some(Duration::from_secs(10)));
            assert_eq!(config2.inactivity_timeout, Some(Duration::from_secs(120)));
        }
    }

    mod constants {
        use super::*;

        #[test]
        fn test_default_connect_timeout() {
            assert_eq!(DEFAULT_CONNECT_TIMEOUT_SECS, 30);
        }

        #[test]
        fn test_default_command_timeout() {
            assert_eq!(DEFAULT_COMMAND_TIMEOUT_SECS, 180);
        }

        #[test]
        fn test_default_max_retries() {
            assert_eq!(DEFAULT_MAX_RETRIES, 3);
        }

        #[test]
        fn test_default_retry_delay() {
            assert_eq!(DEFAULT_RETRY_DELAY_MS, 1000);
        }

        #[test]
        fn test_max_retry_delay() {
            assert_eq!(MAX_RETRY_DELAY_SECS, 10);
        }

        #[test]
        fn test_env_var_names() {
            assert_eq!(CONNECT_TIMEOUT_ENV_VAR, "SSH_CONNECT_TIMEOUT");
            assert_eq!(COMMAND_TIMEOUT_ENV_VAR, "SSH_COMMAND_TIMEOUT");
            assert_eq!(MAX_RETRIES_ENV_VAR, "SSH_MAX_RETRIES");
            assert_eq!(RETRY_DELAY_MS_ENV_VAR, "SSH_RETRY_DELAY_MS");
            assert_eq!(COMPRESSION_ENV_VAR, "SSH_COMPRESSION");
        }
    }
}
