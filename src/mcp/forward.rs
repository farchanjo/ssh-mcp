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

use russh::Channel;
use russh::client::{self, Msg};
use tokio::io;
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error};

use super::session::SshClientHandler;

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
pub async fn setup_port_forwarding(
    handle_arc: Arc<client::Handle<SshClientHandler>>,
    local_port: u16,
    remote_address: &str,
    remote_port: u16,
) -> Result<SocketAddr, String> {
    let listener_addr = format!("127.0.0.1:{local_port}");
    let listener = TcpListener::bind(&listener_addr)
        .await
        .map_err(|e| format!("Failed to bind to local port {local_port}: {e}"))?;

    let local_addr = listener
        .local_addr()
        .map_err(|e| format!("Failed to get local address: {e}"))?;

    let remote_addr_owned = remote_address.to_string();

    tokio::spawn(async move {
        run_accept_loop(listener, handle_arc, &remote_addr_owned, remote_port).await;
    });

    Ok(local_addr)
}

/// Accepts incoming TCP connections and spawns forwarding tasks for each.
async fn run_accept_loop(
    listener: TcpListener,
    handle_arc: Arc<client::Handle<SshClientHandler>>,
    remote_address: &str,
    remote_port: u16,
) {
    debug!("Port forwarding active on {:?}", listener.local_addr());

    loop {
        match listener.accept().await {
            Ok((local_stream, client_addr)) => {
                debug!("New connection from {client_addr} to forwarded port");
                spawn_forward_task(
                    Arc::clone(&handle_arc),
                    local_stream,
                    remote_address.to_owned(),
                    remote_port,
                );
            }
            Err(e) => {
                error!("Error accepting connection: {e}");
                break;
            }
        }
    }
}

/// Spawns a task to handle a single forwarded connection.
fn spawn_forward_task(
    handle_arc: Arc<client::Handle<SshClientHandler>>,
    local_stream: TcpStream,
    remote_host: String,
    remote_port: u16,
) {
    tokio::spawn(async move {
        if let Err(e) =
            handle_port_forward_connection(handle_arc, local_stream, &remote_host, remote_port)
                .await
        {
            debug!("Port forwarding connection error: {e}");
        }
    });
}

/// Handles a single port forwarding connection using async I/O.
///
/// Opens a direct-tcpip channel to the remote destination and performs
/// bidirectional data forwarding between the local TCP stream and the
/// SSH channel.
async fn handle_port_forward_connection(
    handle_arc: Arc<client::Handle<SshClientHandler>>,
    local_stream: TcpStream,
    remote_host: &str,
    remote_port: u16,
) -> Result<(), String> {
    let channel = open_direct_tcpip_channel(&handle_arc, remote_host, remote_port).await?;
    let channel_stream = channel.into_stream();

    let (mut local_read, mut local_write) = io::split(local_stream);
    let (mut channel_read, mut channel_write) = io::split(channel_stream);

    let local_to_remote = io::copy(&mut local_read, &mut channel_write);
    let remote_to_local = io::copy(&mut channel_read, &mut local_write);

    tokio::select! {
        result = local_to_remote => {
            if let Err(e) = result {
                debug!("Local to remote copy ended: {e}");
            }
        }
        result = remote_to_local => {
            if let Err(e) = result {
                debug!("Remote to local copy ended: {e}");
            }
        }
    }

    debug!("Port forwarding connection closed");
    Ok(())
}

/// Opens a direct-tcpip channel to the remote destination via SSH.
async fn open_direct_tcpip_channel(
    handle_arc: &Arc<client::Handle<SshClientHandler>>,
    remote_host: &str,
    remote_port: u16,
) -> Result<Channel<Msg>, String> {
    handle_arc
        .channel_open_direct_tcpip(remote_host, u32::from(remote_port), "127.0.0.1", 0)
        .await
        .map_err(|e| format!("Failed to open direct-tcpip channel: {e}"))
}
