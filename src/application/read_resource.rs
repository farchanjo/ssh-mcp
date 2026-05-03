//! Use case: read the current snapshot of a ssh-mcp resource URI.
//!
//! Translates the legacy v3 `read_resource_impl` into a
//! hexagonal use case orchestrating the URI parser, the four/five
//! repositories, the [`OutputStreamPort`] (for the byte-stream resources:
//! shell + command), and the [`SubscriberRegistryPort`] (for cursor
//! advancement and `last_seq` snapshots).
//!
//! # URI parsing
//!
//! The use case re-parses the v3 wire format directly via
//! [`parse_uri`]; we deliberately do not depend on
//! the legacy `parse_resource_uri` helper because the v3 module pulled
//! rmcp types into scope; H17.6 P3 has now removed it entirely.
//!
//! Supported URIs (mirrors v3 verbatim):
//!
//! - `shell://<id>/output[?cursor=auto|<N>|0]`
//! - `command://<id>/output[?cursor=auto|<N>|0]`
//! - `transfer://<id>/progress`        (no cursor — point-in-time)
//! - `session://<id>/health`           (no cursor — point-in-time)
//! - `forward://<id>/events[?cursor=auto|<N>|0]`
//!
//! # Cursor semantics
//!
//! `?cursor=auto` consults the per-(peer, uri) byte cursor stored in the
//! [`SubscriberRegistryPort`]. `?cursor=<N>` overrides the start offset
//! to an absolute byte position. `?cursor=0` (or no `?cursor`) resets to
//! a full snapshot. After serving the slice the use case bumps the
//! per-peer cursor to the new tail through
//! [`SubscriberRegistryPort::advance_peer_byte_cursor`] so the next
//! `?cursor=auto` read sees only fresh bytes.
//!
//! # Concurrency contract
//!
//! Every `.await` is driven against owned types; the use case never
//! holds a [`dashmap::DashMap`] guard across an await point.

use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::json;

use crate::domain::error::DomainError;
use crate::domain::ids::{CommandId, PeerId, SessionId, ShellId, TransferId};
use crate::ports::command_repo::CommandRepository;
use crate::ports::output_stream::OutputStreamPort;
use crate::ports::session_repo::SessionRepository;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::subscriber_registry::{ResourceKind, SubscriberRegistryPort};
use crate::ports::transfer_repo::TransferRepository;

#[cfg(feature = "port_forward")]
use crate::domain::ids::ForwardId;
#[cfg(feature = "port_forward")]
use crate::ports::forward_repo::ForwardRepository;

/// `?cursor=` mode requested by the inbound caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMode {
    /// Per-peer cursor stored in the subscription registry.
    Auto,
    /// Absolute byte offset.
    At(u64),
    /// Full snapshot (no cursor / `?cursor=0`).
    Snapshot,
}

/// Parsed view of a ssh-mcp resource URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUri {
    /// Resource scheme.
    pub kind: ResourceKind,
    /// Resource id portion of the URI.
    pub id: String,
    /// Cursor mode requested by the caller.
    pub cursor: CursorMode,
}

