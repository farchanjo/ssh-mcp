//! SSH client connection and authentication logic.
//!
//! This module handles the core SSH connection lifecycle including:
//!
//! ## Connection Lifecycle
//!
//! 1. **Address Parsing**: Parse the server address into host and port components.
//!    Supports `host:port` format with default port 22 if not specified.
//!
//! 2. **Client Configuration**: Build the russh client configuration with timeout,
//!    keepalive, and compression settings.
//!
//! 3. **Connection Establishment**: Establish TCP connection to the SSH server
//!    with configurable timeout.
//!
//! 4. **Authentication**: Authenticate using one of:
//!    - Password authentication
//!    - Private key file authentication (explicit path)
//!    - Default `~/.ssh/` keys (auto-discovered when no key path is given)
//!    - SSH agent authentication (always tried as final fallback)
//!
//! 5. **Command Execution**: Execute commands on established sessions and
//!    collect stdout, stderr, and exit code.
//!
//! ## Retry Strategy
//!
//! Connection attempts use exponential backoff with jitter via the `backon` crate:
//!
//! - **Initial delay**: Configurable via `min_delay_ms` parameter (default: 1000ms)
//! - **Maximum delay**: Capped at [`MAX_RETRY_DELAY_SECS`] (10 seconds)
//! - **Maximum attempts**: Configurable via `max_retries` parameter (default: 3)
//! - **Jitter**: Random jitter is added to prevent thundering herd
//!
//! ### Retryable vs Non-Retryable Errors
//!
//! - **Retryable**: Connection refused, timeout, network unreachable, broken pipe
//! - **Non-retryable**: Authentication failures, permission denied, invalid credentials
//!
//! Authentication failures are never retried to avoid account lockouts.

use std::env;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use backon::{ExponentialBuilder, Retryable};
use bytes::Bytes;
use russh::compression;
use russh::{Channel, ChannelMsg, Preferred, client};
use tokio::time;
use tracing::{error, info, warn};

use crate::mcp::async_command::{OutputBuffer, OutputChunk, RunningCommand};
use crate::mcp::auth::chain::AuthChain;
use crate::mcp::auth::traits::AuthStrategy;
use crate::mcp::config::MAX_RETRY_DELAY;
use crate::mcp::error::is_retryable_error;
use crate::mcp::session::SshClientHandler;
use crate::mcp::subscription::{ResourceKind, SUBSCRIPTION_REGISTRY};
use crate::mcp::types::{AsyncCommandStatus, SshCommandResponse};

/// Build russh client configuration with the specified settings.
///
/// Creates an `Arc<client::Config>` with:
/// - Inactivity timeout from dedicated parameter (or `None` if `persistent` is true)
/// - Keepalive interval of 30 seconds with max 3 keepalives
/// - Compression preference based on `compress` flag (ZLIB if enabled, NONE if disabled)
///
/// # Arguments
///
/// * `inactivity_timeout` - Session inactivity timeout (ignored if `persistent` is true)
/// * `compress` - Whether to enable zlib compression
/// * `persistent` - If true, disables inactivity timeout to keep the session open indefinitely
///
/// # Examples
///
/// ```ignore
/// let config = build_client_config(Duration::from_secs(300), true, false);
/// assert_eq!(config.inactivity_timeout, Some(Duration::from_secs(300)));
///
/// let persistent_config = build_client_config(Duration::from_secs(300), true, true);
/// assert_eq!(persistent_config.inactivity_timeout, None);
/// ```
pub fn build_client_config(
    inactivity_timeout: Duration,
    compress: bool,
    persistent: bool,
) -> Arc<client::Config> {
    let compression = if compress {
        (&[compression::ZLIB, compression::NONE][..]).into()
    } else {
        (&[compression::NONE][..]).into()
    };

    let preferred = Preferred {
        compression,
        ..Default::default()
    };

    let timeout = if persistent {
        None
    } else {
        Some(inactivity_timeout)
    };

    Arc::new(client::Config {
        inactivity_timeout: timeout,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        preferred,
        ..Default::default()
    })
}

/// Parse address string into host and port components.
///
/// Supports the following formats:
/// - `host:port` - Returns the specified host and port
/// - `host` - Returns the host with default SSH port (22)
///
/// Uses `rsplit_once` to handle IPv6 addresses correctly (e.g., `[::1]:22`).
///
/// # Arguments
///
/// * `address` - Address string in `host:port` or `host` format
///
/// # Returns
///
/// * `Ok((host, port))` - Parsed host string and port number
/// * `Err(message)` - Error message if port parsing fails
///
/// # Examples
///
/// ```ignore
/// let (host, port) = parse_address("example.com:2222")?;
/// assert_eq!(host, "example.com");
/// assert_eq!(port, 2222);
///
/// let (host, port) = parse_address("192.168.1.1")?;
/// assert_eq!(port, 22); // Default port
/// ```
pub fn parse_address(address: &str) -> Result<(String, u16), String> {
    // Try to parse as "host:port" format
    if let Some((host, port_str)) = address.rsplit_once(':') {
        let port = port_str
            .parse::<u16>()
            .map_err(|e| format!("Invalid port number: {e}"))?;
        Ok((host.to_string(), port))
    } else {
        // No port specified, use default SSH port
        Ok((address.to_string(), 22))
    }
}

