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
use rmcp::RoleServer;
use rmcp::model::{
    AnnotateAble, ListResourcesResult, Meta, RawResource, ReadResourceRequestParams,
    ReadResourceResult, Resource, ResourceContents, SubscribeRequestParams,
    UnsubscribeRequestParams,
};
use rmcp::service::RequestContext;
use serde_json::{Map as JsonMap, Value as JsonValue};

use super::helpers::nonce::generate_nonce;
use super::helpers::output::render_output_block;

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
        // `ResourceGone` is semantically a not-found that documents the
        // closed-then-attached race so the caller can stop polling. Folded
        // into the same arm as the `*NotFound` variants to keep the wire
        // mapping consistent and silence `clippy::match_same_arms`.
        DomainError::SessionNotFound(_)
        | DomainError::ShellNotFound(_)
        | DomainError::CommandNotFound(_)
        | DomainError::TransferNotFound(_)
        | DomainError::ForwardNotFound(_)
        | DomainError::ResourceGone(_) => McpError::resource_not_found(err.to_string(), None),
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
        | DomainError::MaxTransfersExceeded { .. }
        | DomainError::LifecycleStateConflict { .. }
        | DomainError::SessionRefcountUnderflow(_) => {
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
    ctx: &RequestContext<RoleServer>,
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
    let handle = RmcpPeerHandle::resolve(ctx, peer_table);
    let req = ReadResourceRequest {
        uri: request.uri,
        peer_id: handle.id(),
    };
    let outcome = use_case.execute(req).await.map_err(map_resource_error)?;
    Ok(ReadResourceResult::new(vec![render_outcome(outcome)]))
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
    ctx: &RequestContext<RoleServer>,
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
    let handle = RmcpPeerHandle::resolve(ctx, peer_table);
    let req = ReadResourceRequest {
        uri: request.uri,
        peer_id: handle.id(),
    };
    let outcome = use_case.execute(req).await.map_err(map_resource_error)?;
    Ok(ReadResourceResult::new(vec![render_outcome(outcome)]))
}

/// Defensive ceiling on rendered command-output payloads. The registry
/// already truncates upstream; this matches the v3 wire-boundary cap.
const COMMAND_BLOCK_CAP_BYTES: usize = 64 * 1024;

/// Render a [`ReadResourceOutcome`] as a single
/// [`ResourceContents::TextResourceContents`] entry, attaching the v3
/// `_meta` envelope (`cursor` / `buffer_size` / `last_seq` / `status` /
/// `kind`).
///
/// Per-variant rendering lives in
/// [`render_shell_outcome`] / [`render_command_outcome`] /
/// [`render_snapshot_outcome`]; this dispatcher only fans out.
#[allow(
    clippy::too_many_lines,
    reason = "destructuring four to five outcome variants requires explicit field bindings; further extraction hurts readability"
)]
fn render_outcome(outcome: ReadResourceOutcome) -> ResourceContents {
    let (uri, text, mime, meta) = match outcome {
        ReadResourceOutcome::Shell {
            uri,
            data,
            cursor,
            buffer_size,
            last_seq,
            status,
        } => render_shell_outcome(uri, &data, cursor, buffer_size, last_seq, &status),
        ReadResourceOutcome::Command {
            uri,
            stdout,
            stderr,
            cursor,
            buffer_size,
            last_seq,
            status,
        } => render_command_outcome(
            uri,
            &stdout,
            &stderr,
            cursor,
            buffer_size,
            last_seq,
            &status,
        ),
        ReadResourceOutcome::Transfer {
            uri,
            json_payload,
            last_seq,
            status,
        } => render_snapshot_outcome("transfer", uri, json_payload, last_seq, &status),
        ReadResourceOutcome::Session {
            uri,
            json_payload,
            last_seq,
            status,
        } => render_snapshot_outcome("session", uri, json_payload, last_seq, &status),
        #[cfg(feature = "port_forward")]
        ReadResourceOutcome::Forward {
            uri,
            json_payload,
            last_seq,
            status,
        } => render_snapshot_outcome("forward", uri, json_payload, last_seq, &status),
    };
    ResourceContents::TextResourceContents {
        uri,
        mime_type: Some(mime.to_string()),
        text,
        meta: Some(meta),
    }
}

