//! SSH agent authentication.

use async_trait::async_trait;
use russh::{client, keys};
use tracing::{debug, info};

use crate::mcp::session::SshClientHandler;

use super::traits::AuthStrategy;

/// SSH agent authentication strategy.
///
/// Connects to the SSH agent (via SSH_AUTH_SOCK) and tries each available
/// identity until one succeeds.
#[allow(dead_code)]
pub struct AgentAuth;

#[allow(dead_code)]
impl AgentAuth {
    /// Create a new SSH agent authentication strategy.
    pub fn new() -> Self {
        Self
    }
}

impl Default for AgentAuth {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AuthStrategy for AgentAuth {
    async fn authenticate(
        &self,
        handle: &mut client::Handle<SshClientHandler>,
        username: &str,
    ) -> Result<bool, String> {
        // Connect to the SSH agent
        let mut agent = keys::agent::client::AgentClient::connect_env()
            .await
            .map_err(|e| format!("Failed to connect to SSH agent: {}", e))?;

        // Get identities from the agent
        let identities = agent
            .request_identities()
            .await
            .map_err(|e| format!("Failed to get identities from SSH agent: {}", e))?;

        if identities.is_empty() {
            return Err("No identities found in SSH agent".to_string());
        }

        // Try each identity until one succeeds
        for identity in identities {
            debug!("Trying SSH agent identity: {:?}", identity.comment());

            // For RSA keys, use the best supported hash algorithm
            let hash_alg = handle
                .best_supported_rsa_hash()
                .await
                .ok()
                .flatten()
                .flatten();
            debug!("Using RSA hash algorithm: {:?}", hash_alg);

            match handle
                .authenticate_publickey_with(username, identity.clone(), hash_alg, &mut agent)
                .await
            {
                Ok(result) if result.success() => {
                    info!("Successfully authenticated with SSH agent");
                    return Ok(true);
                }
                Ok(_) => {
                    debug!("Agent identity not accepted, trying next...");
                    continue;
                }
                Err(e) => {
                    debug!("Agent authentication error: {}, trying next...", e);
                    continue;
                }
            }
        }

        Err("Agent authentication failed: no identities accepted".to_string())
    }

    fn name(&self) -> &'static str {
        "agent"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_auth_name() {
        let auth = AgentAuth::new();
        assert_eq!(auth.name(), "agent");
    }

    #[test]
    fn test_agent_auth_default() {
        let auth = AgentAuth::default();
        assert_eq!(auth.name(), "agent");
    }

    #[test]
    fn test_agent_auth_new_equals_default() {
        let auth_new = AgentAuth::new();
        let auth_default = AgentAuth::default();
        assert_eq!(auth_new.name(), auth_default.name());
    }

    #[test]
    fn test_agent_auth_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AgentAuth>();
    }

    #[test]
    fn test_agent_auth_multiple_instances() {
        // Verify we can create multiple independent instances
        let auth1 = AgentAuth::new();
        let auth2 = AgentAuth::new();
        let auth3 = AgentAuth::default();

        assert_eq!(auth1.name(), "agent");
        assert_eq!(auth2.name(), "agent");
        assert_eq!(auth3.name(), "agent");
    }

    #[test]
    fn test_agent_auth_implements_auth_strategy_trait() {
        fn requires_auth_strategy(_: &dyn AuthStrategy) {}
        let auth = AgentAuth::new();
        requires_auth_strategy(&auth);
    }
}