/// Connect to SSH with retry logic using exponential backoff with jitter.
///
/// Attempts to establish an SSH connection with automatic retries for transient
/// errors. Authentication failures are never retried.
///
/// # Arguments
///
/// * `address` - Server address in `host:port` format
/// * `username` - SSH username for authentication
/// * `password` - Optional password for password authentication
/// * `key_path` - Optional path to private key file
/// * `timeout` - Connection timeout duration
/// * `inactivity_timeout` - Session inactivity timeout duration
/// * `max_retries` - Maximum number of retry attempts
/// * `min_delay` - Initial delay between retries
/// * `compress` - Whether to enable compression
/// * `persistent` - If true, disables inactivity timeout to keep the session open indefinitely
///
/// # Returns
///
/// * `Ok((handle, retry_count))` - Session handle and number of retries needed
/// * `Err(message)` - Error message describing the failure
///
/// # Retry Behavior
///
/// - Uses exponential backoff starting from `min_delay`
/// - Caps maximum delay at [`MAX_RETRY_DELAY`]
/// - Adds random jitter to prevent thundering herd
/// - Only retries on transient connection errors (not auth failures)
#[allow(
    clippy::too_many_arguments,
    reason = "SSH connection requires many configuration parameters"
)]
pub async fn connect_to_ssh_with_retry(
    address: &str,
    username: &str,
    password: Option<&str>,
    key_path: Option<&str>,
    timeout: Duration,
    inactivity_timeout: Duration,
    max_retries: u32,
    min_delay: Duration,
    compress: bool,
    persistent: bool,
) -> Result<(client::Handle<SshClientHandler>, u32), String> {
    let attempt_counter = AtomicU32::new(0);
    let address = address.to_string();
    let username = username.to_string();
    let password = password.map(ToString::to_string);
    let key_path = key_path.map(ToString::to_string);
    let backoff = build_backoff(min_delay, max_retries);

    let result = execute_with_retry(
        &attempt_counter,
        &address,
        &username,
        password.as_deref(),
        key_path.as_deref(),
        timeout,
        inactivity_timeout,
        compress,
        persistent,
        backoff,
    )
    .await;

    let total_attempts = attempt_counter.load(Ordering::SeqCst);
    let retry_count = total_attempts.saturating_sub(1);
    interpret_retry_result(result, &address, &username, retry_count, total_attempts)
}

/// Build an exponential backoff strategy for SSH connection retries.
fn build_backoff(min_delay: Duration, max_retries: u32) -> ExponentialBuilder {
    ExponentialBuilder::default()
        .with_min_delay(min_delay)
        .with_max_delay(MAX_RETRY_DELAY)
        .with_max_times(usize::try_from(max_retries).unwrap_or(usize::MAX))
        .with_jitter()
}

/// Execute an SSH connection attempt with retry logic.
#[allow(
    clippy::too_many_arguments,
    reason = "retry wrapper needs connection params plus retry state"
)]
async fn execute_with_retry(
    attempt_counter: &AtomicU32,
    address: &str,
    username: &str,
    password: Option<&str>,
    key_path: Option<&str>,
    timeout: Duration,
    inactivity_timeout: Duration,
    compress: bool,
    persistent: bool,
    backoff: ExponentialBuilder,
) -> Result<client::Handle<SshClientHandler>, String> {
    (|| async {
        let current_attempt = attempt_counter.fetch_add(1, Ordering::SeqCst);
        if current_attempt > 0 {
            warn!("SSH connection retry attempt {current_attempt} to {username}@{address}");
        }
        connect_to_ssh(
            address,
            username,
            password,
            key_path,
            timeout,
            inactivity_timeout,
            compress,
            persistent,
        )
        .await
    })
    .retry(backoff)
    .when(|e| {
        let retryable = is_retryable_error(e);
        if !retryable {
            warn!("SSH connection to {username}@{address} failed with non-retryable error: {e}");
        }
        retryable
    })
    .notify(|err, dur| warn!("SSH connection failed: {err}. Retrying in {dur:?}"))
    .await
}

