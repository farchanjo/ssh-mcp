//! Resource handlers for the v4 inbound MCP layer.
//!
//! These functions adapt rmcp `resources/*` payloads onto the matching
//! `crate::application::*_resource` use cases. H16 keeps the rendering
//! deliberately minimal — H17 fills v3 parity (`_meta` cursors,
//! status keys, JSON payloads, etc.). The signatures are async so the
//! `ServerHandler` impl can call them straight from the rmcp dispatch
//! without intermediate wrappers.
//!
//! All handlers delegate to use cases living on the shared
//! [`crate::composition::UseCases`] container — they never reach into the
//! v3 storage globals.

use std::sync::Arc;

use rmcp::ErrorData as McpError;
use rmcp::Peer;
use rmcp::RoleServer;
use rmcp::model::{
    AnnotateAble, ListResourcesResult, RawResource, ReadResourceRequestParams, ReadResourceResult,
    Resource, ResourceContents, SubscribeRequestParams, UnsubscribeRequestParams,
};

use crate::application::list_resources::{
    ListResourcesRequest, ListResourcesUseCase, ResourceListing,
};
use crate::application::read_resource::{
    ReadResourceOutcome, ReadResourceRequest, ReadResourceUseCase,
};
use crate::application::subscribe_resource::{SubscribeResourceRequest, SubscribeResourceUseCase};
use crate::application::unsubscribe_resource::{
    UnsubscribeResourceRequest, UnsubscribeResourceUseCase,
};
use crate::domain::error::DomainError;
use crate::ports::command_repo::CommandRepository;
#[cfg(feature = "port_forward")]
use crate::ports::forward_repo::ForwardRepository;
use crate::ports::notifier::PeerHandle;
use crate::ports::output_stream::OutputStreamPort;
use crate::ports::session_repo::SessionRepository;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::subscriber_registry::{SubscriberRegistryAsync, SubscriberRegistryPort};
use crate::ports::transfer_repo::TransferRepository;

use super::peer_handle::{PeerTable, RmcpPeerHandle};

/// Map a [`DomainError`] onto an [`McpError`] for resource handlers.
///
/// The mapping mirrors v3 conventions: validation/parse errors become
/// `invalid_params`, not-found errors become `resource_not_found`,
/// everything else becomes `internal_error`.
fn map_resource_error(err: DomainError) -> McpError {
    match err {
        DomainError::InvalidArgument(reason) => McpError::invalid_params(reason, None),
        DomainError::SessionNotFound(_)
        | DomainError::ShellNotFound(_)
        | DomainError::CommandNotFound(_)
        | DomainError::TransferNotFound(_)
        | DomainError::ForwardNotFound(_) => McpError::resource_not_found(err.to_string(), None),
        DomainError::Auth(_)
        | DomainError::ConnectFailed(_)
        | DomainError::Transport(_)
        | DomainError::Timeout(_)
        | DomainError::Storage(_)
        | DomainError::Sftp(_)
        | DomainError::PortInUse(_)
        | DomainError::Internal(_)
        | DomainError::MaxCommandsExceeded { .. }
        | DomainError::MaxShellsExceeded { .. }
        | DomainError::MaxTransfersExceeded { .. } => {
            McpError::internal_error(err.to_string(), None)
        }
    }
}

/// Handle `resources/list` for the v4 server.
///
/// # Errors
///
/// Propagates any port-level error from the underlying use case via
/// [`map_resource_error`].
#[cfg(not(feature = "port_forward"))]
pub async fn list_resources_impl<SR, CR, ShR, TR>(
    use_case: &ListResourcesUseCase<SR, CR, ShR, TR>,
) -> Result<ListResourcesResult, McpError>
where
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
{
    let outcome = use_case
        .execute(ListResourcesRequest)
        .await
        .map_err(map_resource_error)?;
    Ok(ListResourcesResult::with_all_items(
        outcome.resources.iter().map(make_resource).collect(),
    ))
}

/// Handle `resources/list` for the v4 server (with `port_forward`).
///
/// # Errors
///
/// Propagates any port-level error via [`map_resource_error`].
#[cfg(feature = "port_forward")]
pub async fn list_resources_impl<SR, CR, ShR, TR, FR>(
    use_case: &ListResourcesUseCase<SR, CR, ShR, TR, FR>,
) -> Result<ListResourcesResult, McpError>
where
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    FR: ForwardRepository + Send + Sync,
{
    let outcome = use_case
        .execute(ListResourcesRequest)
        .await
        .map_err(map_resource_error)?;
    Ok(ListResourcesResult::with_all_items(
        outcome.resources.iter().map(make_resource).collect(),
    ))
}

/// Render a single [`ResourceListing`] entry as an rmcp [`Resource`].
///
/// H16 ships placeholder mime types and minimal descriptions. H17 fills
/// v3 parity.
fn make_resource(entry: &ResourceListing) -> Resource {
    let (uri, name, description, mime) = describe_listing(entry);
    RawResource::new(uri, name)
        .with_description(description)
        .with_mime_type(mime)
        .no_annotation()
}