/// Errors returned by [`parse_uri`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Scheme is not one of the five ssh-mcp schemes.
    BadScheme(String),
    /// The path portion does not contain a non-empty resource id.
    MissingId,
    /// The sub-path does not match the scheme (e.g. `shell://abc/progress`).
    BadSubPath {
        /// Expected sub-path.
        expected: &'static str,
        /// Sub-path observed in the URI.
        got: String,
    },
    /// `?cursor=` value is not `auto`, `0`, or a valid `u64`.
    BadCursor(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadScheme(scheme) => write!(
                f,
                "unsupported scheme {scheme:?}: expected one of shell, command, transfer, session, forward"
            ),
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

const fn sub_path_of(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Shell | ResourceKind::Command => "output",
        ResourceKind::Transfer => "progress",
        ResourceKind::Session => "health",
        ResourceKind::Forward => "events",
    }
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

/// Build the canonical (no-cursor) URI for a `(kind, id)` pair.
#[must_use]
pub fn canonical_uri(kind: ResourceKind, id: &str) -> String {
    format!("{}://{}/{}", scheme_of(kind), id, sub_path_of(kind))
}

/// Parse a ssh-mcp resource URI. Pure function; performs no IO.
///
/// # Errors
///
/// Returns [`ParseError`] when the URI does not match one of the five
/// ssh-mcp resource schemes, when the resource id is missing, when the
/// sub-path does not match the scheme, or when the optional `?cursor=`
/// query carries an invalid value.
pub fn parse_uri(uri: &str) -> Result<ParsedUri, ParseError> {
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
    let expected = sub_path_of(kind);
    if sub_path != expected {
        return Err(ParseError::BadSubPath {
            expected,
            got: sub_path.to_string(),
        });
    }
    let cursor = query.map_or(Ok(CursorMode::Snapshot), parse_cursor_query)?;
    Ok(ParsedUri {
        kind,
        id: id.to_string(),
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

fn parse_cursor_query(query: &str) -> Result<CursorMode, ParseError> {
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("cursor=") {
            return cursor_value(value);
        }
    }
    Ok(CursorMode::Snapshot)
}

fn cursor_value(value: &str) -> Result<CursorMode, ParseError> {
    if value == "auto" {
        return Ok(CursorMode::Auto);
    }
    if value == "0" {
        return Ok(CursorMode::Snapshot);
    }
    value
        .parse::<u64>()
        .map(CursorMode::At)
        .map_err(|_parse_err| ParseError::BadCursor(value.to_string()))
}

/// Inbound DTO. The peer id is required even for stateless resources so
/// the inbound adapter (etapa H16) can reuse the same plumbing it wires
/// for [`crate::application::subscribe_resource`].
#[derive(Debug, Clone)]
pub struct ReadResourceRequest {
    /// Caller-supplied URI.
    pub uri: String,
    /// Stable peer id used for `?cursor=auto` cursor tracking.
    pub peer_id: PeerId,
}

/// Outbound DTO carrying the snapshot and the metadata callers must
/// echo back through `_meta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadResourceOutcome {
    /// Shell PTY output buffer slice.
    Shell {
        /// Canonical URI (no `?cursor=`).
        uri: String,
        /// UTF-8 lossy slice of the rolling buffer.
        data: Bytes,
        /// New cursor value the peer should pass on the next read.
        cursor: u64,
        /// Cumulative buffer length at snapshot time.
        buffer_size: u64,
        /// Latest sequence number observed.
        last_seq: u64,
        /// Lifecycle hint string ("open" or "closed").
        status: String,
    },
    /// Async command output (interleaved stdout + stderr, v3-compatible).
    Command {
        /// Canonical URI.
        uri: String,
        /// Stdout bytes.
        stdout: Bytes,
        /// Stderr bytes.
        stderr: Bytes,
        /// Composite cursor over the combined payload.
        cursor: u64,
        /// Combined buffer length at snapshot time.
        buffer_size: u64,
        /// Latest sequence number observed.
        last_seq: u64,
        /// Lifecycle hint string.
        status: String,
    },
    /// Point-in-time SFTP transfer progress snapshot.
    Transfer {
        /// Canonical URI.
        uri: String,
        /// JSON-serialised payload (already rendered for transport).
        json_payload: String,
        /// Latest sequence number observed.
        last_seq: u64,
        /// Lifecycle hint string.
        status: String,
    },
    /// SSH session health snapshot.
    Session {
        /// Canonical URI.
        uri: String,
        /// JSON-serialised payload (already rendered for transport).
        json_payload: String,
        /// Latest sequence number observed.
        last_seq: u64,
        /// Lifecycle hint string ("healthy"/"unhealthy"/"unknown").
        status: String,
    },
    /// Port-forwarder event snapshot. Feature-gated by `port_forward`.
    #[cfg(feature = "port_forward")]
    Forward {
        /// Canonical URI.
        uri: String,
        /// JSON-serialised payload.
        json_payload: String,
        /// Latest sequence number observed.
        last_seq: u64,
        /// Lifecycle hint string.
        status: String,
    },
}

/// Read-resource use case. Sub-registers retain `Arc` handles to every
/// repository plus the [`OutputStreamPort`] and [`SubscriberRegistryPort`].
///
/// Two flavours are exported, gated by the `port_forward` feature, so
/// the forward repository slot does not require a stub when the feature
/// is disabled.
#[cfg(not(feature = "port_forward"))]
#[derive(Debug)]
pub struct ReadResourceUseCase<ShR, CR, TR, SR, OS, Sub>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    Sub: SubscriberRegistryPort,
{
    shells: Arc<ShR>,
    commands: Arc<CR>,
    transfers: Arc<TR>,
    sessions: Arc<SR>,
    output: Arc<OS>,
    subscribers: Arc<Sub>,
}

/// Read-resource use case with the `port_forward` feature enabled.
#[cfg(feature = "port_forward")]
#[derive(Debug)]
pub struct ReadResourceUseCase<ShR, CR, TR, SR, FR, OS, Sub>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    FR: ForwardRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    Sub: SubscriberRegistryPort,
{
    shells: Arc<ShR>,
    commands: Arc<CR>,
    transfers: Arc<TR>,
    sessions: Arc<SR>,
    forwards: Arc<FR>,
    output: Arc<OS>,
    subscribers: Arc<Sub>,
}

#[cfg(not(feature = "port_forward"))]
impl<ShR, CR, TR, SR, OS, Sub> ReadResourceUseCase<ShR, CR, TR, SR, OS, Sub>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    Sub: SubscriberRegistryPort,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(
        shells: Arc<ShR>,
        commands: Arc<CR>,
        transfers: Arc<TR>,
        sessions: Arc<SR>,
        output: Arc<OS>,
        subscribers: Arc<Sub>,
    ) -> Self {
        Self {
            shells,
            commands,
            transfers,
            sessions,
            output,
            subscribers,
        }
    }

    /// Drive the read orchestration. See module-level docs.
    ///
    /// # Errors
    ///
    /// - [`DomainError::InvalidArgument`] when the URI fails to parse.
    /// - [`DomainError::ShellNotFound`] / `CommandNotFound` /
    ///   `TransferNotFound` / `SessionNotFound` when the resource is
    ///   unknown.
    pub async fn execute(
        &self,
        req: ReadResourceRequest,
    ) -> Result<ReadResourceOutcome, DomainError> {
        let parsed =
            parse_uri(&req.uri).map_err(|e| DomainError::InvalidArgument(e.to_string()))?;
        let canonical = canonical_uri(parsed.kind, &parsed.id);
        match parsed.kind {
            ResourceKind::Shell => {
                read_shell(
                    &*self.shells,
                    &*self.output,
                    &*self.subscribers,
                    &parsed,
                    &canonical,
                    &req.peer_id,
                )
                .await
            }
            ResourceKind::Command => {
                read_command(
                    &*self.commands,
                    &*self.output,
                    &*self.subscribers,
                    &parsed,
                    &canonical,
                    &req.peer_id,
                )
                .await
            }
            ResourceKind::Transfer => {
                read_transfer(&*self.transfers, &*self.subscribers, &parsed, &canonical).await
            }
            ResourceKind::Session => {
                read_session(&*self.sessions, &*self.subscribers, &parsed, &canonical).await
            }
            ResourceKind::Forward => Err(DomainError::InvalidArgument(
                "forward:// resources require the port_forward Cargo feature".to_string(),
            )),
        }
    }
}

#[cfg(feature = "port_forward")]
impl<ShR, CR, TR, SR, FR, OS, Sub> ReadResourceUseCase<ShR, CR, TR, SR, FR, OS, Sub>
where
    ShR: ShellRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    SR: SessionRepository + Send + Sync,
    FR: ForwardRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    Sub: SubscriberRegistryPort,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(
        shells: Arc<ShR>,
        commands: Arc<CR>,
        transfers: Arc<TR>,
        sessions: Arc<SR>,
        forwards: Arc<FR>,
        output: Arc<OS>,
        subscribers: Arc<Sub>,
    ) -> Self {
        Self {
            shells,
            commands,
            transfers,
            sessions,
            forwards,
            output,
            subscribers,
        }
    }

    /// Drive the read orchestration. See module-level docs.
    ///
    /// # Errors
    ///
    /// - [`DomainError::InvalidArgument`] when the URI fails to parse.
    /// - Resource-specific not-found errors when the URI does not
    ///   resolve to a live aggregate.
    #[allow(
        clippy::too_many_lines,
        reason = "five-arm dispatcher: each arm needs explicit port wiring; extracting helpers per arm hurts readability"
    )]
    pub async fn execute(
        &self,
        req: ReadResourceRequest,
    ) -> Result<ReadResourceOutcome, DomainError> {
        let parsed =
            parse_uri(&req.uri).map_err(|e| DomainError::InvalidArgument(e.to_string()))?;
        let canonical = canonical_uri(parsed.kind, &parsed.id);
        match parsed.kind {
            ResourceKind::Shell => {
                read_shell(
                    &*self.shells,
                    &*self.output,
                    &*self.subscribers,
                    &parsed,
                    &canonical,
                    &req.peer_id,
                )
                .await
            }
            ResourceKind::Command => {
                read_command(
                    &*self.commands,
                    &*self.output,
                    &*self.subscribers,
                    &parsed,
                    &canonical,
                    &req.peer_id,
                )
                .await
            }
            ResourceKind::Transfer => {
                read_transfer(&*self.transfers, &*self.subscribers, &parsed, &canonical).await
            }
            ResourceKind::Session => {
                read_session(&*self.sessions, &*self.subscribers, &parsed, &canonical).await
            }
            ResourceKind::Forward => {
                read_forward(&*self.forwards, &*self.subscribers, &parsed, &canonical).await
            }
        }
    }
}