/// Per-variant render: shell PTY buffer slice as UTF-8 lossy text plus a
/// `_meta` envelope.
fn render_shell_outcome(
    uri: String,
    data: &[u8],
    cursor: u64,
    buffer_size: u64,
    last_seq: u64,
    status: &str,
) -> (String, String, &'static str, Meta) {
    let body = String::from_utf8_lossy(data).into_owned();
    let meta = build_stream_meta("shell", cursor, buffer_size, last_seq, status);
    (uri, body, "text/plain", meta)
}

/// Per-variant render: command stdout/stderr v3-style block payload.
fn render_command_outcome(
    uri: String,
    stdout: &[u8],
    stderr: &[u8],
    cursor: u64,
    buffer_size: u64,
    last_seq: u64,
    status: &str,
) -> (String, String, &'static str, Meta) {
    let body = render_command_body(stdout, stderr, status);
    let meta = build_stream_meta("command", cursor, buffer_size, last_seq, status);
    (uri, body, "text/plain", meta)
}

/// Per-variant render: point-in-time JSON snapshot
/// (transfer / session / forward).
fn render_snapshot_outcome(
    kind: &'static str,
    uri: String,
    json_payload: String,
    last_seq: u64,
    status: &str,
) -> (String, String, &'static str, Meta) {
    let meta = build_snapshot_meta(kind, last_seq, status);
    (uri, json_payload, "application/json", meta)
}

/// Render the v3-style stdout/stderr block payload for a command read.
///
/// Mirrors v3 `render_output_block` invocations: one nonce per response
/// shared between the two blocks so callers can correlate them.
fn render_command_body(stdout: &[u8], stderr: &[u8], status: &str) -> String {
    let nonce = generate_nonce();
    let stdout_block = render_output_block(
        "stdout",
        &nonce,
        stdout,
        COMMAND_BLOCK_CAP_BYTES,
        Some(status),
    );
    let stderr_block = render_output_block(
        "stderr",
        &nonce,
        stderr,
        COMMAND_BLOCK_CAP_BYTES,
        Some(status),
    );
    format!("{stdout_block}\n{stderr_block}")
}

/// Build the `_meta` envelope for byte-stream resources (shell / command).
fn build_stream_meta(
    kind: &str,
    cursor: u64,
    buffer_size: u64,
    last_seq: u64,
    status: &str,
) -> Meta {
    let mut map = JsonMap::new();
    map.insert("kind".to_string(), JsonValue::from(kind));
    map.insert("cursor".to_string(), JsonValue::from(cursor));
    map.insert("buffer_size".to_string(), JsonValue::from(buffer_size));
    map.insert("last_seq".to_string(), JsonValue::from(last_seq));
    map.insert("status".to_string(), JsonValue::from(status));
    Meta(map)
}