/// Interpret the result of a retried SSH connection attempt.
fn interpret_retry_result(
    result: Result<client::Handle<SshClientHandler>, String>,
    address: &str,
    username: &str,
    retry_count: u32,
    total_attempts: u32,
) -> Result<(client::Handle<SshClientHandler>, u32), String> {
    match result {
        Ok(handle) => {
            if retry_count > 0 {
                info!(
                    "SSH connection to {username}@{address} succeeded after {retry_count} retry attempt(s)"
                );
            }
            Ok((handle, retry_count))
        }
        Err(e) => {
            error!(
                "SSH connection to {username}@{address} failed after {total_attempts} attempt(s). Last error: {e}"
            );
            Err(format!(
                "SSH connection failed after {total_attempts} attempt(s). Last error: {e}"
            ))
        }
    }
}

/// Establish an SSH connection and authenticate.
///
/// This is the core connection function that:
/// 1. Builds client configuration
/// 2. Parses the address
/// 3. Connects with timeout
/// 4. Authenticates using the appropriate method via [`AuthChain`]
#[allow(
    clippy::too_many_arguments,
    reason = "SSH connection requires many configuration parameters"
)]
async fn connect_to_ssh(
    address: &str,
    username: &str,
    password: Option<&str>,
    key_path: Option<&str>,
    timeout: Duration,
    inactivity_timeout: Duration,
    compress: bool,
    persistent: bool,
) -> Result<client::Handle<SshClientHandler>, String> {
    let config = build_client_config(inactivity_timeout, compress, persistent);
    let handler = SshClientHandler;

    // Parse address into host and port
    let (host, port) = parse_address(address)?;

    // Connect with timeout
    let connect_future = client::connect(config, (host.as_str(), port), handler);

    let mut handle = time::timeout(timeout, connect_future)
        .await
        .map_err(|_elapsed| format!("Connection timed out after {timeout:?}"))?
        .map_err(|e| format!("Failed to connect: {e}"))?;

    // Build authentication chain based on provided credentials
    let auth_chain = build_auth_chain(password, key_path);

    // Authenticate using the chain
    let success = auth_chain.authenticate(&mut handle, username).await?;

    if !success {
        return Err("Authentication failed: no authentication methods succeeded".to_string());
    }

    Ok(handle)
}

/// Default SSH key filenames, in the order OpenSSH tries them.
const DEFAULT_KEY_NAMES: &[&str] = &[
    "id_ed25519",
    "id_ecdsa",
    "id_ecdsa_sk",
    "id_ed25519_sk",
    "id_rsa",
    "id_dsa",
];

/// Discover SSH private keys in `~/.ssh/` following OpenSSH defaults.
///
/// Returns the paths of all default key files that exist on disk.
fn discover_default_keys() -> Vec<String> {
    let Some(home) = env::var("HOME").or_else(|_| env::var("USERPROFILE")).ok() else {
        return Vec::new();
    };

    let ssh_dir = Path::new(&home).join(".ssh");
    DEFAULT_KEY_NAMES
        .iter()
        .map(|name| ssh_dir.join(name))
        .filter(|p| p.is_file())
        .filter_map(|p| p.to_str().map(String::from))
        .collect()
}

/// Build an authentication chain based on the provided credentials.
///
/// The chain is built with the following priority:
/// 1. Password authentication (if password is provided)
/// 2. Key-based authentication (if `key_path` is provided)
/// 3. Default `~/.ssh/` keys (if no explicit key was given)
/// 4. SSH agent authentication (always added as final fallback)
fn build_auth_chain(password: Option<&str>, key_path: Option<&str>) -> AuthChain {
    let mut chain = AuthChain::new();

    if let Some(password) = password {
        chain = chain.with_password(password);
    }

    if let Some(key_path) = key_path {
        chain = chain.with_key(key_path);
    } else {
        for path in discover_default_keys() {
            chain = chain.with_key(path);
        }
    }

    // Always try SSH agent as final fallback
    chain = chain.with_agent();

    chain
}

