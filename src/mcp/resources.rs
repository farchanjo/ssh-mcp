//! MCP `resources/*` request handlers for ssh-mcp's five resource schemes.
//!
//! ## URI schemes
//!
//! - `shell://<shell_id>/output[?cursor=auto|<N>|0]`        — PTY output buffer
//! - `command://<command_id>/output[?cursor=auto|<N>|0]`     — async command stdout/stderr
//! - `transfer://<transfer_id>/progress`                      — SFTP progress (point-in-time, no cursor)
//! - `session://<session_id>/health`                          — session health snapshot
//! - `forward://<forward_id>/events[?cursor=auto|<N>|0]`     — port-forward event log
//!
//! ## `?cursor` semantics
//!
//! - `?cursor=auto` (default for resubscribed reads): return only the bytes /
//!   events newer than the previous read for THIS peer. Server tracks the
//!   per-peer offset via [`SUBSCRIPTION_REGISTRY.peer_progress`][SUBSCRIPTION_REGISTRY].
//! - `?cursor=<N>`: return delta starting at absolute byte offset `N`.
//! - `?cursor=0` or no `?cursor`: full snapshot. Useful after a gap is
//!   detected via `_meta.last_seq`.
//!
//! ## `_meta` fields
//!
//! Every `resources/read` response embeds a `_meta` object on the
//! [`rmcp::model::ResourceContents`]. Common keys:
//!
//! - `cursor` — next cursor value the client should pass next time.
//! - `buffer_size` — bytes currently held in the resource history (or `0`
//!   for stateless resources).
//! - `last_seq` — last sequence number allocated for the resource.
//! - `keepalive` — `true` when no fresh bytes / events were available.
//! - `truncated_since_last_read` — bytes the server dropped from the head
//!   between this read and the previous one (only when positive).
//! - `lagged_since_last_read` — chunks missed from the broadcast channel
//!   (reserved for future broadcast-Lagged recovery; currently omitted).
//! - `shell_status` / `command_status` / `transfer_status` /
//!   `session_status` / `forward_status` — kind-specific status string.
//!
//! ## Wiring (E14)
//!
//! This module is purely functional — it does not implement the rmcp
//! [`ServerHandler`] trait. E14 wires
//! [`list_resources_impl`], [`read_resource_impl`], [`subscribe_impl`],
//! and [`unsubscribe_impl`] into the `McpSshServer` so the rmcp transport
//! routes `resources/list`, `resources/read`, `resources/subscribe`, and
//! `resources/unsubscribe` requests through these helpers.

use std::error::Error as StdError;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use rmcp::ErrorData as McpError;
use rmcp::Peer;
use rmcp::RoleServer;
use rmcp::model::{
    AnnotateAble, ListResourcesResult, Meta, RawResource, ReadResourceRequestParams,
    ReadResourceResult, Resource, ResourceContents, SubscribeRequestParams,
    UnsubscribeRequestParams,
};
use serde_json::{Value, json};

use crate::mcp::async_command::OutputBuffer;
use crate::mcp::config::resolve_output_max_bytes_cap;
use crate::mcp::shell::RingBuffer;
use crate::mcp::storage::command::COMMAND_STORAGE;
use crate::mcp::storage::session::SESSION_STORAGE;
use crate::mcp::storage::shell::SHELL_STORAGE;
use crate::mcp::storage::traits::{CommandStorage, SessionStorage, ShellStorage, TransferStorage};
use crate::mcp::storage::transfer::TRANSFER_STORAGE;
use crate::mcp::subscription::{ResourceKind, SUBSCRIPTION_REGISTRY};
use crate::mcp::transfer::TransferStatus;

/// Parsed view of a ssh-mcp resource URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedResourceUri {
    /// Resource scheme (one of the five ssh-mcp kinds).
    pub kind: ResourceKind,
    /// Resource id (e.g. `shell-abc-123`).
    pub id: String,
    /// One of `"output"`, `"progress"`, `"health"`, `"events"`.
    pub sub_path: &'static str,
    /// Cursor mode requested by the client.
    pub cursor: CursorRequest,
}

/// `?cursor=` query-string mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorRequest {
    /// `?cursor=auto` — server-tracked per-peer cursor.
    Auto,
    /// `?cursor=<N>` — explicit absolute byte offset.
    At(u64),
    /// No `?cursor` query string or `?cursor=0` — full snapshot.
    Snapshot,
}

/// Errors returned by [`parse_resource_uri`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Scheme is not one of `shell`, `command`, `transfer`, `session`, `forward`.
    BadScheme(String),
    /// The path portion does not contain a non-empty resource id.
    MissingId,
    /// The sub-path does not match the scheme (e.g. `shell://abc/progress`).
    BadSubPath { expected: &'static str, got: String },
    /// `?cursor=` value is not `auto`, `0`, or a valid `u64`.
    BadCursor(String),
}

impl Display for ParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadScheme(scheme) => {
                write!(
                    f,
                    "unsupported scheme {scheme:?}: expected one of shell, command, transfer, session, forward"
                )
            }
            Self::MissingId => write!(f, "resource URI is missing the resource id segment"),
            Self::BadSubPath { expected, got } => {
                write!(f, "expected sub-path {expected:?} but found {got:?}")
            }
            Self::BadCursor(value) => write!(
                f,
                "invalid ?cursor= value {value:?}: expected auto, 0, or a non-negative integer"
            ),
        }
    }
}

impl StdError for ParseError {}

