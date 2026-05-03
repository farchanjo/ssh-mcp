//! Russh-handshake strategy trait.
//!
//! Every strategy owns the russh `client::Handle` while the SSH
//! authentication round-trip runs. The trait is intentionally narrow
//! and uses native AFIT (async `fn` in trait), so the chain can hold a
//! statically dispatched enum without going through `Box<dyn Future>`
//! or the `async-trait` crate.
//!
//! Heterogeneous strategies are stored in an enum
//! ([`super::chain::Strategy`]) so the chain remains dyn-free even
//! though AFIT itself is not dyn-safe.

use russh::client;

use crate::adapters::ssh::internal::session::SshClientHandler;

/// Trait for SSH authentication strategies that drive the russh
/// handshake.
///
/// Implementations must be thread-safe (`Send + Sync`) so they can run
/// inside a `tokio::spawn`-ed task. Each strategy represents a
/// different authentication method (password, key file, SSH agent,
/// ...).
pub(crate) trait AuthStrategy: Send + Sync {
    /// Attempt to authenticate against the SSH server.
    ///
    /// # Arguments
    ///
    /// * `handle` - Mutable reference to the russh client handle.
    /// * `username` - Username for authentication.
    ///
    /// # Returns
    ///
    /// * `Ok(true)` - Authentication succeeded.
    /// * `Ok(false)` - Authentication failed (credentials rejected).
    /// * `Err(message)` - Error during the authentication attempt.
    async fn authenticate(
        &self,
        handle: &mut client::Handle<SshClientHandler>,
        username: &str,
    ) -> Result<bool, String>;

    /// Stable name for logging (`"password"`, `"key"`, `"agent"`,
    /// `"chain"`).
    fn name(&self) -> &'static str;
}
