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
//!    - Private key file authentication
//!    - SSH agent authentication (tries all available identities)
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

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use backon::{ExponentialBuilder, Retryable};
use russh::{ChannelMsg, client, keys};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::mcp::config::MAX_RETRY_DELAY;
use crate::mcp::error::is_retryable_error;
use crate::mcp::session::SshClientHandler;
use crate::mcp::types::SshCommandResponse;

/// Build russh client configuration with the specified settings.
///
/// Creates an `Arc<client::Config>` with:
/// - Inactivity timeout set to the provided `timeout` (or `None` if `persistent` is true)
/// - Keepalive interval of 30 seconds with max 3 keepalives
/// - Compression preference based on `compress` flag (ZLIB if enabled, NONE if disabled)
///
/// # Arguments
///
/// * `timeout` - Inactivity timeout duration (ignored if `persistent` is true)
/// * `compress` - Whether to enable zlib compression
/// * `persistent` - If true, disables inactivity timeout to keep the session open indefinitely
///
/// # Examples
///
/// ```ignore
/// let config = build_client_config(Duration::from_secs(30), true, false);
/// assert_eq!(config.inactivity_timeout, Some(Duration::from_secs(30)));
///
/// let persistent_config = build_client_config(Duration::from_secs(30), true, true);
/// assert_eq!(persistent_config.inactivity_timeout, None);
/// ```
pub(crate) fn build_client_config(
    timeout: Duration,
    compress: bool,
    persistent: bool,
) -> Arc<client::Config> {
    let compression = if compress {
        (&[russh::compression::ZLIB, russh::compression::NONE][..]).into()
    } else {
        (&[russh::compression::NONE][..]).into()
    };

    let preferred = russh::Preferred {
        compression,
        ..Default::default()
    };

    // When persistent is true, disable inactivity timeout to keep session open indefinitely
    let inactivity_timeout = if persistent { None } else { Some(timeout) };

    Arc::new(client::Config {
        inactivity_timeout,
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
pub(crate) fn parse_address(address: &str) -> Result<(String, u16), String> {
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
#[allow(clippy::too_many_arguments)]
pub(crate) async fn connect_to_ssh_with_retry(
    address: &str,
    username: &str,
    password: Option<&str>,
    key_path: Option<&str>,
    timeout: Duration,
    max_retries: u32,
    min_delay: Duration,
    compress: bool,
    persistent: bool,
) -> Result<(client::Handle<SshClientHandler>, u32), String> {
    // Track retry attempts using atomic counter
    let attempt_counter = AtomicU32::new(0);

    // Clone values for the retry closure
    let address = address.to_string();
    let username = username.to_string();
    let password = password.map(|s| s.to_string());
    let key_path = key_path.map(|s| s.to_string());

    let backoff = ExponentialBuilder::default()
        .with_min_delay(min_delay)
        .with_max_delay(MAX_RETRY_DELAY)
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
            timeout,
            compress,
            persistent,
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

/// Establish an SSH connection and authenticate.
///
/// This is the core connection function that:
/// 1. Builds client configuration
/// 2. Parses the address
/// 3. Connects with timeout
/// 4. Authenticates using the appropriate method
async fn connect_to_ssh(
    address: &str,
    username: &str,
    password: Option<&str>,
    key_path: Option<&str>,
    timeout: Duration,
    compress: bool,
    persistent: bool,
) -> Result<client::Handle<SshClientHandler>, String> {
    let config = build_client_config(timeout, compress, persistent);
    let handler = SshClientHandler;

    // Parse address into host and port
    let (host, port) = parse_address(address)?;

    // Connect with timeout
    let connect_future = client::connect(config, (host.as_str(), port), handler);

    let mut handle = tokio::time::timeout(timeout, connect_future)
        .await
        .map_err(|_| format!("Connection timed out after {:?}", timeout))?
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

/// Authenticate using a private key file.
///
/// Loads the private key from the specified path and attempts public key
/// authentication. Currently supports passphrase-less keys.
async fn authenticate_with_key(
    handle: &mut client::Handle<SshClientHandler>,
    username: &str,
    key_path: &str,
) -> Result<client::AuthResult, String> {
    let path = Path::new(key_path);

    // Load the secret key (supports passphrase-less keys or will prompt if needed)
    let key_pair = keys::load_secret_key(path, None)
        .map_err(|e| format!("Failed to load private key from {}: {}", key_path, e))?;

    // For RSA keys, we need to use the best supported hash algorithm (rsa-sha2-256 or rsa-sha2-512)
    // Otherwise, the server may reject the legacy ssh-rsa (SHA1) signature
    let hash_alg = handle
        .best_supported_rsa_hash()
        .await
        .ok()
        .flatten()
        .flatten();
    debug!("Using RSA hash algorithm for key auth: {:?}", hash_alg);

    // Wrap the key with the preferred hash algorithm
    let key_with_hash = keys::PrivateKeyWithHashAlg::new(Arc::new(key_pair), hash_alg);

    handle
        .authenticate_publickey(username, key_with_hash)
        .await
        .map_err(|e| format!("Key authentication failed: {}", e))
}

/// Authenticate using SSH agent.
///
/// Connects to the SSH agent (via SSH_AUTH_SOCK) and tries each available
/// identity until one succeeds.
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

        // For RSA keys, we need to use the best supported hash algorithm (rsa-sha2-256 or rsa-sha2-512)
        // Otherwise, the server may reject the legacy ssh-rsa (SHA1) signature
        let hash_alg = handle
            .best_supported_rsa_hash()
            .await
            .ok()
            .flatten()
            .flatten();
        debug!("Using RSA hash algorithm: {:?}", hash_alg);

        match handle
            .authenticate_publickey_with(username, identity.clone(), hash_alg, &mut agent)
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
pub(crate) async fn execute_ssh_command(
    handle_arc: &Arc<Mutex<client::Handle<SshClientHandler>>>,
    command: &str,
    timeout: Duration,
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
    let mut timed_out = false;

    // Read channel messages with timeout
    let result = tokio::time::timeout(timeout, async {
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
    })
    .await;

    // Handle timeout - return partial output, don't treat as error
    if result.is_err() {
        timed_out = true;
        warn!(
            "Command timed out after {:?}, returning partial output ({} bytes stdout, {} bytes stderr)",
            timeout,
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
        exit_code: exit_code.map(|c| c as i32).unwrap_or(-1),
        timed_out,
    })
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
        fn test_builds_config_with_timeout() {
            let config = build_client_config(Duration::from_secs(45), true, false);
            assert_eq!(config.inactivity_timeout, Some(Duration::from_secs(45)));
        }

        #[test]
        fn test_builds_config_with_keepalive() {
            let config = build_client_config(Duration::from_secs(30), true, false);
            assert_eq!(config.keepalive_interval, Some(Duration::from_secs(30)));
            assert_eq!(config.keepalive_max, 3);
        }

        #[test]
        fn test_compression_enabled_includes_zlib() {
            let config = build_client_config(Duration::from_secs(30), true, false);
            // When compression is enabled, ZLIB should be preferred
            let compression = &config.preferred.compression;
            assert!(!compression.is_empty());
        }

        #[test]
        fn test_compression_disabled() {
            let config = build_client_config(Duration::from_secs(30), false, false);
            // When compression is disabled, only NONE should be available
            let compression = &config.preferred.compression;
            assert!(!compression.is_empty());
        }

        #[test]
        fn test_different_timeouts() {
            let config1 = build_client_config(Duration::from_secs(10), true, false);
            let config2 = build_client_config(Duration::from_secs(120), true, false);

            assert_eq!(config1.inactivity_timeout, Some(Duration::from_secs(10)));
            assert_eq!(config2.inactivity_timeout, Some(Duration::from_secs(120)));
        }

        #[test]
        fn test_persistent_disables_inactivity_timeout() {
            let config = build_client_config(Duration::from_secs(30), true, true);
            assert_eq!(config.inactivity_timeout, None);
            // Keepalive should still be active for persistent sessions
            assert_eq!(config.keepalive_interval, Some(Duration::from_secs(30)));
        }

        #[test]
        fn test_non_persistent_has_inactivity_timeout() {
            let config = build_client_config(Duration::from_secs(60), true, false);
            assert_eq!(config.inactivity_timeout, Some(Duration::from_secs(60)));
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
}
