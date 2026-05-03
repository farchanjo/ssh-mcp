//! Shell repository port.

use crate::domain::error::DomainError;
use crate::domain::ids::{SessionId, ShellId};
use crate::domain::shell::ShellEntity;

/// Shell repository port.
#[trait_variant::make(ShellRepository: Send)]
pub trait LocalShellRepository: Send + Sync {
    /// Insert a shell entity.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn insert(&self, entity: ShellEntity) -> Result<(), DomainError>;

    /// Update a stored shell (status flip, etc.).
    ///
    /// # Errors
    ///
    /// Returns `DomainError::ShellNotFound` if the id is unknown, or
    /// `DomainError::Storage` on backend failure.
    async fn update(&self, entity: ShellEntity) -> Result<(), DomainError>;

    /// Look up a shell by id.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn get(&self, id: &ShellId) -> Result<Option<ShellEntity>, DomainError>;

    /// Remove a shell by id.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn remove(&self, id: &ShellId) -> Result<Option<ShellEntity>, DomainError>;

    /// Count shells owned by `session_id`.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn count_by_session(&self, session_id: &SessionId) -> Result<usize, DomainError>;

    /// List shells filtered by optional session id.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn list_filtered(
        &self,
        session_id: Option<&SessionId>,
    ) -> Result<Vec<ShellEntity>, DomainError>;
}

#[cfg(test)]
mod tests {
    use super::ShellRepository;

    fn _assert_port<T: ShellRepository>() {}
}