/// Execute a command on an SSH session with timeout support.
///
/// Opens a session channel, executes the command, and collects the output.
/// If the command times out, returns partial output with `timed_out: true`
/// instead of an error, keeping the session alive.
///
/// # Arguments
///
/// * `handle_arc` - Shared handle to the SSH session
/// * `command` - Shell command to execute
/// * `timeout` - Command execution timeout duration
///
/// # Returns
///
/// * `Ok(SshCommandResponse)` - Command output with stdout, stderr, exit code, and timeout flag
/// * `Err(message)` - Error message if execution fails (NOT for timeouts)
///
/// # Timeout Behavior
///
/// On timeout, the function:
/// 1. Returns partial stdout/stderr collected so far
/// 2. Sets `timed_out: true` in the response
/// 3. Sets `exit_code: -1` (no exit code available)
/// 4. Closes the channel gracefully to keep the session alive
///
/// # Exit Code
///
/// Returns -1 as exit code if the remote server doesn't provide one or on timeout.
pub async fn execute_ssh_command(
    handle_arc: &Arc<client::Handle<SshClientHandler>>,
    command: &str,
    timeout: Duration,
) -> Result<SshCommandResponse, String> {
    let mut channel = open_and_exec_command(handle_arc, command).await?;

    // Pre-allocate buffers to reduce reallocations during output collection
    let mut stdout = Vec::with_capacity(4096);
    let mut stderr = Vec::with_capacity(1024);
    let mut exit_code: Option<u32> = None;
    let mut timed_out = false;

    // Read channel messages with timeout
    let result = time::timeout(
        timeout,
        collect_sync_output(&mut channel, &mut stdout, &mut stderr, &mut exit_code),
    )
    .await;

    // Handle timeout - return partial output, don't treat as error
    if result.is_err() {
        timed_out = true;
        warn!(
            "Command timed out after {timeout:?}, returning partial output ({} bytes stdout, {} bytes stderr)",
            stdout.len(),
            stderr.len()
        );
    }

    // Always close the channel gracefully to keep the session alive
    let _ = channel.close().await;

    let stdout_str = String::from_utf8_lossy(&stdout).into_owned();
    let stderr_str = String::from_utf8_lossy(&stderr).into_owned();

    Ok(SshCommandResponse {
        stdout: stdout_str,
        stderr: stderr_str,
        exit_code: exit_code.map_or(-1, u32::cast_signed),
        timed_out,
    })
}

/// Open a session channel and execute a command on it.
///
/// Helper that opens a session channel and issues the exec request.
async fn open_and_exec_command(
    handle_arc: &Arc<client::Handle<SshClientHandler>>,
    command: &str,
) -> Result<Channel<client::Msg>, String> {
    let channel = handle_arc
        .channel_open_session()
        .await
        .map_err(|e| format!("Failed to open channel: {e}"))?;

    channel
        .exec(true, command)
        .await
        .map_err(|e| format!("Failed to execute command: {e}"))?;

    Ok(channel)
}

/// Collect output from a channel synchronously into provided buffers.
///
/// Reads channel messages until the channel closes, collecting stdout,
/// stderr, and exit code.
async fn collect_sync_output(
    channel: &mut Channel<client::Msg>,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    exit_code: &mut Option<u32>,
) {
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
                *exit_code = Some(exit_status);
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
}

/// Execute a command asynchronously on an SSH session.
///
/// This function runs in a background task and collects output incrementally
/// into the lock-free state stored in `RunningCommand`. Subscribers see
/// updates two ways:
///
/// - `command.output_history` (`ArcSwap<OutputBuffer>`) holds the latest
///   snapshot for new readers and recovery from broadcast lag.
/// - `command.output_tx` broadcasts each delta as `OutputChunk`, terminating
///   with `OutputChunk::Closed(exit_code)`.
pub async fn execute_ssh_command_async(
    handle: Arc<client::Handle<SshClientHandler>>,
    command_text: String,
    timeout: Duration,
    command: Arc<RunningCommand>,
) {
    let Some(mut channel) = open_async_channel(&handle, &command).await else {
        return;
    };

    if exec_async_command(&channel, &command_text, &command).await == Err(()) {
        return;
    }

    run_with_cancellation_and_timeout(&mut channel, &command_text, timeout, &command).await;
}

/// Open a session channel for async command execution.
///
/// Returns `None` and sets the error/status if the channel cannot be opened.
///
/// Retries transient `ConnectFailed` errors with short backoff. OpenSSH's
/// default `MaxSessions=10` can refuse new channels under rapid
/// `execute+cancel` bursts because the server needs a small window to
/// reclaim channel slots after a `CHANNEL_CLOSE`. The retry loop lets the
/// client wait for slot reclamation instead of surfacing a transient
/// failure as a permanent `Failed` command.
async fn open_async_channel(
    handle: &Arc<client::Handle<SshClientHandler>>,
    command: &Arc<RunningCommand>,
) -> Option<Channel<client::Msg>> {
    const MAX_ATTEMPTS: u32 = 25;
    let mut attempt = 0_u32;
    loop {
        if command.cancel_token.is_cancelled() {
            broadcast_close(command, None);
            let _ = command.status_tx.send(AsyncCommandStatus::Cancelled);
            return None;
        }
        attempt += 1;
        match handle.channel_open_session().await {
            Ok(ch) => {
                if attempt > 1 {
                    info!("channel_open_session succeeded on attempt {attempt}");
                }
                return Some(ch);
            }
            Err(e) => {
                let msg = e.to_string();
                warn!("channel_open_session attempt {attempt} failed: {msg}");
                if is_transient_open_error(&msg) && attempt < MAX_ATTEMPTS {
                    if wait_with_cancel(&command.cancel_token, attempt).await {
                        broadcast_close(command, None);
                        let _ = command.status_tx.send(AsyncCommandStatus::Cancelled);
                        return None;
                    }
                    continue;
                }
                let _ = command.error.set(format!("Failed to open channel: {e}"));
                broadcast_close(command, None);
                let _ = command.status_tx.send(AsyncCommandStatus::Failed);
                return None;
            }
        }
    }
}

