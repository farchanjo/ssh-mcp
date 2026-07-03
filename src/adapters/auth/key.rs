//! Private-key [`AuthStrategyPort`] adapter.
//!
//! Routes [`Credentials::PrivateKey`] and validates the supplied PEM with
//! [`russh::keys::decode_secret_key`] (RSA / Ed25519 / ECDSA). A successful
//! decode yields [`AuthOutcome::Authenticated`]; non-matching credential
//! variants yield [`AuthOutcome::Rejected`]; an undecodable PEM (or wrong
//! passphrase) becomes [`AuthError::InvalidCredential`]. Like the password
//! adapter, the russh handshake itself stays in the
//! [`crate::ports::ssh_client::SshClientPort`] adapter (etapa H6).

use russh::keys;

use crate::domain::auth::{AuthError, AuthOutcome};
use crate::domain::identity::Credentials;
use crate::ports::auth_strategy::AuthStrategyPort;

/// Private-key authentication strategy adapter.
///
/// Stateless, zero-sized. The actual PEM material travels in the
/// [`Credentials::PrivateKey`] payload, so a single instance can serve every
/// authentication attempt.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyAuth;

impl AuthStrategyPort for KeyAuth {
    fn name(&self) -> &'static str {
        "private_key"
    }

    async fn authenticate(&self, credentials: &Credentials) -> Result<AuthOutcome, AuthError> {
        match credentials {
            Credentials::PrivateKey {
                key_pem,
                passphrase,
                ..
            } => {
                let pass: Option<&str> = passphrase.as_deref();
                match keys::decode_secret_key(key_pem, pass) {
                    Ok(_) => Ok(AuthOutcome::Authenticated),
                    Err(err) => Err(AuthError::InvalidCredential(format!(
                        "failed to decode private key: {err}"
                    ))),
                }
            }
            Credentials::Password { .. } | Credentials::Agent { .. } => Ok(AuthOutcome::Rejected),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::KeyAuth;
    use crate::domain::auth::{AuthError, AuthOutcome};
    use crate::domain::identity::{AgentSocketPath, Credentials};
    use crate::ports::auth_strategy::AuthStrategyPort;
    use std::path::PathBuf;

    fn _assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn key_auth_is_send_sync() {
        _assert_send_sync::<KeyAuth>();
    }

    #[test]
    fn name_is_private_key() {
        assert_eq!(KeyAuth.name(), "private_key");
    }

    #[tokio::test]
    async fn password_credentials_are_rejected() {
        let creds = Credentials::Password {
            username: "alice".to_string(),
            password: "secret".to_string(),
        };
        let outcome = KeyAuth.authenticate(&creds).await;
        assert_eq!(outcome, Ok(AuthOutcome::Rejected));
    }

    #[tokio::test]
    async fn agent_credentials_are_rejected() {
        let creds = Credentials::Agent {
            username: "carol".to_string(),
            socket: Some(AgentSocketPath::new(PathBuf::from("/tmp/agent.sock"))),
        };
        let outcome = KeyAuth.authenticate(&creds).await;
        assert_eq!(outcome, Ok(AuthOutcome::Rejected));
    }

    #[tokio::test]
    async fn malformed_pem_is_invalid_credential() {
        let creds = Credentials::PrivateKey {
            username: "bob".to_string(),
            key_pem: "not-a-valid-pem".to_string(),
            passphrase: None,
        };
        let outcome = KeyAuth.authenticate(&creds).await;
        assert!(matches!(outcome, Err(AuthError::InvalidCredential(_))));
    }

    #[tokio::test]
    async fn valid_ed25519_pem_authenticates() -> Result<(), Box<dyn std::error::Error>> {
        use russh::keys::ssh_key::{Algorithm, LineEnding, PrivateKey};

        // Generate a fresh Ed25519 key in-memory and round-trip through
        // the OpenSSH PEM serialiser so we feed a structurally valid PEM
        // to the adapter without committing private material to the repo.
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let pem = key.to_openssh(LineEnding::LF)?;

        let creds = Credentials::PrivateKey {
            username: "bob".to_string(),
            key_pem: pem.to_string(),
            passphrase: None,
        };
        let outcome = KeyAuth.authenticate(&creds).await;
        assert_eq!(outcome, Ok(AuthOutcome::Authenticated));
        Ok(())
    }
}