/// Build the canonical (no-cursor) URI for a `(kind, id)` pair.
#[must_use]
pub fn resource_uri(kind: ResourceKind, id: &str) -> String {
    let scheme = scheme_of(kind);
    let suffix = sub_path_of(kind);
    format!("{scheme}://{id}/{suffix}")
}

const fn scheme_of(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Shell => "shell",
        ResourceKind::Command => "command",
        ResourceKind::Transfer => "transfer",
        ResourceKind::Session => "session",
        ResourceKind::Forward => "forward",
    }
}

const fn sub_path_of(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Shell | ResourceKind::Command => "output",
        ResourceKind::Transfer => "progress",
        ResourceKind::Session => "health",
        ResourceKind::Forward => "events",
    }
}

const fn mime_of(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Shell | ResourceKind::Command => "text/plain",
        ResourceKind::Transfer | ResourceKind::Session | ResourceKind::Forward => {
            "application/json"
        }
    }
}

const fn status_meta_key(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Shell => "shell_status",
        ResourceKind::Command => "command_status",
        ResourceKind::Transfer => "transfer_status",
        ResourceKind::Session => "session_status",
        ResourceKind::Forward => "forward_status",
    }
}

/// Parse a ssh-mcp resource URI without performing any IO.
///
/// # Errors
///
/// Returns [`ParseError`] when the URI does not match one of the five
/// ssh-mcp resource schemes, when the resource id is missing, when the
/// sub-path does not match the scheme, or when the optional `?cursor=`
/// query carries an invalid value.
pub fn parse_resource_uri(uri: &str) -> Result<ParsedResourceUri, ParseError> {
    let (scheme, rest) = uri
        .split_once("://")
        .ok_or_else(|| ParseError::BadScheme(uri.to_string()))?;
    let kind = parse_scheme(scheme)?;
    let (path, query) = rest
        .split_once('?')
        .map_or((rest, None), |(p, q)| (p, Some(q)));
    let (id, sub_path) = path.split_once('/').ok_or(ParseError::MissingId)?;
    if id.is_empty() {
        return Err(ParseError::MissingId);
    }
    let expected_sub_path = sub_path_of(kind);
    if sub_path != expected_sub_path {
        return Err(ParseError::BadSubPath {
            expected: expected_sub_path,
            got: sub_path.to_string(),
        });
    }
    let cursor = query.map_or(Ok(CursorRequest::Snapshot), parse_cursor_query)?;
    Ok(ParsedResourceUri {
        kind,
        id: id.to_string(),
        sub_path: expected_sub_path,
        cursor,
    })
}

fn parse_scheme(scheme: &str) -> Result<ResourceKind, ParseError> {
    match scheme {
        "shell" => Ok(ResourceKind::Shell),
        "command" => Ok(ResourceKind::Command),
        "transfer" => Ok(ResourceKind::Transfer),
        "session" => Ok(ResourceKind::Session),
        "forward" => Ok(ResourceKind::Forward),
        other => Err(ParseError::BadScheme(other.to_string())),
    }
}

fn parse_cursor_query(query: &str) -> Result<CursorRequest, ParseError> {
    // We accept multi-pair queries (`a=b&cursor=auto&...`) and look for
    // the first `cursor=` key. Anything else is silently ignored.
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("cursor=") {
            return cursor_value(value);
        }
    }
    Ok(CursorRequest::Snapshot)
}

fn cursor_value(value: &str) -> Result<CursorRequest, ParseError> {
    if value == "auto" {
        return Ok(CursorRequest::Auto);
    }
    if value == "0" {
        return Ok(CursorRequest::Snapshot);
    }
    value
        .parse::<u64>()
        .map(CursorRequest::At)
        .map_err(|_parse_err| ParseError::BadCursor(value.to_string()))
}

/// List every active ssh-mcp resource as an rmcp [`Resource`].
///
/// Aggregates open shells, running commands, active transfers, and
/// connected sessions. Forward listing is intentionally empty for now —
/// `forward://` resources are only enumerable once a dedicated forward
/// storage layer ships. Subscriptions still work by URI.
#[must_use]
pub fn list_resources_impl() -> ListResourcesResult {
    let mut resources: Vec<Resource> = Vec::new();
    collect_shell_resources(&mut resources);
    collect_command_resources(&mut resources);
    collect_transfer_resources(&mut resources);
    collect_session_resources(&mut resources);
    // Forwards: no enumerable storage layer yet — reserved for a future
    // `ForwardStorage`. Subscribe / read still work by URI.
    ListResourcesResult::with_all_items(resources)
}

fn collect_shell_resources(out: &mut Vec<Resource>) {
    for info in SHELL_STORAGE.list_all() {
        let uri = resource_uri(ResourceKind::Shell, &info.shell_id);
        let name = format!("Shell {} (session {})", info.shell_id, info.session_id);
        let description = format!(
            "PTY output buffer for shell {} ({}, {}x{}).",
            info.shell_id, info.term_type, info.cols, info.rows
        );
        out.push(make_resource(
            uri,
            name,
            description,
            mime_of(ResourceKind::Shell),
        ));
    }
}

fn collect_command_resources(out: &mut Vec<Resource>) {
    for info in COMMAND_STORAGE.list_all() {
        let uri = resource_uri(ResourceKind::Command, &info.command_id);
        let name = format!("Command {} (session {})", info.command_id, info.session_id);
        let description = format!(
            "Async command output stream — status {}, command: {}",
            info.status, info.command
        );
        out.push(make_resource(
            uri,
            name,
            description,
            mime_of(ResourceKind::Command),
        ));
    }
}

