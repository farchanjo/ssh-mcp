//! Session repository port.

use chrono::{DateTime, Utc};

use crate::domain::error::DomainError;
use crate::domain::ids::{AgentId, SessionId};
use crate::domain::session::SessionEntity;

/// Session repository port. Implementations may be sync internally
/// (`DashMap`) but the trait stays async so future remote-backed adapters
/// (Postgres, Redis) fit without breaking the API.
#[trait_variant::make(SessionRepository: Send)]
pub trait LocalSessionRepository: Sync {
    /// Insert a session entity. Replacing an existing id is treated as an
    /// internal error so callers must call `remove` first if they intend to
    /// rebind the id.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn insert(&self, entity: SessionEntity) -> Result<(), DomainError>;

    /// Look up a session by id.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure. Missing entries
    /// resolve to `Ok(None)` rather than an error.
    async fn get(&self, id: &SessionId) -> Result<Option<SessionEntity>, DomainError>;

    /// Remove a session by id, returning the removed entity if any.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn remove(&self, id: &SessionId) -> Result<Option<SessionEntity>, DomainError>;

    /// List every stored session.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn list(&self) -> Result<Vec<SessionEntity>, DomainError>;

    /// Update the health probe outcome for a session.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::SessionNotFound` if the id is unknown, or
    /// `DomainError::Storage` on backend failure.
    async fn update_health(
        &self,
        id: &SessionId,
        at: DateTime<Utc>,
        healthy: bool,
    ) -> Result<(), DomainError>;

    /// Bind a session to an agent.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn register_agent(
        &self,
        agent_id: &AgentId,
        session_id: &SessionId,
    ) -> Result<(), DomainError>;

    /// Unbind a session from an agent.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn unregister_agent(
        &self,
        agent_id: &AgentId,
        session_id: &SessionId,
    ) -> Result<(), DomainError>;

    /// Return all session ids bound to `agent_id`.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn list_by_agent(&self, agent_id: &AgentId) -> Result<Vec<SessionId>, DomainError>;

    /// Remove every session bound to `agent_id` and return the removed ids.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn remove_by_agent(&self, agent_id: &AgentId) -> Result<Vec<SessionId>, DomainError>;
}

#[cfg(test)]
mod tests {
    use super::SessionRepository;

    fn _assert_port<T: SessionRepository>() {}
}
