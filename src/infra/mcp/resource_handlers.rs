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

use crate::adapters::lifecycle::leak_watcher::{
    LeakRiskAlert, LeakRiskSeverity, LeakWatcherProbe, alert_canonical_uri,
};
use crate::application::list_resources::{
    ListResourcesRequest, ListResourcesUseCase, ResourceListing,
};
use crate::application::read_resource::{
    ReadResourceOutcome, ReadResourceRequest, ReadResourceUseCase,
};
use crate::application::rsync_sync::{RsyncStatsSnapshot, RsyncSyncUseCase};
use crate::application::subscribe_resource::{
    SubscribeResourceOutcome, SubscribeResourceRequest, SubscribeResourceUseCase,
};
use crate::application::unsubscribe_resource::{
    UnsubscribeResourceRequest, UnsubscribeResourceUseCase,
};
use crate::domain::error::DomainError;
use crate::domain::rsync_ids::RsyncId;
use crate::ports::command_repo::CommandRepository;
use crate::ports::config::ConfigPort;
#[cfg(feature = "port_forward")]
use crate::ports::forward_repo::ForwardRepository;
use crate::ports::id_generator::IdGeneratorPort;
use crate::ports::notifier::PeerHandle;
use crate::ports::output_stream::OutputStreamPort;
use crate::ports::rsync_repo::RsyncRepository;
use crate::ports::rsync_sftp_fs::RsyncSftpFsPort;
use crate::ports::rsync_transport::RsyncTransportPort;
use crate::ports::session_repo::SessionRepository;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::ssh_client::SshClientPort;
use crate::ports::subscriber_registry::{
    ResourceKind, SubscriberRegistryAsync, SubscriberRegistryPort,
};
use crate::ports::transfer_repo::TransferRepository;

use super::peer_handle::{PeerTable, RmcpPeerHandle};

/// Map a [`DomainError`] onto an [`McpError`] for resource handlers.
///
/// The mapping mirrors v3 conventions: validation / parse errors
/// become `invalid_params`, not-found errors become
/// `resource_not_found`, everything else becomes `internal_error`.
fn map_resource_error(err: &DomainError) -> McpError {
    let message = err.to_string();
    match resource_error_category(err) {
        ResourceErrorCategory::InvalidParams => McpError::invalid_params(message, None),
        ResourceErrorCategory::NotFound => McpError::resource_not_found(message, None),
        ResourceErrorCategory::Internal => McpError::internal_error(message, None),
    }
}

/// Classify a [`DomainError`] for resource-handler rendering.
///
/// Split out of [`map_resource_error`] so the dispatcher stays under
/// the 30-line clippy threshold while keeping every variant under
/// compile-time scrutiny.
enum ResourceErrorCategory {
    InvalidParams,
    NotFound,
    Internal,
}