fn collect_transfer_resources(out: &mut Vec<Resource>) {
    for info in TRANSFER_STORAGE.list_all() {
        let uri = resource_uri(ResourceKind::Transfer, &info.transfer_id);
        let name = format!(
            "Transfer {} ({} {} <-> {})",
            info.transfer_id, info.direction, info.local_path, info.remote_path
        );
        let description = format!(
            "SFTP {} progress for transfer {}.",
            info.direction, info.transfer_id
        );
        out.push(make_resource(
            uri,
            name,
            description,
            mime_of(ResourceKind::Transfer),
        ));
    }
}

fn collect_session_resources(out: &mut Vec<Resource>) {
    for info in SESSION_STORAGE.list() {
        let uri = resource_uri(ResourceKind::Session, &info.session_id);
        let name = format!(
            "Session {} ({}@{})",
            info.session_id, info.username, info.host
        );
        let description = format!(
            "SSH session health snapshot for {}@{}.",
            info.username, info.host
        );
        out.push(make_resource(
            uri,
            name,
            description,
            mime_of(ResourceKind::Session),
        ));
    }
}

fn make_resource(uri: String, name: String, description: String, mime: &str) -> Resource {
    RawResource::new(uri, name)
        .with_description(description)
        .with_mime_type(mime)
        .no_annotation()
}

/// Read the current snapshot for a ssh-mcp resource URI.
///
/// `peer` is needed so the per-peer cursor can be tracked for `?cursor=auto`
/// reads. The function never blocks on producers — it only loads the
/// current `ArcSwap` / atomic snapshots.
///
/// # Errors
///
/// Returns [`McpError::invalid_params`] when the URI does not parse, and
/// [`McpError::resource_not_found`] when no live resource matches the
/// `(kind, id)` pair.
#[allow(
    clippy::unused_async,
    reason = "rmcp ServerHandler::read_resource is uniformly async; keeping the signature lets E14 wire it without a wrapper"
)]
pub async fn read_resource_impl(
    request: ReadResourceRequestParams,
    peer: &Peer<RoleServer>,
) -> Result<ReadResourceResult, McpError> {
    let parsed = parse_resource_uri(&request.uri)
        .map_err(|err| McpError::invalid_params(err.to_string(), None))?;

    let peer_id = peer_id_for(peer);
    let canonical_uri = resource_uri(parsed.kind, &parsed.id);

    let contents = match parsed.kind {
        ResourceKind::Shell => read_shell(&parsed, &canonical_uri, &peer_id)?,
        ResourceKind::Command => read_command(&parsed, &canonical_uri, &peer_id)?,
        ResourceKind::Transfer => read_transfer(&parsed, &canonical_uri)?,
        ResourceKind::Session => read_session(&parsed, &canonical_uri)?,
        ResourceKind::Forward => read_forward(&parsed, &canonical_uri),
    };

    Ok(ReadResourceResult::new(vec![contents]))
}

fn read_shell(
    parsed: &ParsedResourceUri,
    uri: &str,
    peer_id: &str,
) -> Result<ResourceContents, McpError> {
    let history: Arc<RingBuffer> = SHELL_STORAGE
        .get_direct(&parsed.id)
        .map(|guard| guard.history.load_full())
        .ok_or_else(|| {
            McpError::resource_not_found(format!("shell {} not found", parsed.id), None)
        })?;
    let view = byte_stream_view(parsed, uri, peer_id, &history.data);
    let status = if SHELL_STORAGE.get_direct(&parsed.id).is_some() {
        "open"
    } else {
        "closed"
    };
    let meta = build_text_meta(&meta_for_byte_view(parsed.kind, &view, status));
    Ok(text_resource_contents(
        uri,
        mime_of(parsed.kind),
        view.text,
        meta,
    ))
}

fn read_command(
    parsed: &ParsedResourceUri,
    uri: &str,
    peer_id: &str,
) -> Result<ResourceContents, McpError> {
    let snapshot: Arc<OutputBuffer> = COMMAND_STORAGE
        .get_direct(&parsed.id)
        .map(|guard| guard.output_history.load_full())
        .ok_or_else(|| {
            McpError::resource_not_found(format!("command {} not found", parsed.id), None)
        })?;
    let combined = format!(
        "--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&snapshot.stdout),
        String::from_utf8_lossy(&snapshot.stderr)
    );
    let view = byte_stream_view(parsed, uri, peer_id, combined.as_bytes());
    let status = COMMAND_STORAGE
        .get_ref(&parsed.id)
        .map_or_else(|| "unknown".to_string(), |c| c.info.status.to_string());
    let meta = build_text_meta(&meta_for_byte_view(parsed.kind, &view, &status));
    Ok(text_resource_contents(
        uri,
        mime_of(parsed.kind),
        view.text,
        meta,
    ))
}

/// Result of slicing a byte-stream resource (shell / command output) for a
/// given peer cursor.
struct ByteStreamView {
    text: String,
    cursor: u64,
    buffer_len: u64,
    last_seq: u64,
    truncated_since_last_read: u64,
    keepalive: bool,
}

