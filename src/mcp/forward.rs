//! Port forwarding implementation for SSH MCP.
//!
//! This module provides SSH port forwarding (local tunnel) functionality using the
//! `direct-tcpip` channel type defined in RFC 4254.
//!
//! # Architecture
//!
//! The port forwarding system consists of two main components:
//!
//! 1. **TCP Listener**: A local TCP listener binds to the specified port on `127.0.0.1`.
//!    When a client connects to this port, a new forwarding session is spawned.
//!
//! 2. **Bidirectional I/O**: Each forwarding session creates a `direct-tcpip` channel
//!    to the remote destination. Data flows in both directions:
//!    - Local client -> SSH channel -> Remote destination
//!    - Remote destination -> SSH channel -> Local client
//!
//!    This is achieved using `tokio::io::copy` for efficient zero-copy forwarding,
//!    with `tokio::select!` to handle both directions concurrently until either
//!    side closes the connection.
//!
//! # Feature Gate
//!
//! This module is only compiled when the `port_forward` feature is enabled.

use std::net::SocketAddr;
use std::sync::Arc;

use russh::client;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tracing::{debug, error};

use super::ssh_commands::SshClientHandler;

/// Sets up port forwarding from a local port to a remote destination via SSH.
///
/// This function creates a TCP listener on the specified local port and spawns
/// an async task that accepts connections and forwards them through the SSH session.
///
/// # Arguments
///
/// * `handle_arc` - Arc-wrapped mutex containing the SSH client handle
/// * `local_port` - The local port to listen on (binds to 127.0.0.1)
/// * `remote_address` - The remote host to forward connections to
/// * `remote_port` - The remote port to forward connections to
///
/// # Returns
///
/// Returns the actual bound socket address on success, or an error message on failure.
pub(crate) async fn setup_port_forwarding(
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

/// Handles a single port forwarding connection using async I/O.
///
/// Opens a direct-tcpip channel to the remote destination and performs
/// bidirectional data forwarding between the local TCP stream and the
/// SSH channel.
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