#[allow(
    clippy::too_many_lines,
    reason = "exhaustive DomainError match — pre-existing, unchanged by lane-bridge plumbing"
)]
const fn resource_error_category(err: &DomainError) -> ResourceErrorCategory {
    match err {
        DomainError::InvalidArgument(_)
        | DomainError::InvalidLagPolicy(_)
        | DomainError::InvalidLifetime(_) => ResourceErrorCategory::InvalidParams,
        // `ResourceGone` folds in here so the wire mapping stays consistent
        // with the `*NotFound` variants.
        DomainError::SessionNotFound(_)
        | DomainError::ShellNotFound(_)
        | DomainError::CommandNotFound(_)
        | DomainError::TransferNotFound(_)
        | DomainError::ForwardNotFound(_)
        | DomainError::SerialNotFound(_)
        | DomainError::ResourceGone(_)
        | DomainError::SubNotFound(_)
        // ADR 0011 — rsync transport "not found" maps onto the
        // resource-NOT_FOUND class (the rsync binary is the missing
        // resource; same wire shape as `SESSION_NOT_FOUND`).
        | DomainError::RsyncNotFound(_) => ResourceErrorCategory::NotFound,
        DomainError::Auth(_)
        | DomainError::ConnectFailed(_)
        | DomainError::Transport(_)
        | DomainError::Timeout(_)
        | DomainError::Storage(_)
        | DomainError::Sftp(_)
        | DomainError::Serial(_)
        | DomainError::PortInUse(_)
        | DomainError::Internal(_)
        | DomainError::MaxCommandsExceeded { .. }
        | DomainError::MaxShellsExceeded { .. }
        | DomainError::MaxTransfersExceeded { .. }
        | DomainError::LifecycleStateConflict { .. }
        | DomainError::SessionRefcountUnderflow(_)
        | DomainError::MaxSubsPerUriExceeded { .. }
        | DomainError::MaxSubsTotalExceeded { .. }
        | DomainError::LaneBufferFull { .. }
        | DomainError::LagDetected { .. }
        | DomainError::MuxBackpressure
        | DomainError::RsyncVersionTooOld(_)
        | DomainError::RsyncProtocolError(_)
        | DomainError::RsyncFileListTooLarge { .. }
        | DomainError::RsyncPartialTransfer(_)
        | DomainError::SftpFeatureMissing(_) => ResourceErrorCategory::Internal,
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
    leak_probe: Option<&Arc<dyn LeakWatcherProbe>>,
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
        .map_err(|e| map_resource_error(&e))?;
    Ok(attach_leak_warnings(
        ListResourcesResult::with_all_items(outcome.resources.iter().map(make_resource).collect()),
        leak_probe,
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
    leak_probe: Option<&Arc<dyn LeakWatcherProbe>>,
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
        .map_err(|e| map_resource_error(&e))?;
    Ok(attach_leak_warnings(
        ListResourcesResult::with_all_items(outcome.resources.iter().map(make_resource).collect()),
        leak_probe,
    ))
}

/// Attach a `_meta.warnings` array describing every in-effect
/// `SUB_LEAK_RISK` alert. Pure helper so the cfg-gated list helpers
/// share one implementation.
///
/// The MCP spec leaves `_meta` open-ended; we use `warnings` as a
/// stable namespaced key MCP clients can branch on. Empty alert lists
/// produce no `_meta` entry.
fn attach_leak_warnings(
    mut result: ListResourcesResult,
    leak_probe: Option<&Arc<dyn LeakWatcherProbe>>,
) -> ListResourcesResult {
    let alerts = leak_probe.map_or_else(Vec::new, |p| p.current_alerts());
    if alerts.is_empty() {
        return result;
    }
    let mut meta = result.meta.unwrap_or_default();
    meta.0
        .insert("warnings".to_string(), build_warnings_meta(&alerts));
    result.meta = Some(meta);
    result
}

fn build_warnings_meta(alerts: &[LeakRiskAlert]) -> JsonValue {
    let entries: Vec<JsonValue> = alerts.iter().map(alert_to_json).collect();
    JsonValue::Array(entries)
}

/// Encode a single [`LeakRiskAlert`] as the per-warning JSON object the
/// MCP `_meta.warnings` payload exposes. Pulled out so the parent
/// helper stays under the 30-line cognitive threshold.
fn alert_to_json(alert: &LeakRiskAlert) -> JsonValue {
    let mut obj = JsonMap::new();
    obj.insert(
        "code".to_string(),
        JsonValue::String("SUB_LEAK_RISK".to_string()),
    );
    obj.insert(
        "resource".to_string(),
        JsonValue::String(alert_canonical_uri(alert)),
    );
    obj.insert("age_ms".to_string(), JsonValue::Number(alert.age_ms.into()));
    obj.insert(
        "severity".to_string(),
        JsonValue::String(severity_label(alert.severity).to_string()),
    );
    obj.insert(
        "msg".to_string(),
        JsonValue::String(format!(
            "{} {} stayed Owned past the SUB_LEAK_RISK_WARN threshold",
            kind_label(alert.kind),
            alert.resource_id,
        )),
    );
    JsonValue::Object(obj)
}

const fn severity_label(s: LeakRiskSeverity) -> &'static str {
    match s {
        LeakRiskSeverity::Warn => "warn",
        LeakRiskSeverity::Kill => "kill",
    }
}

const fn kind_label(k: ResourceKind) -> &'static str {
    match k {
        ResourceKind::Shell => "shell",
        ResourceKind::Command => "command",
        ResourceKind::Transfer => "transfer",
        ResourceKind::Session => "session",
        ResourceKind::Forward => "forward",
        ResourceKind::Serial => "serial",
        ResourceKind::Rsync => "rsync",
    }
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
    if let Some(content) = try_serial_read(&request.uri) {
        return Ok(ReadResourceResult::new(vec![content]));
    }
    let handle = RmcpPeerHandle::resolve(ctx, peer_table);
    let req = ReadResourceRequest {
        uri: request.uri,
        peer_id: handle.id(),
    };
    let outcome = use_case
        .execute(req)
        .await
        .map_err(|e| map_resource_error(&e))?;
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
    if let Some(content) = try_serial_read(&request.uri) {
        return Ok(ReadResourceResult::new(vec![content]));
    }
    let handle = RmcpPeerHandle::resolve(ctx, peer_table);
    let req = ReadResourceRequest {
        uri: request.uri,
        peer_id: handle.id(),
    };
    let outcome = use_case
        .execute(req)
        .await
        .map_err(|e| map_resource_error(&e))?;
    Ok(ReadResourceResult::new(vec![render_outcome(outcome)]))
}

/// Defensive ceiling on rendered command-output payloads. The registry
/// already truncates upstream; this matches the v3 wire-boundary cap.
const COMMAND_BLOCK_CAP_BYTES: usize = 64 * 1024;

/// Short-circuit `resources/read` for `serial://<id>/output` URIs.
///
/// v5.2 (ADR 0009) — serial state lives on the static
/// `SERIAL_REGISTRY` and is therefore reachable directly from the
/// MCP infra layer without going through the application read use
/// case. Returns `None` for any non-serial URI so the caller can
/// fall through to the existing flow.
fn try_serial_read(uri: &str) -> Option<ResourceContents> {
    use crate::adapters::serial::state::{SERIAL_REGISTRY, read_history_from_cursor};
    use crate::domain::ids::SerialId;

    let (scheme, rest) = uri.split_once("://")?;
    if scheme != "serial" {
        return None;
    }
    let serial_id = rest.split_once('/').map_or(rest, |(id, _)| id).to_string();
    let id = SerialId::new(serial_id);
    let state = SERIAL_REGISTRY.get(&id)?;
    let (data, cursor) = read_history_from_cursor(&state, 0);
    let text = String::from_utf8_lossy(&data).into_owned();
    let meta = serde_json::json!({
        "cursor":      cursor,
        "buffer_size": data.len(),
        "kind":        "serial",
    });
    Some(ResourceContents::TextResourceContents {
        uri: state.uri(),
        mime_type: Some("text/plain".to_string()),
        text,
        meta: Some(serde_json::from_value(meta).unwrap_or_default()),
    })
}

/// Error mapping helper specific to the rsync read short-circuit.
///
/// Mirrors [`map_resource_error`] but pre-classifies
/// [`DomainError::ResourceGone`] onto `resource_not_found` so
/// the MCP host can branch on the not-found shape.
#[must_use]
pub fn map_rsync_read_error(err: &DomainError) -> McpError {
    map_resource_error(err)
}

/// Short-circuit `resources/read` for `rsync://<id>/progress` URIs.
///
/// ADR 0011 phase 3 — read the latest progress snapshot directly
/// from the [`RsyncSyncUseCase`].
///
/// Reads atomic counters in the
/// [`crate::domain::rsync::RsyncSession`] aggregate. Returns `None`
/// for any non-rsync URI so the caller can fall through to the
/// existing flow. Returns `Some(Err(...))` when the URI is rsync but
/// the id is unknown — the `ServerHandler` maps that onto
/// `RESOURCE_NOT_FOUND`.
///
/// # Errors
///
/// Surfaces [`DomainError::ResourceGone`] for unknown ids and
/// propagates any storage-layer error from the repository.
pub async fn try_rsync_read<W, Sf, Sfs, R, SR, Ssh, Idg, Cfg>(
    use_case: &RsyncSyncUseCase<W, Sf, Sfs, R, SR, Ssh, Idg, Cfg>,
    uri: &str,
) -> Option<Result<ResourceContents, DomainError>>
where
    W: RsyncTransportPort + Send + Sync + 'static,
    Sf: RsyncTransportPort + Send + Sync + 'static,
    Sfs: RsyncSftpFsPort + Send + Sync + 'static,
    R: RsyncRepository + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    Ssh: SshClientPort + Send + Sync + 'static,
    Idg: IdGeneratorPort + Send + Sync + 'static,
    Cfg: ConfigPort + Send + Sync + 'static,
{
    let id = parse_rsync_progress_uri(uri)?;
    Some(render_rsync_progress(use_case, id).await)
}

async fn render_rsync_progress<W, Sf, Sfs, R, SR, Ssh, Idg, Cfg>(
    use_case: &RsyncSyncUseCase<W, Sf, Sfs, R, SR, Ssh, Idg, Cfg>,
    rsync_id: String,
) -> Result<ResourceContents, DomainError>
where
    W: RsyncTransportPort + Send + Sync + 'static,
    Sf: RsyncTransportPort + Send + Sync + 'static,
    Sfs: RsyncSftpFsPort + Send + Sync + 'static,
    R: RsyncRepository + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    Ssh: SshClientPort + Send + Sync + 'static,
    Idg: IdGeneratorPort + Send + Sync + 'static,
    Cfg: ConfigPort + Send + Sync + 'static,
{
    use crate::domain::ids::SessionId;
    let id = RsyncId::new(rsync_id);
    let snapshot = use_case
        .try_stats(&id)
        .await?
        .ok_or_else(|| DomainError::ResourceGone(format!("rsync://{id}/progress")))?;
    let session_id = use_case.owning_session(&id).await?;
    let canonical = format!("rsync://{}/progress", snapshot.rsync_id);
    let body = rsync_progress_body(&snapshot, session_id.as_ref().map(SessionId::as_str));
    let meta = build_snapshot_meta("rsync", 0, rsync_status_label(&snapshot));
    Ok(ResourceContents::TextResourceContents {
        uri: canonical,
        mime_type: Some("application/json".to_string()),
        text: body,
        meta: Some(meta),
    })
}

/// Render the JSON body for a `rsync://<id>/progress` snapshot.
/// Mirrors the `transfer://` shape: a single JSON object with the
/// stable counter fields plus the latest [`crate::domain::rsync::RsyncStatus`]
/// label. Reads bypass the `_seq` cursor used by byte-stream
/// resources — rsync events are point-in-time aggregate counters
/// rebuilt on every read.
fn rsync_progress_body(snapshot: &RsyncStatsSnapshot, session_id: Option<&str>) -> String {
    let stats = &snapshot.stats;
    serde_json::json!({
        "rsync_id": snapshot.rsync_id.as_str(),
        "session_id": session_id,
        "status": rsync_status_label(snapshot),
        "files_total": stats.files_total,
        "files_done": stats.files_done,
        "bytes_total": stats.bytes_total,
        "bytes_transferred": stats.bytes_transferred,
        "bytes_skipped": stats.bytes_skipped,
        "files_deleted": stats.files_deleted,
        "files_failed": stats.files_failed,
    })
    .to_string()
}

const fn rsync_status_label(snapshot: &RsyncStatsSnapshot) -> &'static str {
    use crate::domain::rsync::RsyncStatus;
    match snapshot.status {
        RsyncStatus::Pending => "pending",
        RsyncStatus::Probing => "probing",
        RsyncStatus::Running => "running",
        RsyncStatus::Completed => "completed",
        RsyncStatus::Failed => "failed",
        RsyncStatus::Cancelled => "cancelled",
    }
}