/// Classify a `channel_open_session` error as transient (slot may reopen soon).
fn is_transient_open_error(msg: &str) -> bool {
    // OpenSSH surfaces `MaxSessions` exhaustion as `ConnectFailed` /
    // `Administratively prohibited`. `RESOURCE_SHORTAGE` is the RFC label.
    msg.contains("ConnectFailed")
        || msg.contains("RESOURCE_SHORTAGE")
        || msg.contains("Administratively")
}

/// Sleep the exponential backoff for `attempt`, or return `true` if cancelled.
async fn wait_with_cancel(
    cancel_token: &tokio_util::sync::CancellationToken,
    attempt: u32,
) -> bool {
    const BASE_BACKOFF_MS: u64 = 100;
    const MAX_BACKOFF_MS: u64 = 1_000;
    let step = BASE_BACKOFF_MS.saturating_mul(u64::from(1_u32 << (attempt - 1)));
    let wait_ms = step.min(MAX_BACKOFF_MS);
    tokio::select! {
        () = cancel_token.cancelled() => true,
        () = time::sleep(Duration::from_millis(wait_ms)) => false,
    }
}

/// Execute a command on an async channel.
///
/// Returns `Err(())` and sets the error/status if the command cannot be executed.
async fn exec_async_command(
    channel: &Channel<client::Msg>,
    command_text: &str,
    command: &Arc<RunningCommand>,
) -> Result<(), ()> {
    if let Err(e) = channel.exec(true, command_text).await {
        let _ = command.error.set(format!("Failed to execute command: {e}"));
        broadcast_close(command, None);
        let _ = command.status_tx.send(AsyncCommandStatus::Failed);
        return Err(());
    }
    Ok(())
}

/// Run async output collection with cancellation and timeout support.
async fn run_with_cancellation_and_timeout(
    channel: &mut Channel<client::Msg>,
    command_text: &str,
    timeout: Duration,
    command: &Arc<RunningCommand>,
) {
    tokio::select! {
        biased;
        () = command.cancel_token.cancelled() => {
            handle_cancellation(channel, command_text, command).await;
        }
        () = time::sleep(timeout) => {
            handle_timeout(channel, command_text, timeout, command).await;
        }
        result = collect_async_output(channel, command) => {
            if let Some(code) = result {
                let _ = command.exit_code.set(code);
            }
            broadcast_close(command, result);
            let _ = command.status_tx.send(AsyncCommandStatus::Completed);
        }
    }
}

/// Handle async command cancellation.
///
/// Sends `CHANNEL_CLOSE`, then drains inbound messages until the server
/// acknowledges the close (bounded by timeout), and finally pauses briefly
/// so OpenSSH can reclaim the channel slot before the next
/// `channel_open_session` call on the same SSH session.
///
/// Under rapid `execute + cancel` bursts this pacing prevents
/// `MaxSessions` exhaustion: without it, the next open lands on the
/// server before the `CHANNEL_CLOSE` is fully applied and the server
/// refuses with `ConnectFailed` / `Administratively prohibited`.
async fn handle_cancellation(
    channel: &mut Channel<client::Msg>,
    command_text: &str,
    command: &Arc<RunningCommand>,
) {
    use russh::ChannelMsg;
    warn!("Async command cancelled: {command_text}");
    let _ = channel.close().await;
    // Drain inbound messages until the server acknowledges the close.
    // Generous timeout so a slow server doesn't leave us guessing whether
    // the channel slot has actually been released.
    let drain = async {
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Close | ChannelMsg::Eof) | None => break,
                Some(_) => {}
            }
        }
    };
    let _ = time::timeout(Duration::from_secs(2), drain).await;
    broadcast_close(command, None);
    let _ = command.status_tx.send(AsyncCommandStatus::Cancelled);
}

/// Handle async command timeout.
async fn handle_timeout(
    channel: &Channel<client::Msg>,
    command_text: &str,
    timeout: Duration,
    command: &Arc<RunningCommand>,
) {
    warn!("Async command timed out after {timeout:?}: {command_text}");
    command.timed_out.store(true, Ordering::SeqCst);
    let _ = channel.close().await;
    broadcast_close(command, None);
    let _ = command.status_tx.send(AsyncCommandStatus::Completed);
}