async fn read_shell<ShR, OS, Sub>(
    shells: &ShR,
    output: &OS,
    subscribers: &Sub,
    parsed: &ParsedUri,
    canonical: &str,
    peer_id: &PeerId,
) -> Result<ReadResourceOutcome, DomainError>
where
    ShR: ShellRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    Sub: SubscriberRegistryPort,
{
    let id = ShellId::new(parsed.id.clone());
    let entity = shells
        .get(&id)
        .await?
        .ok_or_else(|| DomainError::ShellNotFound(id.clone()))?;
    let snapshot = output.snapshot_shell(&id).await?;
    let buffer_len = u64_from_usize(snapshot.stdout.len());
    let start = resolve_start_offset(parsed.cursor, subscribers, peer_id, canonical, buffer_len);
    let slice = slice_from(&snapshot.stdout, start);
    let cursor = start.saturating_add(u64_from_usize(slice.len()));
    let _new_cursor = subscribers.advance_peer_byte_cursor(peer_id, canonical, cursor);
    let status = entity.status.to_string();
    Ok(ReadResourceOutcome::Shell {
        uri: canonical.to_string(),
        data: Bytes::copy_from_slice(slice),
        cursor,
        buffer_size: buffer_len,
        last_seq: snapshot.last_seq,
        status,
    })
}