fn byte_stream_view(
    parsed: &ParsedResourceUri,
    uri: &str,
    peer_id: &str,
    buffer: &[u8],
) -> ByteStreamView {
    let buffer_len = u64_from_usize(buffer.len());
    let progress = SUBSCRIPTION_REGISTRY.peer_progress(peer_id, uri);
    let previous_cursor = progress.byte_cursor.load(Ordering::SeqCst);
    let start = resolve_start_offset(parsed.cursor, previous_cursor, buffer_len);
    let slice = slice_from(buffer, start);
    let trimmed = head_truncate(slice, resolve_output_max_bytes_cap());
    let text = String::from_utf8_lossy(trimmed).into_owned();
    let cursor = start.saturating_add(u64_from_usize(trimmed.len()));
    progress.byte_cursor.fetch_max(cursor, Ordering::SeqCst);
    ByteStreamView {
        text,
        cursor,
        buffer_len,
        last_seq: SUBSCRIPTION_REGISTRY.current_seq(parsed.kind, &parsed.id),
        truncated_since_last_read: compute_truncated_since(
            previous_cursor,
            buffer_len,
            slice.len(),
        ),
        keepalive: trimmed.is_empty() && start == previous_cursor,
    }
}

const fn meta_for_byte_view<'a>(
    kind: ResourceKind,
    view: &'a ByteStreamView,
    status: &'a str,
) -> MetaInputs<'a> {
    MetaInputs {
        kind,
        cursor: view.cursor,
        buffer_size: view.buffer_len,
        last_seq: view.last_seq,
        status,
        truncated_since_last_read: view.truncated_since_last_read,
        keepalive: view.keepalive,
    }
}

fn read_transfer(parsed: &ParsedResourceUri, uri: &str) -> Result<ResourceContents, McpError> {
    let guard = TRANSFER_STORAGE.get_direct(&parsed.id).ok_or_else(|| {
        McpError::resource_not_found(format!("transfer {} not found", parsed.id), None)
    })?;
    let bytes_transferred = guard.bytes_transferred.load(Ordering::SeqCst);
    let total_bytes = guard.total_bytes.load(Ordering::SeqCst);
    let status: TransferStatus = *guard.status_rx.borrow();
    let error = guard.error.get().cloned();
    let info = guard.info.clone();
    drop(guard);
    let last_seq = SUBSCRIPTION_REGISTRY.current_seq(parsed.kind, &parsed.id);
    let payload = json!({
        "transfer_id": info.transfer_id,
        "session_id": info.session_id,
        "direction": info.direction.to_string(),
        "local_path": info.local_path,
        "remote_path": info.remote_path,
        "started_at": info.started_at,
        "status": status.to_string(),
        "bytes_transferred": bytes_transferred,
        "total_bytes": total_bytes,
        "error": error,
        "last_seq": last_seq,
    });
    Ok(point_in_time_contents(
        parsed.kind,
        uri,
        &payload,
        last_seq,
        &status.to_string(),
    ))
}

fn read_session(parsed: &ParsedResourceUri, uri: &str) -> Result<ResourceContents, McpError> {
    let session = SESSION_STORAGE.get(&parsed.id).ok_or_else(|| {
        McpError::resource_not_found(format!("session {} not found", parsed.id), None)
    })?;
    let info = session.info;
    let last_seq = SUBSCRIPTION_REGISTRY.current_seq(parsed.kind, &parsed.id);
    let healthy_str = info
        .healthy
        .map_or("unknown", |h| if h { "healthy" } else { "unhealthy" });
    let payload = json!({
        "session_id": info.session_id,
        "host": info.host,
        "username": info.username,
        "agent_id": info.agent_id,
        "healthy": info.healthy,
        "last_health_check": info.last_health_check,
        "connected_at": info.connected_at,
        "compression_enabled": info.compression_enabled,
        "last_seq": last_seq,
    });
    Ok(point_in_time_contents(
        parsed.kind,
        uri,
        &payload,
        last_seq,
        healthy_str,
    ))
}

fn point_in_time_contents(
    kind: ResourceKind,
    uri: &str,
    payload: &Value,
    last_seq: u64,
    status: &str,
) -> ResourceContents {
    let meta = build_json_meta(&MetaInputs {
        kind,
        cursor: 0,
        buffer_size: 0,
        last_seq,
        status,
        truncated_since_last_read: 0,
        keepalive: false,
    });
    text_resource_contents(uri, mime_of(kind), payload.to_string(), meta)
}

fn read_forward(parsed: &ParsedResourceUri, uri: &str) -> ResourceContents {
    // Forward state is not enumerable yet — there is no `ForwardStorage`.
    // We still serve a stable point-in-time snapshot so subscribers can
    // poll by URI; status is "unknown" until the storage lands.
    let last_seq = SUBSCRIPTION_REGISTRY.current_seq(parsed.kind, &parsed.id);
    let payload = json!({
        "forward_id": parsed.id,
        "status": "unknown",
        "last_seq": last_seq,
    });
    point_in_time_contents(parsed.kind, uri, &payload, last_seq, "unknown")
}

fn slice_from(buffer: &[u8], start: u64) -> &[u8] {
    let len = u64_from_usize(buffer.len());
    if start >= len {
        return &[];
    }
    let start_usize = usize_from_u64_saturating(start);
    &buffer[start_usize..]
}

fn head_truncate(buffer: &[u8], cap: usize) -> &[u8] {
    if buffer.len() <= cap {
        return buffer;
    }
    &buffer[..cap]
}

fn resolve_start_offset(cursor: CursorRequest, previous: u64, buffer_len: u64) -> u64 {
    match cursor {
        CursorRequest::Snapshot => 0,
        CursorRequest::Auto => previous.min(buffer_len),
        CursorRequest::At(n) => n.min(buffer_len),
    }
}