/// Parse an `rsync://<id>/progress` URI and extract the id segment.
/// Returns `None` for non-rsync URIs so the caller can fall through
/// to the regular dispatcher.
fn parse_rsync_progress_uri(uri: &str) -> Option<String> {
    let (scheme, rest) = uri.split_once("://")?;
    if scheme != "rsync" {
        return None;
    }
    let (id, sub_path) = rest.split_once('/')?;
    if id.is_empty() || sub_path != "progress" {
        return None;
    }
    Some(id.to_string())
}

/// Append `rsync://<id>/progress` entries onto the [`ListResourcesResult`].
///
/// Mirrors the v6 `transfer://` injection — runs before the
/// leak-warning attachment so warnings see the full live URI set.
///
/// # Errors
///
/// Propagates any storage-layer error from the repository.
pub async fn attach_rsync_resources<W, Sf, Sfs, R, SR, Ssh, Idg, Cfg>(
    result: &mut ListResourcesResult,
    use_case: &RsyncSyncUseCase<W, Sf, Sfs, R, SR, Ssh, Idg, Cfg>,
) -> Result<(), DomainError>
where
    W: RsyncTransportPort + Send + Sync + 'static,
    Sf: RsyncTransportPort + Send + Sync + 'static,
    Sfs: RsyncSftpFsPort + Send + Sync + 'static,
    R: RsyncRepository + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    Ssh: SshClientPort + Send + Sync + 'static,
    Idg: IdGeneratorPort + Send + Sync + 'static,
    Cfg: ConfigPort + Send + Sync + 'static,
{
    let snapshots = use_case.list_active().await?;
    for snapshot in snapshots {
        let uri = format!("rsync://{}/progress", snapshot.rsync_id);
        let resource = RawResource::new(uri.clone(), format!("Rsync {}", snapshot.rsync_id))
            .with_description(format!(
                "Rsync session {} progress (status={})",
                snapshot.rsync_id,
                rsync_status_label(&snapshot),
            ))
            .with_mime_type("application/json")
            .no_annotation();
        result.resources.push(resource);
    }
    Ok(())
}

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
    let outcome = use_case
        .execute(SubscribeResourceRequest {
            uri: request.uri,
            peer: handle,
        })
        .await
        .map_err(|e| map_resource_error(&e))?;
    log_subscribe_outcome(&outcome);
    Ok(())
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
    let outcome = use_case
        .execute(SubscribeResourceRequest {
            uri: request.uri,
            peer: handle,
        })
        .await
        .map_err(|e| map_resource_error(&e))?;
    log_subscribe_outcome(&outcome);
    Ok(())
}