async fn read_command<CR, OS, Sub>(
    commands: &CR,
    output: &OS,
    subscribers: &Sub,
    parsed: &ParsedUri,
    canonical: &str,
    peer_id: &PeerId,
) -> Result<ReadResourceOutcome, DomainError>
where
    CR: CommandRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    Sub: SubscriberRegistryPort,
{
    let id = CommandId::new(parsed.id.clone());
    let entity = commands
        .get(&id)
        .await?
        .ok_or_else(|| DomainError::CommandNotFound(id.clone()))?;
    let snapshot = output.snapshot_command(&id).await?;
    let combined_len =
        u64_from_usize(snapshot.stdout.len()).saturating_add(u64_from_usize(snapshot.stderr.len()));
    let start = resolve_start_offset(parsed.cursor, subscribers, peer_id, canonical, combined_len);
    // Cursor advances over the concatenated stdout+stderr stream.
    let cursor = start.saturating_add(combined_len.saturating_sub(start));
    let _new_cursor = subscribers.advance_peer_byte_cursor(peer_id, canonical, cursor);
    let status = entity.status.to_string();
    Ok(ReadResourceOutcome::Command {
        uri: canonical.to_string(),
        stdout: snapshot.stdout,
        stderr: snapshot.stderr,
        cursor,
        buffer_size: combined_len,
        last_seq: snapshot.last_seq,
        status,
    })
}

async fn read_transfer<TR, Sub>(
    transfers: &TR,
    subscribers: &Sub,
    parsed: &ParsedUri,
    canonical: &str,
) -> Result<ReadResourceOutcome, DomainError>
where
    TR: TransferRepository + Send + Sync,
    Sub: SubscriberRegistryPort,
{
    let id = TransferId::new(parsed.id.clone());
    let entity = transfers
        .get(&id)
        .await?
        .ok_or_else(|| DomainError::TransferNotFound(id.clone()))?;
    let last_seq = subscribers.current_seq(parsed.kind, &parsed.id);
    let payload = json!({
        "transfer_id": entity.id.as_str(),
        "session_id": entity.session_id.as_str(),
        "direction": entity.direction.to_string(),
        "local_path": entity.local_path,
        "remote_path": entity.remote_path,
        "started_at": entity.started_at,
        "status": entity.status.to_string(),
        "bytes_transferred": entity.bytes_transferred,
        "total_bytes": entity.total_bytes,
        "error": entity.error,
        "last_seq": last_seq,
    });
    Ok(ReadResourceOutcome::Transfer {
        uri: canonical.to_string(),
        json_payload: payload.to_string(),
        last_seq,
        status: entity.status.to_string(),
    })
}

async fn read_session<SR, Sub>(
    sessions: &SR,
    subscribers: &Sub,
    parsed: &ParsedUri,
    canonical: &str,
) -> Result<ReadResourceOutcome, DomainError>
where
    SR: SessionRepository + Send + Sync,
    Sub: SubscriberRegistryPort,
{
    let id = SessionId::new(parsed.id.clone());
    let entity = sessions
        .get(&id)
        .await?
        .ok_or_else(|| DomainError::SessionNotFound(id.clone()))?;
    let last_seq = subscribers.current_seq(parsed.kind, &parsed.id);
    let healthy_str = entity
        .healthy
        .map_or("unknown", |h| if h { "healthy" } else { "unhealthy" });
    let payload = json!({
        "session_id": entity.id.as_str(),
        "host": entity.address.host(),
        "port": entity.address.port(),
        "username": entity.username,
        "agent_id": entity.agent_id.as_ref().map(|a| a.as_str().to_string()),
        "healthy": entity.healthy,
        "last_health_check": entity.last_health_check,
        "connected_at": entity.connected_at,
        "compression_enabled": entity.compression_enabled,
        "last_seq": last_seq,
    });
    Ok(ReadResourceOutcome::Session {
        uri: canonical.to_string(),
        json_payload: payload.to_string(),
        last_seq,
        status: healthy_str.to_string(),
    })
}

#[cfg(feature = "port_forward")]
async fn read_forward<FR, Sub>(
    forwards: &FR,
    subscribers: &Sub,
    parsed: &ParsedUri,
    canonical: &str,
) -> Result<ReadResourceOutcome, DomainError>
where
    FR: ForwardRepository + Send + Sync,
    Sub: SubscriberRegistryPort,
{
    let id = ForwardId::new(parsed.id.clone());
    let entity = forwards
        .get(&id)
        .await?
        .ok_or_else(|| DomainError::ForwardNotFound(id.clone()))?;
    let last_seq = subscribers.current_seq(parsed.kind, &parsed.id);
    let payload = json!({
        "forward_id": entity.id.as_str(),
        "session_id": entity.session_id.as_str(),
        "local_port": entity.local_port,
        "remote_host": entity.remote_host,
        "remote_port": entity.remote_port,
        "started_at": entity.started_at,
        "status": entity.status.to_string(),
        "last_seq": last_seq,
    });
    Ok(ReadResourceOutcome::Forward {
        uri: canonical.to_string(),
        json_payload: payload.to_string(),
        last_seq,
        status: entity.status.to_string(),
    })
}

