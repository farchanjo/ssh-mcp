//! SSH session management.
//!
//! This module provides the SSH client handler for managing SSH connections.
//! Session storage is handled by the `storage` module's `SessionStorage` trait.
//!
//! # Architecture
//!
//! - `SshClientHandler`: A russh client handler that accepts all host keys (similar to
//!   `StrictHostKeyChecking=no` in OpenSSH). In production environments, this should be
//!   extended to verify against known_hosts.
//!
//! # Thread Safety
//!
//! The `client::Handle<SshClientHandler>` is wrapped in `Arc<>` in storage because it's not
//! `Clone`, and we need to share it across multiple async operations (execute, forward, etc.).

use russh::{client, keys};

/// Client handler for russh that accepts all host keys.
///
/// This implementation accepts all server public keys without verification,
/// similar to `StrictHostKeyChecking=no` in OpenSSH configuration.
///
/// # Security Note
///
/// In production environments, you should implement proper host key verification
/// against a known_hosts file to prevent man-in-the-middle attacks.
pub struct SshClientHandler;

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
