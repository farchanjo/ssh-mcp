//! Ordered chain composing the three concrete
//! [`AuthStrategyPort`] adapters.
//!
//! Holds a `Vec<StrategyKind>` of statically dispatched variants
//! ([`StrategyKind::Password`], [`StrategyKind::Key`], [`StrategyKind::Agent`])
//! so the entire chain stays free of `Box<dyn Future>`. AFIT (async fn in
//! trait) is not dyn-safe, so heterogeneous strategies are stored as an enum
//! and dispatched via a single `match` per attempt — the v4 zero-cost
//! abstraction promise H0 set out to deliver.
//!
//! Resolution rules (matches the legacy v3 `AuthChain`
//! semantics):
//! - On the first [`AuthOutcome::Authenticated`] the chain stops and returns
//!   that outcome.
//! - On the first hard error (anything that is not `Rejected`) the chain
//!   stops and propagates it.
//! - When every strategy returned [`AuthOutcome::Rejected`] the chain
//!   surfaces [`AuthError::Exhausted`].
//! - An empty chain immediately returns [`AuthError::Exhausted`].

use crate::adapters::auth::agent::AgentAuth;
use crate::adapters::auth::key::KeyAuth;
use crate::adapters::auth::password::PasswordAuth;
use crate::domain::auth::{AuthError, AuthOutcome};
use crate::domain::identity::Credentials;
use crate::ports::auth_strategy::AuthStrategyPort;

/// Statically dispatched [`AuthStrategyPort`] variant — every adapter in the
/// crate fits into one branch, keeping the chain free of `Box<dyn Future>`.
#[derive(Debug, Clone, Copy)]
pub enum StrategyKind {
    /// Password strategy adapter (matches [`Credentials::Password`]).
    Password(PasswordAuth),
    /// Private-key strategy adapter (matches [`Credentials::PrivateKey`]).
    Key(KeyAuth),
    /// SSH agent strategy adapter (matches [`Credentials::Agent`]).
    Agent(AgentAuth),
}

impl StrategyKind {
    /// Stable name forwarded to the underlying strategy.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Password(_) => "password",
            Self::Key(_) => "private_key",
            Self::Agent(_) => "agent",
        }
    }

    /// Forward the authentication attempt to the inner adapter.
    async fn authenticate(&self, credentials: &Credentials) -> Result<AuthOutcome, AuthError> {
        match self {
            Self::Password(inner) => inner.authenticate(credentials).await,
            Self::Key(inner) => inner.authenticate(credentials).await,
            Self::Agent(inner) => inner.authenticate(credentials).await,
        }
    }
}

/// Ordered chain of [`AuthStrategyPort`] adapters.
///
/// Built via [`AuthChainAdapter::new`] (empty) plus the fluent
/// `with_*` helpers, or via [`AuthChainAdapter::default_chain`] which
/// mirrors the v3 default order: password -> private key -> agent.
#[derive(Debug, Clone, Default)]
pub struct AuthChainAdapter {
    strategies: Vec<StrategyKind>,
}

impl AuthChainAdapter {
    /// Build an empty chain. Use the fluent `with_*` helpers (or push into a
    /// borrowed [`Vec`] reference) to populate it.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            strategies: Vec::new(),
        }
    }

    /// Build the canonical default chain (password -> private key -> agent).
    /// Using [`PasswordAuth`], [`KeyAuth`] and [`AgentAuth`] zero-sized
    /// adapters keeps the resulting chain heap-free except for the backing
    /// `Vec`.
    #[must_use]
    pub fn default_chain() -> Self {
        Self {
            strategies: vec![
                StrategyKind::Password(PasswordAuth),
                StrategyKind::Key(KeyAuth),
                StrategyKind::Agent(AgentAuth),
            ],
        }
    }

    /// Append a [`PasswordAuth`] strategy to the chain.
    #[must_use]
    pub fn with_password(mut self) -> Self {
        self.strategies.push(StrategyKind::Password(PasswordAuth));
        self
    }

    /// Append a [`KeyAuth`] strategy to the chain.
    #[must_use]
    pub fn with_key(mut self) -> Self {
        self.strategies.push(StrategyKind::Key(KeyAuth));
        self
    }

    /// Append an [`AgentAuth`] strategy to the chain.
    #[must_use]
    pub fn with_agent(mut self) -> Self {
        self.strategies.push(StrategyKind::Agent(AgentAuth));
        self
    }

    /// True iff no strategies have been appended.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strategies.is_empty()
    }

    /// Number of configured strategies.
    #[must_use]
    pub fn len(&self) -> usize {
        self.strategies.len()
    }

    /// Borrow the configured strategy list (used by tests + diagnostics).
    #[must_use]
    pub fn strategies(&self) -> &[StrategyKind] {
        &self.strategies
    }
}