fn resolve_start_offset<Sub: SubscriberRegistryPort>(
    cursor: CursorMode,
    subscribers: &Sub,
    peer_id: &PeerId,
    canonical: &str,
    buffer_len: u64,
) -> u64 {
    match cursor {
        CursorMode::Snapshot => 0,
        CursorMode::Auto => subscribers
            .peer_byte_cursor(peer_id, canonical)
            .min(buffer_len),
        CursorMode::At(n) => n.min(buffer_len),
    }
}

fn slice_from(buffer: &[u8], start: u64) -> &[u8] {
    let len = u64_from_usize(buffer.len());
    if start >= len {
        return &[];
    }
    let start_usize = usize::try_from(start).unwrap_or(usize::MAX);
    &buffer[start_usize..]
}

fn u64_from_usize(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive coverage of the dispatcher requires many small fixtures"
)]
mod tests {
    use super::{
        CursorMode, ParseError, ReadResourceOutcome, ReadResourceRequest, ReadResourceUseCase,
        canonical_uri, parse_uri,
    };
    use crate::adapters::repo::dashmap::command::DashMapCommandRepo;
    use crate::adapters::repo::dashmap::session::DashMapSessionRepo;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::adapters::repo::dashmap::transfer::DashMapTransferRepo;
    use crate::domain::command::CommandEntity;
    use crate::domain::error::DomainError;
    use crate::domain::identity::Address;
    use crate::domain::ids::{CommandId, PeerId, SessionId, ShellId, TransferId};
    use crate::domain::session::SessionEntity;
    use crate::domain::shell::{ShellEntity, ShellTerminal};
    use crate::domain::transfer::{TransferDirection, TransferEntity};
    use crate::ports::command_repo::CommandRepository;
    use crate::ports::output_stream::{OutputSnapshot, OutputStreamPort};
    use crate::ports::session_repo::SessionRepository;
    use crate::ports::shell_repo::ShellRepository;
    use crate::ports::subscriber_registry::{
        ResourceKind, SubscriberRegistryPort, SubscriberSnapshot,
    };
    use crate::ports::transfer_repo::TransferRepository;
    use bytes::Bytes;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    #[cfg(feature = "port_forward")]
    use crate::adapters::repo::dashmap::forward::DashMapForwardRepo;
    #[cfg(feature = "port_forward")]
    use crate::domain::forward::ForwardEntity;
    #[cfg(feature = "port_forward")]
    use crate::domain::ids::ForwardId;
    #[cfg(feature = "port_forward")]
    use crate::ports::forward_repo::ForwardRepository;

    #[derive(Debug, Default)]
    struct StubOutput {
        shell: Mutex<Option<OutputSnapshot>>,
        command: Mutex<Option<OutputSnapshot>>,
    }

    impl StubOutput {
        fn set_shell(&self, snap: OutputSnapshot) {
            if let Ok(mut g) = self.shell.lock() {
                *g = Some(snap);
            }
        }

        fn set_command(&self, snap: OutputSnapshot) {
            if let Ok(mut g) = self.command.lock() {
                *g = Some(snap);
            }
        }
    }

    impl OutputStreamPort for StubOutput {
        async fn snapshot_command(&self, id: &CommandId) -> Result<OutputSnapshot, DomainError> {
            self.command
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .ok_or_else(|| DomainError::CommandNotFound(id.clone()))
        }

        async fn snapshot_shell(&self, id: &ShellId) -> Result<OutputSnapshot, DomainError> {
            self.shell
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .ok_or_else(|| DomainError::ShellNotFound(id.clone()))
        }
    }

    #[derive(Debug, Default)]
    struct StubRegistry {
        cursors: dashmap::DashMap<(PeerId, String), AtomicU64>,
    }

    impl SubscriberRegistryPort for StubRegistry {
        fn next_seq(&self, _kind: ResourceKind, _id: &str) -> u64 {
            0
        }
        fn current_seq(&self, _kind: ResourceKind, _id: &str) -> u64 {
            7
        }
        fn poke(&self, _k: ResourceKind, _id: &str) {}
        fn compensate_truncation(&self, _uri: &str, _bytes: u64) {}
        fn snapshot_subscribers(&self, _uri: &str) -> Vec<SubscriberSnapshot> {
            Vec::new()
        }
        fn peer_byte_cursor(&self, peer_id: &PeerId, uri: &str) -> u64 {
            self.cursors
                .get(&(peer_id.clone(), uri.to_string()))
                .map_or(0, |entry| entry.load(Ordering::Relaxed))
        }
        fn advance_peer_byte_cursor(&self, peer_id: &PeerId, uri: &str, target: u64) -> u64 {
            let entry = self
                .cursors
                .entry((peer_id.clone(), uri.to_string()))
                .or_default();
            entry.fetch_max(target, Ordering::Relaxed);
            entry.load(Ordering::Relaxed)
        }
        fn gc_closed_peers(&self) -> usize {
            0
        }
    }

