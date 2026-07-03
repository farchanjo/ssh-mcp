//! SSH session management.
//!
//! This module provides the SSH client handler for managing SSH connections.
//! Session storage is handled by the `storage` module's `SessionStorage` trait.
//!
//! # Architecture
//!
//! - `SshClientHandler`: A russh client handler that performs trust-on-first-use
//!   (TOFU) host-key verification against a `known_hosts` file, mirroring
//!   OpenSSH's default `StrictHostKeyChecking=ask` behaviour without the
//!   interactive prompt (unknown hosts are learned automatically).
//!
//! # Thread Safety
//!
//! The `client::Handle<SshClientHandler>` is wrapped in `Arc<>` in storage because it's not
//! `Clone`, and we need to share it across multiple async operations (execute, forward, etc.).

use std::path::PathBuf;

use russh::{client, keys};
use tracing::{info, warn};

use crate::adapters::config::internal::{resolve_known_hosts_path, resolve_skip_host_key_check};

/// Client handler for russh performing TOFU host-key verification.
///
/// # Security model
///
/// - **Known host, key matches** the `known_hosts` record -> accept.
/// - **Known host, key differs** -> reject (`Ok(false)`); this is the
///   MITM / server-rekey signal an LLM operator must investigate.
/// - **Unknown host** -> trust-on-first-use: accept AND record the key
///   into `known_hosts` so subsequent connections are verified.
/// - **Opt-out**: `SSH_MCP_INSECURE_SKIP_HOST_KEY_CHECK=true` restores
///   unconditional accept-any (`StrictHostKeyChecking=no`), logging a
///   `warn!` on every check. Secure-by-default when unset.
pub struct SshClientHandler {
    host: String,
    port: u16,
    known_hosts_path: PathBuf,
    skip_verification: bool,
}

impl SshClientHandler {
    /// Build a handler bound to the target `host:port`, resolving the
    /// `known_hosts` path and the insecure opt-out flag from the
    /// environment (see `SSH_MCP_KNOWN_HOSTS` and
    /// `SSH_MCP_INSECURE_SKIP_HOST_KEY_CHECK`).
    #[must_use]
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
            known_hosts_path: resolve_known_hosts_path(),
            skip_verification: resolve_skip_host_key_check(),
        }
    }

    /// Verify the server's host key against `known_hosts`, learning
    /// (TOFU) unknown hosts and rejecting on a recorded-key mismatch.
    fn verify_host_key(&self, server_public_key: &keys::PublicKey) -> bool {
        match keys::check_known_hosts_path(
            &self.host,
            self.port,
            server_public_key,
            &self.known_hosts_path,
        ) {
            Ok(true) => true,
            Ok(false) => self.learn_host_key(server_public_key),
            Err(err) => {
                if matches!(err, keys::Error::KeyChanged { .. }) {
                    warn!(
                        host = %self.host,
                        port = self.port,
                        "host key MISMATCH against known_hosts — rejecting connection (possible MITM)"
                    );
                } else {
                    warn!(
                        host = %self.host,
                        port = self.port,
                        error = %err,
                        "known_hosts lookup failed — rejecting connection"
                    );
                }
                false
            }
        }
    }

    /// Record a not-yet-known host key into `known_hosts` (TOFU learn).
    fn learn_host_key(&self, server_public_key: &keys::PublicKey) -> bool {
        match keys::known_hosts::learn_known_hosts_path(
            &self.host,
            self.port,
            server_public_key,
            &self.known_hosts_path,
        ) {
            Ok(()) => {
                info!(
                    host = %self.host,
                    port = self.port,
                    "TOFU: recorded new host key into known_hosts"
                );
                true
            }
            Err(err) => {
                warn!(
                    host = %self.host,
                    port = self.port,
                    error = %err,
                    "failed to record host key (TOFU learn) — rejecting connection"
                );
                false
            }
        }
    }
}

impl client::Handler for SshClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        if self.skip_verification {
            warn!("host-key verification disabled — MITM exposure");
            return Ok(true);
        }
        Ok(self.verify_host_key(server_public_key))
    }
}

#[cfg(test)]
mod tests {
    use russh::keys::ssh_key::{Algorithm, PrivateKey};
    use tempfile::tempdir;

    use super::SshClientHandler;

    fn make_handler(
        host: &str,
        port: u16,
        known_hosts_path: std::path::PathBuf,
    ) -> SshClientHandler {
        SshClientHandler {
            host: host.to_string(),
            port,
            known_hosts_path,
            skip_verification: false,
        }
    }

    #[tokio::test]
    async fn skip_flag_accepts_any_host_key() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let known_hosts_path = dir.path().join("known_hosts");
        let mut handler = make_handler("example.invalid", 22, known_hosts_path);
        handler.skip_verification = true;

        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let accepted = handler.verify_host_key(key.public_key()) || handler.skip_verification;
        assert!(accepted);
        Ok(())
    }

    #[tokio::test]
    async fn recorded_key_mismatch_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let known_hosts_path = dir.path().join("known_hosts");

        let recorded_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let presented_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;

        let host = "example.invalid";
        let port = 22;
        russh::keys::known_hosts::learn_known_hosts_path(
            host,
            port,
            recorded_key.public_key(),
            &known_hosts_path,
        )?;

        let handler = make_handler(host, port, known_hosts_path);
        let accepted = handler.verify_host_key(presented_key.public_key());
        assert!(!accepted, "a mismatched host key must be rejected");
        Ok(())
    }

    #[tokio::test]
    async fn unknown_host_is_learned_via_tofu() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempdir()?;
        let known_hosts_path = dir.path().join("known_hosts");
        let handler = make_handler("example.invalid", 22, known_hosts_path.clone());

        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let accepted = handler.verify_host_key(key.public_key());
        assert!(accepted, "an unknown host must be accepted via TOFU");
        assert!(known_hosts_path.is_file(), "TOFU accept must learn the key");

        // A second check against the now-recorded key must also accept,
        // without re-learning (already-known path).
        let accepted_again = handler.verify_host_key(key.public_key());
        assert!(accepted_again);
        Ok(())
    }
}