fn compute_truncated_since(previous_cursor: u64, current_buffer_len: u64, available: usize) -> u64 {
    // If the producer dropped bytes from the head between reads, the
    // available delta will be smaller than the previously-tracked cursor
    // would suggest. Return how many bytes were lost.
    let available_u64 = u64_from_usize(available);
    let claimed = previous_cursor.saturating_add(available_u64);
    claimed.saturating_sub(current_buffer_len)
}

struct MetaInputs<'a> {
    kind: ResourceKind,
    cursor: u64,
    buffer_size: u64,
    last_seq: u64,
    status: &'a str,
    truncated_since_last_read: u64,
    keepalive: bool,
}

fn build_text_meta(inputs: &MetaInputs<'_>) -> Meta {
    build_meta(inputs)
}

fn build_json_meta(inputs: &MetaInputs<'_>) -> Meta {
    build_meta(inputs)
}

fn build_meta(inputs: &MetaInputs<'_>) -> Meta {
    let mut meta = Meta::new();
    meta.insert("cursor".to_string(), Value::from(inputs.cursor));
    meta.insert("buffer_size".to_string(), Value::from(inputs.buffer_size));
    meta.insert("last_seq".to_string(), Value::from(inputs.last_seq));
    meta.insert(
        status_meta_key(inputs.kind).to_string(),
        Value::from(inputs.status),
    );
    if inputs.truncated_since_last_read > 0 {
        meta.insert(
            "truncated_since_last_read".to_string(),
            Value::from(inputs.truncated_since_last_read),
        );
    }
    if inputs.keepalive {
        meta.insert("keepalive".to_string(), Value::from(true));
    }
    meta
}

fn text_resource_contents(uri: &str, mime: &str, text: String, meta: Meta) -> ResourceContents {
    ResourceContents::TextResourceContents {
        uri: uri.to_string(),
        mime_type: Some(mime.to_string()),
        text,
        meta: Some(meta),
    }
}

fn u64_from_usize(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn usize_from_u64_saturating(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Subscribe a peer to a ssh-mcp resource URI.
///
/// Validates the URI, confirms the underlying resource exists, then
/// delegates to [`SUBSCRIPTION_REGISTRY.subscribe`][SUBSCRIPTION_REGISTRY].
///
/// # Errors
///
/// Returns [`McpError::invalid_params`] for malformed URIs and
/// [`McpError::resource_not_found`] when the resource is not registered.
#[allow(
    clippy::unused_async,
    reason = "rmcp ServerHandler::subscribe is uniformly async; keeping the signature lets E14 wire it without a wrapper"
)]
pub async fn subscribe_impl(
    request: SubscribeRequestParams,
    peer: Peer<RoleServer>,
) -> Result<(), McpError> {
    let parsed = parse_resource_uri(&request.uri)
        .map_err(|err| McpError::invalid_params(err.to_string(), None))?;

    ensure_resource_exists(parsed.kind, &parsed.id)?;

    let peer_id = peer_id_for(&peer);
    let canonical_uri = resource_uri(parsed.kind, &parsed.id);
    SUBSCRIPTION_REGISTRY
        .subscribe(parsed.kind, parsed.id, canonical_uri, peer_id, peer)
        .map(|_first| ())
        .map_err(|err| McpError::internal_error(err, None))
}

/// Unsubscribe a peer from a ssh-mcp resource URI.
///
/// `peer_id` MUST be the same identifier used at subscribe time. The
/// canonical id derivation is [`peer_id_for`].
///
/// # Errors
///
/// Returns [`McpError::invalid_params`] when the URI does not parse.
/// Unknown URIs / not-subscribed peers are a silent no-op (matches MCP
/// semantics — `unsubscribe` is idempotent).
#[allow(
    clippy::unused_async,
    reason = "rmcp ServerHandler::unsubscribe is uniformly async; keeping the signature lets E14 wire it without a wrapper"
)]
pub async fn unsubscribe_impl(
    request: UnsubscribeRequestParams,
    peer_id: &str,
) -> Result<(), McpError> {
    let parsed = parse_resource_uri(&request.uri)
        .map_err(|err| McpError::invalid_params(err.to_string(), None))?;
    let canonical_uri = resource_uri(parsed.kind, &parsed.id);
    SUBSCRIPTION_REGISTRY.unsubscribe(peer_id, &canonical_uri);
    Ok(())
}

fn ensure_resource_exists(kind: ResourceKind, id: &str) -> Result<(), McpError> {
    let exists = match kind {
        ResourceKind::Shell => SHELL_STORAGE.get_direct(id).is_some(),
        ResourceKind::Command => COMMAND_STORAGE.get_direct(id).is_some(),
        ResourceKind::Transfer => TRANSFER_STORAGE.get_direct(id).is_some(),
        ResourceKind::Session => SESSION_STORAGE.contains(id),
        // Forwards have no enumerable storage yet — accept all subscribes
        // so producers (E13 forward.rs) can broadcast events that future
        // ForwardStorage will index.
        ResourceKind::Forward => true,
    };
    if exists {
        Ok(())
    } else {
        Err(McpError::resource_not_found(
            format!("{} {} not found", scheme_of(kind), id),
            None,
        ))
    }
}

/// Derive a stable peer identifier from a [`Peer<RoleServer>`].
///
/// rmcp 1.6 does not expose a public `Peer` id. Every `Peer` is internally
/// backed by a unique `tokio::sync::mpsc::Sender`; that sender's `Debug`
/// representation embeds a pointer to its shared channel state and stays
/// stable for the lifetime of the peer. We piggyback on the `Peer`'s
/// `Debug` impl (which forwards the sender's `Debug`) to obtain a stable
/// per-process handle without touching private fields.
///
/// This is an implementation detail — once rmcp grows a public `Peer::id`
/// (or transport-level identity), swap this out without breaking callers.
#[must_use]
pub fn peer_id_for(peer: &Peer<RoleServer>) -> String {
    format!("peer-{peer:?}")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests use unwrap on values constructed in the test itself"
)]
mod tests {
    use super::*;