    #[cfg(feature = "port_forward")]
    type UseCaseUnderTest = ReadResourceUseCase<
        DashMapShellRepo,
        DashMapCommandRepo,
        DashMapTransferRepo,
        DashMapSessionRepo,
        DashMapForwardRepo,
        StubOutput,
        StubRegistry,
    >;

    #[cfg(not(feature = "port_forward"))]
    type UseCaseUnderTest = ReadResourceUseCase<
        DashMapShellRepo,
        DashMapCommandRepo,
        DashMapTransferRepo,
        DashMapSessionRepo,
        StubOutput,
        StubRegistry,
    >;

    struct Harness {
        uc: UseCaseUnderTest,
        shells: Arc<DashMapShellRepo>,
        commands: Arc<DashMapCommandRepo>,
        transfers: Arc<DashMapTransferRepo>,
        sessions: Arc<DashMapSessionRepo>,
        #[cfg(feature = "port_forward")]
        forwards: Arc<DashMapForwardRepo>,
        output: Arc<StubOutput>,
        subscribers: Arc<StubRegistry>,
    }

    fn build_harness() -> Harness {
        let shells = Arc::new(DashMapShellRepo::new());
        let commands = Arc::new(DashMapCommandRepo::new());
        let transfers = Arc::new(DashMapTransferRepo::new());
        let sessions = Arc::new(DashMapSessionRepo::new());
        #[cfg(feature = "port_forward")]
        let forwards = Arc::new(DashMapForwardRepo::new());
        let output = Arc::new(StubOutput::default());
        let subscribers = Arc::new(StubRegistry::default());
        #[cfg(feature = "port_forward")]
        let uc = ReadResourceUseCase::new(
            Arc::clone(&shells),
            Arc::clone(&commands),
            Arc::clone(&transfers),
            Arc::clone(&sessions),
            Arc::clone(&forwards),
            Arc::clone(&output),
            Arc::clone(&subscribers),
        );
        #[cfg(not(feature = "port_forward"))]
        let uc = ReadResourceUseCase::new(
            Arc::clone(&shells),
            Arc::clone(&commands),
            Arc::clone(&transfers),
            Arc::clone(&sessions),
            Arc::clone(&output),
            Arc::clone(&subscribers),
        );
        Harness {
            uc,
            shells,
            commands,
            transfers,
            sessions,
            #[cfg(feature = "port_forward")]
            forwards,
            output,
            subscribers,
        }
    }

    fn sample_address() -> Address {
        Address::new("h.example.com".to_string(), 22).expect("address")
    }

    async fn seed_shell(repo: &DashMapShellRepo, id: &str, sid: &str) {
        repo.insert(ShellEntity::new(
            ShellId::new(id.to_string()),
            SessionId::new(sid.to_string()),
            ShellTerminal::new("xterm".to_string(), 80, 24),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            Duration::from_secs(60),
            10 * 1024 * 1024,
        ))
        .await
        .expect("seed shell");
    }

    async fn seed_command(repo: &DashMapCommandRepo, id: &str, sid: &str, cmd: &str) {
        repo.insert(CommandEntity::new(
            CommandId::new(id.to_string()),
            SessionId::new(sid.to_string()),
            cmd.to_string(),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        ))
        .await
        .expect("seed command");
    }

    async fn seed_transfer(repo: &DashMapTransferRepo, id: &str, sid: &str) {
        repo.insert(TransferEntity::new(
            TransferId::new(id.to_string()),
            SessionId::new(sid.to_string()),
            TransferDirection::Upload,
            "/tmp/x".to_string(),
            "/r/x".to_string(),
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            1024,
        ))
        .await
        .expect("seed transfer");
    }

    async fn seed_session(repo: &DashMapSessionRepo, id: &str, healthy: Option<bool>) {
        repo.insert(SessionEntity {
            id: SessionId::new(id.to_string()),
            name: None,
            agent_id: None,
            address: sample_address(),
            username: "alice".to_string(),
            connected_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            default_timeout: Duration::from_secs(30),
            retry_attempts: 0,
            compression_enabled: true,
            last_health_check: None,
            healthy,
        })
        .await
        .expect("seed session");
    }

    #[cfg(feature = "port_forward")]
    async fn seed_forward(repo: &DashMapForwardRepo, id: &str, sid: &str) {
        repo.insert(ForwardEntity::new(
            ForwardId::new(id.to_string()),
            SessionId::new(sid.to_string()),
            8080,
            "internal".to_string(),
            80,
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        ))
        .await
        .expect("seed forward");
    }

    fn req(uri: &str, peer: &str) -> ReadResourceRequest {
        ReadResourceRequest {
            uri: uri.to_string(),
            peer_id: PeerId::new(peer.to_string()),
        }
    }

    // ---- parser tests --------------------------------------------------

    #[test]
    fn parse_uri_supports_every_scheme() {
        for kind in [
            ResourceKind::Shell,
            ResourceKind::Command,
            ResourceKind::Transfer,
            ResourceKind::Session,
            ResourceKind::Forward,
        ] {
            let uri = canonical_uri(kind, "abc");
            let parsed = parse_uri(&uri).expect("parse");
            assert_eq!(parsed.kind, kind);
            assert_eq!(parsed.id, "abc");
            assert_eq!(parsed.cursor, CursorMode::Snapshot);
        }
    }