/// Open a PTY channel with an interactive shell.
///
/// Allocates a pseudo-terminal and starts a shell on the remote server.
/// Returns a pair of channels: one for reading output, one for writing input.
///
/// # Arguments
///
/// * `handle` - Shared handle to the SSH session
/// * `term` - Terminal type (e.g., "xterm", "vt100", "ansi")
/// * `cols` - Terminal width in columns
/// * `rows` - Terminal height in rows
///
/// # Returns
///
/// * `Ok(channel)` - The SSH channel with PTY and shell allocated
/// * `Err(message)` - Error if PTY or shell request fails
pub async fn open_pty_shell(
    handle: &Arc<client::Handle<SshClientHandler>>,
    term: &str,
    cols: u32,
    rows: u32,
) -> Result<Channel<client::Msg>, String> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("Failed to open channel: {e}"))?;

    channel
        .request_pty(true, term, cols, rows, 0, 0, &[])
        .await
        .map_err(|e| format!("Failed to request PTY: {e}"))?;

    channel
        .request_shell(true)
        .await
        .map_err(|e| format!("Failed to request shell: {e}"))?;

    Ok(channel)
}

/// Execute a command asynchronously with PTY allocation.
///
/// Like `execute_ssh_command_async` but allocates a PTY before executing.
/// All output goes to the stdout buffer (no stderr separation in PTY mode).
///
/// Use this for commands that require a terminal (sudo, top, etc.).
pub async fn execute_ssh_command_async_pty(
    handle: Arc<client::Handle<SshClientHandler>>,
    command_text: String,
    timeout: Duration,
    command: Arc<RunningCommand>,
) {
    let Some(mut channel) = open_async_channel(&handle, &command).await else {
        return;
    };

    if request_pty_on_channel(&channel, &command).await == Err(()) {
        return;
    }

    if exec_async_command(&channel, &command_text, &command).await == Err(()) {
        return;
    }

    run_with_cancellation_and_timeout(&mut channel, &command_text, timeout, &command).await;
}

/// Request a PTY on an already-opened channel.
///
/// Returns `Err(())` and sets the error/status if the PTY request fails.
async fn request_pty_on_channel(
    channel: &Channel<client::Msg>,
    command: &Arc<RunningCommand>,
) -> Result<(), ()> {
    if let Err(e) = channel.request_pty(true, "xterm", 80, 24, 0, 0, &[]).await {
        let _ = command.error.set(format!("Failed to request PTY: {e}"));
        broadcast_close(command, None);
        let _ = command.status_tx.send(AsyncCommandStatus::Failed);
        return Err(());
    }
    Ok(())
}

/// Flush threshold for batched output (8KB)
const FLUSH_THRESHOLD: usize = 8192;

/// Collect output from an SSH channel into the lock-free state.
///
/// The reader task owns the `OutputBuffer` exclusively (no shared mutex).
/// After each batch is flushed it publishes a fresh snapshot through
/// `output_history.store(...)` and broadcasts the delta as an `OutputChunk`.
///
/// Returns the exit code when the channel closes.
async fn collect_async_output(
    channel: &mut Channel<client::Msg>,
    command: &Arc<RunningCommand>,
) -> Option<i32> {
    let max_buffer = super::config::resolve_command_max_buffer_size();
    let max = usize::try_from(max_buffer).unwrap_or(usize::MAX);
    let mut exit_code: Option<i32> = None;
    let mut owned = OutputBuffer::with_capacity(4096, 1024);
    let mut local_stdout = Vec::with_capacity(4096);
    let mut local_stderr = Vec::with_capacity(1024);

    loop {
        let msg = channel.wait().await;
        let should_break = process_channel_msg(
            msg,
            &mut exit_code,
            &mut local_stdout,
            &mut local_stderr,
            &mut owned,
            command,
            max,
        );
        if should_break {
            break;
        }
    }

    flush_local_buffers(
        &mut local_stdout,
        &mut local_stderr,
        &mut owned,
        command,
        max,
    );
    let _ = channel.close().await;
    exit_code
}