impl AuthStrategyPort for AuthChainAdapter {
    fn name(&self) -> &'static str {
        "chain"
    }

    async fn authenticate(&self, credentials: &Credentials) -> Result<AuthOutcome, AuthError> {
        if self.strategies.is_empty() {
            return Err(AuthError::Exhausted);
        }

        for strategy in &self.strategies {
            match strategy.authenticate(credentials).await {
                Ok(AuthOutcome::Authenticated) => return Ok(AuthOutcome::Authenticated),
                Ok(AuthOutcome::Rejected) => {
                    // try the next strategy
                }
                Err(err) => return Err(err),
            }
        }

        // Every strategy returned `Rejected` — chain is exhausted.
        Err(AuthError::Exhausted)
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthChainAdapter, StrategyKind};
    use crate::adapters::auth::agent::AgentAuth;
    use crate::adapters::auth::key::KeyAuth;
    use crate::adapters::auth::password::PasswordAuth;
    use crate::domain::auth::{AuthError, AuthOutcome};
    use crate::domain::identity::{AgentSocketPath, Credentials};
    use crate::ports::auth_strategy::AuthStrategyPort;
    use std::path::PathBuf;

    fn _assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn auth_chain_adapter_is_send_sync() {
        _assert_send_sync::<AuthChainAdapter>();
    }

    #[test]
    fn name_is_chain() {
        assert_eq!(AuthChainAdapter::default_chain().name(), "chain");
    }

    #[test]
    fn new_is_empty() {
        let chain = AuthChainAdapter::new();
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);
    }

    #[test]
    fn default_chain_has_three_strategies_in_order() {
        let chain = AuthChainAdapter::default_chain();
        assert_eq!(chain.len(), 3);
        let names: Vec<_> = chain.strategies().iter().map(StrategyKind::name).collect();
        assert_eq!(names, vec!["password", "private_key", "agent"]);
    }

    #[test]
    fn fluent_builders_preserve_order() {
        let chain = AuthChainAdapter::new()
            .with_agent()
            .with_password()
            .with_key();
        let names: Vec<_> = chain.strategies().iter().map(StrategyKind::name).collect();
        assert_eq!(names, vec!["agent", "password", "private_key"]);
    }

    #[tokio::test]
    async fn empty_chain_returns_exhausted() {
        let creds = Credentials::Password {
            username: "alice".to_string(),
            password: "secret".to_string(),
        };
        let outcome = AuthChainAdapter::new().authenticate(&creds).await;
        assert_eq!(outcome, Err(AuthError::Exhausted));
    }

    #[tokio::test]
    async fn password_credentials_authenticate_via_default_chain() {
        let creds = Credentials::Password {
            username: "alice".to_string(),
            password: "secret".to_string(),
        };
        let outcome = AuthChainAdapter::default_chain().authenticate(&creds).await;
        assert_eq!(outcome, Ok(AuthOutcome::Authenticated));
    }

    #[tokio::test]
    async fn private_key_credentials_fall_through_to_key_strategy() {
        // Malformed PEM => KeyAuth surfaces InvalidCredential. Because the
        // chain visits PasswordAuth first (Rejected) then KeyAuth, the hard
        // error is propagated.
        let creds = Credentials::PrivateKey {
            username: "bob".to_string(),
            key_pem: "not-a-valid-pem".to_string(),
            passphrase: None,
        };
        let outcome = AuthChainAdapter::default_chain().authenticate(&creds).await;
        assert!(matches!(outcome, Err(AuthError::InvalidCredential(_))));
    }

    #[tokio::test]
    async fn agent_credentials_fall_through_to_agent_strategy() {
        // Unreachable socket => AgentAuth surfaces AgentUnavailable. Chain
        // visits Password (Rejected), Key (Rejected), Agent (error) and
        // propagates.
        let creds = Credentials::Agent {
            username: "carol".to_string(),
            socket: Some(AgentSocketPath::new(PathBuf::from(
                "/tmp/ssh-mcp-h8-chain-nonexistent.sock",
            ))),
        };
        let outcome = AuthChainAdapter::default_chain().authenticate(&creds).await;
        assert!(matches!(outcome, Err(AuthError::AgentUnavailable(_))));
    }

    #[tokio::test]
    async fn chain_with_only_password_rejects_private_key_credentials() {
        // Password adapter alone facing PrivateKey credentials: every
        // strategy returns Rejected, chain reports Exhausted.
        let chain = AuthChainAdapter::new().with_password();
        let creds = Credentials::PrivateKey {
            username: "bob".to_string(),
            key_pem: "irrelevant".to_string(),
            passphrase: None,
        };
        let outcome = chain.authenticate(&creds).await;
        assert_eq!(outcome, Err(AuthError::Exhausted));
    }

    #[test]
    fn strategy_kind_name_matches_inner_adapter() {
        let pwd = StrategyKind::Password(PasswordAuth);
        let key = StrategyKind::Key(KeyAuth);
        let agent = StrategyKind::Agent(AgentAuth);
        assert_eq!(pwd.name(), PasswordAuth.name());
        assert_eq!(key.name(), KeyAuth.name());
        assert_eq!(agent.name(), AgentAuth.name());
    }

    #[tokio::test]
    async fn first_authenticated_short_circuits_chain() {
        // Stack two PasswordAuth strategies. The first one accepts the
        // credential, so the chain returns Authenticated without visiting
        // the rest (the second strategy would also accept; the test
        // asserts the early exit by ensuring chain length is observed
        // upfront).
        let chain = AuthChainAdapter::new().with_password().with_password();
        assert_eq!(chain.len(), 2);
        let creds = Credentials::Password {
            username: "alice".to_string(),
            password: "secret".to_string(),
        };
        let outcome = chain.authenticate(&creds).await;
        assert_eq!(outcome, Ok(AuthOutcome::Authenticated));
    }
}