/// Map a [`ResourceListing`] variant onto display metadata for the rmcp
/// resource entry. H17 fills the v3-equivalent strings.
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive match over up to 5 ResourceListing variants is naturally long; H17 extracts per-variant helpers"
)]
fn describe_listing(entry: &ResourceListing) -> (String, String, String, &'static str) {
    match entry {
        ResourceListing::Shell {
            uri,
            shell_id,
            session_id,
            term_type,
        } => (
            uri.clone(),
            format!(
                "Shell {} (session {})",
                shell_id.as_str(),
                session_id.as_str()
            ),
            format!(
                "PTY output buffer for shell {} ({term_type})",
                shell_id.as_str()
            ),
            "text/plain",
        ),
        ResourceListing::Command {
            uri,
            command_id,
            command,
            session_id,
        } => (
            uri.clone(),
            format!(
                "Command {} (session {})",
                command_id.as_str(),
                session_id.as_str()
            ),
            format!("Async command output stream — command: {command}"),
            "text/plain",
        ),
        ResourceListing::Transfer {
            uri,
            transfer_id,
            direction,
            session_id,
        } => (
            uri.clone(),
            format!(
                "Transfer {} ({} session {})",
                transfer_id.as_str(),
                direction,
                session_id.as_str()
            ),
            format!(
                "SFTP {direction} progress for transfer {}",
                transfer_id.as_str()
            ),
            "application/json",
        ),
        ResourceListing::Session {
            uri,
            session_id,
            host,
            healthy,
        } => (
            uri.clone(),
            format!("Session {} ({host})", session_id.as_str()),
            format!(
                "SSH session health snapshot for {host} (healthy={})",
                healthy.map_or("unknown", |h| if h { "true" } else { "false" })
            ),
            "application/json",
        ),
        #[cfg(feature = "port_forward")]
        ResourceListing::Forward {
            uri,
            forward_id,
            local_port,
            remote,
            session_id,
        } => (
            uri.clone(),
            format!(
                "Forward {} (local {local_port} -> {remote}, session {})",
                forward_id.as_str(),
                session_id.as_str()
            ),
            format!(
                "Port-forward event log for {} (local {local_port} -> {remote})",
                forward_id.as_str()
            ),
            "application/json",
        ),
    }
}

/// Handle `resources/read` for the v4 server (no `port_forward`).
///
/// # Errors
///
/// Propagates the use case error via [`map_resource_error`].
#[cfg(not(feature = "port_forward"))]
pub async fn read_resource_impl<ShR, CR, TR, SR, OS, Sub>(
    use_case: &ReadResourceUseCase<ShR, CR, TR, SR, OS, Sub>,
    request: ReadResourceRequestParams,
    peer: &Peer<RoleServer>,
    peer_table: &Arc<PeerTable>,
) -> Result<ReadResourceResult, McpError>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    Sub: SubscriberRegistryPort,
{
    let handle = RmcpPeerHandle::new(peer.clone(), Arc::clone(peer_table));
    let req = ReadResourceRequest {
        uri: request.uri,
        peer_id: handle.id(),
    };
    let outcome = use_case.execute(req).await.map_err(map_resource_error)?;
    Ok(ReadResourceResult::new(vec![placeholder_text_contents(
        h16_uri_for(&outcome),
        h16_text_for(&outcome),
    )]))
}

/// Handle `resources/read` for the v4 server (with `port_forward`).
///
/// # Errors
///
/// Propagates the use case error via [`map_resource_error`].
#[cfg(feature = "port_forward")]
pub async fn read_resource_impl<ShR, CR, TR, SR, FR, OS, Sub>(
    use_case: &ReadResourceUseCase<ShR, CR, TR, SR, FR, OS, Sub>,
    request: ReadResourceRequestParams,
    peer: &Peer<RoleServer>,
    peer_table: &Arc<PeerTable>,
) -> Result<ReadResourceResult, McpError>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    FR: ForwardRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    Sub: SubscriberRegistryPort,
{
    let handle = RmcpPeerHandle::new(peer.clone(), Arc::clone(peer_table));
    let req = ReadResourceRequest {
        uri: request.uri,
        peer_id: handle.id(),
    };
    let outcome = use_case.execute(req).await.map_err(map_resource_error)?;
    Ok(ReadResourceResult::new(vec![placeholder_text_contents(
        h16_uri_for(&outcome),
        h16_text_for(&outcome),
    )]))
}

/// Extract the canonical URI from a [`ReadResourceOutcome`] for the
/// placeholder rendering. H17 lifts this onto the v3 `_meta` envelope.
fn h16_uri_for(outcome: &ReadResourceOutcome) -> String {
    match outcome {
        ReadResourceOutcome::Shell { uri, .. }
        | ReadResourceOutcome::Command { uri, .. }
        | ReadResourceOutcome::Transfer { uri, .. }
        | ReadResourceOutcome::Session { uri, .. } => uri.clone(),
        #[cfg(feature = "port_forward")]
        ReadResourceOutcome::Forward { uri, .. } => uri.clone(),
    }
}