/// v5 Phase 2: log the synthesised `SubId` so operators can
/// correlate subscriptions across logs. The Phase 3 `sub_open` tool
/// returns the `SubId` in the response body for callers that drive
/// the lane explicitly.
fn log_subscribe_outcome(outcome: &SubscribeResourceOutcome) {
    if let Some(sub_id) = outcome.sub_id.as_ref() {
        tracing::info!(
            uri = %outcome.uri,
            sub_id = %sub_id,
            "resources/subscribe: SubId synthesised"
        );
    }
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
            // v5 Phase 2: the rmcp `resources/unsubscribe` schema
            // does not carry a `sub_id`; the v6.0 `sub_close` tool
            // (formerly Phase 3 `ssh_sub_close`) is the path that
            // passes it explicitly. The use case skips the
            // lane close-by-id path when the protocol cannot supply
            // the sub_id.
            sub_id: None,
        })
        .await
        .map(|_outcome| ())
        .map_err(|e| map_resource_error(&e))
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
        let err = map_resource_error(&DomainError::InvalidArgument("x".to_string()));
        assert!(err.message.contains('x'));
    }

    #[test]
    fn transport_error_maps_to_internal_error() {
        let err = map_resource_error(&DomainError::Transport("boom".to_string()));
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

    // -- v5 Phase 3 — list_resources WARN injection ------------------------

    use super::attach_leak_warnings;
    use crate::adapters::lifecycle::leak_watcher::{
        LeakRiskAlert, LeakRiskSeverity, LeakWatcherProbe,
    };
    use crate::ports::subscriber_registry::ResourceKind;
    use rmcp::model::ListResourcesResult;
    use std::sync::Arc;

    /// Test-only fake probe returning a fixed list.
    #[derive(Debug)]
    struct StubProbe {
        alerts: Vec<LeakRiskAlert>,
    }

    impl LeakWatcherProbe for StubProbe {
        fn current_alerts(&self) -> Vec<LeakRiskAlert> {
            self.alerts.clone()
        }
        fn alert_for(&self, kind: ResourceKind, resource_id: &str) -> Option<LeakRiskAlert> {
            self.alerts
                .iter()
                .find(|a| a.kind == kind && a.resource_id == resource_id)
                .cloned()
        }
        fn alert_for_uri(&self, _uri: &str) -> Option<LeakRiskAlert> {
            None
        }
    }

    fn warn_alert(kind: ResourceKind, id: &str, age_ms: u64) -> LeakRiskAlert {
        LeakRiskAlert {
            kind,
            resource_id: id.to_string(),
            age_ms,
            severity: LeakRiskSeverity::Warn,
        }
    }

    #[test]
    fn attach_leak_warnings_with_no_probe_keeps_meta_unset() {
        let result = ListResourcesResult::with_all_items(vec![]);
        let attached = attach_leak_warnings(result, None);
        assert!(
            attached.meta.is_none(),
            "missing probe must leave _meta untouched"
        );
    }

    #[test]
    fn attach_leak_warnings_with_empty_alerts_keeps_meta_unset() {
        let probe: Arc<dyn LeakWatcherProbe> = Arc::new(StubProbe { alerts: vec![] });
        let result = ListResourcesResult::with_all_items(vec![]);
        let attached = attach_leak_warnings(result, Some(&probe));
        assert!(
            attached.meta.is_none(),
            "empty alerts must leave _meta untouched"
        );
    }

    #[test]
    fn attach_leak_warnings_populates_meta_with_alert_entries() {
        let alerts = vec![
            warn_alert(ResourceKind::Shell, "leaky-shell", 2_500),
            warn_alert(ResourceKind::Command, "leaky-cmd", 4_000),
        ];
        let probe: Arc<dyn LeakWatcherProbe> = Arc::new(StubProbe { alerts });
        let result = ListResourcesResult::with_all_items(vec![]);
        let attached = attach_leak_warnings(result, Some(&probe));
        let meta = attached.meta.expect("meta must be populated");
        let warnings = meta.0.get("warnings").expect("warnings key present");
        let arr = warnings.as_array().expect("warnings is array");
        assert_eq!(arr.len(), 2);
        let first = &arr[0];
        assert_eq!(
            first.get("code").and_then(|v| v.as_str()),
            Some("SUB_LEAK_RISK")
        );
        assert_eq!(
            first.get("resource").and_then(|v| v.as_str()),
            Some("shell://leaky-shell/output")
        );
        assert_eq!(first.get("severity").and_then(|v| v.as_str()), Some("warn"));
    }
}

// ---------------------------------------------------------------------------
// Test stub helpers — keep clippy happy when use cases are not exported
// ---------------------------------------------------------------------------

// (Intentionally no internal helpers exposed beyond the public API — the
// dependent rmcp `ServerHandler` impl in `tool_router.rs` calls every
// helper above directly.)
