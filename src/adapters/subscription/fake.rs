//! Test doubles for the subscription adapters.
//!
//! The whole module is gated behind `#[cfg(any(test, feature = "test-fixtures"))]`
//! in [`super`], the same way [`crate::adapters::ssh::fake`] is.

use std::sync::Arc;

use crate::domain::error::DomainError;
use crate::domain::inline_payload::InlinePayload;
use crate::ports::notifier::{NotifierPort, PeerHandle};

/// Notifier that accepts and discards every fan-out. Shared by tests that
/// need a [`NotifierPort`] but do not assert on delivery.
#[derive(Debug, Default)]
pub struct StubNotifier;

impl NotifierPort for StubNotifier {
    async fn notify_resource_updated(
        &self,
        _peer: Arc<dyn PeerHandle>,
        _uri: &str,
    ) -> Result<(), DomainError> {
        Ok(())
    }

    async fn notify_ssh_output(
        &self,
        _peer: Arc<dyn PeerHandle>,
        _payload: InlinePayload,
    ) -> Result<(), DomainError> {
        Ok(())
    }
}
