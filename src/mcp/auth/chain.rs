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
pub struct AuthChain {
    strategies: Vec<Box<dyn AuthStrategy>>,
}

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
    #[cfg(test)]
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

    #[test]
    fn test_auth_chain_fluent_api_preserves_order() {
        let chain = AuthChain::new()
            .with_password("pass1")
            .with_key("/key1")
            .with_password("pass2")
            .with_agent()
            .with_key("/key2");

        // Check we have 5 strategies
        assert_eq!(chain.len(), 5);

        // Verify order by checking strategy names
        let names: Vec<_> = chain.strategies.iter().map(|s| s.name()).collect();
        assert_eq!(names, vec!["password", "key", "password", "agent", "key"]);
    }

    #[test]
    fn test_auth_chain_multiple_same_type() {
        let chain = AuthChain::new()
            .with_password("secret1")
            .with_password("secret2")
            .with_password("secret3");

        assert_eq!(chain.len(), 3);

        let names: Vec<_> = chain.strategies.iter().map(|s| s.name()).collect();
        assert_eq!(names, vec!["password", "password", "password"]);
    }

    #[test]
    fn test_auth_chain_single_strategy_not_empty() {
        let chain = AuthChain::new().with_agent();
        assert!(!chain.is_empty());
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn test_auth_chain_with_pathbuf_key() {
        let path = PathBuf::from("/custom/path/to/key");
        let chain = AuthChain::new().with_key(path);
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn test_auth_chain_with_string_password() {
        let password = String::from("my_password");
        let chain = AuthChain::new().with_password(password);
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn test_auth_chain_chaining_after_empty() {
        let chain = AuthChain::new();
        assert!(chain.is_empty());

        // Can still chain after checking
        let chain = chain.with_password("secret");
        assert!(!chain.is_empty());
    }

    #[test]
    fn test_auth_chain_implements_auth_strategy() {
        let chain = AuthChain::new().with_password("secret");

        // Verify it can be used as an AuthStrategy (compile-time check)
        fn requires_auth_strategy(_: &dyn AuthStrategy) {}
        requires_auth_strategy(&chain);
    }

    #[test]
    fn test_auth_chain_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AuthChain>();
    }

    #[test]
    fn test_auth_chain_empty_password() {
        let chain = AuthChain::new().with_password("");
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn test_auth_chain_empty_key_path() {
        let chain = AuthChain::new().with_key("");
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn test_auth_chain_many_strategies() {
        let mut chain = AuthChain::new();
        for i in 0..100 {
            chain = chain.with_password(format!("pass{}", i));
        }
        assert_eq!(chain.len(), 100);
    }

    #[test]
    fn test_auth_chain_mixed_types_preserves_count() {
        let chain = AuthChain::new()
            .with_password("p1")
            .with_key("/k1")
            .with_agent()
            .with_password("p2")
            .with_key("/k2")
            .with_agent();

        assert_eq!(chain.len(), 6);
    }

    #[test]
    fn test_auth_chain_all_agents() {
        let chain = AuthChain::new().with_agent().with_agent().with_agent();

        assert_eq!(chain.len(), 3);
        let names: Vec<_> = chain.strategies.iter().map(|s| s.name()).collect();
        assert!(names.iter().all(|n| *n == "agent"));
    }

    #[test]
    fn test_auth_chain_all_keys() {
        let chain = AuthChain::new()
            .with_key("/path/to/key1")
            .with_key("/path/to/key2")
            .with_key("/path/to/key3");

        assert_eq!(chain.len(), 3);
        let names: Vec<_> = chain.strategies.iter().map(|s| s.name()).collect();
        assert!(names.iter().all(|n| *n == "key"));
    }

    #[test]
    fn test_auth_chain_is_empty_after_clear_by_recreating() {
        let chain = AuthChain::new().with_password("secret");
        assert!(!chain.is_empty());

        // "Clear" by creating new chain
        let chain = AuthChain::new();
        assert!(chain.is_empty());
    }

    #[test]
    fn test_auth_chain_default_is_empty() {
        let chain = AuthChain::default();
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);
        assert_eq!(chain.name(), "chain");
    }
}