/// Build the `_meta` envelope for point-in-time snapshots
/// (transfer / session / forward) — no cursor, no buffer size.
fn build_snapshot_meta(kind: &str, last_seq: u64, status: &str) -> Meta {
    let mut map = JsonMap::new();
    map.insert("kind".to_string(), JsonValue::from(kind));
    map.insert("last_seq".to_string(), JsonValue::from(last_seq));
    map.insert("status".to_string(), JsonValue::from(status));
    Meta(map)
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
    ctx: &RequestContext<RoleServer>,
    peer_table: &Arc<PeerTable>,
) -> Result<(), McpError>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    Sub: SubscriberRegistryAsync + Send + Sync,
{
    let handle: Arc<dyn PeerHandle> = Arc::new(RmcpPeerHandle::resolve(ctx, peer_table));
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
    ctx: &RequestContext<RoleServer>,
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
    let handle: Arc<dyn PeerHandle> = Arc::new(RmcpPeerHandle::resolve(ctx, peer_table));
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
/// Resolves the [`PeerHandle`] through the shared [`PeerTable`] keyed by
/// the transport-level [`super::peer_handle::PeerKey`] derived from
/// `ctx`, so the [`crate::domain::ids::PeerId`] always matches the one
/// `subscribe` minted on the same connection. Without this fix
/// `unsubscribe` was a no-op (every call minted a fresh `UUIDv4`).
///
/// # Errors
///
/// Propagates the use case error via [`map_resource_error`].
pub async fn unsubscribe_impl<Sub>(
    use_case: &UnsubscribeResourceUseCase<Sub>,
    request: UnsubscribeRequestParams,
    ctx: &RequestContext<RoleServer>,
    peer_table: &Arc<PeerTable>,
) -> Result<(), McpError>
where
    Sub: SubscriberRegistryAsync + Send + Sync,
{
    let handle = RmcpPeerHandle::resolve(ctx, peer_table);
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
    use super::{map_resource_error, render_outcome};
    use crate::application::read_resource::ReadResourceOutcome;
    use crate::domain::error::DomainError;
    use bytes::Bytes;
    use rmcp::model::ResourceContents;

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

    #[test]
    fn read_resource_shell_returns_bytes_and_meta() {
        let outcome = ReadResourceOutcome::Shell {
            uri: "shell://sh-1/output".to_string(),
            data: Bytes::from_static(b"hello world"),
            cursor: 11,
            buffer_size: 11,
            last_seq: 3,
            status: "open".to_string(),
        };
        let contents = render_outcome(outcome);
        match contents {
            ResourceContents::TextResourceContents {
                uri,
                mime_type,
                text,
                meta,
            } => {
                assert_eq!(uri, "shell://sh-1/output");
                assert_eq!(mime_type.as_deref(), Some("text/plain"));
                assert_eq!(text, "hello world");
                let meta = meta.expect("meta envelope must be set on shell read");
                assert_eq!(meta.0.get("kind").and_then(|v| v.as_str()), Some("shell"));
                assert_eq!(meta.0.get("cursor").and_then(|v| v.as_u64()), Some(11));
                assert_eq!(meta.0.get("buffer_size").and_then(|v| v.as_u64()), Some(11));
                assert_eq!(meta.0.get("last_seq").and_then(|v| v.as_u64()), Some(3));
                assert_eq!(meta.0.get("status").and_then(|v| v.as_str()), Some("open"));
            }
            other => panic!("unexpected contents variant: {other:?}"),
        }
    }

    #[test]
    fn read_resource_command_returns_block_payload_with_meta() {
        let outcome = ReadResourceOutcome::Command {
            uri: "command://c-1/output".to_string(),
            stdout: Bytes::from_static(b"hi"),
            stderr: Bytes::from_static(b"err"),
            cursor: 5,
            buffer_size: 5,
            last_seq: 7,
            status: "running".to_string(),
        };
        let contents = render_outcome(outcome);
        match contents {
            ResourceContents::TextResourceContents {
                mime_type,
                text,
                meta,
                ..
            } => {
                assert_eq!(mime_type.as_deref(), Some("text/plain"));
                assert!(
                    text.contains("--- stdout ["),
                    "missing v3 stdout block: {text}"
                );
                assert!(
                    text.contains("--- stderr ["),
                    "missing v3 stderr block: {text}"
                );
                assert!(text.contains("hi"), "stdout payload missing: {text}");
                assert!(text.contains("err"), "stderr payload missing: {text}");
                let meta = meta.expect("meta envelope must be set on command read");
                assert_eq!(meta.0.get("kind").and_then(|v| v.as_str()), Some("command"));
                assert_eq!(meta.0.get("cursor").and_then(|v| v.as_u64()), Some(5));
                assert_eq!(meta.0.get("last_seq").and_then(|v| v.as_u64()), Some(7));
            }
            other => panic!("unexpected contents variant: {other:?}"),
        }
    }

    #[test]
    fn read_resource_transfer_returns_json_payload_and_meta() {
        let outcome = ReadResourceOutcome::Transfer {
            uri: "transfer://t-1/progress".to_string(),
            json_payload: r#"{"transfer_id":"t-1","status":"running"}"#.to_string(),
            last_seq: 12,
            status: "running".to_string(),
        };
        let contents = render_outcome(outcome);
        match contents {
            ResourceContents::TextResourceContents {
                mime_type,
                text,
                meta,
                ..
            } => {
                assert_eq!(mime_type.as_deref(), Some("application/json"));
                assert!(text.contains("t-1"));
                let meta = meta.expect("meta envelope must be set on transfer read");
                assert_eq!(
                    meta.0.get("kind").and_then(|v| v.as_str()),
                    Some("transfer")
                );
                assert_eq!(meta.0.get("last_seq").and_then(|v| v.as_u64()), Some(12));
                assert_eq!(
                    meta.0.get("status").and_then(|v| v.as_str()),
                    Some("running")
                );
                // No cursor/buffer_size on point-in-time snapshots.
                assert!(meta.0.get("cursor").is_none());
                assert!(meta.0.get("buffer_size").is_none());
            }
            other => panic!("unexpected contents variant: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Test stub helpers — keep clippy happy when use cases are not exported
// ---------------------------------------------------------------------------

// (Intentionally no internal helpers exposed beyond the public API — the
// dependent rmcp `ServerHandler` impl in `tool_router.rs` calls every
// helper above directly.)
