//! Authentication chain for trying multiple strategies.

use std::path::PathBuf;

use async_trait::async_trait;
use russh::client;
use tracing::debug;

use crate::mcp::session::SshClientHandler;

use super::traits::AuthStrategy;
use super::{AgentAuth, KeyAuth, PasswordAuth};

/// Authentication chain that tries multiple strategies in order.
///
/// Strategies are tried in the order they were added. The first successful
/// authentication stops the chain and returns success.
///
/// # Example
///
/// ```ignore
/// let chain = AuthChain::new()
///     .with_password("secret")
///     .with_key("/path/to/key")
///     .with_agent();
///
/// let result = chain.authenticate(&mut handle, "username").await?;
/// ```
#[allow(dead_code)]
pub struct AuthChain {
    strategies: Vec<Box<dyn AuthStrategy>>,
}

#[allow(dead_code)]
impl AuthChain {
    /// Create a new empty authentication chain.
    pub fn new() -> Self {
        Self {
            strategies: Vec::new(),
        }
    }

    /// Add password authentication to the chain.
    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.strategies.push(Box::new(PasswordAuth::new(password)));
        self
    }

    /// Add key-based authentication to the chain.
    pub fn with_key(mut self, key_path: impl Into<PathBuf>) -> Self {
        self.strategies.push(Box::new(KeyAuth::new(key_path)));
        self
    }

    /// Add SSH agent authentication to the chain.
    pub fn with_agent(mut self) -> Self {
        self.strategies.push(Box::new(AgentAuth::new()));
        self
    }

    /// Check if the chain has any authentication strategies.
    pub fn is_empty(&self) -> bool {
        self.strategies.is_empty()
    }

    /// Get the number of strategies in the chain.
    pub fn len(&self) -> usize {
        self.strategies.len()
    }
}

impl Default for AuthChain {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AuthStrategy for AuthChain {
    async fn authenticate(
        &self,
        handle: &mut client::Handle<SshClientHandler>,
        username: &str,
    ) -> Result<bool, String> {
        if self.strategies.is_empty() {
            return Err("No authentication strategies configured".to_string());
        }

        let mut last_error = None;

        for strategy in &self.strategies {
            debug!("Trying authentication strategy: {}", strategy.name());

            match strategy.authenticate(handle, username).await {
                Ok(true) => {
                    debug!(
                        "Authentication succeeded with strategy: {}",
                        strategy.name()
                    );
                    return Ok(true);
                }
                Ok(false) => {
                    debug!("Authentication failed with strategy: {}", strategy.name());
                    last_error = Some(format!("{} authentication rejected", strategy.name()));
                }
                Err(e) => {
                    debug!(
                        "Authentication error with strategy {}: {}",
                        strategy.name(),
                        e
                    );
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| "All authentication methods failed".to_string()))
    }

    fn name(&self) -> &'static str {
        "chain"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_chain_empty() {
        let chain = AuthChain::new();
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);
    }

    #[test]
    fn test_auth_chain_with_password() {
        let chain = AuthChain::new().with_password("secret");
        assert!(!chain.is_empty());
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn test_auth_chain_with_key() {
        let chain = AuthChain::new().with_key("/path/to/key");
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn test_auth_chain_with_agent() {
        let chain = AuthChain::new().with_agent();
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn test_auth_chain_multiple() {
        let chain = AuthChain::new()
            .with_password("secret")
            .with_key("/path/to/key")
            .with_agent();
        assert_eq!(chain.len(), 3);
    }

    #[test]
    fn test_auth_chain_name() {
        let chain = AuthChain::new();
        assert_eq!(chain.name(), "chain");
    }

    #[test]
    fn test_auth_chain_default() {
        let chain = AuthChain::default();
        assert!(chain.is_empty());
    }
}
