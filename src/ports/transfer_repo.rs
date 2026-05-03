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

    /// Atomic check-and-insert: insert `entity` only if the count of
    /// transfers owned by `entity.session_id` is strictly below `cap`.
    ///
    /// This closes the TOCTOU race observed in
    /// [`crate::application::upload_file`] /
    /// [`crate::application::download_file`] when many concurrent uploads
    /// land on the same session: a non-atomic
    /// `count_by_session` + `insert` pair could observe N concurrent
    /// readers below the cap and let N inserts past the gate (so the
    /// shard ended up with `N` running transfers when `cap` was the
    /// agreed ceiling).
    ///
    /// Implementations must perform the count probe and the insert under
    /// a single shard guard — no `await` between them — so the cap is
    /// honoured globally rather than per-caller.
    ///
    /// # Errors
    ///
    /// - [`DomainError::MaxTransfersExceeded`] when the session bucket
    ///   already holds `cap` entries.
    /// - [`DomainError::Internal`] when the id collides with an existing
    ///   row (matches the [`Self::insert`] contract).
    /// - [`DomainError::Storage`] on backend failure.
    async fn insert_if_under_cap(
        &self,
        entity: TransferEntity,
        cap: usize,
    ) -> Result<(), DomainError>;

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
