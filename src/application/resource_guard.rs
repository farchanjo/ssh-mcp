//! Existence guard shared by the two subscribe surfaces.
//!
//! The policy is application policy over repository ports: the same four
//! probes `resources/subscribe` already ran, behind a cold-path trait object
//! so `SubscribeUseCase` keeps its shape. No adapter implements the trait, so
//! it lives here rather than in `ports`.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::application::subscribe_resource::ensure_exists;
use crate::domain::error::DomainError;
use crate::ports::command_repo::CommandRepository;
use crate::ports::session_repo::SessionRepository;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::subscriber_registry::ResourceKind;
use crate::ports::transfer_repo::TransferRepository;

/// Boxed future so the guard stays dyn-safe.
pub type GuardFuture<'a> = Pin<Box<dyn Future<Output = Result<(), DomainError>> + Send + 'a>>;

/// Refuse a repository-backed resource that does not exist.
pub trait ResourceGuard: fmt::Debug + Send + Sync + 'static {
    /// Check `kind`/`id`.
    ///
    /// # Errors
    ///
    /// The per-kind not-found variant, or a storage error from the repository.
    fn ensure<'a>(&'a self, kind: ResourceKind, id: &'a str) -> GuardFuture<'a>;
}

/// Guard over the four repositories.
#[derive(Debug)]
pub struct RepositoryGuard<ShR, CR, TR, SR> {
    shells: Arc<ShR>,
    commands: Arc<CR>,
    transfers: Arc<TR>,
    sessions: Arc<SR>,
}

impl<ShR, CR, TR, SR> RepositoryGuard<ShR, CR, TR, SR> {
    /// Wire the guard.
    pub const fn new(
        shells: Arc<ShR>,
        commands: Arc<CR>,
        transfers: Arc<TR>,
        sessions: Arc<SR>,
    ) -> Self {
        Self {
            shells,
            commands,
            transfers,
            sessions,
        }
    }
}

impl<ShR, CR, TR, SR> ResourceGuard for RepositoryGuard<ShR, CR, TR, SR>
where
    ShR: ShellRepository + fmt::Debug + Send + Sync + 'static,
    CR: CommandRepository + fmt::Debug + Send + Sync + 'static,
    TR: TransferRepository + fmt::Debug + Send + Sync + 'static,
    SR: SessionRepository + fmt::Debug + Send + Sync + 'static,
{
    fn ensure<'a>(&'a self, kind: ResourceKind, id: &'a str) -> GuardFuture<'a> {
        Box::pin(async move {
            ensure_exists(
                kind,
                id,
                &*self.shells,
                &*self.commands,
                &*self.transfers,
                &*self.sessions,
            )
            .await
        })
    }
}
