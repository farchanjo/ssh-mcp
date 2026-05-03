//! Transfer repository port.

use crate::domain::error::DomainError;
use crate::domain::ids::{SessionId, TransferId};
use crate::domain::transfer::TransferEntity;

/// Transfer repository port.
#[trait_variant::make(TransferRepository: Send)]
pub trait LocalTransferRepository: Send + Sync {
    /// Insert a transfer entity.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn insert(&self, entity: TransferEntity) -> Result<(), DomainError>;

    /// Update a stored transfer.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::TransferNotFound` if the id is unknown, or
    /// `DomainError::Storage` on backend failure.
    async fn update(&self, entity: TransferEntity) -> Result<(), DomainError>;

    /// Look up a transfer by id.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn get(&self, id: &TransferId) -> Result<Option<TransferEntity>, DomainError>;

    /// Remove a transfer by id.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn remove(&self, id: &TransferId) -> Result<Option<TransferEntity>, DomainError>;

    /// Count transfers owned by `session_id`.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn count_by_session(&self, session_id: &SessionId) -> Result<usize, DomainError>;

    /// List transfers filtered by optional session id.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn list_filtered(
        &self,
        session_id: Option<&SessionId>,
    ) -> Result<Vec<TransferEntity>, DomainError>;
}

#[cfg(test)]
mod tests {
    use super::TransferRepository;

    fn _assert_port<T: TransferRepository>() {}
}
