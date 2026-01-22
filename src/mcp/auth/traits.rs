//! Authentication strategy trait definition.
//!
//! Defines the interface for authentication strategies, enabling
//! the Strategy pattern for SSH authentication methods.

use async_trait::async_trait;
use russh::client;

use crate::mcp::session::SshClientHandler;

/// Trait for SSH authentication strategies.
///
/// Implementations must be thread-safe (`Send + Sync`) for use across
/// async tasks. Each strategy represents a different authentication
/// method (password, key file, SSH agent, etc.).
#[async_trait]
pub trait AuthStrategy: Send + Sync {
    /// Attempt to authenticate with the SSH server.
    ///
    /// # Arguments
    ///
    /// * `handle` - Mutable reference to the SSH client handle
    /// * `username` - Username for authentication
    ///
    /// # Returns
    ///
    /// * `Ok(true)` - Authentication succeeded
    /// * `Ok(false)` - Authentication failed (credentials rejected)
    /// * `Err(message)` - Error during authentication attempt
    async fn authenticate(
        &self,
        handle: &mut client::Handle<SshClientHandler>,
        username: &str,
    ) -> Result<bool, String>;

    /// Get the name of this authentication strategy.
    ///
    /// Used for logging and debugging purposes.
    fn name(&self) -> &'static str;
}
