//! Private key file SSH authentication.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh::{client, keys};
use tracing::debug;

use crate::adapters::sftp::internal::sftp::expand_tilde;
use crate::adapters::ssh::internal::session::SshClientHandler;

use super::traits::AuthStrategy;

/// Private key file authentication strategy.
///
/// Loads a private key from a file and uses it for public key
/// authentication. Currently supports passphrase-less keys.
pub(crate) struct KeyAuth {
    key_path: PathBuf,
}

impl KeyAuth {
    /// Create a new key authentication strategy.
    ///
    /// # Arguments
    ///
    /// * `key_path` - Path to the private key file.
    pub(crate) fn new(key_path: impl Into<PathBuf>) -> Self {
        let raw: PathBuf = key_path.into();
        let expanded = expand_tilde(&raw.to_string_lossy());
        Self {
            key_path: PathBuf::from(expanded),
        }
    }
}

impl AuthStrategy for KeyAuth {
    async fn authenticate(
        &self,
        handle: &mut client::Handle<SshClientHandler>,
        username: &str,
    ) -> Result<bool, String> {
        let path = Path::new(&self.key_path);

        // Load the secret key (supports passphrase-less keys).
        let key_pair = keys::load_secret_key(path, None).map_err(|e| {
            let path = &self.key_path;
            format!("Failed to load private key from {}: {e}", path.display())
        })?;

        // For RSA keys, use the best supported hash algorithm.
        let hash_alg = handle
            .best_supported_rsa_hash()
            .await
            .ok()
            .flatten()
            .flatten();
        debug!("Using RSA hash algorithm for key auth: {:?}", hash_alg);

        // Wrap the key with the preferred hash algorithm.
        let key_with_hash = keys::PrivateKeyWithHashAlg::new(Arc::new(key_pair), hash_alg);

        let result = handle
            .authenticate_publickey(username, key_with_hash)
            .await
            .map_err(|e| format!("Key authentication failed: {e}"))?;

        Ok(result.success())
    }

    fn name(&self) -> &'static str {
        "key"
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthStrategy, KeyAuth, PathBuf};

    #[test]
    fn name_is_key() {
        let auth = KeyAuth::new("/path/to/key");
        assert_eq!(auth.name(), "key");
    }

    #[test]
    fn stores_path() {
        let auth = KeyAuth::new("/home/user/.ssh/id_rsa");
        assert_eq!(auth.key_path, PathBuf::from("/home/user/.ssh/id_rsa"));
    }

    #[test]
    fn accepts_owned_pathbuf() {
        let path = PathBuf::from("/path/to/key");
        let auth = KeyAuth::new(path.clone());
        assert_eq!(auth.key_path, path);
    }

    #[test]
    fn accepts_str_slice() {
        let auth = KeyAuth::new("/path/to/key");
        assert_eq!(auth.key_path.to_str(), Some("/path/to/key"));
    }

    #[test]
    fn tilde_is_expanded() {
        let auth = KeyAuth::new("~/.ssh/id_rsa");
        assert!(!auth.key_path.starts_with("~"));
        assert!(auth.key_path.to_string_lossy().ends_with(".ssh/id_rsa"));
    }

    #[test]
    fn supports_different_key_types() {
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
    fn path_with_spaces_round_trips() {
        let auth = KeyAuth::new("/path/with spaces/key file");
        assert_eq!(auth.key_path, PathBuf::from("/path/with spaces/key file"));
    }

    #[test]
    fn empty_path_is_allowed() {
        let auth = KeyAuth::new("");
        assert_eq!(auth.key_path, PathBuf::from(""));
    }

    #[test]
    fn windows_style_path_round_trips() {
        let auth = KeyAuth::new("C:\\Users\\user\\.ssh\\id_rsa");
        assert_eq!(
            auth.key_path,
            PathBuf::from("C:\\Users\\user\\.ssh\\id_rsa")
        );
    }

    #[test]
    fn unicode_path_round_trips() {
        let auth = KeyAuth::new("/home/usér/chaves/私の鍵");
        assert_eq!(auth.key_path, PathBuf::from("/home/usér/chaves/私の鍵"));
    }

    #[test]
    fn dot_files_round_trip() {
        let auth = KeyAuth::new("/home/user/.ssh/.hidden_key");
        assert_eq!(
            auth.key_path.file_name().and_then(|n| n.to_str()),
            Some(".hidden_key")
        );
    }

    #[test]
    fn symlink_style_path_round_trips() {
        let auth = KeyAuth::new("/proc/1/root/home/user/.ssh/id_rsa");
        assert!(auth.key_path.starts_with("/proc"));
    }

    #[test]
    fn path_with_dots_round_trips() {
        let auth = KeyAuth::new("/home/user/../other_user/.ssh/id_rsa");
        assert!(auth.key_path.to_string_lossy().contains(".."));
    }

    #[test]
    fn path_with_multiple_extensions_round_trips() {
        let auth = KeyAuth::new("/home/user/.ssh/id_rsa.backup.old");
        assert_eq!(
            auth.key_path.file_name().and_then(|n| n.to_str()),
            Some("id_rsa.backup.old")
        );
    }

    #[test]
    fn auth_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<KeyAuth>();
    }

    #[test]
    fn very_long_path_round_trips() {
        let long_path = format!("/home/user/{}/id_rsa", "subdir/".repeat(100));
        let auth = KeyAuth::new(&long_path);
        assert!(auth.key_path.to_string_lossy().len() > 700);
    }

    #[test]
    fn name_is_static_str() {
        let auth = KeyAuth::new("/x");
        let n: &'static str = auth.name();
        assert_eq!(n, "key");
    }

    #[test]
    fn relative_path_preserved() {
        let auth = KeyAuth::new("relative/path/to/key");
        assert_eq!(auth.key_path, PathBuf::from("relative/path/to/key"));
    }

    #[test]
    fn tilde_only_expanded_when_at_start() {
        let auth = KeyAuth::new("/home/user/~/id_rsa");
        assert!(auth.key_path.to_string_lossy().contains('~'));
    }

    #[test]
    fn nested_tilde_path_expanded_at_start() {
        let auth = KeyAuth::new("~/.ssh/keys/server-prod");
        assert!(!auth.key_path.starts_with("~"));
        assert!(
            auth.key_path
                .to_string_lossy()
                .ends_with(".ssh/keys/server-prod")
        );
    }

    #[test]
    fn key_path_can_round_trip_to_string() {
        let auth = KeyAuth::new("/etc/ssh/host_key");
        assert_eq!(auth.key_path.to_string_lossy(), "/etc/ssh/host_key");
    }
}
