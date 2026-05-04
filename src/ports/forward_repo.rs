//! Forward repository port.
//!
//! Feature-gated to mirror the v3 `port_forward` Cargo feature. The trait
//! itself is small and dyn-safe via `trait_variant`.

#![cfg(feature = "port_forward")]

use crate::domain::error::DomainError;
use crate::domain::forward::ForwardEntity;
use crate::domain::ids::{ForwardId, SessionId};

/// Forward repository port.
#[trait_variant::make(ForwardRepository: Send)]
pub trait LocalForwardRepository: Sync {
    /// Insert a forwarder entity.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn insert(&self, entity: ForwardEntity) -> Result<(), DomainError>;

    /// Update a stored forwarder.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::ForwardNotFound` if the id is unknown, or
    /// `DomainError::Storage` on backend failure.
    async fn update(&self, entity: ForwardEntity) -> Result<(), DomainError>;

    /// Look up a forwarder by id.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn get(&self, id: &ForwardId) -> Result<Option<ForwardEntity>, DomainError>;

    /// Remove a forwarder by id.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn remove(&self, id: &ForwardId) -> Result<Option<ForwardEntity>, DomainError>;

    /// List forwarders owned by an optional session id.
    ///
    /// # Errors
    ///
    /// Returns `DomainError::Storage` on backend failure.
    async fn list_filtered(
        &self,
        session_id: Option<&SessionId>,
    ) -> Result<Vec<ForwardEntity>, DomainError>;
}

#[cfg(test)]
mod tests {
    use super::ForwardRepository;

    fn _assert_port<T: ForwardRepository>() {}
}
