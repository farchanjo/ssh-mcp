//! Production [`crate::infra::mcp::tool_router::IdLister`] implementation.
//!
//! Bridges the v4 repositories to the closest-match suggestion path the
//! tool router walks when a `NOT_FOUND` error fires. Lives in the
//! composition layer because the lister couples the inbound trait to
//! the concrete repository instances — a coupling that belongs at the
//! wiring seam, not inside any single adapter.
//!
//! All accessors are best-effort. Repository failures surface as empty
//! lists (and a `debug!` trace) so the suggestion plumbing never blows
//! up the user-visible response.

use std::sync::Arc;

use tracing::debug;

use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
#[cfg(feature = "port_forward")]
use crate::adapters::repo::dashmap::forward::DashMapForwardRepo;
use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
use crate::infra::mcp::tool_router::{IdFuture, IdLister};
use crate::ports::command_repo::CommandRepository;
#[cfg(feature = "port_forward")]
use crate::ports::forward_repo::ForwardRepository;
use crate::ports::session_repo::SessionRepository;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::transfer_repo::TransferRepository;

/// Production [`IdLister`] backed by the production repositories.
#[derive(Debug)]
pub struct ProdIdLister {
    sessions: Arc<DashMapSessionRepo>,
    commands: Arc<DashMapCommandRepo>,
    shells: Arc<DashMapShellRepo>,
    transfers: Arc<DashMapTransferRepo>,
    #[cfg(feature = "port_forward")]
    forwards: Arc<DashMapForwardRepo>,
}

impl ProdIdLister {
    /// Wire the lister with shared handles to every repository.
    #[must_use]
    pub const fn new(
        sessions: Arc<DashMapSessionRepo>,
        commands: Arc<DashMapCommandRepo>,
        shells: Arc<DashMapShellRepo>,
        transfers: Arc<DashMapTransferRepo>,
        #[cfg(feature = "port_forward")] forwards: Arc<DashMapForwardRepo>,
    ) -> Self {
        Self {
            sessions,
            commands,
            shells,
            transfers,
            #[cfg(feature = "port_forward")]
            forwards,
        }
    }
}

impl IdLister for ProdIdLister {
    fn list_sessions(&self) -> IdFuture<'_> {
        Box::pin(async move {
            match self.sessions.list().await {
                Ok(rows) => rows
                    .into_iter()
                    .map(|s| s.id.as_str().to_string())
                    .collect(),
                Err(err) => {
                    debug!(?err, "ProdIdLister::list_sessions failed");
                    Vec::new()
                }
            }
        })
    }

    fn list_shells(&self) -> IdFuture<'_> {
        Box::pin(async move {
            match self.shells.list_filtered(None).await {
                Ok(rows) => rows
                    .into_iter()
                    .map(|s| s.id.as_str().to_string())
                    .collect(),
                Err(err) => {
                    debug!(?err, "ProdIdLister::list_shells failed");
                    Vec::new()
                }
            }
        })
    }

    fn list_commands(&self) -> IdFuture<'_> {
        Box::pin(async move {
            match self.commands.list_filtered(None, None).await {
                Ok(rows) => rows
                    .into_iter()
                    .map(|c| c.id.as_str().to_string())
                    .collect(),
                Err(err) => {
                    debug!(?err, "ProdIdLister::list_commands failed");
                    Vec::new()
                }
            }
        })
    }

    fn list_transfers(&self) -> IdFuture<'_> {
        Box::pin(async move {
            match self.transfers.list_filtered(None).await {
                Ok(rows) => rows
                    .into_iter()
                    .map(|t| t.id.as_str().to_string())
                    .collect(),
                Err(err) => {
                    debug!(?err, "ProdIdLister::list_transfers failed");
                    Vec::new()
                }
            }
        })
    }

    #[cfg(feature = "port_forward")]
    fn list_forwards(&self) -> IdFuture<'_> {
        Box::pin(async move {
            match self.forwards.list_filtered(None).await {
                Ok(rows) => rows
                    .into_iter()
                    .map(|f| f.id.as_str().to_string())
                    .collect(),
                Err(err) => {
                    debug!(?err, "ProdIdLister::list_forwards failed");
                    Vec::new()
                }
            }
        })
    }
}
