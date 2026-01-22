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

    #[test]
    fn test_key_auth_from_str() {
        let auth = KeyAuth::new("/path/to/key");
        assert_eq!(auth.key_path.to_str(), Some("/path/to/key"));
    }

    #[test]
    fn test_key_auth_relative_path() {
        let auth = KeyAuth::new("~/.ssh/id_rsa");
        assert_eq!(auth.key_path, PathBuf::from("~/.ssh/id_rsa"));
    }

    #[test]
    fn test_key_auth_different_key_types() {
        // RSA key
        let rsa = KeyAuth::new("/home/user/.ssh/id_rsa");
        assert_eq!(rsa.name(), "key");
        assert_eq!(
            rsa.key_path.file_name().and_then(|n| n.to_str()),
            Some("id_rsa")
        );

        // Ed25519 key
        let ed25519 = KeyAuth::new("/home/user/.ssh/id_ed25519");
        assert_eq!(ed25519.name(), "key");
        assert_eq!(
            ed25519.key_path.file_name().and_then(|n| n.to_str()),
            Some("id_ed25519")
        );

        // ECDSA key
        let ecdsa = KeyAuth::new("/home/user/.ssh/id_ecdsa");
        assert_eq!(ecdsa.name(), "key");
        assert_eq!(
            ecdsa.key_path.file_name().and_then(|n| n.to_str()),
            Some("id_ecdsa")
        );
    }

    #[test]
    fn test_key_auth_path_with_spaces() {
        let auth = KeyAuth::new("/path/with spaces/key file");
        assert_eq!(auth.key_path, PathBuf::from("/path/with spaces/key file"));
    }

    #[test]
    fn test_key_auth_empty_path() {
        let auth = KeyAuth::new("");
        assert_eq!(auth.key_path, PathBuf::from(""));
    }

    #[test]
    fn test_key_auth_windows_style_path() {
        let auth = KeyAuth::new("C:\\Users\\user\\.ssh\\id_rsa");
        assert_eq!(
            auth.key_path,
            PathBuf::from("C:\\Users\\user\\.ssh\\id_rsa")
        );
    }

    #[test]
    fn test_key_auth_unicode_path() {
        let auth = KeyAuth::new("/home/usér/chaves/私の鍵");
        assert_eq!(auth.key_path, PathBuf::from("/home/usér/chaves/私の鍵"));
    }

    #[test]
    fn test_key_auth_dot_files() {
        let auth = KeyAuth::new("/home/user/.ssh/.hidden_key");
        assert_eq!(
            auth.key_path.file_name().and_then(|n| n.to_str()),
            Some(".hidden_key")
        );
    }

    #[test]
    fn test_key_auth_symlink_style_path() {
        // Path that looks like it could be a symlink target
        let auth = KeyAuth::new("/proc/1/root/home/user/.ssh/id_rsa");
        assert!(auth.key_path.starts_with("/proc"));
    }

    #[test]
    fn test_key_auth_path_with_dots() {
        let auth = KeyAuth::new("/home/user/../other_user/.ssh/id_rsa");
        assert!(auth.key_path.to_str().unwrap_or("").contains(".."));
    }

    #[test]
    fn test_key_auth_path_with_multiple_extensions() {
        let auth = KeyAuth::new("/home/user/.ssh/id_rsa.backup.old");
        assert_eq!(
            auth.key_path.file_name().and_then(|n| n.to_str()),
            Some("id_rsa.backup.old")
        );
    }

    #[test]
    fn test_key_auth_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<KeyAuth>();
    }

    #[test]
    fn test_key_auth_very_long_path() {
        let long_path = format!("/home/user/{}/id_rsa", "subdir/".repeat(100));
        let auth = KeyAuth::new(&long_path);
        assert!(auth.key_path.to_str().unwrap_or("").len() > 700);
    }
}
