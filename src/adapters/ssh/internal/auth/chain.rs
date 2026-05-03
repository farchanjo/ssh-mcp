//! Authentication chain for trying multiple strategies.
//!
//! Each variant of [`Strategy`] wraps one concrete russh-handshake
//! strategy. Since AFIT (async `fn` in trait) is not dyn-safe, we
//! cannot store `Box<dyn AuthStrategy>` — the chain holds an enum and
//! dispatches statically through a single `match`. The trade-off is
//! one `match` per strategy attempt in exchange for dropping the
//! `async-trait` direct dependency from the crate.

use std::path::PathBuf;

use russh::client;
use tracing::debug;

use crate::adapters::ssh::internal::session::SshClientHandler;

use super::agent::AgentAuth;
use super::key::KeyAuth;
use super::password::PasswordAuth;
use super::traits::AuthStrategy;

/// Statically dispatched russh-handshake strategy variant.
///
/// AFIT is not dyn-safe, so heterogeneous strategies cannot be stored
/// behind `Box<dyn AuthStrategy>`. This enum keeps the chain free of
/// dynamic dispatch (and therefore free of `async-trait`) while still
/// supporting every authentication method the v3 chain offered.
pub(crate) enum Strategy {
    /// Password handshake.
    Password(PasswordAuth),
    /// Private key file handshake.
    Key(KeyAuth),
    /// SSH agent handshake.
    Agent(AgentAuth),
}

impl Strategy {
    /// Forward the authentication call to the inner strategy.
    async fn authenticate(
        &self,
        handle: &mut client::Handle<SshClientHandler>,
        username: &str,
    ) -> Result<bool, String> {
        match self {
            Self::Password(inner) => inner.authenticate(handle, username).await,
            Self::Key(inner) => inner.authenticate(handle, username).await,
            Self::Agent(inner) => inner.authenticate(handle, username).await,
        }
    }

    /// Forward the strategy name (used for logging).
    const fn name(&self) -> &'static str {
        match self {
            Self::Password(_) => "password",
            Self::Key(_) => "key",
            Self::Agent(_) => "agent",
        }
    }
}

/// Authentication chain that tries multiple strategies in order.
///
/// Strategies are tried in the order they were added. The first
/// successful authentication stops the chain and returns success.
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
pub(crate) struct AuthChain {
    strategies: Vec<Strategy>,
}

impl AuthChain {
    /// Create a new empty authentication chain.
    pub(crate) const fn new() -> Self {
        Self {
            strategies: Vec::new(),
        }
    }

    /// Add password authentication to the chain.
    #[must_use]
    pub(crate) fn with_password(mut self, password: impl Into<String>) -> Self {
        self.strategies
            .push(Strategy::Password(PasswordAuth::new(password)));
        self
    }

    /// Add key-based authentication to the chain.
    #[must_use]
    pub(crate) fn with_key(mut self, key_path: impl Into<PathBuf>) -> Self {
        self.strategies.push(Strategy::Key(KeyAuth::new(key_path)));
        self
    }

    /// Add SSH agent authentication to the chain.
    #[must_use]
    pub(crate) fn with_agent(mut self) -> Self {
        self.strategies.push(Strategy::Agent(AgentAuth::new()));
        self
    }

    /// Check if the chain has any authentication strategies.
    pub(crate) fn is_empty(&self) -> bool {
        self.strategies.is_empty()
    }

    /// Get the number of strategies in the chain.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.strategies.len()
    }
}