    fn p(uri: &str) -> ParsedResourceUri {
        parse_resource_uri(uri).unwrap()
    }

    #[test]
    fn parse_shell_snapshot() {
        let parsed = p("shell://abc-123/output");
        assert_eq!(parsed.kind, ResourceKind::Shell);
        assert_eq!(parsed.id, "abc-123");
        assert_eq!(parsed.sub_path, "output");
        assert_eq!(parsed.cursor, CursorRequest::Snapshot);
    }

    #[test]
    fn parse_shell_auto_cursor() {
        let parsed = p("shell://abc-123/output?cursor=auto");
        assert_eq!(parsed.cursor, CursorRequest::Auto);
    }

    #[test]
    fn parse_shell_explicit_cursor() {
        let parsed = p("shell://abc-123/output?cursor=4096");
        assert_eq!(parsed.cursor, CursorRequest::At(4096));
    }

    #[test]
    fn parse_command_zero_cursor_is_snapshot() {
        let parsed = p("command://cmd-1/output?cursor=0");
        assert_eq!(parsed.kind, ResourceKind::Command);
        assert_eq!(parsed.cursor, CursorRequest::Snapshot);
    }

    #[test]
    fn parse_transfer_progress() {
        let parsed = p("transfer://xfer-1/progress");
        assert_eq!(parsed.kind, ResourceKind::Transfer);
        assert_eq!(parsed.sub_path, "progress");
        assert_eq!(parsed.cursor, CursorRequest::Snapshot);
    }

    #[test]
    fn parse_session_health() {
        let parsed = p("session://sess-1/health");
        assert_eq!(parsed.kind, ResourceKind::Session);
        assert_eq!(parsed.sub_path, "health");
    }

    #[test]
    fn parse_forward_events_with_auto() {
        let parsed = p("forward://fwd-1/events?cursor=auto");
        assert_eq!(parsed.kind, ResourceKind::Forward);
        assert_eq!(parsed.sub_path, "events");
        assert_eq!(parsed.cursor, CursorRequest::Auto);
    }

    #[test]
    fn parse_bad_scheme() {
        assert!(matches!(
            parse_resource_uri("ftp://abc/output"),
            Err(ParseError::BadScheme(_))
        ));
        assert!(matches!(
            parse_resource_uri("not-a-uri"),
            Err(ParseError::BadScheme(_))
        ));
    }

    #[test]
    fn parse_missing_id() {
        assert_eq!(
            parse_resource_uri("shell:///output"),
            Err(ParseError::MissingId)
        );
        assert_eq!(
            parse_resource_uri("shell://abc"),
            Err(ParseError::MissingId)
        );
    }

    #[test]
    fn parse_bad_subpath() {
        let err = parse_resource_uri("shell://abc/progress").unwrap_err();
        match err {
            ParseError::BadSubPath { expected, got } => {
                assert_eq!(expected, "output");
                assert_eq!(got, "progress");
            }
            ParseError::BadScheme(_) | ParseError::MissingId | ParseError::BadCursor(_) => {
                panic!("wrong variant: {err:?}")
            }
        }
    }

    #[test]
    fn parse_bad_cursor() {
        let err = parse_resource_uri("shell://abc/output?cursor=banana").unwrap_err();
        match err {
            ParseError::BadCursor(value) => assert_eq!(value, "banana"),
            ParseError::BadScheme(_) | ParseError::MissingId | ParseError::BadSubPath { .. } => {
                panic!("wrong variant: {err:?}")
            }
        }
    }

    #[test]
    fn resource_uri_round_trips_through_parser() {
        for kind in [
            ResourceKind::Shell,
            ResourceKind::Command,
            ResourceKind::Transfer,
            ResourceKind::Session,
            ResourceKind::Forward,
        ] {
            let uri = resource_uri(kind, "abc");
            let parsed = parse_resource_uri(&uri).unwrap();
            assert_eq!(parsed.kind, kind);
            assert_eq!(parsed.id, "abc");
            assert_eq!(parsed.cursor, CursorRequest::Snapshot);
        }
    }

    #[test]
    fn list_resources_impl_does_not_panic() {
        // Storage may be empty or have entries from concurrent tests; the
        // contract is just that the call never panics and returns a typed
        // payload with optional `next_cursor = None` (we don't paginate).
        let result = list_resources_impl();
        assert!(result.next_cursor.is_none());
    }

    #[test]
    fn read_unknown_uri_returns_invalid_params() {
        // We exercise the parser-error path without needing a Peer — the
        // dispatcher fails before constructing a peer cursor.
        let parse = parse_resource_uri("ftp://x/output");
        match parse {
            Err(ParseError::BadScheme(scheme)) => assert_eq!(scheme, "ftp"),
            Err(other) => panic!("unexpected parser error: {other:?}"),
            Ok(parsed) => panic!("unexpected success: {parsed:?}"),
        }
    }

