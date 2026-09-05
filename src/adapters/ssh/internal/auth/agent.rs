//! SSH agent authentication.

use russh::{client, keys};
#[cfg(unix)]
use tokio::net::UnixStream;
use tracing::{debug, info};

use crate::adapters::ssh::internal::session::SshClientHandler;

use super::traits::AuthStrategy;

/// OpenSSH's well-known agent pipe on Windows.
#[cfg(windows)]
const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// Connect an [`AgentClient`] to the platform agent endpoint.
///
/// Unix: the socket named by `SSH_AUTH_SOCK`. Windows: the OpenSSH
/// For Windows agent named pipe (`\\.\pipe\openssh-ssh-agent`).
#[cfg(unix)]
async fn connect_agent() -> Result<keys::agent::client::AgentClient<UnixStream>, String> {
    keys::agent::client::AgentClient::connect_env()
        .await
        .map_err(|e| format!("Failed to connect to SSH agent: {e}"))
}

/// Windows counterpart of [`connect_agent`]: connects to the OpenSSH
/// agent named pipe. `SSH_AUTH_SOCK` has no meaning on Windows —
/// OpenSSH for Windows always uses the fixed pipe path.
#[cfg(windows)]
async fn connect_agent() -> Result<
    keys::agent::client::AgentClient<tokio::net::windows::named_pipe::NamedPipeClient>,
    String,
> {
    keys::agent::client::AgentClient::connect_named_pipe(OPENSSH_AGENT_PIPE)
        .await
        .map_err(|e| format!("Failed to connect to SSH agent named pipe: {e}"))
}

/// SSH agent authentication strategy.
///
/// Connects to the SSH agent (via `SSH_AUTH_SOCK`) and tries each
/// available identity until one succeeds.
pub struct AgentAuth;

impl AgentAuth {
    /// Create a new SSH agent authentication strategy.
    pub const fn new() -> Self {
        Self
    }
}

impl Default for AgentAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthStrategy for AgentAuth {
    async fn authenticate(
        &self,
        handle: &mut client::Handle<SshClientHandler>,
        username: &str,
    ) -> Result<bool, String> {
        let mut agent = connect_agent().await?;

        let identities = agent
            .request_identities()
            .await
            .map_err(|e| format!("Failed to get identities from SSH agent: {e}"))?;

        if identities.is_empty() {
            return Err("No identities found in SSH agent".to_string());
        }

        try_identities(handle, username, &identities, &mut agent).await
    }
}

/// Try each identity from the SSH agent until one succeeds.
///
/// Generic over the agent transport stream so the same loop drives the
/// Unix-domain socket (Unix) and the named pipe (Windows).
async fn try_identities<S>(
    handle: &mut client::Handle<SshClientHandler>,
    username: &str,
    identities: &[keys::agent::AgentIdentity],
    agent: &mut keys::agent::client::AgentClient<S>,
) -> Result<bool, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    for identity in identities {
        debug!("Trying SSH agent identity: {:?}", identity.comment());

        let hash_alg = handle
            .best_supported_rsa_hash()
            .await
            .ok()
            .flatten()
            .flatten();
        debug!("Using RSA hash algorithm: {:?}", hash_alg);

        let public_key = identity.public_key().into_owned();
        match handle
            .authenticate_publickey_with(username, public_key, hash_alg, agent)
            .await
        {
            Ok(result) if result.success() => {
                info!("Successfully authenticated with SSH agent");
                return Ok(true);
            }
            Ok(_) => {
                debug!("Agent identity not accepted, trying next...");
            }
            Err(e) => {
                debug!("Agent authentication error: {e}, trying next...");
            }
        }
    }

    Err("Agent authentication failed: no identities accepted".to_string())
}

#[cfg(test)]
mod tests {
    use super::AgentAuth;

    #[test]
    fn auth_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AgentAuth>();
    }

    #[test]
    fn auth_is_const_constructible() {
        // `new` is `const fn` — the result can be assigned to a const.
        const _AGENT: AgentAuth = AgentAuth::new();
    }

    #[test]
    fn auth_unit_struct_size_is_zero() {
        // Unit-like struct: zero-sized.
        assert_eq!(std::mem::size_of::<AgentAuth>(), 0);
    }
}