/// Process a single channel message, returning true if the loop should break.
#[allow(
    clippy::too_many_arguments,
    reason = "channel message dispatch handles every shared piece of reader state"
)]
fn process_channel_msg(
    msg: Option<ChannelMsg>,
    exit_code: &mut Option<i32>,
    local_stdout: &mut Vec<u8>,
    local_stderr: &mut Vec<u8>,
    owned: &mut OutputBuffer,
    command: &Arc<RunningCommand>,
    max_buffer: usize,
) -> bool {
    match msg {
        Some(ChannelMsg::Data { data }) => {
            local_stdout.extend_from_slice(&data);
            if local_stdout.len() >= FLUSH_THRESHOLD {
                publish_stdout(owned, local_stdout, command, max_buffer);
            }
            false
        }
        Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
            local_stderr.extend_from_slice(&data);
            if local_stderr.len() >= FLUSH_THRESHOLD {
                publish_stderr(owned, local_stderr, command, max_buffer);
            }
            false
        }
        Some(ChannelMsg::ExitStatus { exit_status }) => {
            *exit_code = Some(exit_status.cast_signed());
            false
        }
        Some(ChannelMsg::Eof) => exit_code.is_some(),
        Some(ChannelMsg::Close) | None => true,
        Some(ChannelMsg::ExtendedData { .. } | ChannelMsg::OpenFailure(_)) => false,
        Some(_) => false,
    }
}

/// Publish a stdout batch: broadcast the delta and store a fresh snapshot.
fn publish_stdout(
    owned: &mut OutputBuffer,
    local: &mut Vec<u8>,
    command: &Arc<RunningCommand>,
    max_buffer: usize,
) {
    let chunk = Bytes::copy_from_slice(local);
    let before = owned.stdout.len();
    owned.append_stdout_slice(local, max_buffer);
    let after = owned.stdout.len();
    let dropped = chunk.len().saturating_add(before).saturating_sub(after);
    if dropped > 0 {
        let dropped_u64 = u64::try_from(dropped).unwrap_or(u64::MAX);
        let uri = format!("command://{}/output", command.info.command_id);
        SUBSCRIPTION_REGISTRY.compensate_truncation(&uri, dropped_u64);
    }
    command.output_history.store(Arc::new(owned.clone()));
    let seq = SUBSCRIPTION_REGISTRY.next_seq(ResourceKind::Command, &command.info.command_id);
    let _ = command
        .output_tx
        .send(OutputChunk::Stdout { seq, data: chunk });
    SUBSCRIPTION_REGISTRY.poke(ResourceKind::Command, &command.info.command_id);
}

/// Publish a stderr batch: broadcast the delta and store a fresh snapshot.
fn publish_stderr(
    owned: &mut OutputBuffer,
    local: &mut Vec<u8>,
    command: &Arc<RunningCommand>,
    max_buffer: usize,
) {
    let chunk = Bytes::copy_from_slice(local);
    let before = owned.stderr.len();
    owned.append_stderr_slice(local, max_buffer);
    let after = owned.stderr.len();
    let dropped = chunk.len().saturating_add(before).saturating_sub(after);
    if dropped > 0 {
        let dropped_u64 = u64::try_from(dropped).unwrap_or(u64::MAX);
        let uri = format!("command://{}/output", command.info.command_id);
        SUBSCRIPTION_REGISTRY.compensate_truncation(&uri, dropped_u64);
    }
    command.output_history.store(Arc::new(owned.clone()));
    let seq = SUBSCRIPTION_REGISTRY.next_seq(ResourceKind::Command, &command.info.command_id);
    let _ = command
        .output_tx
        .send(OutputChunk::Stderr { seq, data: chunk });
    SUBSCRIPTION_REGISTRY.poke(ResourceKind::Command, &command.info.command_id);
}

/// Flush remaining local buffers to the lock-free output state.
fn flush_local_buffers(
    local_stdout: &mut Vec<u8>,
    local_stderr: &mut Vec<u8>,
    owned: &mut OutputBuffer,
    command: &Arc<RunningCommand>,
    max_buffer: usize,
) {
    let had_stdout = !local_stdout.is_empty();
    let had_stderr = !local_stderr.is_empty();
    if had_stdout {
        publish_stdout(owned, local_stdout, command, max_buffer);
    }
    if had_stderr {
        publish_stderr(owned, local_stderr, command, max_buffer);
    }
}

/// Broadcast the terminal `Closed` frame. Errors are intentionally ignored:
/// no subscribers means there's nobody to notify.
fn broadcast_close(command: &Arc<RunningCommand>, exit_code: Option<i32>) {
    let seq = SUBSCRIPTION_REGISTRY.next_seq(ResourceKind::Command, &command.info.command_id);
    let _ = command
        .output_tx
        .send(OutputChunk::Closed { seq, exit_code });
    SUBSCRIPTION_REGISTRY.poke(ResourceKind::Command, &command.info.command_id);
}

#[cfg(test)]
mod tests {
    use super::*;

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

    mod client_config {
        use super::*;

        #[test]
        fn test_builds_config_with_inactivity_timeout() {
            let config = build_client_config(Duration::from_secs(300), true, false);
            assert_eq!(config.inactivity_timeout, Some(Duration::from_secs(300)));
        }

