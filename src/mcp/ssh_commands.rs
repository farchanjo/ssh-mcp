use std::io::{Read, Write};
use std::net::{SocketAddr, ToSocketAddrs, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use once_cell::sync::Lazy;
use poem_mcpserver::{Tools, content::Text, tool::StructuredContent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ssh2::Session;
use tokio::sync::Mutex;
use tracing::{debug, error, info};
use uuid::Uuid;

/// Session metadata for tracking connection information
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionInfo {
    pub session_id: String,
    pub host: String,
    pub username: String,
    pub connected_at: String,
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
    async fn ssh_connect(
        &self,
        address: String,
        username: String,
        password: Option<String>,
        key_path: Option<String>,
    ) -> Result<StructuredContent<SshConnectResponse>, String> {
        info!("Attempting SSH connection to {}@{}", username, address);

        match connect_to_ssh(
            &address,
            &username,
            password.as_deref(),
            key_path.as_deref(),
        )
        .await
        {
            Ok(session) => {
                let session_id = Uuid::new_v4().to_string();
                let connected_at = chrono::Utc::now().to_rfc3339();

                let session_info = SessionInfo {
                    session_id: session_id.clone(),
                    host: address.clone(),
                    username: username.clone(),
                    connected_at,
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

                Ok(StructuredContent(SshConnectResponse {
                    session_id,
                    message: format!("Successfully connected to {}@{}", username, address),
                    authenticated: true,
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
    ) -> Result<StructuredContent<SshCommandResponse>, String> {
        info!(
            "Executing command on SSH session {}: {}",
            session_id, command
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
        execute_ssh_command(&session, &command)
            .await
            .map(StructuredContent)
            .map_err(|e| {
                error!("Command execution failed: {}", e);
                e
            })
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

// Implementation functions for SSH operations

async fn connect_to_ssh(
    address: &str,
    username: &str,
    password: Option<&str>,
    key_path: Option<&str>,
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

        let tcp = TcpStream::connect_timeout(&socket_addr, Duration::from_secs(30))
            .map_err(|e| format!("Failed to connect (timeout 30s): {}", e))?;

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
