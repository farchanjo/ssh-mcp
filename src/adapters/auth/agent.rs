//! SSH agent [`AuthStrategyPort`] adapter.
//!
//! Routes [`Credentials::Agent`] and probes the agent socket so the chain
//! short-circuits when the agent is unreachable. We deliberately keep the
//! probe to a low-cost identity listing — the actual public-key handshake
//! requires the russh `Handle` and lives in the
//! [`crate::ports::ssh_client::SshClientPort`] adapter (etapa H6).
//!
//! Outcomes:
//! - [`AuthOutcome::Authenticated`] when the agent answered with at least one
//!   identity.
//! - [`AuthOutcome::Rejected`] for non-`Agent` credential variants OR when
//!   the agent is reachable but holds no identities (the chain falls through
//!   to the next strategy).
//! - [`AuthError::AgentUnavailable`] when the socket cannot be opened or the
//!   `request_identities` round-trip fails.

use std::path::Path;

use russh::keys;
#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(windows)]
use tokio::net::windows::named_pipe::NamedPipeClient;

use crate::domain::auth::{AuthError, AuthOutcome};
use crate::domain::identity::{AgentSocketPath, Credentials};
use crate::ports::auth_strategy::AuthStrategyPort;

/// SSH agent authentication strategy adapter.
///
/// Stateless, zero-sized. The agent socket path travels inside the
/// [`Credentials::Agent`] payload (`None` => fall back to `SSH_AUTH_SOCK`).
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentAuth;

/// Open the platform agent transport for the given socket/pipe path.
///
/// Unix: `socket` (or `SSH_AUTH_SOCK` when `None`) names a Unix-domain
/// socket. Windows: OpenSSH for Windows always serves the fixed named
/// pipe `\\.\pipe\openssh-ssh-agent` — `socket`, when present, overrides
/// it with a custom pipe path.
#[cfg(unix)]
async fn open_agent_client(
    socket: Option<&Path>,
) -> Result<keys::agent::client::AgentClient<UnixStream>, AuthError> {
    let fail = |err: keys::Error| AuthError::AgentUnavailable(err.to_string());
    match socket {
        Some(path) => keys::agent::client::AgentClient::connect_uds(path)
            .await
            .map_err(fail),
        None => keys::agent::client::AgentClient::connect_env()
            .await
            .map_err(fail),
    }
}

/// Windows counterpart of [`open_agent_client`].
#[cfg(windows)]
async fn open_agent_client(
    socket: Option<&Path>,
) -> Result<keys::agent::client::AgentClient<NamedPipeClient>, AuthError> {
    let fail = |err: keys::Error| AuthError::AgentUnavailable(err.to_string());
    let default_pipe = Path::new(r"\\.\pipe\openssh-ssh-agent");
    keys::agent::client::AgentClient::connect_named_pipe(socket.unwrap_or(default_pipe))
        .await
        .map_err(fail)
}

/// Unsupported-platform stub of [`open_agent_client`].
#[cfg(not(any(unix, windows)))]
async fn open_agent_client(
    _socket: Option<&Path>,
) -> Result<keys::agent::client::AgentClient<std::io::Cursor<Vec<u8>>>, AuthError> {
    Err(AuthError::AgentUnavailable(
        "SSH agent transport unsupported on this platform".to_string(),
    ))
}

impl AuthStrategyPort for AgentAuth {
    fn name(&self) -> &'static str {
        "agent"
    }

    async fn authenticate(&self, credentials: &Credentials) -> Result<AuthOutcome, AuthError> {
        let Credentials::Agent { socket, .. } = credentials else {
            return Ok(AuthOutcome::Rejected);
        };

        let mut client = open_agent_client(socket.as_ref().map(AgentSocketPath::as_path)).await?;

        let identities = client
            .request_identities()
            .await
            .map_err(|err| AuthError::AgentUnavailable(err.to_string()))?;

        if identities.is_empty() {
            Ok(AuthOutcome::Rejected)
        } else {
            Ok(AuthOutcome::Authenticated)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AgentAuth;
    use crate::domain::auth::{AuthError, AuthOutcome};
    use crate::domain::identity::{AgentSocketPath, Credentials};
    use crate::ports::auth_strategy::AuthStrategyPort;
    use std::path::PathBuf;

    fn _assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn agent_auth_is_send_sync() {
        _assert_send_sync::<AgentAuth>();
    }

    #[test]
    fn name_is_agent() {
        assert_eq!(AgentAuth.name(), "agent");
    }

    #[tokio::test]
    async fn password_credentials_are_rejected() {
        let creds = Credentials::Password {
            username: "alice".to_string(),
            password: "secret".to_string(),
        };
        let outcome = AgentAuth.authenticate(&creds).await;
        assert_eq!(outcome, Ok(AuthOutcome::Rejected));
    }

    #[tokio::test]
    async fn private_key_credentials_are_rejected() {
        let creds = Credentials::PrivateKey {
            username: "bob".to_string(),
            key_pem: "irrelevant".to_string(),
            passphrase: None,
        };
        let outcome = AgentAuth.authenticate(&creds).await;
        assert_eq!(outcome, Ok(AuthOutcome::Rejected));
    }

    #[tokio::test]
    async fn unreachable_socket_yields_agent_unavailable() {
        // A non-existent path forces the UDS connect to fail synchronously.
        let creds = Credentials::Agent {
            username: "carol".to_string(),
            socket: Some(AgentSocketPath::new(PathBuf::from(
                "/tmp/ssh-mcp-h8-nonexistent.sock",
            ))),
        };
        let outcome = AgentAuth.authenticate(&creds).await;
        assert!(matches!(outcome, Err(AuthError::AgentUnavailable(_))));
    }
}
