//! Authentication strategy port.
//!
//! Replaces the v3 `#[async_trait]` `AuthStrategy` with an AFIT skeleton
//! built via `trait_variant`. Use cases hold a chain of strategies and call
//! them sequentially until one returns [`AuthOutcome::Authenticated`] or
//! the chain is exhausted.

use crate::domain::auth::{AuthError, AuthOutcome};
use crate::domain::identity::Credentials;

/// SSH authentication strategy port.
#[trait_variant::make(AuthStrategyPort: Send)]
pub trait LocalAuthStrategyPort: Sync {
    /// Stable name for logging (`"password"`, `"private_key"`, `"agent"`).
    fn name(&self) -> &'static str;

    /// Attempt the authentication handshake using `credentials`.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Rejected`] when the server rejects the
    /// credentials, [`AuthError::AgentUnavailable`] when an agent socket
    /// cannot be reached, or [`AuthError::Transport`] for protocol errors.
    async fn authenticate(&self, credentials: &Credentials) -> Result<AuthOutcome, AuthError>;
}

#[cfg(test)]
mod tests {
    use super::AuthStrategyPort;

    fn _assert_port<T: AuthStrategyPort>() {}
}
