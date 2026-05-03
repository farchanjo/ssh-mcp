//! Password [`AuthStrategyPort`] adapter.
//!
//! Routes [`Credentials::Password`] to a positive `Authenticated` outcome and
//! signals `Rejected` for any other credential variant. The adapter does
//! **not** drive the russh handshake itself — that lives in the
//! [`crate::ports::ssh_client::SshClientPort`] adapter (etapa H6) where the
//! `russh::client::Handle` is owned. Validation here is limited to: the
//! supplied password must be non-empty.
//!
//! Keeping the strategy chain as a *routing* layer (variant + sanity check
//! only) lets use cases compose strategies generically while delegating the
//! transport handshake to the single owner of the russh handle.

use crate::domain::auth::{AuthError, AuthOutcome};
use crate::domain::identity::Credentials;
use crate::ports::auth_strategy::AuthStrategyPort;

/// Password authentication strategy adapter.
///
/// Stateless, zero-sized. Construct with [`PasswordAuth::default`] (or
/// `PasswordAuth`) and share via `Arc<PasswordAuth>` when used inside the
/// chain.
#[derive(Debug, Default, Clone, Copy)]
pub struct PasswordAuth;

impl AuthStrategyPort for PasswordAuth {
    fn name(&self) -> &'static str {
        "password"
    }

    async fn authenticate(&self, credentials: &Credentials) -> Result<AuthOutcome, AuthError> {
        match credentials {
            Credentials::Password { password, .. } => {
                if password.is_empty() {
                    return Err(AuthError::InvalidCredential(
                        "password must not be empty".to_string(),
                    ));
                }
                Ok(AuthOutcome::Authenticated)
            }
            Credentials::PrivateKey { .. } | Credentials::Agent { .. } => Ok(AuthOutcome::Rejected),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PasswordAuth;
    use crate::domain::auth::{AuthError, AuthOutcome};
    use crate::domain::identity::{AgentSocketPath, Credentials};
    use crate::ports::auth_strategy::AuthStrategyPort;
    use std::path::PathBuf;

    fn _assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn password_auth_is_send_sync() {
        _assert_send_sync::<PasswordAuth>();
    }

    #[test]
    fn name_is_password() {
        assert_eq!(PasswordAuth.name(), "password");
    }

    #[tokio::test]
    async fn password_credentials_authenticate() {
        let creds = Credentials::Password {
            username: "alice".to_string(),
            password: "secret".to_string(),
        };
        let outcome = PasswordAuth.authenticate(&creds).await;
        assert_eq!(outcome, Ok(AuthOutcome::Authenticated));
    }

    #[tokio::test]
    async fn private_key_credentials_are_rejected() {
        let creds = Credentials::PrivateKey {
            username: "bob".to_string(),
            key_pem: "irrelevant".to_string(),
            passphrase: None,
        };
        let outcome = PasswordAuth.authenticate(&creds).await;
        assert_eq!(outcome, Ok(AuthOutcome::Rejected));
    }

    #[tokio::test]
    async fn agent_credentials_are_rejected() {
        let creds = Credentials::Agent {
            username: "carol".to_string(),
            socket: Some(AgentSocketPath::new(PathBuf::from("/tmp/agent.sock"))),
        };
        let outcome = PasswordAuth.authenticate(&creds).await;
        assert_eq!(outcome, Ok(AuthOutcome::Rejected));
    }

    #[tokio::test]
    async fn empty_password_is_invalid_credential() {
        let creds = Credentials::Password {
            username: "alice".to_string(),
            password: String::new(),
        };
        let outcome = PasswordAuth.authenticate(&creds).await;
        assert_eq!(
            outcome,
            Err(AuthError::InvalidCredential(
                "password must not be empty".to_string()
            ))
        );
    }
}
