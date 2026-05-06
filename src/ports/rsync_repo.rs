//! Rsync session repository port (ADR 0011 phase 5).
//!
//! Mirrors [`crate::ports::transfer_repo::TransferRepository`] verbatim
//! — primary key is the [`RsyncId`], secondary index is the owning
//! [`SessionId`]. The repo is the persistent face of the live
//! [`crate::domain::rsync::RsyncSession`] aggregate; the
//! `RsyncSyncUseCase` (phase 5) reads / writes through this port to
//! keep `ssh_rsync_stats` and `ssh_rsync_cancel` honest against the
//! same `DashMap` as the producer.
//!
//! Because the aggregate carries `Atomic*` fields, repository values
//! are stored behind [`Arc`] — clones share the same atomic counters
//! so the producer's `record_file_done` is observed by the read-side
//! snapshot.

use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::ids::SessionId;
use crate::domain::rsync::RsyncSession;
use crate::domain::rsync_ids::RsyncId;

/// Rsync session repository port. AFIT first; the `Send`-bounded alias
/// `RsyncRepository` is the surface use cases consume.
#[trait_variant::make(RsyncRepository: Send)]
pub trait LocalRsyncRepository: Sync {
    /// Insert a fresh session aggregate.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Internal`] if the id collides with an
    /// existing entry (caller must `remove` first), or
    /// [`DomainError::Storage`] on backend failure.
    async fn insert(&self, entity: Arc<RsyncSession>) -> Result<(), DomainError>;

    /// Atomic check-and-insert: insert `entity` only if the count of
    /// rsync sessions owned by `entity.session_id()` is strictly
    /// below `cap`. Closes the same TOCTOU race
    /// [`crate::ports::transfer_repo::TransferRepository::insert_if_under_cap`]
    /// closes for transfers.
    ///
    /// # Errors
    ///
    /// - [`DomainError::MaxTransfersExceeded`] (re-used variant — see
    ///   ADR 0011 § "Configuration surface" `max_rsync_sessions_per_session`)
    ///   when the session bucket is full.
    /// - [`DomainError::Internal`] when the id collides.
    async fn insert_if_under_cap(
        &self,
        entity: Arc<RsyncSession>,
        cap: usize,
    ) -> Result<(), DomainError>;

    /// Look up a session by id.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Storage`] on backend failure.
    async fn get(&self, id: &RsyncId) -> Result<Option<Arc<RsyncSession>>, DomainError>;

    /// Remove a session by id.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Storage`] on backend failure.
    async fn remove(&self, id: &RsyncId) -> Result<Option<Arc<RsyncSession>>, DomainError>;

    /// Count rsync sessions owned by `session_id`.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Storage`] on backend failure.
    async fn count_by_session(&self, session_id: &SessionId) -> Result<usize, DomainError>;

    /// List rsync sessions filtered by optional session id.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Storage`] on backend failure.
    async fn list_filtered(
        &self,
        session_id: Option<&SessionId>,
    ) -> Result<Vec<Arc<RsyncSession>>, DomainError>;
}

#[cfg(test)]
mod tests {
    use super::RsyncRepository;

    fn _assert_port<T: RsyncRepository>() {}
}