impl Default for AuthChain {
    fn default() -> Self {
        Self::new()
    }
}

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
    use super::{AuthChain, AuthStrategy, PathBuf};

    #[test]
    fn empty_chain_has_zero_strategies() {
        let chain = AuthChain::new();
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);
    }

    #[test]
    fn with_password_appends_strategy() {
        let chain = AuthChain::new().with_password("secret");
        assert!(!chain.is_empty());
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn with_key_appends_strategy() {
        let chain = AuthChain::new().with_key("/path/to/key");
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn with_agent_appends_strategy() {
        let chain = AuthChain::new().with_agent();
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn fluent_api_appends_in_order() {
        let chain = AuthChain::new()
            .with_password("secret")
            .with_key("/path/to/key")
            .with_agent();
        assert_eq!(chain.len(), 3);
    }

    #[test]
    fn name_is_chain() {
        let chain = AuthChain::new();
        assert_eq!(chain.name(), "chain");
    }

    #[test]
    fn default_constructs_empty_chain() {
        let chain = AuthChain::default();
        assert!(chain.is_empty());
    }

    #[test]
    fn fluent_api_preserves_order() {
        let chain = AuthChain::new()
            .with_password("pass1")
            .with_key("/key1")
            .with_password("pass2")
            .with_agent()
            .with_key("/key2");

        assert_eq!(chain.len(), 5);

        let names: Vec<_> = chain.strategies.iter().map(super::Strategy::name).collect();
        assert_eq!(names, vec!["password", "key", "password", "agent", "key"]);
    }

    #[test]
    fn supports_multiple_password_strategies() {
        let chain = AuthChain::new()
            .with_password("secret1")
            .with_password("secret2")
            .with_password("secret3");

        assert_eq!(chain.len(), 3);

        let names: Vec<_> = chain.strategies.iter().map(super::Strategy::name).collect();
        assert_eq!(names, vec!["password", "password", "password"]);
    }

    #[test]
    fn single_strategy_makes_chain_non_empty() {
        let chain = AuthChain::new().with_agent();
        assert!(!chain.is_empty());
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn accepts_owned_pathbuf_for_key() {
        let path = PathBuf::from("/custom/path/to/key");
        let chain = AuthChain::new().with_key(path);
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn accepts_owned_string_for_password() {
        let password = String::from("my_password");
        let chain = AuthChain::new().with_password(password);
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn supports_chaining_after_empty_check() {
        let chain = AuthChain::new();
        assert!(chain.is_empty());

        let chain = chain.with_password("secret");
        assert!(!chain.is_empty());
    }

    #[test]
    fn chain_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AuthChain>();
    }

    #[test]
    fn empty_password_is_allowed() {
        let chain = AuthChain::new().with_password("");
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn empty_key_path_is_allowed() {
        let chain = AuthChain::new().with_key("");
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn supports_many_strategies() {
        let mut chain = AuthChain::new();
        for i in 0..100_u32 {
            chain = chain.with_password(format!("pass{i}"));
        }
        assert_eq!(chain.len(), 100);
    }

    #[test]
    fn mixed_types_preserve_count() {
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
    fn supports_only_agent_strategies() {
        let chain = AuthChain::new().with_agent().with_agent().with_agent();

        assert_eq!(chain.len(), 3);
        let names: Vec<_> = chain.strategies.iter().map(super::Strategy::name).collect();
        assert!(names.iter().all(|n| *n == "agent"));
    }

    #[test]
    fn supports_only_key_strategies() {
        let chain = AuthChain::new()
            .with_key("/path/to/key1")
            .with_key("/path/to/key2")
            .with_key("/path/to/key3");

        assert_eq!(chain.len(), 3);
        let names: Vec<_> = chain.strategies.iter().map(super::Strategy::name).collect();
        assert!(names.iter().all(|n| *n == "key"));
    }

    #[test]
    fn rebuilding_chain_resets_state() {
        let chain = AuthChain::new().with_password("secret");
        assert!(!chain.is_empty());

        let chain = AuthChain::new();
        assert!(chain.is_empty());
    }

    #[test]
    fn default_is_empty_with_chain_name() {
        let chain = AuthChain::default();
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);
        assert_eq!(chain.name(), "chain");
    }

    #[test]
    fn chain_name_returns_static_str() {
        let chain = AuthChain::new();
        let n: &'static str = chain.name();
        assert_eq!(n, "chain");
    }

    #[test]
    fn explicit_strategies_keep_order() {
        let chain = AuthChain::new()
            .with_agent()
            .with_password("p")
            .with_key("/k");
        let names: Vec<_> = chain.strategies.iter().map(super::Strategy::name).collect();
        assert_eq!(names, vec!["agent", "password", "key"]);
    }

    #[test]
    fn len_grows_monotonically() {
        let mut chain = AuthChain::new();
        for i in 1..=10_usize {
            chain = chain.with_password(format!("p{i}"));
            assert_eq!(chain.len(), i);
        }
    }

    #[test]
    fn empty_chain_reports_is_empty() {
        let chain = AuthChain::new();
        assert!(chain.is_empty());
    }
}