    #[test]
    fn parse_uri_rejects_bad_scheme_with_diagnostic() {
        let err = parse_uri("ftp://x/output").expect_err("must error");
        match err {
            ParseError::BadScheme(scheme) => assert_eq!(scheme, "ftp"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_uri_rejects_missing_id() {
        assert!(matches!(
            parse_uri("shell:///output"),
            Err(ParseError::MissingId)
        ));
    }

    #[test]
    fn parse_uri_rejects_bad_subpath() {
        let err = parse_uri("shell://abc/progress").expect_err("must error");
        match err {
            ParseError::BadSubPath { expected, got } => {
                assert_eq!(expected, "output");
                assert_eq!(got, "progress");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_uri_rejects_bad_cursor() {
        let err = parse_uri("shell://abc/output?cursor=banana").expect_err("must error");
        match err {
            ParseError::BadCursor(value) => assert_eq!(value, "banana"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    // ---- happy-path dispatcher ----------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn read_shell_returns_buffer_slice_and_advances_cursor() {
        let h = build_harness();
        seed_shell(&h.shells, "sh-1", "s-1").await;
        h.output.set_shell(OutputSnapshot {
            byte_cursor: 0,
            last_seq: 3,
            stdout: Bytes::from_static(b"hello world"),
            stderr: Bytes::new(),
        });
        let outcome =
            h.uc.execute(req("shell://sh-1/output", "peer-1"))
                .await
                .expect("execute");
        match outcome {
            ReadResourceOutcome::Shell {
                uri,
                data,
                cursor,
                buffer_size,
                last_seq,
                status,
            } => {
                assert_eq!(uri, "shell://sh-1/output");
                assert_eq!(data.as_ref(), b"hello world");
                assert_eq!(cursor, 11);
                assert_eq!(buffer_size, 11);
                assert_eq!(last_seq, 3);
                assert_eq!(status, "open");
            }
            other => panic!("expected Shell, got {other:?}"),
        }
        // Cursor was bumped to 11 in the registry.
        assert_eq!(
            h.subscribers
                .peer_byte_cursor(&PeerId::new("peer-1".to_string()), "shell://sh-1/output"),
            11
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_shell_with_auto_cursor_returns_only_fresh_bytes() {
        let h = build_harness();
        seed_shell(&h.shells, "sh-1", "s-1").await;
        h.output.set_shell(OutputSnapshot {
            byte_cursor: 0,
            last_seq: 1,
            stdout: Bytes::from_static(b"hello world"),
            stderr: Bytes::new(),
        });
        // First read consumes everything and bumps cursor to 11.
        let _ =
            h.uc.execute(req("shell://sh-1/output?cursor=auto", "peer-1"))
                .await
                .expect("first");
        // Producer adds " more" -> total 16 bytes.
        h.output.set_shell(OutputSnapshot {
            byte_cursor: 0,
            last_seq: 2,
            stdout: Bytes::from_static(b"hello world more"),
            stderr: Bytes::new(),
        });
        let outcome =
            h.uc.execute(req("shell://sh-1/output?cursor=auto", "peer-1"))
                .await
                .expect("second");
        match outcome {
            ReadResourceOutcome::Shell { data, cursor, .. } => {
                assert_eq!(data.as_ref(), b" more");
                assert_eq!(cursor, 16);
            }
            other => panic!("expected Shell, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_shell_with_explicit_cursor_overrides_registry_state() {
        let h = build_harness();
        seed_shell(&h.shells, "sh-1", "s-1").await;
        h.output.set_shell(OutputSnapshot {
            byte_cursor: 0,
            last_seq: 1,
            stdout: Bytes::from_static(b"hello world"),
            stderr: Bytes::new(),
        });
        let outcome =
            h.uc.execute(req("shell://sh-1/output?cursor=6", "peer-1"))
                .await
                .expect("execute");
        match outcome {
            ReadResourceOutcome::Shell { data, cursor, .. } => {
                assert_eq!(data.as_ref(), b"world");
                assert_eq!(cursor, 11);
            }
            other => panic!("expected Shell, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_shell_unknown_id_returns_shell_not_found() {
        let h = build_harness();
        let err =
            h.uc.execute(req("shell://ghost/output", "peer-1"))
                .await
                .expect_err("must fail");
        match err {
            DomainError::ShellNotFound(id) => assert_eq!(id.as_str(), "ghost"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_command_concatenates_stdout_and_stderr_lengths() {
        let h = build_harness();
        seed_command(&h.commands, "c-1", "s-1", "echo hi").await;
        h.output.set_command(OutputSnapshot {
            byte_cursor: 0,
            last_seq: 5,
            stdout: Bytes::from_static(b"hi"),
            stderr: Bytes::from_static(b"err"),
        });
        let outcome =
            h.uc.execute(req("command://c-1/output", "peer-1"))
                .await
                .expect("execute");
        match outcome {
            ReadResourceOutcome::Command {
                stdout,
                stderr,
                buffer_size,
                cursor,
                last_seq,
                ..
            } => {
                assert_eq!(stdout.as_ref(), b"hi");
                assert_eq!(stderr.as_ref(), b"err");
                assert_eq!(buffer_size, 5);
                assert_eq!(cursor, 5);
                assert_eq!(last_seq, 5);
            }
            other => panic!("expected Command, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_command_unknown_id_returns_command_not_found() {
        let h = build_harness();
        let err =
            h.uc.execute(req("command://ghost/output", "peer-1"))
                .await
                .expect_err("must fail");
        match err {
            DomainError::CommandNotFound(id) => assert_eq!(id.as_str(), "ghost"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_transfer_renders_json_payload() {
        let h = build_harness();
        seed_transfer(&h.transfers, "t-1", "s-1").await;
        let outcome =
            h.uc.execute(req("transfer://t-1/progress", "peer-1"))
                .await
                .expect("execute");
        match outcome {
            ReadResourceOutcome::Transfer {
                json_payload,
                last_seq,
                status,
                ..
            } => {
                assert!(json_payload.contains("t-1"));
                assert!(json_payload.contains("upload"));
                assert_eq!(last_seq, 7);
                assert_eq!(status, "running");
            }
            other => panic!("expected Transfer, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_transfer_unknown_id_returns_transfer_not_found() {
        let h = build_harness();
        let err =
            h.uc.execute(req("transfer://ghost/progress", "peer-1"))
                .await
                .expect_err("must fail");
        match err {
            DomainError::TransferNotFound(id) => assert_eq!(id.as_str(), "ghost"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_session_renders_json_payload_with_health_status() {
        let h = build_harness();
        seed_session(&h.sessions, "s-1", Some(true)).await;
        let outcome =
            h.uc.execute(req("session://s-1/health", "peer-1"))
                .await
                .expect("execute");
        match outcome {
            ReadResourceOutcome::Session {
                json_payload,
                status,
                ..
            } => {
                assert!(json_payload.contains("s-1"));
                assert!(json_payload.contains("alice"));
                assert_eq!(status, "healthy");
            }
            other => panic!("expected Session, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_session_never_probed_renders_unknown_status() {
        let h = build_harness();
        seed_session(&h.sessions, "fresh", None).await;
        let outcome =
            h.uc.execute(req("session://fresh/health", "peer-1"))
                .await
                .expect("execute");
        match outcome {
            ReadResourceOutcome::Session { status, .. } => assert_eq!(status, "unknown"),
            other => panic!("expected Session, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_session_unknown_id_returns_session_not_found() {
        let h = build_harness();
        let err =
            h.uc.execute(req("session://ghost/health", "peer-1"))
                .await
                .expect_err("must fail");
        match err {
            DomainError::SessionNotFound(id) => assert_eq!(id.as_str(), "ghost"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[cfg(feature = "port_forward")]
    #[tokio::test(flavor = "multi_thread")]
    async fn read_forward_renders_json_payload() {
        let h = build_harness();
        seed_forward(&h.forwards, "f-1", "s-1").await;
        let outcome =
            h.uc.execute(req("forward://f-1/events", "peer-1"))
                .await
                .expect("execute");
        match outcome {
            ReadResourceOutcome::Forward {
                json_payload,
                status,
                ..
            } => {
                assert!(json_payload.contains("f-1"));
                assert!(json_payload.contains("8080"));
                assert_eq!(status, "running");
            }
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    #[cfg(feature = "port_forward")]
    #[tokio::test(flavor = "multi_thread")]
    async fn read_forward_unknown_id_returns_forward_not_found() {
        let h = build_harness();
        let err =
            h.uc.execute(req("forward://ghost/events", "peer-1"))
                .await
                .expect_err("must fail");
        match err {
            DomainError::ForwardNotFound(id) => assert_eq!(id.as_str(), "ghost"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn malformed_uri_returns_invalid_argument() {
        let h = build_harness();
        let err =
            h.uc.execute(req("ftp://x/output", "peer-1"))
                .await
                .expect_err("must fail");
        match err {
            DomainError::InvalidArgument(msg) => assert!(msg.contains("ftp")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shell_cursor_advance_persists_across_consecutive_reads() {
        let h = build_harness();
        seed_shell(&h.shells, "sh-1", "s-1").await;
        h.output.set_shell(OutputSnapshot {
            byte_cursor: 0,
            last_seq: 1,
            stdout: Bytes::from_static(b"abcdef"),
            stderr: Bytes::new(),
        });
        let _ =
            h.uc.execute(req("shell://sh-1/output?cursor=auto", "peer-1"))
                .await
                .expect("first");
        // Same buffer, no producer activity. Auto cursor must yield empty.
        let outcome =
            h.uc.execute(req("shell://sh-1/output?cursor=auto", "peer-1"))
                .await
                .expect("second");
        match outcome {
            ReadResourceOutcome::Shell { data, .. } => assert!(data.is_empty()),
            other => panic!("expected Shell, got {other:?}"),
        }
    }
}