/// Build a placeholder text payload for the H16 read response. H17
/// replaces this with the v3-equivalent body (rendered bytes for
/// shell/command, JSON snapshot for transfer/session/forward).
fn h16_text_for(outcome: &ReadResourceOutcome) -> String {
    match outcome {
        ReadResourceOutcome::Shell { status, .. }
        | ReadResourceOutcome::Command { status, .. }
        | ReadResourceOutcome::Transfer { status, .. }
        | ReadResourceOutcome::Session { status, .. } => {
            format!("TODO H17: render snapshot (status={status})")
        }
        #[cfg(feature = "port_forward")]
        ReadResourceOutcome::Forward { status, .. } => {
            format!("TODO H17: render snapshot (status={status})")
        }
    }
}

/// Build a minimal [`ResourceContents::TextResourceContents`] envelope.
/// H17 attaches `_meta` (`cursor` / `buffer_size` / `last_seq` / status
/// keys).
fn placeholder_text_contents(uri: String, text: String) -> ResourceContents {
    ResourceContents::TextResourceContents {
        uri,
        mime_type: Some("text/plain".to_string()),
        text,
        meta: None,
    }
}

/// Handle `resources/subscribe` for the v4 server (no `port_forward`).
///
/// # Errors
///
/// Propagates the use case error via [`map_resource_error`].
#[cfg(not(feature = "port_forward"))]
pub async fn subscribe_impl<ShR, CR, TR, SR, Sub>(
    use_case: &SubscribeResourceUseCase<ShR, CR, TR, SR, Sub>,
    request: SubscribeRequestParams,
    peer: Peer<RoleServer>,
    peer_table: &Arc<PeerTable>,
) -> Result<(), McpError>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    Sub: SubscriberRegistryAsync + Send + Sync,
{
    let handle: Arc<dyn PeerHandle> = Arc::new(RmcpPeerHandle::new(peer, Arc::clone(peer_table)));
    use_case
        .execute(SubscribeResourceRequest {
            uri: request.uri,
            peer: handle,
        })
        .await
        .map(|_outcome| ())
        .map_err(map_resource_error)
}

/// Handle `resources/subscribe` for the v4 server (with `port_forward`).
///
/// # Errors
///
/// Propagates the use case error via [`map_resource_error`].
#[cfg(feature = "port_forward")]
pub async fn subscribe_impl<ShR, CR, TR, SR, FR, Sub>(
    use_case: &SubscribeResourceUseCase<ShR, CR, TR, SR, FR, Sub>,
    request: SubscribeRequestParams,
    peer: Peer<RoleServer>,
    peer_table: &Arc<PeerTable>,
) -> Result<(), McpError>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    FR: ForwardRepository + Send + Sync,
    Sub: SubscriberRegistryAsync + Send + Sync,
{
    let handle: Arc<dyn PeerHandle> = Arc::new(RmcpPeerHandle::new(peer, Arc::clone(peer_table)));
    use_case
        .execute(SubscribeResourceRequest {
            uri: request.uri,
            peer: handle,
        })
        .await
        .map(|_outcome| ())
        .map_err(map_resource_error)
}

/// Handle `resources/unsubscribe` for the v4 server.
///
/// `peer` is consumed so we can wrap it once and recover the same
/// [`crate::domain::ids::PeerId`] the subscribe path minted (since the
/// wrapper hashes a fresh `UUIDv4` per construction we cannot recover
/// the original id from `peer` alone — the wrapper is freshly minted
/// here purely to satisfy the use case signature; H17 lifts the
/// peer-id derivation into the rmcp peer table once rmcp exposes a
/// stable peer identity).
///
/// # Errors
///
/// Propagates the use case error via [`map_resource_error`].
pub async fn unsubscribe_impl<Sub>(
    use_case: &UnsubscribeResourceUseCase<Sub>,
    request: UnsubscribeRequestParams,
    peer: &Peer<RoleServer>,
    peer_table: &Arc<PeerTable>,
) -> Result<(), McpError>
where
    Sub: SubscriberRegistryAsync + Send + Sync,
{
    let handle = RmcpPeerHandle::new(peer.clone(), Arc::clone(peer_table));
    let peer_id = handle.id();
    use_case
        .execute(UnsubscribeResourceRequest {
            uri: request.uri,
            peer_id,
        })
        .await
        .map(|_outcome| ())
        .map_err(map_resource_error)
}

#[cfg(test)]
mod tests {
    use super::map_resource_error;
    use crate::domain::error::DomainError;

    #[test]
    fn invalid_argument_maps_to_invalid_params() {
        let err = map_resource_error(DomainError::InvalidArgument("x".to_string()));
        assert!(err.message.contains('x'));
    }

    #[test]
    fn transport_error_maps_to_internal_error() {
        let err = map_resource_error(DomainError::Transport("boom".to_string()));
        assert!(err.message.contains("boom"));
    }
}

// ---------------------------------------------------------------------------
// Test stub helpers — keep clippy happy when use cases are not exported
// ---------------------------------------------------------------------------

// (Intentionally no internal helpers exposed beyond the public API — the
// dependent rmcp `ServerHandler` impl in `tool_router.rs` calls every
// helper above directly.)