        #[test]
        fn test_builds_config_with_keepalive() {
            let config = build_client_config(Duration::from_secs(300), true, false);
            assert_eq!(config.keepalive_interval, Some(Duration::from_secs(30)));
            assert_eq!(config.keepalive_max, 3);
        }

        #[test]
        fn test_compression_enabled_includes_zlib() {
            let config = build_client_config(Duration::from_secs(300), true, false);
            let compression = &config.preferred.compression;
            assert!(!compression.is_empty());
        }

        #[test]
        fn test_compression_disabled() {
            let config = build_client_config(Duration::from_secs(300), false, false);
            let compression = &config.preferred.compression;
            assert!(!compression.is_empty());
        }

        #[test]
        fn test_different_inactivity_timeouts() {
            let config1 = build_client_config(Duration::from_secs(60), true, false);
            let config2 = build_client_config(Duration::from_secs(600), true, false);

            assert_eq!(config1.inactivity_timeout, Some(Duration::from_secs(60)));
            assert_eq!(config2.inactivity_timeout, Some(Duration::from_secs(600)));
        }

        #[test]
        fn test_persistent_disables_inactivity_timeout() {
            let config = build_client_config(Duration::from_secs(300), true, true);
            assert_eq!(config.inactivity_timeout, None);
            assert_eq!(config.keepalive_interval, Some(Duration::from_secs(30)));
        }

        #[test]
        fn test_non_persistent_has_inactivity_timeout() {
            let config = build_client_config(Duration::from_secs(300), true, false);
            assert_eq!(config.inactivity_timeout, Some(Duration::from_secs(300)));
        }
    }

    mod retry_delay_constant {
        use super::*;

        #[test]
        fn test_max_retry_delay_value() {
            assert_eq!(MAX_RETRY_DELAY, Duration::from_secs(10));
        }

        #[test]
        fn test_max_retry_delay_is_reasonable() {
            // Ensure the max delay is between 5 and 60 seconds (reasonable bounds)
            assert!(MAX_RETRY_DELAY.as_secs() >= 5);
            assert!(MAX_RETRY_DELAY.as_secs() <= 60);
        }

        #[test]
        fn test_max_retry_delay_duration_properties() {
            assert_eq!(MAX_RETRY_DELAY.as_secs(), 10);
            assert_eq!(MAX_RETRY_DELAY.as_millis(), 10_000);
        }
    }

    mod default_key_discovery {
        use super::*;

        #[test]
        fn test_default_key_names_order() {
            assert_eq!(DEFAULT_KEY_NAMES[0], "id_ed25519");
            assert_eq!(DEFAULT_KEY_NAMES[1], "id_ecdsa");
            assert_eq!(DEFAULT_KEY_NAMES[4], "id_rsa");
        }

        #[test]
        fn test_default_key_names_count() {
            assert_eq!(DEFAULT_KEY_NAMES.len(), 6);
        }

        #[test]
        fn test_default_key_names_contains_expected_types() {
            assert!(DEFAULT_KEY_NAMES.contains(&"id_ed25519"));
            assert!(DEFAULT_KEY_NAMES.contains(&"id_rsa"));
            assert!(DEFAULT_KEY_NAMES.contains(&"id_ecdsa"));
            assert!(DEFAULT_KEY_NAMES.contains(&"id_ecdsa_sk"));
            assert!(DEFAULT_KEY_NAMES.contains(&"id_ed25519_sk"));
            assert!(DEFAULT_KEY_NAMES.contains(&"id_dsa"));
        }

        #[test]
        fn test_ed25519_is_preferred_over_rsa() {
            let ed25519_pos = DEFAULT_KEY_NAMES.iter().position(|k| *k == "id_ed25519");
            let rsa_pos = DEFAULT_KEY_NAMES.iter().position(|k| *k == "id_rsa");
            assert!(ed25519_pos < rsa_pos);
        }
    }

    mod auth_chain_building {
        use super::*;

        #[test]
        fn test_with_explicit_key_skips_discovery() {
            let chain = build_auth_chain(None, Some("/tmp/my_key"));
            // Explicit key + agent fallback
            assert!(!chain.is_empty());
        }

        #[test]
        fn test_with_password_only() {
            let chain = build_auth_chain(Some("secret"), None);
            assert!(!chain.is_empty());
        }

        #[test]
        fn test_with_no_credentials_includes_agent() {
            let chain = build_auth_chain(None, None);
            // Even with no keys found, agent should always be present
            assert!(!chain.is_empty());
        }

        #[test]
        fn test_with_password_and_key() {
            let chain = build_auth_chain(Some("pass"), Some("/tmp/key"));
            assert!(!chain.is_empty());
        }
    }
}
