//! Command repository port.

use crate::domain::command::CommandEntity;
use crate::domain::error::DomainError;
use crate::domain::ids::{CommandId, SessionId};
use crate::domain::policy::CommandStatusFilter;

/// Command repository port.
#[trait_variant::make(CommandRepository: Send)]
pub trait LocalCommandRepository: Send + Sync {
    /// Insert a command entity.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn insert(&self, entity: CommandEntity) -> Result<(), DomainError>;

    /// Update a stored command (status, exit code, etc.). Implementations
    /// must replace the existing entry atomically.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::CommandNotFound` if the id is unknown, or
    /// `DomainError::Storage` on backend failure.
    async fn update(&self, entity: CommandEntity) -> Result<(), DomainError>;

    /// Look up a command by id.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure. Missing entries
    /// resolve to `Ok(None)`.
    async fn get(&self, id: &CommandId) -> Result<Option<CommandEntity>, DomainError>;

    /// Remove a command by id, returning the removed entity if any.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn remove(&self, id: &CommandId) -> Result<Option<CommandEntity>, DomainError>;

    /// Count commands owned by `session_id`.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn count_by_session(&self, session_id: &SessionId) -> Result<usize, DomainError>;

    /// Count *running* commands owned by `session_id`. Used for
    /// `MaxCommandsExceeded` enforcement.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn count_running_by_session(&self, session_id: &SessionId) -> Result<usize, DomainError>;

    /// List commands filtered by optional session id and/or status.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn list_filtered(
        &self,
        session_id: Option<&SessionId>,
        status: Option<CommandStatusFilter>,
    ) -> Result<Vec<CommandEntity>, DomainError>;
}

#[cfg(test)]
mod tests {
    use super::CommandRepository;

    fn _assert_port<T: CommandRepository>() {}
}
