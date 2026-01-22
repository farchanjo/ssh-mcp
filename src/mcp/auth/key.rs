//! Private key file SSH authentication.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use russh::{client, keys};
use tracing::debug;

use crate::mcp::session::SshClientHandler;

use super::traits::AuthStrategy;

/// Private key file authentication strategy.
///
/// Loads a private key from a file and uses it for public key authentication.
/// Currently supports passphrase-less keys.
#[allow(dead_code)]
pub struct KeyAuth {
    key_path: PathBuf,
}

#[allow(dead_code)]
impl KeyAuth {
    /// Create a new key authentication strategy.
    ///
    /// # Arguments
    ///
    /// * `key_path` - Path to the private key file
    pub fn new(key_path: impl Into<PathBuf>) -> Self {
        Self {
            key_path: key_path.into(),
        }
    }
}

#[async_trait]
impl AuthStrategy for KeyAuth {
    async fn authenticate(
        &self,
        handle: &mut client::Handle<SshClientHandler>,
        username: &str,
    ) -> Result<bool, String> {
        let path = Path::new(&self.key_path);

        // Load the secret key (supports passphrase-less keys)
        let key_pair = keys::load_secret_key(path, None)
            .map_err(|e| format!("Failed to load private key from {:?}: {}", self.key_path, e))?;

        // For RSA keys, use the best supported hash algorithm
        let hash_alg = handle
            .best_supported_rsa_hash()
            .await
            .ok()
            .flatten()
            .flatten();
        debug!("Using RSA hash algorithm for key auth: {:?}", hash_alg);

        // Wrap the key with the preferred hash algorithm
        let key_with_hash = keys::PrivateKeyWithHashAlg::new(Arc::new(key_pair), hash_alg);

        let result = handle
            .authenticate_publickey(username, key_with_hash)
            .await
            .map_err(|e| format!("Key authentication failed: {}", e))?;

        Ok(result.success())
    }

    fn name(&self) -> &'static str {
        "key"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_auth_name() {
        let auth = KeyAuth::new("/path/to/key");
        assert_eq!(auth.name(), "key");
    }

    #[test]
    fn test_key_auth_creation() {
        let auth = KeyAuth::new("/home/user/.ssh/id_rsa");
        assert_eq!(auth.key_path, PathBuf::from("/home/user/.ssh/id_rsa"));
    }

    #[test]
    fn test_key_auth_from_pathbuf() {
        let path = PathBuf::from("/path/to/key");
        let auth = KeyAuth::new(path.clone());
        assert_eq!(auth.key_path, path);
    }
}