    #[test]
    fn resolve_start_offset_modes() {
        // Snapshot always starts at 0.
        assert_eq!(resolve_start_offset(CursorRequest::Snapshot, 100, 200), 0);
        // Auto clamps to buffer_len.
        assert_eq!(resolve_start_offset(CursorRequest::Auto, 50, 200), 50);
        assert_eq!(resolve_start_offset(CursorRequest::Auto, 500, 200), 200);
        // Explicit At clamps to buffer_len.
        assert_eq!(resolve_start_offset(CursorRequest::At(80), 0, 200), 80);
        assert_eq!(resolve_start_offset(CursorRequest::At(999), 0, 200), 200);
    }

    #[test]
    fn head_truncate_caps_oversize_buffer() {
        let bytes = vec![b'x'; 1024];
        let trimmed = head_truncate(&bytes, 256);
        assert_eq!(trimmed.len(), 256);
        let untouched = head_truncate(&bytes, 4096);
        assert_eq!(untouched.len(), 1024);
    }

    #[test]
    fn slice_from_handles_oob_offset() {
        let buf = b"hello";
        assert_eq!(slice_from(buf, 0), b"hello");
        assert_eq!(slice_from(buf, 2), b"llo");
        assert_eq!(slice_from(buf, 5), b"");
        assert_eq!(slice_from(buf, 99), b"");
    }

    #[test]
    fn compute_truncated_since_detects_head_drop() {
        // Previous cursor 100, buffer now 50, available delta 0 -> we
        // dropped 50 bytes from the head.
        assert_eq!(compute_truncated_since(100, 50, 0), 50);
        // No drop when buffer matches.
        assert_eq!(compute_truncated_since(100, 200, 100), 0);
        // Drop is saturating: never goes negative.
        assert_eq!(compute_truncated_since(0, 0, 0), 0);
    }

    #[test]
    fn build_meta_omits_optional_fields_when_zero_or_false() {
        let meta = build_meta(&MetaInputs {
            kind: ResourceKind::Shell,
            cursor: 12,
            buffer_size: 34,
            last_seq: 7,
            status: "open",
            truncated_since_last_read: 0,
            keepalive: false,
        });
        assert!(meta.contains_key("cursor"));
        assert!(meta.contains_key("buffer_size"));
        assert!(meta.contains_key("last_seq"));
        assert!(meta.contains_key("shell_status"));
        assert!(!meta.contains_key("truncated_since_last_read"));
        assert!(!meta.contains_key("keepalive"));
    }

    #[test]
    fn build_meta_emits_optional_fields_when_set() {
        let meta = build_meta(&MetaInputs {
            kind: ResourceKind::Command,
            cursor: 0,
            buffer_size: 0,
            last_seq: 0,
            status: "running",
            truncated_since_last_read: 64,
            keepalive: true,
        });
        assert_eq!(
            meta.get("truncated_since_last_read"),
            Some(&Value::from(64_u64))
        );
        assert_eq!(meta.get("keepalive"), Some(&Value::from(true)));
        assert_eq!(meta.get("command_status"), Some(&Value::from("running")));
    }

    #[test]
    fn ensure_resource_exists_forward_is_permissive() {
        // Forward has no storage yet — subscribes always succeed.
        ensure_resource_exists(ResourceKind::Forward, "any-id").unwrap();
    }

    #[test]
    fn ensure_resource_exists_session_unknown_fails() {
        let id = format!("missing-{}", uuid::Uuid::new_v4());
        let err = ensure_resource_exists(ResourceKind::Session, &id).unwrap_err();
        assert!(err.message.contains("session"));
    }

    #[test]
    fn cursor_query_supports_extra_pairs() {
        let parsed = p("shell://abc/output?other=1&cursor=auto&z=2");
        assert_eq!(parsed.cursor, CursorRequest::Auto);
    }

    #[test]
    fn cursor_query_no_cursor_key_is_snapshot() {
        let parsed = p("shell://abc/output?other=1&z=2");
        assert_eq!(parsed.cursor, CursorRequest::Snapshot);
    }

    mod e15_extra {
        use super::*;

        #[test]
        fn resource_uri_emits_shell_scheme() {
            assert_eq!(resource_uri(ResourceKind::Shell, "x"), "shell://x/output");
        }

        #[test]
        fn resource_uri_emits_command_scheme() {
            assert_eq!(
                resource_uri(ResourceKind::Command, "x"),
                "command://x/output"
            );
        }

        #[test]
        fn resource_uri_emits_transfer_scheme() {
            assert_eq!(
                resource_uri(ResourceKind::Transfer, "x"),
                "transfer://x/progress"
            );
        }

        #[test]
        fn resource_uri_emits_session_scheme() {
            assert_eq!(
                resource_uri(ResourceKind::Session, "x"),
                "session://x/health"
            );
        }

        #[test]
        fn resource_uri_emits_forward_scheme() {
            assert_eq!(
                resource_uri(ResourceKind::Forward, "x"),
                "forward://x/events"
            );
        }

        #[test]
        fn resource_uri_round_trip_with_auto_cursor() {
            let parsed = p("shell://abc/output?cursor=auto");
            // Build a NEW URI from parsed; round-trip drops the cursor (the
            // canonical URI has no query string).
            let uri = resource_uri(parsed.kind, &parsed.id);
            assert_eq!(uri, "shell://abc/output");
        }

        #[test]
        fn parse_error_display_has_diagnostic_text() {
            let err = parse_resource_uri("ftp://x/output").unwrap_err();
            let text = err.to_string();
            assert!(text.contains("ftp"));
            assert!(text.contains("expected one of"));
        }

        #[test]
        fn parse_error_display_for_missing_id() {
            let err = parse_resource_uri("shell:///output").unwrap_err();
            let text = err.to_string();
            assert!(text.contains("missing"));
        }

        #[test]
        fn parse_error_display_for_bad_subpath() {
            let err = parse_resource_uri("shell://abc/progress").unwrap_err();
            let text = err.to_string();
            assert!(text.contains("output"));
            assert!(text.contains("progress"));
        }

        #[test]
        fn parse_error_display_for_bad_cursor() {
            let err = parse_resource_uri("shell://abc/output?cursor=banana").unwrap_err();
            let text = err.to_string();
            assert!(text.contains("banana"));
            assert!(text.contains("auto"));
        }

        #[test]
        fn parse_command_with_explicit_cursor() {
            let parsed = p("command://cmd-99/output?cursor=12345");
            assert_eq!(parsed.kind, ResourceKind::Command);
            assert_eq!(parsed.id, "cmd-99");
            assert_eq!(parsed.cursor, CursorRequest::At(12345));
        }

        #[test]
        fn parse_transfer_extra_query_pairs_tolerated() {
            let parsed = p("transfer://xfer-1/progress?other=1&zzz=2");
            assert_eq!(parsed.kind, ResourceKind::Transfer);
            assert_eq!(parsed.cursor, CursorRequest::Snapshot);
        }

        #[test]
        fn parse_session_extra_query_pairs_tolerated() {
            // Health is point-in-time; cursor should still default to Snapshot.
            let parsed = p("session://sess-7/health?ignored=value");
            assert_eq!(parsed.kind, ResourceKind::Session);
            assert_eq!(parsed.cursor, CursorRequest::Snapshot);
        }

        #[test]
        fn url_encoded_id_is_passed_through_verbatim() {
            // The parser does NOT decode URL-encoded IDs. Document this
            // behaviour so future changes are intentional.
            let parsed = p("shell://abc%2F123/output");
            assert_eq!(parsed.id, "abc%2F123");
        }

        #[test]
        fn empty_query_string_is_snapshot() {
            // `?` with no key/value is treated as Snapshot — the loop never
            // finds a `cursor=` prefix.
            let parsed = p("shell://abc/output?");
            assert_eq!(parsed.cursor, CursorRequest::Snapshot);
        }

        #[test]
        fn cursor_value_zero_is_snapshot() {
            let parsed = p("shell://abc/output?cursor=0");
            assert_eq!(parsed.cursor, CursorRequest::Snapshot);
        }

        #[test]
        fn cursor_value_max_u64_is_at_max() {
            let uri = format!("shell://abc/output?cursor={}", u64::MAX);
            let parsed = p(&uri);
            assert_eq!(parsed.cursor, CursorRequest::At(u64::MAX));
        }

        #[test]
        fn cursor_value_overflow_is_bad_cursor() {
            // u64::MAX + 1 doesn't fit -> BadCursor.
            let err =
                parse_resource_uri("shell://abc/output?cursor=99999999999999999999").unwrap_err();
            match err {
                ParseError::BadCursor(value) => {
                    assert_eq!(value, "99999999999999999999");
                }
                ParseError::BadScheme(_)
                | ParseError::MissingId
                | ParseError::BadSubPath { .. } => panic!("wrong variant: {err:?}"),
            }
        }

        #[test]
        fn cursor_value_negative_is_bad_cursor() {
            let err = parse_resource_uri("shell://abc/output?cursor=-1").unwrap_err();
            match err {
                ParseError::BadCursor(value) => assert_eq!(value, "-1"),
                ParseError::BadScheme(_)
                | ParseError::MissingId
                | ParseError::BadSubPath { .. } => panic!("wrong variant: {err:?}"),
            }
        }

        #[test]
        fn parse_resource_uri_with_id_containing_dashes_and_dots() {
            let parsed = p("shell://abc-123.def/output");
            assert_eq!(parsed.id, "abc-123.def");
        }

        #[test]
        fn ensure_resource_exists_session_known_does_not_panic_on_unknown() {
            // We can't easily inject a known session here, but we can assert
            // unknown sessions return resource_not_found cleanly.
            let id = format!("ghost-{}", uuid::Uuid::new_v4());
            let err = ensure_resource_exists(ResourceKind::Session, &id).unwrap_err();
            assert!(err.message.contains("session"));
        }

        #[test]
        fn parse_error_equality() {
            assert_eq!(ParseError::MissingId, ParseError::MissingId);
            assert_eq!(
                ParseError::BadCursor("x".to_string()),
                ParseError::BadCursor("x".to_string())
            );
            assert_ne!(
                ParseError::BadCursor("x".to_string()),
                ParseError::BadCursor("y".to_string())
            );
        }

        #[test]
        fn build_meta_status_keys_match_kind() {
            // Exercise every status_meta_key branch.
            for kind in [
                ResourceKind::Shell,
                ResourceKind::Command,
                ResourceKind::Transfer,
                ResourceKind::Session,
                ResourceKind::Forward,
            ] {
                let meta = build_meta(&MetaInputs {
                    kind,
                    cursor: 0,
                    buffer_size: 0,
                    last_seq: 0,
                    status: "ok",
                    truncated_since_last_read: 0,
                    keepalive: false,
                });
                let expected_key = match kind {
                    ResourceKind::Shell => "shell_status",
                    ResourceKind::Command => "command_status",
                    ResourceKind::Transfer => "transfer_status",
                    ResourceKind::Session => "session_status",
                    ResourceKind::Forward => "forward_status",
                };
                assert!(meta.contains_key(expected_key));
            }
        }
    }
}
