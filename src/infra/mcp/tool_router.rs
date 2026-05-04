//! `#[tool_router]` impl + `ServerHandler` impl for [`super::server::McpSshServer`].
//!
//! 18 tool fns + 4 resource methods all delegate to the use case container
//! sitting on [`super::server::McpSshServer::use_cases`]. H17 wires the
//! v3-equivalent block-style markdown bodies through the
//! [`super::render`] helpers; the v4 path is now end-to-end functional.
//!
//! ## Generic shape
//!
//! The impl block specialises on the production-shaped
//! [`crate::composition::UseCases`] type. Every generic adapter
//! parameter is forwarded as-is; the test harness (H18) builds a
//! `UseCases<Fakes...>` instance with the exact same shape.

use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::common::schema_for_type;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, GetPromptRequestParams, GetPromptResult, Icon, Implementation,
    ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
    ProtocolVersion, ReadResourceRequestParams, ReadResourceResult, ServerCapabilities, ServerInfo,
    SubscribeRequestParams, UnsubscribeRequestParams,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, tool, tool_handler, tool_router};
use tokio::time::{Instant, MissedTickBehavior, interval};

use crate::adapters::lifecycle::leak_watcher::LeakWatcher;
use crate::application::cancel_command::CancelCommandRequest;
use crate::application::close_shell::CloseShellRequest;
use crate::application::connect_session::{ConnectOutcome, ConnectRequest, ConnectSessionUseCase};
use crate::application::disconnect_agent::DisconnectAgentRequest;
use crate::application::disconnect_session::{DisconnectRequest, DisconnectSessionUseCase};
use crate::application::download_file::DownloadRequest;
use crate::application::execute_command::{ExecuteCommandUseCase, ExecuteRequest};
#[cfg(feature = "port_forward")]
use crate::application::forward_port::ForwardPortRequest;
use crate::application::get_command_output::{
    GetCommandOutputRequest, GetCommandOutputResult, GetCommandOutputUseCase,
};
use crate::application::get_transfer_progress::{
    GetTransferProgressRequest, GetTransferProgressResult, GetTransferProgressUseCase,
};
use crate::application::list_commands::ListCommandsRequest;
use crate::application::list_sessions::ListSessionsRequest;
use crate::application::open_shell::OpenShellRequest;
use crate::application::read_shell::ReadShellRequest;
use crate::application::send_key::SendKeyRequest;
use crate::application::subscription_admin::{
    DaemonStatsUseCase, ListSubsRequest, ListSubsUseCase, PauseSubUseCase, ReplayRequest,
    ReplaySubUseCase, ResumeSubUseCase, SetFilterRequest, SetFilterUseCase, SubStatsRequest,
    SubStatsUseCase, SubToggleRequest, SubscribeRequest, SubscribeUseCase, UnsubscribeRequest,
    UnsubscribeUseCase,
};
use crate::application::upload_file::UploadRequest;
use crate::application::wait_for_pattern::{
    WaitForPatternOutcome, WaitForPatternRequest, WaitForPatternUseCase,
};
use crate::application::write_shell::WriteShellRequest;
use crate::composition::UseCases;
use crate::domain::command::CommandStatus;
use crate::domain::error::DomainError;
use crate::domain::identity::{Address, Credentials};
use crate::domain::ids::{AgentId, CommandId, SessionId, ShellId, TransferId};
use crate::domain::keys::KeyModifiers;
use crate::domain::lifecycle::LifecyclePolicy;
use crate::domain::policy::ReusePolicy as DomainReusePolicy;
use crate::domain::subscription::{FilterRule, LagPolicy, SubId, SubscriptionLifetime};
use crate::infra::mcp::error_detail;
use crate::infra::mcp::helpers::error::{format_error, format_error_structured};
use crate::infra::mcp::helpers::structured::{error_text_and_structured, ok_text_and_structured};
use crate::infra::mcp::idempotency::{
    IDEMPOTENCY_KEY_MAX_BYTES, IdempotencyCache, IdempotencyOutcome, KeyOutcome,
    extract_idempotency_key, fingerprint_args, replay,
};
use crate::infra::mcp::leak_warn_bridge::{LeakWarnBridgeHandle, spawn_bridge};
use crate::infra::mcp::progress::{COMMAND_TICK, ProgressEmitter, WAIT_FOR_TICK};
use crate::infra::mcp::prompts;
use crate::infra::mcp::render;
use crate::infra::mcp::resource_templates;
#[cfg(feature = "port_forward")]
use crate::infra::mcp::results::SshForwardResult;
use crate::infra::mcp::results::{
    SshCancelCommandResult, SshConnectResult, SshDisconnectAgentResult, SshDisconnectManyResult,
    SshDisconnectResult, SshDownloadResult, SshExecuteBatchResult, SshExecuteResult,
    SshGetCommandOutputResult, SshGetTransferProgressResult, SshListCommandsResult,
    SshListSessionsResult, SshRunResult, SshShellCloseResult, SshShellOpenResult,
    SshShellReadResult, SshShellSendKeyResult, SshShellWaitForResult, SshShellWriteResult,
    SshUploadResult,
};
use crate::infra::mcp::suggestions::{closest_ids, render_closest_matches};
use crate::ports::auth_strategy::AuthStrategyPort;
use crate::ports::clock::ClockPort;
use crate::ports::command_repo::CommandRepository;
use crate::ports::config::ConfigPort;
#[cfg(feature = "port_forward")]
use crate::ports::forward_repo::ForwardRepository;
use crate::ports::id_generator::IdGeneratorPort;
use crate::ports::notifier::NotifierPort;
use crate::ports::output_stream::OutputStreamPort;
use crate::ports::session_repo::SessionRepository;
use crate::ports::sftp_client::SftpClientPort;
use crate::ports::shell_repo::ShellRepository;
use crate::ports::ssh_client::SshClientPort;
use crate::ports::subscriber_registry::{SubscriberRegistryAsync, SubscriberRegistryPort};
use crate::ports::transfer_repo::TransferRepository;

use super::args::connection::{
    SshConnectArgs, SshDisconnectAgentArgs, SshDisconnectArgs, SshDisconnectManyArgs,
    SshListSessionsArgs,
};
use super::args::execute::{
    SshCancelCommandArgs, SshExecuteArgs, SshExecuteBatchArgs, SshGetCommandOutputArgs,
    SshListCommandsArgs, SshRunArgs,
};
#[cfg(feature = "port_forward")]
use super::args::forward::SshForwardArgs;
use super::args::sftp::{SshDownloadArgs, SshGetTransferProgressArgs, SshUploadArgs};
use super::args::shell::{
    SshShellCloseArgs, SshShellOpenArgs, SshShellReadArgs, SshShellSendKeyArgs,
    SshShellWaitForArgs, SshShellWriteArgs,
};
use super::args::subscription::{
    LifetimeKind, SshDaemonStatsArgs, SshSubFilterArgs, SshSubListArgs, SshSubPauseArgs,
    SshSubReplayArgs, SshSubResumeArgs, SshSubStatsArgs, SshSubscribeArgs, SshUnsubscribeArgs,
};
use super::peer_handle::PeerTable;
use super::resource_handlers;
use super::server::McpSshServer;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Build a `CallToolResult::error` body using the v3 standardized
/// `TOOL: ERROR / REASON: [CODE] message` format and the v4.7 structured
/// JSON twin. The mapping picks a stable code per [`DomainError`]
/// variant so the LLM can branch on it.
pub(crate) fn render_tool_error(tool: &str, err: &DomainError) -> CallToolResult {
    let (code, reason, detail) = classify_error(err);
    let merged = error_detail::with_detail(code, detail.as_deref());
    let body = format_error(tool, code, &reason, merged.as_deref());
    let structured = format_error_structured(tool, code, &reason, merged.as_deref());
    error_text_and_structured(body, structured)
}

/// Decide whether `err` triggers the v4.7-step6 closest-match
/// suggestion path. Pure helper: the dispatcher uses this to skip the
/// extra repository scan for non-NOT_FOUND errors.
const fn is_not_found(err: &DomainError) -> bool {
    matches!(
        err,
        DomainError::SessionNotFound(_)
            | DomainError::ShellNotFound(_)
            | DomainError::CommandNotFound(_)
            | DomainError::TransferNotFound(_)
            | DomainError::ForwardNotFound(_)
    )
}

/// Render an error response, augmenting `NOT_FOUND` with the closest
/// matches from the lister. Non-`NOT_FOUND` falls through to the legacy
/// [`render_tool_error`] without paying the repository scan.
pub(crate) async fn render_tool_error_smart(
    tool: &str,
    err: &DomainError,
    lister: &dyn IdLister,
) -> CallToolResult {
    if is_not_found(err) {
        render_tool_error_with_suggestions(tool, err, lister).await
    } else {
        render_tool_error(tool, err)
    }
}

/// Variant of [`render_tool_error`] that augments a `NOT_FOUND` error
/// detail with the closest-match suggestions returned by the matching
/// repository. Falls back to [`render_tool_error`] when the lister has
/// no live ids.
pub(crate) async fn render_tool_error_with_suggestions(
    tool: &str,
    err: &DomainError,
    lister: &dyn IdLister,
) -> CallToolResult {
    let (code, reason, detail) = classify_error(err);
    let candidates = collect_suggestions(err, lister).await;
    // Compose: <static cure>; <classify_error detail>; <closest matches>.
    // Each segment is inserted only when present; missing segments do
    // not produce dangling separators.
    let merged_static = error_detail::with_detail(code, detail.as_deref());
    let detail_with_hints = match (
        merged_static.as_deref(),
        render_closest_matches(&candidates),
    ) {
        (Some(d), Some(hint)) => Some(format!("{d}; {hint}")),
        (Some(d), None) => Some(d.to_string()),
        (None, Some(hint)) => Some(hint),
        (None, None) => None,
    };
    let body = format_error(tool, code, &reason, detail_with_hints.as_deref());
    let structured = format_error_structured(tool, code, &reason, detail_with_hints.as_deref());
    error_text_and_structured(body, structured)
}

/// Top-3 closest matches surfaced on every `*_NOT_FOUND` variant.
/// Returns an empty vector for non-`NOT_FOUND` errors.
const SUGGEST_TOP_N: usize = 3;

async fn collect_suggestions(err: &DomainError, lister: &dyn IdLister) -> Vec<String> {
    if let Some(target) = lookup_target(err) {
        return suggest_for(target, lister).await;
    }
    Vec::new()
}

/// Variants that benefit from a closest-match suggestion.
enum LookupTarget<'a> {
    Session(&'a str),
    Shell(&'a str),
    Command(&'a str),
    Transfer(&'a str),
    Forward(&'a str),
}

/// Pick the lookup arm — exhaustive match keeps every existing
/// `DomainError` variant under compile-time scrutiny without inflating
/// `collect_suggestions` past the 30-line threshold.
fn lookup_target(err: &DomainError) -> Option<LookupTarget<'_>> {
    match err {
        DomainError::SessionNotFound(id) => Some(LookupTarget::Session(id.as_str())),
        DomainError::ShellNotFound(id) => Some(LookupTarget::Shell(id.as_str())),
        DomainError::CommandNotFound(id) => Some(LookupTarget::Command(id.as_str())),
        DomainError::TransferNotFound(id) => Some(LookupTarget::Transfer(id.as_str())),
        DomainError::ForwardNotFound(id) => Some(LookupTarget::Forward(id.as_str())),
        DomainError::InvalidArgument(_)
        | DomainError::Auth(_)
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
        | DomainError::ResourceGone(_)
        | DomainError::LifecycleStateConflict { .. }
        | DomainError::SessionRefcountUnderflow(_)
        | DomainError::SubNotFound(_)
        | DomainError::MaxSubsPerUriExceeded { .. }
        | DomainError::MaxSubsTotalExceeded { .. }
        | DomainError::LaneBufferFull { .. }
        | DomainError::LagDetected { .. }
        | DomainError::MuxBackpressure
        | DomainError::InvalidLagPolicy(_)
        | DomainError::InvalidLifetime(_) => None,
    }
}

/// Resolve a [`LookupTarget`] into the closest-id list.
async fn suggest_for(target: LookupTarget<'_>, lister: &dyn IdLister) -> Vec<String> {
    match target {
        LookupTarget::Session(id) => closest_ids(id, lister.list_sessions().await, SUGGEST_TOP_N),
        LookupTarget::Shell(id) => closest_ids(id, lister.list_shells().await, SUGGEST_TOP_N),
        LookupTarget::Command(id) => closest_ids(id, lister.list_commands().await, SUGGEST_TOP_N),
        LookupTarget::Transfer(id) => closest_ids(id, lister.list_transfers().await, SUGGEST_TOP_N),
        LookupTarget::Forward(id) => collect_forward_suggestions(id, lister).await,
    }
}

#[cfg(feature = "port_forward")]
async fn collect_forward_suggestions(target: &str, lister: &dyn IdLister) -> Vec<String> {
    closest_ids(target, lister.list_forwards().await, SUGGEST_TOP_N)
}

#[cfg(not(feature = "port_forward"))]
async fn collect_forward_suggestions(_target: &str, _lister: &dyn IdLister) -> Vec<String> {
    Vec::new()
}

/// Box-erased async ids future. Used by [`IdLister`] so the trait
/// stays dyn-compatible without inheriting the more elaborate
/// `trait-variant` Send-bounded variant.
pub type IdFuture<'a> = Pin<Box<dyn Future<Output = Vec<String>> + Send + 'a>>;

/// Trait implemented by an adapter handle so the tool router can
/// enumerate live `SESSION` / `SHELL` / `COMMAND` / `TRANSFER` /
/// `FORWARD` ids without depending on the concrete repository types.
///
/// Used purely for the v4.7-step6 closest-match suggestions on
/// `NOT_FOUND` errors; never for primary control flow. The trait surface
/// is intentionally dyn-safe — the dispatcher holds `&dyn IdLister` so
/// repo type churn stays inside the composition root. Each method
/// returns a boxed future because async fn in trait is not yet dyn-safe
/// in stable Rust.
pub trait IdLister: Send + Sync {
    /// All live `SESSION_ID` values.
    fn list_sessions(&self) -> IdFuture<'_>;
    /// All live `SHELL_ID` values.
    fn list_shells(&self) -> IdFuture<'_>;
    /// All live `COMMAND_ID` values.
    fn list_commands(&self) -> IdFuture<'_>;
    /// All live `TRANSFER_ID` values.
    fn list_transfers(&self) -> IdFuture<'_>;
    /// All live `FORWARD_ID` values (feature-gated).
    #[cfg(feature = "port_forward")]
    fn list_forwards(&self) -> IdFuture<'_>;
}

/// [`IdLister`] that always returns empty lists. Used when the inbound
/// layer was built without repository wiring (e.g. a unit test that
/// only exercises the rendering path).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopIdLister;

impl IdLister for NoopIdLister {
    fn list_sessions(&self) -> IdFuture<'_> {
        Box::pin(async { Vec::new() })
    }
    fn list_shells(&self) -> IdFuture<'_> {
        Box::pin(async { Vec::new() })
    }
    fn list_commands(&self) -> IdFuture<'_> {
        Box::pin(async { Vec::new() })
    }
    fn list_transfers(&self) -> IdFuture<'_> {
        Box::pin(async { Vec::new() })
    }
    #[cfg(feature = "port_forward")]
    fn list_forwards(&self) -> IdFuture<'_> {
        Box::pin(async { Vec::new() })
    }
}

/// Static empty lister surfaced when the inbound layer has not been
/// configured with a richer adapter. Useful for the `default()` /
/// constructor variants used in tests.
#[must_use]
pub fn noop_id_lister() -> Arc<dyn IdLister> {
    Arc::new(NoopIdLister)
}

/// Drive a mutating tool call under the v4.7-step5 idempotency cache.
///
/// When the request supplies `_meta.idempotency_key`:
///  * Lookup the `(tool, key)` entry comparing the supplied
///    `args_fingerprint`. On a [`IdempotencyOutcome::Hit`] replay the
///    cached response verbatim. On a [`IdempotencyOutcome::Mismatch`]
///    surface `IDEMPOTENCY_KEY_MISMATCH` and skip the use case.
///  * On a [`IdempotencyOutcome::Miss`] drive the use case `f` and
///    cache the rendered response if it was a success path. Errors are
///    intentionally NOT cached (the LLM should be able to retry after
///    fixing the input).
///
/// When the request omits the key the use case is driven directly.
/// When the key is too long, return `IDEMPOTENCY_KEY_TOO_LONG` as an
/// `INVALID_ARGUMENT` error and skip the use case.
///
/// Callers compute `args_fingerprint` via
/// [`crate::infra::mcp::idempotency::fingerprint_args`] BEFORE
/// invoking this helper so the per-tool args struct can be moved into
/// the use case closure without competing with the borrow used to
/// build the digest.
async fn with_idempotency<F, Fut>(
    cache: &IdempotencyCache,
    ctx: &RequestContext<RoleServer>,
    tool: &str,
    args_fingerprint: String,
    f: F,
) -> Result<CallToolResult, McpError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<CallToolResult, McpError>>,
{
    with_idempotency_keyed(
        cache,
        extract_idempotency_key(ctx),
        tool,
        args_fingerprint,
        f,
    )
    .await
}

/// Inner driver shared with the unit-test path. Splitting this out lets
/// the regression test for v4.7.1 Bug #3 (the "replay is a pure
/// passthrough" invariant) drive the same code path the inbound
/// handlers use without constructing a full
/// [`RequestContext<RoleServer>`] (the rmcp `Peer::new` constructor is
/// `pub(crate)`).
async fn with_idempotency_keyed<F, Fut>(
    cache: &IdempotencyCache,
    outcome: KeyOutcome,
    tool: &str,
    args_fingerprint: String,
    f: F,
) -> Result<CallToolResult, McpError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<CallToolResult, McpError>>,
{
    let key = match resolve_key(outcome, tool) {
        KeyResolution::Run => return f().await,
        KeyResolution::Reject(error) => return Ok(error),
        KeyResolution::Use(k) => k,
    };
    match cache.get_with_fingerprint(tool, &key, &args_fingerprint) {
        IdempotencyOutcome::Hit(cached) => return Ok(replay(&cached)),
        IdempotencyOutcome::Mismatch => return Ok(render_mismatch(tool, &key)),
        IdempotencyOutcome::Miss => {}
    }
    let response = f().await?;
    if response.is_error != Some(true) {
        cache_success(cache, tool, &key, args_fingerprint, &response);
    }
    Ok(response)
}

/// Outcome of normalising the inbound idempotency key. Returned by
/// [`resolve_key`] so [`with_idempotency_keyed`] stays under the 30
/// line ceiling without touching public surface.
enum KeyResolution {
    /// `_meta.idempotency_key` is absent — the use case must run
    /// without caching.
    Run,
    /// `_meta.idempotency_key` failed validation — render an
    /// `INVALID_ARGUMENT` response and skip the use case.
    Reject(CallToolResult),
    /// Key validated; carries the trimmed value for the cache lookup.
    Use(String),
}

fn resolve_key(outcome: KeyOutcome, tool: &str) -> KeyResolution {
    match outcome {
        KeyOutcome::Absent => KeyResolution::Run,
        KeyOutcome::TooLong => KeyResolution::Reject(render_tool_error(
            tool,
            &DomainError::InvalidArgument(format!(
                "IDEMPOTENCY_KEY_TOO_LONG: idempotency_key exceeds {IDEMPOTENCY_KEY_MAX_BYTES} bytes"
            )),
        )),
        KeyOutcome::Present(k) => KeyResolution::Use(k),
    }
}

fn render_mismatch(tool: &str, key: &str) -> CallToolResult {
    render_tool_error(
        tool,
        &DomainError::InvalidArgument(format!(
            "IDEMPOTENCY_KEY_MISMATCH: idempotency_key '{key}' was previously used with different arguments"
        )),
    )
}

/// Persist a successful response into the idempotency cache. Pulled
/// out so the [`with_idempotency`] body stays small and so we can swap
/// in a no-op stub later if the rendering shape changes.
fn cache_success(
    cache: &IdempotencyCache,
    tool: &str,
    key: &str,
    fingerprint: String,
    response: &CallToolResult,
) {
    let body = response
        .content
        .first()
        .and_then(|c| c.as_text().map(|t| t.text.clone()))
        .unwrap_or_default();
    let structured = response
        .structured_content
        .clone()
        .unwrap_or(serde_json::Value::Null);
    cache.put_with_fingerprint(tool, key, fingerprint, body, structured);
}

/// Tags surfaced through [`DomainError::InvalidArgument`] messages. Each
/// tag corresponds to a documented v4.5 wire error code that downstream
/// LLMs branch on for recovery logic; tagged emissions prefix the
/// reason with `{TAG}: {message}` so [`extract_tag`] can promote the
/// specific code without adding new [`DomainError`] variants.
const ARG_TAGS: &[&str] = &[
    "EMPTY_PATTERNS",
    "TOO_MANY_PATTERNS",
    "PATTERN_TOO_LONG",
    "MODIFIER_NOT_ALLOWED",
    "INVALID_REPEAT",
    "FEATURE_DISABLED",
];

/// Tags surfaced through [`DomainError::Transport`] messages.
const TRANSPORT_TAGS: &[&str] = &[
    "WRITE_FAILED",
    "CHANNEL_FAILED",
    "COMMAND_FAILED",
    "FORWARD_FAILED",
];

/// Tags surfaced through [`DomainError::Sftp`] messages.
const SFTP_TAGS: &[&str] = &[
    "LOCAL_FILE_ERROR",
    "LOCAL_NOT_FILE",
    "SFTP_OPEN_FAILED",
    "REMOTE_METADATA_ERROR",
];

/// Try to peel a `TAG: message` prefix off `msg` against `allowed`. On a
/// hit returns the static tag and the trimmed remainder; on a miss
/// returns `None` so the caller can fall back to the legacy flat code.
fn extract_tag<'a>(msg: &'a str, allowed: &[&'static str]) -> Option<(&'static str, &'a str)> {
    let (head, rest) = msg.split_once(':')?;
    for &tag in allowed {
        if head == tag {
            return Some((tag, rest.trim_start()));
        }
    }
    None
}

/// Pick a v3-compatible error code, reason and optional detail per
/// [`DomainError`] variant. v4.5 promotes per-site tag prefixes
/// (see [`ARG_TAGS`], [`TRANSPORT_TAGS`], [`SFTP_TAGS`]) to specific wire
/// codes so smaller LLMs can branch on a precise failure class instead
/// of the collapsed `INVALID_ARGUMENT` / `TRANSPORT_ERROR` / `SFTP_ERROR`
/// fallbacks. Untagged messages keep emitting the legacy flat code.
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive match over the 17 DomainError variants plus the v4.5 tag dispatch is naturally long; restructuring to a HashMap or sub-helper hurts readability"
)]
fn classify_error(err: &DomainError) -> (&'static str, String, Option<String>) {
    match err {
        DomainError::InvalidArgument(reason) => {
            if let Some((tag, rest)) = extract_tag(reason, ARG_TAGS) {
                (tag, rest.to_string(), None)
            } else {
                ("INVALID_ARGUMENT", reason.clone(), None)
            }
        }
        DomainError::SessionNotFound(id) => (
            "SESSION_NOT_FOUND",
            "no active SSH session with the given ID".to_string(),
            Some(id.as_str().to_string()),
        ),
        DomainError::ShellNotFound(id) => (
            "SHELL_NOT_FOUND",
            "no active shell with the given ID".to_string(),
            Some(id.as_str().to_string()),
        ),
        DomainError::CommandNotFound(id) => (
            "COMMAND_NOT_FOUND",
            "no async command with the given ID".to_string(),
            Some(id.as_str().to_string()),
        ),
        DomainError::TransferNotFound(id) => (
            "TRANSFER_NOT_FOUND",
            "no transfer with the given ID".to_string(),
            Some(id.as_str().to_string()),
        ),
        DomainError::ForwardNotFound(id) => (
            "FORWARD_NOT_FOUND",
            "no forwarder with the given ID".to_string(),
            Some(id.as_str().to_string()),
        ),
        DomainError::Auth(reason) => ("AUTH_FAILED", reason.to_string(), None),
        DomainError::ConnectFailed(reason) => ("CONNECTION_FAILED", reason.clone(), None),
        DomainError::Transport(reason) => {
            if let Some((tag, rest)) = extract_tag(reason, TRANSPORT_TAGS) {
                (tag, rest.to_string(), None)
            } else {
                ("TRANSPORT_ERROR", reason.clone(), None)
            }
        }
        DomainError::Timeout(reason) => ("TIMEOUT", reason.clone(), None),
        DomainError::Storage(reason) => ("STORAGE_ERROR", reason.clone(), None),
        DomainError::Sftp(reason) => {
            if let Some((tag, rest)) = extract_tag(reason, SFTP_TAGS) {
                (tag, rest.to_string(), None)
            } else {
                ("SFTP_ERROR", reason.clone(), None)
            }
        }
        DomainError::PortInUse(port) => (
            "PORT_IN_USE",
            format!("local port {port} already in use"),
            Some(format!("port={port}")),
        ),
        DomainError::Internal(reason) => ("INTERNAL_ERROR", reason.clone(), None),
        DomainError::MaxCommandsExceeded { limit } => (
            "MAX_COMMANDS_EXCEEDED",
            "maximum running async commands per session reached".to_string(),
            Some(format!("limit={limit}")),
        ),
        DomainError::MaxShellsExceeded { limit } => (
            "MAX_SHELLS_EXCEEDED",
            "maximum shells per session reached".to_string(),
            Some(format!("limit={limit}")),
        ),
        DomainError::MaxTransfersExceeded { limit } => (
            "MAX_TRANSFERS_EXCEEDED",
            "maximum transfers per session reached".to_string(),
            Some(format!("limit={limit}")),
        ),
        DomainError::ResourceGone(uri) => (
            "RESOURCE_GONE",
            "resource has been closed and is no longer observable".to_string(),
            Some(uri.clone()),
        ),
        DomainError::LifecycleStateConflict { current, attempted } => (
            "LIFECYCLE_STATE_CONFLICT",
            format!("cannot apply '{attempted}' while in {current:?}"),
            None,
        ),
        DomainError::SessionRefcountUnderflow(id) => (
            "INTERNAL_ERROR",
            format!("session refcount underflow on {id}"),
            Some(id.as_str().to_string()),
        ),
        DomainError::SubNotFound(sub_id) => (
            "SUB_NOT_FOUND",
            "no subscription with the given sub_id".to_string(),
            Some(sub_id.as_str().to_string()),
        ),
        DomainError::MaxSubsPerUriExceeded { uri, limit } => (
            "MAX_SUBS_PER_URI_EXCEEDED",
            "per-URI subscription cap reached".to_string(),
            Some(format!("uri={uri},limit={limit}")),
        ),
        DomainError::MaxSubsTotalExceeded { limit } => (
            "MAX_SUBS_TOTAL_EXCEEDED",
            "global subscription cap reached".to_string(),
            Some(format!("limit={limit}")),
        ),
        DomainError::LaneBufferFull { sub_id, capacity } => (
            "LANE_BUFFER_FULL",
            "lane mpsc full and policy refused to drop".to_string(),
            Some(format!("sub_id={sub_id},capacity={capacity}")),
        ),
        DomainError::LagDetected { sub_id, dropped } => (
            "LAG_DETECTED",
            "events dropped under lag policy".to_string(),
            Some(format!("sub_id={sub_id},dropped={dropped}")),
        ),
        DomainError::MuxBackpressure => (
            "MUX_BACKPRESSURE",
            "mux outbound writer is blocked".to_string(),
            None,
        ),
        DomainError::InvalidLagPolicy(value) => (
            "INVALID_LAG_POLICY",
            "lag_policy must be one of {block_slow, drop_oldest, drop_newest, snapshot}"
                .to_string(),
            Some(value.clone()),
        ),
        DomainError::InvalidLifetime(value) => (
            "INVALID_LIFETIME",
            "lifetime must be one of {manual, auto_close, lease}".to_string(),
            Some(value.clone()),
        ),
    }
}

/// Convert a string `host[:port]` into a domain [`Address`].
fn parse_address(input: &str) -> Result<Address, DomainError> {
    if let Some((host, port_str)) = input.rsplit_once(':') {
        let port = port_str
            .parse::<u16>()
            .map_err(|e| DomainError::InvalidArgument(format!("invalid port {port_str:?}: {e}")))?;
        Address::new(host.to_string(), port)
            .map_err(|e| DomainError::InvalidArgument(e.to_string()))
    } else {
        Address::with_default_port(input.to_string())
            .map_err(|e| DomainError::InvalidArgument(e.to_string()))
    }
}

/// Spawn the v5 Phase 3 leak-warn bridge when the request supplies a
/// `progressToken` AND the server has a watcher wired. Returns a
/// [`LeakWarnBridgeHandle`] the caller flips on tool exit so stale
/// alerts never bleed into the next call. The handle is a no-op when
/// either side is missing — same shape regardless of wiring state.
fn spawn_leak_warn_bridge_if_wired(
    emitter: &ProgressEmitter,
    watcher: Option<&Arc<LeakWatcher>>,
) -> LeakWarnBridgeHandle {
    if !emitter.is_enabled() {
        return LeakWarnBridgeHandle::noop();
    }
    watcher.map_or_else(LeakWarnBridgeHandle::noop, |w| {
        spawn_bridge(emitter.clone(), w.as_ref())
    })
}

/// Drive a long-running command-output use case under a bounded
/// progress-emission interval.
///
/// Best-effort: the emitter swallows transport errors and the use case
/// future is the source of the user-visible outcome.
///
/// The pump only ticks while `req.wait` is `true` and the emitter is
/// enabled; when either is false the use case is awaited directly so the
/// fast path stays a single `.await`.
async fn drive_with_command_progress<CR, OS>(
    use_case: &GetCommandOutputUseCase<CR, OS>,
    streams: &Arc<OS>,
    req: GetCommandOutputRequest,
    emitter: ProgressEmitter,
) -> Result<GetCommandOutputResult, DomainError>
where
    CR: CommandRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
{
    if !req.wait || !emitter.is_enabled() {
        return use_case.execute(req).await;
    }
    let cmd_id = req.command_id.clone();
    let exec_fut = use_case.execute(req);
    tokio::pin!(exec_fut);
    let mut tick = interval(COMMAND_TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tick.tick().await; // consume the immediate first tick
    loop {
        tokio::select! {
            biased;
            outcome = &mut exec_fut => return outcome,
            _ = tick.tick() => emit_command_snapshot(streams, &cmd_id, &emitter).await,
        }
    }
}

/// Pull a fresh command output snapshot and emit it through the progress
/// emitter.
///
/// Errors during the snapshot pull are silently ignored so the pump never
/// blows up on a transient port hiccup.
async fn emit_command_snapshot<OS>(
    streams: &Arc<OS>,
    command_id: &CommandId,
    emitter: &ProgressEmitter,
) where
    OS: OutputStreamPort + Send + Sync,
{
    let Ok(snap) = streams.snapshot_command(command_id).await else {
        return;
    };
    let bytes = super::progress::usize_to_progress(snap.stdout.len());
    emitter
        .emit(bytes, None, Some("command running".to_string()))
        .await;
}

/// Drive a long-running transfer-progress use case under a bounded
/// progress-emission interval.
///
/// Same shape as [`drive_with_command_progress`] but pulls a fresh
/// `TransferEntity` for each tick so the emission carries
/// `(bytes_transferred, total_bytes)`.
async fn drive_with_transfer_progress<TR>(
    use_case: &GetTransferProgressUseCase<TR>,
    transfers: &Arc<TR>,
    req: GetTransferProgressRequest,
    emitter: ProgressEmitter,
) -> Result<GetTransferProgressResult, DomainError>
where
    TR: TransferRepository + Send + Sync,
{
    if !req.wait || !emitter.is_enabled() {
        return use_case.execute(req).await;
    }
    let xfer_id = req.transfer_id.clone();
    let exec_fut = use_case.execute(req);
    tokio::pin!(exec_fut);
    let mut tick = interval(COMMAND_TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tick.tick().await; // consume the immediate first tick
    loop {
        tokio::select! {
            biased;
            outcome = &mut exec_fut => return outcome,
            _ = tick.tick() => emit_transfer_snapshot(transfers, &xfer_id, &emitter).await,
        }
    }
}

/// Pull a fresh transfer entity and emit
/// `(bytes_transferred, total_bytes)` through the progress emitter.
///
/// Errors during the lookup are silently ignored.
async fn emit_transfer_snapshot<TR>(
    transfers: &Arc<TR>,
    transfer_id: &TransferId,
    emitter: &ProgressEmitter,
) where
    TR: TransferRepository + Send + Sync,
{
    let Ok(Some(entity)) = transfers.get(transfer_id).await else {
        return;
    };
    let progress = super::progress::u64_to_progress(entity.bytes_transferred);
    let total = super::progress::u64_to_progress(entity.total_bytes);
    emitter
        .emit(progress, Some(total), Some("transfer running".to_string()))
        .await;
}

/// Drive a long-running shell `wait_for_pattern` use case under a 1 s
/// progress-emission interval, reporting `(elapsed_secs, timeout_secs)`
/// so the LLM sees forward motion while it polls.
async fn drive_with_wait_for_progress<ShR, OS, C, Cfg>(
    use_case: &WaitForPatternUseCase<ShR, OS, C, Cfg>,
    req: WaitForPatternRequest,
    emitter: ProgressEmitter,
) -> Result<WaitForPatternOutcome, DomainError>
where
    ShR: ShellRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    C: ClockPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    if !emitter.is_enabled() {
        return use_case.execute(req).await;
    }
    let total = req.timeout.map(|t| t.as_secs_f64());
    let started = Instant::now();
    let exec_fut = use_case.execute(req);
    tokio::pin!(exec_fut);
    let mut tick = interval(WAIT_FOR_TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tick.tick().await;
    loop {
        tokio::select! {
            biased;
            outcome = &mut exec_fut => return outcome,
            _ = tick.tick() => emit_wait_progress(started, total, &emitter).await,
        }
    }
}

/// Emit one elapsed-seconds tick toward the configured timeout.
///
/// Pure helper extracted from [`drive_with_wait_for_progress`] so the
/// pump body stays under the 30-line ceiling.
async fn emit_wait_progress(started: Instant, total: Option<f64>, emitter: &ProgressEmitter) {
    let elapsed = started.elapsed().as_secs_f64();
    emitter
        .emit(elapsed, total, Some("waiting for pattern".to_string()))
        .await;
}

/// Build the [`Credentials`] DTO from the rmcp connect args following the
/// v3 priority: explicit password takes precedence over key path; an
/// empty payload falls back to the agent placeholder so the auth chain
/// can wire SSH-agent lookup.
///
/// The v3 SSH adapter overloads the [`Credentials::PrivateKey::key_pem`]
/// slot to carry a *file path* (not PEM material) until the H8 auth
/// chain finishes its full bridge — see
/// `src/adapters/ssh/russh_adapter.rs::split_credentials` for the
/// matching reader side.
fn pick_credentials(username: &str, password: Option<&str>, key_path: Option<&str>) -> Credentials {
    if let Some(pw) = password.filter(|s| !s.is_empty()) {
        return Credentials::Password {
            username: username.to_string(),
            password: pw.to_string(),
        };
    }
    if let Some(path) = key_path.filter(|s| !s.is_empty()) {
        return Credentials::PrivateKey {
            username: username.to_string(),
            key_pem: path.to_string(),
            passphrase: None,
        };
    }
    Credentials::Agent {
        username: username.to_string(),
        socket: None,
    }
}

/// Build a [`KeyModifiers`] bag from the optional flags carried on the
/// rmcp `ssh_shell_send_key` payload. Mirrors the v3 fall-back-to-`false`
/// semantics.
fn pick_modifiers(shift: Option<bool>, alt: Option<bool>, ctrl: Option<bool>) -> KeyModifiers {
    KeyModifiers {
        shift: shift.unwrap_or(false),
        alt: alt.unwrap_or(false),
        ctrl: ctrl.unwrap_or(false),
    }
}

// ---------------------------------------------------------------------------
// v4.7-step7 — `INITIAL_BUFFER` for `ssh_shell_open`
// ---------------------------------------------------------------------------

/// Maximum time (ms) the inbound layer waits for the freshly opened
/// PTY to emit its first byte before returning the response. Default
/// 100ms; override via `SSH_SHELL_OPEN_INITIAL_PEEK_MS`.
pub const SSH_SHELL_OPEN_INITIAL_PEEK_MS: u64 = 100;

/// Tick cadence for the snapshot poll. Default 5ms; override via
/// `SSH_SHELL_OPEN_INITIAL_PEEK_TICK_MS`.
pub const SSH_SHELL_OPEN_INITIAL_PEEK_TICK_MS: u64 = 5;

/// Hard cap on the rendered `INITIAL_BUFFER` (bytes). Excess bytes are
/// head-truncated. Default 4096; override via
/// `SSH_SHELL_OPEN_INITIAL_BUFFER_MAX_BYTES`.
pub const SSH_SHELL_OPEN_INITIAL_BUFFER_MAX_BYTES: usize = 4096;

/// Env var resolving the snapshot peek window.
const SSH_SHELL_OPEN_INITIAL_PEEK_MS_ENV: &str = "SSH_SHELL_OPEN_INITIAL_PEEK_MS";
/// Env var resolving the per-tick sleep budget.
const SSH_SHELL_OPEN_INITIAL_PEEK_TICK_MS_ENV: &str = "SSH_SHELL_OPEN_INITIAL_PEEK_TICK_MS";
/// Env var resolving the rendered `INITIAL_BUFFER` head-cap.
const SSH_SHELL_OPEN_INITIAL_BUFFER_MAX_BYTES_ENV: &str = "SSH_SHELL_OPEN_INITIAL_BUFFER_MAX_BYTES";

/// Resolve the snapshot peek window in milliseconds.
fn resolve_initial_peek_ms() -> u64 {
    use std::env;
    env::var(SSH_SHELL_OPEN_INITIAL_PEEK_MS_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(SSH_SHELL_OPEN_INITIAL_PEEK_MS)
}

/// Resolve the per-tick sleep budget in milliseconds.
fn resolve_initial_peek_tick_ms() -> u64 {
    use std::env;
    env::var(SSH_SHELL_OPEN_INITIAL_PEEK_TICK_MS_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(SSH_SHELL_OPEN_INITIAL_PEEK_TICK_MS)
}

/// Resolve the rendered `INITIAL_BUFFER` head-cap.
fn resolve_initial_buffer_max_bytes() -> usize {
    use std::env;
    env::var(SSH_SHELL_OPEN_INITIAL_BUFFER_MAX_BYTES_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(SSH_SHELL_OPEN_INITIAL_BUFFER_MAX_BYTES)
}

/// Polled snapshot summary returned by [`peek_initial_shell_buffer`].
#[derive(Debug, Clone)]
pub struct InitialBufferPeek {
    /// Head-truncated byte slice ready for rendering.
    pub bytes: Vec<u8>,
    /// `true` when the original snapshot exceeded the head cap.
    pub truncated: bool,
}

/// Poll the freshly opened shell's stdout buffer until the first byte
/// arrives or the budget expires.
///
/// Returns `None` when nothing was emitted (the legacy behaviour).
/// The loop sleeps with `tokio::time::sleep` (never busy-waits) and
/// always honours a hard budget so a never-emitting shell does not
/// stall the response.
pub async fn peek_initial_shell_buffer<OS>(
    streams: &OS,
    shell_id: &ShellId,
) -> Option<InitialBufferPeek>
where
    OS: OutputStreamPort,
{
    use tokio::time::{Instant, sleep};
    let budget = Duration::from_millis(resolve_initial_peek_ms());
    let tick = Duration::from_millis(resolve_initial_peek_tick_ms());
    let cap = resolve_initial_buffer_max_bytes();
    let deadline = Instant::now() + budget;
    loop {
        if let Ok(snap) = streams.snapshot_shell(shell_id).await
            && !snap.stdout.is_empty()
        {
            let total = snap.stdout.len();
            let take = total.min(cap);
            let bytes = snap.stdout[..take].to_vec();
            return Some(InitialBufferPeek {
                bytes,
                truncated: total > cap,
            });
        }
        if Instant::now() + tick >= deadline {
            return None;
        }
        sleep(tick).await;
    }
}

// ---------------------------------------------------------------------------
// v5 Phase 3 — subscription tool helpers (shared between both
// `#[tool_router]` impls)
// ---------------------------------------------------------------------------

/// Drive `ssh_subscribe`. Folds the wire `SshSubscribeArgs` into the
/// application request and renders the v5 block + structured output.
async fn run_sub_subscribe(
    use_case: &SubscribeUseCase,
    args: SshSubscribeArgs,
) -> Result<CallToolResult, McpError> {
    let req = SubscribeRequest {
        uri: args.uri,
        lifetime: lifetime_from_args(args.lifetime, args.grace_ms, args.ttl_secs),
        lag_policy: args.lag_policy.unwrap_or(LagPolicy::Snapshot),
        filter: filter_from_str(args.filter.as_deref()),
    };
    match use_case.execute(req).await {
        Ok(outcome) => {
            let structured = render::subscription::subscribe_structured(&outcome);
            let body = render::subscription::subscribe_render(&outcome);
            Ok(ok_text_and_structured(body, structured))
        }
        Err(err) => Ok(render_tool_error("SSH_SUBSCRIBE", &err)),
    }
}

/// Translate the v5 Phase 3 args fields (`release_when_no_subs`,
/// `grace_ms`) into a [`LifecyclePolicy`].
///
/// Returns `None` when both fields are absent — keeps the v4 default
/// (no auto-release) so the existing behaviour stays byte-identical.
fn lifecycle_from_args(
    release_when_no_subs: Option<bool>,
    grace_ms: Option<u32>,
) -> Option<LifecyclePolicy> {
    if release_when_no_subs.is_none() && grace_ms.is_none() {
        return None;
    }
    Some(LifecyclePolicy {
        release_when_no_subs: release_when_no_subs.unwrap_or(false),
        grace_ms: grace_ms.unwrap_or(2_000),
        cascade_session: false,
    })
}

fn lifetime_from_args(
    kind: Option<LifetimeKind>,
    grace_ms: Option<u32>,
    ttl_secs: Option<u32>,
) -> SubscriptionLifetime {
    match kind.unwrap_or(LifetimeKind::Manual) {
        LifetimeKind::Manual => SubscriptionLifetime::Manual,
        LifetimeKind::AutoClose => SubscriptionLifetime::AutoClose {
            grace_ms: grace_ms.unwrap_or(2_000),
        },
        LifetimeKind::Lease => SubscriptionLifetime::Lease {
            ttl_secs: ttl_secs.unwrap_or(60),
        },
    }
}

fn filter_from_str(s: Option<&str>) -> FilterRule {
    match s {
        Some(text) if !text.is_empty() => FilterRule::Regex(text.to_string()),
        _ => FilterRule::None,
    }
}

/// Drive `ssh_unsubscribe`.
async fn run_sub_unsubscribe(
    use_case: &UnsubscribeUseCase,
    args: SshUnsubscribeArgs,
) -> Result<CallToolResult, McpError> {
    let req = UnsubscribeRequest {
        sub_id: SubId::new(args.sub_id),
    };
    match use_case.execute(req).await {
        Ok(outcome) => {
            let structured = render::subscription::unsubscribe_structured(&outcome);
            let body = render::subscription::unsubscribe_render(&outcome);
            Ok(ok_text_and_structured(body, structured))
        }
        Err(err) => Ok(render_tool_error("SSH_UNSUBSCRIBE", &err)),
    }
}

/// Drive `ssh_sub_pause`.
async fn run_sub_pause(
    use_case: &PauseSubUseCase,
    args: SshSubPauseArgs,
) -> Result<CallToolResult, McpError> {
    let req = SubToggleRequest {
        sub_id: SubId::new(args.sub_id),
    };
    match use_case.execute(req).await {
        Ok(outcome) => {
            let structured = render::subscription::pause_structured(&outcome);
            let body = render::subscription::pause_render(&outcome);
            Ok(ok_text_and_structured(body, structured))
        }
        Err(err) => Ok(render_tool_error("SSH_SUB_PAUSE", &err)),
    }
}

/// Drive `ssh_sub_resume`.
async fn run_sub_resume(
    use_case: &ResumeSubUseCase,
    args: SshSubResumeArgs,
) -> Result<CallToolResult, McpError> {
    let req = SubToggleRequest {
        sub_id: SubId::new(args.sub_id),
    };
    match use_case.execute(req).await {
        Ok(outcome) => {
            let structured = render::subscription::resume_structured(&outcome);
            let body = render::subscription::resume_render(&outcome);
            Ok(ok_text_and_structured(body, structured))
        }
        Err(err) => Ok(render_tool_error("SSH_SUB_RESUME", &err)),
    }
}

/// Drive `ssh_sub_filter`.
async fn run_sub_filter(
    use_case: &SetFilterUseCase,
    args: SshSubFilterArgs,
) -> Result<CallToolResult, McpError> {
    let filter = if args.regex.is_empty() {
        FilterRule::None
    } else {
        FilterRule::Regex(args.regex)
    };
    let req = SetFilterRequest {
        sub_id: SubId::new(args.sub_id),
        filter,
    };
    match use_case.execute(req).await {
        Ok(outcome) => {
            let structured = render::subscription::filter_structured(&outcome);
            let body = render::subscription::filter_render(&outcome);
            Ok(ok_text_and_structured(body, structured))
        }
        Err(err) => Ok(render_tool_error("SSH_SUB_FILTER", &err)),
    }
}

/// Drive `ssh_sub_replay`.
async fn run_sub_replay(
    use_case: &ReplaySubUseCase,
    args: SshSubReplayArgs,
) -> Result<CallToolResult, McpError> {
    let req = ReplayRequest {
        sub_id: SubId::new(args.sub_id),
        from_cursor: args.from_cursor.unwrap_or(0),
    };
    match use_case.execute(req).await {
        Ok(outcome) => {
            let structured = render::subscription::replay_structured(&outcome);
            let body = render::subscription::replay_render(&outcome);
            Ok(ok_text_and_structured(body, structured))
        }
        Err(err) => Ok(render_tool_error("SSH_SUB_REPLAY", &err)),
    }
}

/// Drive `ssh_sub_list`. Errors-free use case — surface as an
/// always-`Ok` body so the rmcp wrapper can stay symmetric with the
/// async tools.
fn run_sub_list(use_case: &ListSubsUseCase, args: SshSubListArgs) -> CallToolResult {
    let req = ListSubsRequest {
        uri_prefix: args.uri_prefix,
        peer_id: args.peer_id,
    };
    match use_case.execute(&req) {
        Ok(outcome) => {
            let structured = render::subscription::list_structured(&outcome);
            let body = render::subscription::list_render(&outcome);
            ok_text_and_structured(body, structured)
        }
        Err(err) => render_tool_error("SSH_SUB_LIST", &err),
    }
}

/// Drive `ssh_sub_stats`.
fn run_sub_stats(use_case: &SubStatsUseCase, args: SshSubStatsArgs) -> CallToolResult {
    let req = SubStatsRequest {
        sub_id: SubId::new(args.sub_id),
    };
    match use_case.execute(&req) {
        Ok(outcome) => {
            let structured = render::subscription::sub_stats_structured(&outcome);
            let body = render::subscription::sub_stats_render(&outcome);
            ok_text_and_structured(body, structured)
        }
        Err(err) => render_tool_error("SSH_SUB_STATS", &err),
    }
}

/// Drive `ssh_daemon_stats`.
fn run_daemon_stats(use_case: &DaemonStatsUseCase) -> CallToolResult {
    match use_case.execute() {
        Ok(outcome) => {
            let structured = render::subscription::daemon_stats_structured(&outcome);
            let body = render::subscription::daemon_stats_render(&outcome);
            ok_text_and_structured(body, structured)
        }
        Err(err) => render_tool_error("SSH_DAEMON_STATS", &err),
    }
}

// ---------------------------------------------------------------------------
// `#[tool_router]` impl — `port_forward` enabled
// ---------------------------------------------------------------------------

#[cfg(feature = "port_forward")]
#[tool_router]
impl<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg>
    McpSshServer<UseCases<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg>>
where
    S: SshClientPort + Send + Sync + 'static,
    F: SftpClientPort + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    CR: CommandRepository + Send + Sync + 'static,
    ShR: ShellRepository + Send + Sync + 'static,
    TR: TransferRepository + Send + Sync + 'static,
    FR: ForwardRepository + Send + Sync + 'static,
    N: NotifierPort + Send + Sync + 'static,
    AS: AuthStrategyPort + Send + Sync + 'static,
    OS: OutputStreamPort + Send + Sync + 'static,
    SubR: SubscriberRegistryPort + SubscriberRegistryAsync + Send + Sync + 'static,
    C: ClockPort + Send + Sync + 'static,
    Cfg: ConfigPort + Send + Sync + 'static,
    Idg: IdGeneratorPort + Send + Sync + 'static,
{
    /// Build an [`McpSshServer`] with the provided container, peer
    /// table, and shared idempotency cache.
    #[must_use]
    #[allow(
        clippy::type_complexity,
        reason = "the Arc<UseCases<...>> generic surface is the natural shape of the production wiring; the prod alias `ProdUseCases` collapses it at the call site"
    )]
    pub fn new(
        use_cases: Arc<UseCases<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg>>,
        peer_table: Arc<PeerTable>,
        idempotency: Arc<IdempotencyCache>,
    ) -> Self {
        Self::from_parts(use_cases, peer_table, idempotency)
    }

    // ---------- Connection domain ------------------------------------

    #[tool(
        title = "Connect to SSH server",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Connect to an SSH server and store the session.\n\nWhen to use:\n- Establishing a new SSH connection to run commands, open shells, or transfer files.\n- Reusing an already-connected session by passing its `session_id`.\n\nImportant identifiers in response:\n- `SESSION_ID`: passed to ssh_execute, ssh_shell_open, ssh_upload, ssh_download, ssh_disconnect, ssh_forward.\n- `AGENT_ID`: optional grouping; passed to ssh_list_sessions (filter) and ssh_disconnect_agent (cleanup).\n- `EXPIRES_AT`: RFC3339 deadline when the session is auto-reaped by the inactivity sweeper. Ping (e.g. ssh_execute `: ` or any cheap call) before this fires to keep the session alive. Replaced by `PERSISTENT: true` when the caller opted out.\n\nWorkflow:\n1. Call ssh_connect once per remote host.\n2. Use the returned SESSION_ID for subsequent tool calls.\n3. Call ssh_disconnect (or ssh_disconnect_agent) when done.\n\nTip: pass `reuse=auto` to let the server pick the most recent healthy match in a single round-trip. Use `reuse=suggest` (default) when you want to inspect matches before reusing. Use `reuse=force_new` to bypass identity matching entirely.\nTip: pass `agent_id` so subsequent sessions are grouped and you can bulk-cleanup with `ssh_disconnect_agent`. When `agent_id` is set, `reuse=auto`/`reuse=suggest` rank sessions owned by the same agent first.\n\nStatus values: OK, REUSED, SUGGESTED.\n\nErrors: CONNECTION_FAILED, AUTH_FAILED.\n\nCost: 1 SSH handshake (typical 200-2000ms). Cheap to retry with reuse=auto.\n\nIdempotency: pass `_meta.idempotency_key` to dedup retried calls within the v4.7-step5 cache TTL.",
        output_schema = schema_for_type::<SshConnectResult>()
    )]
    async fn ssh_connect(
        &self,
        Parameters(args): Parameters<SshConnectArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_connect",
            fingerprint_args(&args),
            || async { run_connect(self.use_cases.connect.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "Disconnect SSH session",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        ),
        description = "Disconnect an SSH session.\n\nWhen to use:\n- Tearing down a single SSH session previously opened with ssh_connect.\n- Cancels every async command, closes every PTY, and aborts every in-flight SFTP transfer for the session.\n\nWorkflow:\n1. Pass the `session_id` returned from ssh_connect.\n2. Subsequent tool calls against that id return SESSION_NOT_FOUND.\n\nStatus values: OK.\n\nErrors: SESSION_NOT_FOUND, TRANSPORT_ERROR.\n\nCost: O(1). Always succeeds.",
        output_schema = schema_for_type::<SshDisconnectResult>()
    )]
    async fn ssh_disconnect(
        &self,
        Parameters(args): Parameters<SshDisconnectArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_disconnect",
            fingerprint_args(&args),
            || async {
                match self
                    .use_cases
                    .disconnect
                    .execute(DisconnectRequest {
                        session_id: SessionId::new(args.session_id),
                    })
                    .await
                {
                    Ok(outcome) => {
                        let structured = render::connection::disconnect_structured(&outcome);
                        let body = render::connection::disconnect_render(&outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error_smart(
                        "SSH_DISCONNECT",
                        &err,
                        self.id_lister.as_ref(),
                    )
                    .await),
                }
            },
        )
        .await
    }

    #[tool(
        title = "List SSH sessions",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "List active SSH sessions on the server.\n\nWhen to use:\n- Inspecting sessions known to this server (optionally narrowed to one agent).\n\nWorkflow:\n1. Optional `agent_id` filter to scope the list to sessions tagged with that AGENT_ID.\n2. Optional `max_items` cap (default 500, env `SSH_MCP_LIST_MAX_ITEMS`).\n\nStatus values: OK.\n\nErrors: STORAGE_ERROR.\n\nCost: O(N) over current sessions. Cheap to call repeatedly.",
        output_schema = schema_for_type::<SshListSessionsResult>()
    )]
    async fn ssh_list_sessions(
        &self,
        Parameters(args): Parameters<SshListSessionsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let filter = args.agent_id.clone();
        match self
            .use_cases
            .list_sessions
            .execute(ListSessionsRequest {
                filter_agent_id: args.agent_id.map(AgentId::new),
                max_items: args.max_items,
            })
            .await
        {
            Ok(outcome) => {
                let alerts = self
                    .leak_probe
                    .as_ref()
                    .map(|p| p.current_alerts())
                    .unwrap_or_default();
                let structured = render::connection::list_sessions_structured_with_warnings(
                    &outcome,
                    filter.as_deref(),
                    &alerts,
                );
                let body = render::connection::list_sessions_render_with_warnings(outcome, &alerts);
                Ok(ok_text_and_structured(body, structured))
            }
            Err(err) => Ok(render_tool_error("SSH_LIST_SESSIONS", &err)),
        }
    }

    #[tool(
        title = "Disconnect all agent sessions",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        ),
        description = "Disconnect every session bound to a given agent.\n\nWhen to use:\n- Bulk-cleanup of every SSH session tagged with a given AGENT_ID.\n- Cancels async commands, closes shells, and aborts transfers per disconnected session.\n\nWorkflow:\n1. Pass the AGENT_ID returned from a previous ssh_connect.\n2. Sessions owned by other agents are not affected.\n\nStatus values: OK.\n\nErrors: STORAGE_ERROR.\n\nCost: O(N) over agent sessions. Tens of ms typical.",
        output_schema = schema_for_type::<SshDisconnectAgentResult>()
    )]
    async fn ssh_disconnect_agent(
        &self,
        Parameters(args): Parameters<SshDisconnectAgentArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_disconnect_agent",
            fingerprint_args(&args),
            || async {
                match self
                    .use_cases
                    .disconnect_agent
                    .execute(DisconnectAgentRequest {
                        agent_id: AgentId::new(args.agent_id),
                    })
                    .await
                {
                    Ok(outcome) => {
                        let structured = render::connection::disconnect_agent_structured(&outcome);
                        let body = render::connection::disconnect_agent_render(&outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error("SSH_DISCONNECT_AGENT", &err)),
                }
            },
        )
        .await
    }

    // ---------- Execute domain ---------------------------------------

    #[tool(
        title = "Run remote command",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        ),
        description = "Spawn an asynchronous command on an SSH session.\n\nWhen to use:\n- Starting a command and polling its output via ssh_get_command_output.\n- Set `pty=true` for commands requiring a controlling terminal (e.g. sudo).\n\nImportant identifiers in response:\n- `COMMAND_ID`: passed to ssh_get_command_output, ssh_cancel_command.\n\nWorkflow:\n1. Call ssh_execute with the SESSION_ID and command line.\n2. Use ssh_get_command_output to fetch progress / completion.\n3. Optional ssh_cancel_command to interrupt.\n\nStatus values: STARTED.\n\nErrors: SESSION_NOT_FOUND, MAX_COMMANDS_EXCEEDED, TRANSPORT_ERROR.\n\nCost: 1 SSH channel open. Returns immediately when wait=false (default async).",
        output_schema = schema_for_type::<SshExecuteResult>()
    )]
    async fn ssh_execute(
        &self,
        Parameters(args): Parameters<SshExecuteArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_execute",
            fingerprint_args(&args),
            || async {
                let req = ExecuteRequest {
                    session_id: SessionId::new(args.session_id),
                    command: args.command,
                    timeout: args.timeout_secs.map(Duration::from_secs),
                    use_pty: args.pty.unwrap_or(false),
                    lifecycle_policy: lifecycle_from_args(args.release_when_no_subs, args.grace_ms),
                };
                match self.use_cases.execute.execute(req).await {
                    Ok(outcome) => {
                        let structured = render::execute::execute_structured(&outcome);
                        let body = render::execute::execute_render(outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => {
                        Ok(
                            render_tool_error_smart("SSH_EXECUTE", &err, self.id_lister.as_ref())
                                .await,
                        )
                    }
                }
            },
        )
        .await
    }

    #[tool(
        title = "Get command output",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Fetch the current output of an asynchronous command.\n\nWhen to use:\n- Polling stdout/stderr for a command spawned with ssh_execute.\n- Optionally blocking until the command completes (`wait=true`).\n\nWorkflow:\n1. Pass the COMMAND_ID returned from ssh_execute.\n2. Set `wait=true` to block; capped at `wait_timeout_secs` (default 30, max 300).\n3. `max_output_bytes` head-truncates very large outputs (default 16384).\n\nStatus values: RUNNING, COMPLETED, TIMEOUT, CANCELLED, FAILED.\n\nErrors: COMMAND_NOT_FOUND.\n\nCost: O(buffer). Cheap with wait=false. With wait=true blocks up to wait_timeout_secs.\n\nProgress: when `wait=true` and `_meta.progressToken` is set, mid-flight `notifications/progress` updates fire every ~5s with the running stdout byte count (best-effort).",
        output_schema = schema_for_type::<SshGetCommandOutputResult>()
    )]
    async fn ssh_get_command_output(
        &self,
        Parameters(args): Parameters<SshGetCommandOutputArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let req = GetCommandOutputRequest {
            command_id: CommandId::new(args.command_id),
            wait: args.wait.unwrap_or(false),
            wait_timeout: args.wait_timeout_secs.map(Duration::from_secs),
            max_output_bytes: args.max_output_bytes,
        };
        let emitter = ProgressEmitter::new(&ctx);
        let leak_bridge = spawn_leak_warn_bridge_if_wired(&emitter, self.leak_watcher.as_ref());
        let use_case = self.use_cases.get_command_output.as_ref();
        let streams = use_case.streams();
        let outcome = drive_with_command_progress(use_case, streams, req, emitter).await;
        leak_bridge.shutdown().await;
        match outcome {
            Ok(result) => {
                let structured = render::execute::get_command_output_structured(&result);
                let body = render::execute::get_command_output_render(result);
                Ok(ok_text_and_structured(body, structured))
            }
            Err(err) => Ok(render_tool_error_smart(
                "SSH_GET_COMMAND_OUTPUT",
                &err,
                self.id_lister.as_ref(),
            )
            .await),
        }
    }

    #[tool(
        title = "List async commands",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "List asynchronous commands tracked on the server.\n\nWhen to use:\n- Inspecting every command (optionally filtered by session and/or status).\n\nWorkflow:\n1. Optional `session_id` to narrow to one session.\n2. Optional `status` filter (`running`, `completed`, `cancelled`, `failed`).\n3. Optional `max_items` cap (default 500).\n\nStatus values: OK.\n\nErrors: STORAGE_ERROR.\n\nCost: O(N) over async commands. Cheap.",
        output_schema = schema_for_type::<SshListCommandsResult>()
    )]
    async fn ssh_list_commands(
        &self,
        Parameters(args): Parameters<SshListCommandsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = ListCommandsRequest {
            filter_session_id: args.session_id.map(SessionId::new),
            filter_status: args
                .status
                .map(super::args::execute::CommandStatus::into_domain),
            max_items: args.max_items,
        };
        match self.use_cases.list_commands.execute(req).await {
            Ok(outcome) => {
                let alerts = self
                    .leak_probe
                    .as_ref()
                    .map(|p| p.current_alerts())
                    .unwrap_or_default();
                let structured =
                    render::execute::list_commands_structured_with_warnings(&outcome, &alerts);
                let body = render::execute::list_commands_render_with_warnings(outcome, &alerts);
                Ok(ok_text_and_structured(body, structured))
            }
            Err(err) => Ok(render_tool_error("SSH_LIST_COMMANDS", &err)),
        }
    }

    #[tool(
        title = "Cancel running command",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        ),
        description = "Cancel an asynchronous command.\n\nWhen to use:\n- Interrupting a long-running command spawned with ssh_execute.\n- Returns the partial stdout/stderr captured so far when the command was running.\n\nStatus values: CANCELLED, NOOP.\n\nErrors: COMMAND_NOT_FOUND.\n\nCost: O(1). Always succeeds (NOOP for already-finished commands).",
        output_schema = schema_for_type::<SshCancelCommandResult>()
    )]
    async fn ssh_cancel_command(
        &self,
        Parameters(args): Parameters<SshCancelCommandArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_cancel_command",
            fingerprint_args(&args),
            || async {
                let req = CancelCommandRequest {
                    command_id: CommandId::new(args.command_id),
                    max_output_bytes: args.max_output_bytes,
                };
                match self.use_cases.cancel_command.execute(req).await {
                    Ok(outcome) => {
                        let structured = render::execute::cancel_command_structured(&outcome);
                        let body = render::execute::cancel_command_render(outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error_smart(
                        "SSH_CANCEL_COMMAND",
                        &err,
                        self.id_lister.as_ref(),
                    )
                    .await),
                }
            },
        )
        .await
    }

    // ---------- Shell domain -----------------------------------------

    #[tool(
        title = "Open PTY shell",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        ),
        description = "Open an interactive PTY shell on an SSH session.\n\nWhen to use:\n- Driving an interactive program (vim, htop, REPL, sudo prompt) that needs a TTY.\n- Prefer subscribing to `shell://<shell_id>/output` over polling ssh_shell_read.\n\nImportant identifiers in response:\n- `SHELL_ID`: passed to ssh_shell_write, ssh_shell_send_key, ssh_shell_read, ssh_shell_wait_for, ssh_shell_close.\n\nStatus values: OK.\n\nErrors: SESSION_NOT_FOUND, MAX_SHELLS_EXCEEDED, TRANSPORT_ERROR.\n\nCost: 1 SSH PTY allocation (typical 50-500ms). One PTY per shell_id.",
        output_schema = schema_for_type::<SshShellOpenResult>()
    )]
    async fn ssh_shell_open(
        &self,
        Parameters(args): Parameters<SshShellOpenArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_shell_open",
            fingerprint_args(&args),
            || async {
                let req = OpenShellRequest {
                    session_id: SessionId::new(args.session_id),
                    term: args.term,
                    cols: args.cols,
                    rows: args.rows,
                    inactivity_ttl_secs: args.inactivity_ttl,
                    max_buffer_size: parse_human_bytes(args.max_buffer_size.as_deref()),
                    lifecycle_policy: lifecycle_from_args(args.release_when_no_subs, args.grace_ms),
                };
                match self.use_cases.open_shell.execute(req).await {
                    Ok(outcome) => {
                        let streams = self.use_cases.read_shell.streams();
                        let peek =
                            peek_initial_shell_buffer(streams.as_ref(), &outcome.shell.id).await;
                        let bytes_ref = peek.as_ref().map(|p| p.bytes.as_slice());
                        let structured =
                            render::shell::shell_open_structured_with_initial(&outcome, bytes_ref);
                        let body =
                            render::shell::shell_open_render_with_initial(outcome, bytes_ref);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error_smart(
                        "SSH_SHELL_OPEN",
                        &err,
                        self.id_lister.as_ref(),
                    )
                    .await),
                }
            },
        )
        .await
    }

    #[tool(
        title = "Write to PTY shell",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        ),
        description = "Write raw bytes to a PTY shell.\n\nWhen to use:\n- Submitting a typed command (append `\\n`).\n- Sending raw control sequences (e.g. `\\x03` for Ctrl+C, `\\x1b[A` for arrow up).\n- Prefer ssh_shell_send_key for named keystrokes.\n\nStatus values: OK.\n\nErrors: SHELL_NOT_FOUND, TRANSPORT_ERROR.\n\nCost: O(input.len). Sub-ms typical. Subscribe to shell://<id>/output for response.",
        output_schema = schema_for_type::<SshShellWriteResult>()
    )]
    async fn ssh_shell_write(
        &self,
        Parameters(args): Parameters<SshShellWriteArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_shell_write",
            fingerprint_args(&args),
            || async {
                let req = WriteShellRequest {
                    shell_id: ShellId::new(args.shell_id),
                    bytes: Bytes::from(args.input.into_bytes()),
                };
                match self.use_cases.write_shell.execute(req).await {
                    Ok(outcome) => {
                        let structured = render::shell::shell_write_structured(&outcome);
                        let body = render::shell::shell_write_render(outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error_smart(
                        "SSH_SHELL_WRITE",
                        &err,
                        self.id_lister.as_ref(),
                    )
                    .await),
                }
            },
        )
        .await
    }

    #[tool(
        title = "Send keystroke to PTY",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        ),
        description = "Send a named keystroke (with optional modifiers) to a PTY shell.\n\nWhen to use:\n- Sending arrows, function keys, control codes, navigation keys without crafting the bytes manually.\n- Optional Shift / Alt / Ctrl modifiers; optional `repeat` (1..=64).\n\nStatus values: OK.\n\nErrors: SHELL_NOT_FOUND, INVALID_ARGUMENT (bad repeat / modifier combination), TRANSPORT_ERROR.\n\nCost: O(repeat). Sub-ms typical. Subscribe to shell://<id>/output for response.",
        output_schema = schema_for_type::<SshShellSendKeyResult>()
    )]
    async fn ssh_shell_send_key(
        &self,
        Parameters(args): Parameters<SshShellSendKeyArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_shell_send_key",
            fingerprint_args(&args),
            || async {
                let req = SendKeyRequest {
                    shell_id: ShellId::new(args.shell_id),
                    key: args.key,
                    modifiers: pick_modifiers(args.shift, args.alt, args.ctrl),
                    repeat: args.repeat.unwrap_or(1),
                };
                match self.use_cases.send_key.execute(req).await {
                    Ok(outcome) => {
                        let structured = render::shell::shell_send_key_structured(&outcome);
                        let body = render::shell::shell_send_key_render(outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error_smart(
                        "SSH_SHELL_SEND_KEY",
                        &err,
                        self.id_lister.as_ref(),
                    )
                    .await),
                }
            },
        )
        .await
    }

    #[tool(
        title = "Read PTY buffer",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Read the buffered output of a PTY shell.\n\nWhen to use:\n- FALLBACK polling when subscribing to `shell://<shell_id>/output` is not feasible.\n- `clear=true` (default) drains the rendered head; `clear=false` keeps the buffer for re-inspection.\n- Optional long-poll via `wait=true` (`min_bytes` / `wait_timeout_secs`).\n\nStatus values: OPEN, CLOSED, TIMEOUT.\n\nErrors: SHELL_NOT_FOUND.\n\nCost: O(buffer). Cheap with wait=false. With wait=true blocks up to wait_timeout_secs.",
        output_schema = schema_for_type::<SshShellReadResult>()
    )]
    async fn ssh_shell_read(
        &self,
        Parameters(args): Parameters<SshShellReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        let cleared = args.clear.unwrap_or(true);
        let req = ReadShellRequest {
            shell_id: ShellId::new(args.shell_id),
            clear: cleared,
            max_output_bytes: args.max_output_bytes,
            wait: args.wait.unwrap_or(false),
            wait_timeout: args.wait_timeout_secs.map(Duration::from_secs),
            min_bytes: args.min_bytes,
        };
        match self.use_cases.read_shell.execute(req).await {
            Ok(outcome) => {
                let structured = render::shell::shell_read_structured(&outcome, cleared);
                let body = render::shell::shell_read_render(outcome);
                Ok(ok_text_and_structured(body, structured))
            }
            Err(err) => {
                Ok(render_tool_error_smart("SSH_SHELL_READ", &err, self.id_lister.as_ref()).await)
            }
        }
    }

    #[tool(
        title = "Wait for shell pattern",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Block until a substring pattern appears in the shell output.\n\nWhen to use:\n- Single-shot prompt gating before issuing the next command (e.g. wait for `\"$ \"`).\n- Up to 16 patterns (≤1024 bytes each); first match wins.\n- Prefer subscribing to `shell://<shell_id>/output` for realtime push.\n\nStatus values: MATCHED, TIMEOUT, CLOSED.\n\nErrors: SHELL_NOT_FOUND, INVALID_ARGUMENT.\n\nCost: blocks up to timeout_secs. Use for single-shot prompt gating.\n\nProgress: when `_meta.progressToken` is set, emits `notifications/progress` once per second carrying `(elapsed_secs, timeout_secs)` while the loop runs (best-effort).",
        output_schema = schema_for_type::<SshShellWaitForResult>()
    )]
    async fn ssh_shell_wait_for(
        &self,
        Parameters(args): Parameters<SshShellWaitForArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let req = WaitForPatternRequest {
            shell_id: ShellId::new(args.shell_id),
            patterns: args.patterns,
            timeout: args.timeout_secs.map(Duration::from_secs),
            max_output_bytes: args.max_output_bytes,
            clear: args.clear.unwrap_or(true),
        };
        let emitter = ProgressEmitter::new(&ctx);
        let leak_bridge = spawn_leak_warn_bridge_if_wired(&emitter, self.leak_watcher.as_ref());
        let outcome =
            drive_with_wait_for_progress(self.use_cases.wait_for_pattern.as_ref(), req, emitter)
                .await;
        leak_bridge.shutdown().await;
        match outcome {
            Ok(outcome) => {
                let structured = render::shell::shell_wait_for_structured(&outcome);
                let body = render::shell::shell_wait_for_render(&outcome);
                Ok(ok_text_and_structured(body, structured))
            }
            Err(err) => {
                Ok(
                    render_tool_error_smart("SSH_SHELL_WAIT_FOR", &err, self.id_lister.as_ref())
                        .await,
                )
            }
        }
    }

    #[tool(
        title = "Close PTY shell",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        ),
        description = "Close a PTY shell and free its resources.\n\nStatus values: OK.\n\nErrors: SHELL_NOT_FOUND, TRANSPORT_ERROR.\n\nCost: O(1). Always succeeds.",
        output_schema = schema_for_type::<SshShellCloseResult>()
    )]
    async fn ssh_shell_close(
        &self,
        Parameters(args): Parameters<SshShellCloseArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_shell_close",
            fingerprint_args(&args),
            || async {
                match self
                    .use_cases
                    .close_shell
                    .execute(CloseShellRequest {
                        shell_id: ShellId::new(args.shell_id),
                    })
                    .await
                {
                    Ok(outcome) => {
                        let structured = render::shell::shell_close_structured(&outcome);
                        let body = render::shell::shell_close_render(&outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error_smart(
                        "SSH_SHELL_CLOSE",
                        &err,
                        self.id_lister.as_ref(),
                    )
                    .await),
                }
            },
        )
        .await
    }

    // ---------- SFTP domain ------------------------------------------

    #[tool(
        title = "Upload file via SFTP",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        ),
        description = "Upload a local file to the remote host via SFTP.\n\nWhen to use:\n- Streaming a local file to the remote host in 32 KiB chunks.\n- Subscribe to `transfer://<transfer_id>/progress` for live progress events.\n\nImportant identifiers in response:\n- `TRANSFER_ID`: passed to ssh_get_transfer_progress.\n\nStatus values: STARTED.\n\nErrors: SESSION_NOT_FOUND, MAX_TRANSFERS_EXCEEDED, SFTP_ERROR.\n\nCost: O(file.size). Returns immediately, transfer runs async. Subscribe to transfer://<id>/progress.",
        output_schema = schema_for_type::<SshUploadResult>()
    )]
    async fn ssh_upload(
        &self,
        Parameters(args): Parameters<SshUploadArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_upload",
            fingerprint_args(&args),
            || async {
                let req = UploadRequest {
                    session_id: SessionId::new(args.session_id),
                    local_path: args.local_path,
                    remote_path: args.remote_path,
                    lifecycle_policy: lifecycle_from_args(args.release_when_no_subs, args.grace_ms),
                };
                match self.use_cases.upload_file.execute(req).await {
                    Ok(outcome) => {
                        let structured = render::sftp::upload_structured(&outcome);
                        let body = render::sftp::upload_render(outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => {
                        Ok(
                            render_tool_error_smart("SSH_UPLOAD", &err, self.id_lister.as_ref())
                                .await,
                        )
                    }
                }
            },
        )
        .await
    }

    #[tool(
        title = "Download file via SFTP",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        ),
        description = "Download a remote file via SFTP.\n\nWhen to use:\n- Streaming a remote file to the local host in 32 KiB chunks.\n- Subscribe to `transfer://<transfer_id>/progress` for live progress events.\n\nStatus values: STARTED.\n\nErrors: SESSION_NOT_FOUND, MAX_TRANSFERS_EXCEEDED, SFTP_ERROR.\n\nCost: O(file.size). Returns immediately, transfer runs async. Subscribe to transfer://<id>/progress.",
        output_schema = schema_for_type::<SshDownloadResult>()
    )]
    async fn ssh_download(
        &self,
        Parameters(args): Parameters<SshDownloadArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_download",
            fingerprint_args(&args),
            || async {
                let req = DownloadRequest {
                    session_id: SessionId::new(args.session_id),
                    remote_path: args.remote_path,
                    local_path: args.local_path,
                    lifecycle_policy: lifecycle_from_args(args.release_when_no_subs, args.grace_ms),
                };
                match self.use_cases.download_file.execute(req).await {
                    Ok(outcome) => {
                        let structured = render::sftp::download_structured(&outcome);
                        let body = render::sftp::download_render(outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => {
                        Ok(
                            render_tool_error_smart("SSH_DOWNLOAD", &err, self.id_lister.as_ref())
                                .await,
                        )
                    }
                }
            },
        )
        .await
    }

    #[tool(
        title = "Get transfer progress",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Snapshot the progress of an SFTP transfer.\n\nWhen to use:\n- Polling progress for an upload/download.\n- Optional `wait=true` blocks until the transfer reaches a terminal state.\n\nStatus values: RUNNING, COMPLETED, FAILED, CANCELLED.\n\nErrors: TRANSFER_NOT_FOUND.\n\nCost: O(1). Cheap with wait=false. With wait=true blocks until done or wait_timeout_secs.\n\nProgress: when `wait=true` and `_meta.progressToken` is set, mid-flight `notifications/progress` updates fire every ~5s carrying `(bytes_transferred, total_bytes)` (best-effort).",
        output_schema = schema_for_type::<SshGetTransferProgressResult>()
    )]
    async fn ssh_get_transfer_progress(
        &self,
        Parameters(args): Parameters<SshGetTransferProgressArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let req = GetTransferProgressRequest {
            transfer_id: TransferId::new(args.transfer_id),
            wait: args.wait.unwrap_or(false),
            wait_timeout: args.wait_timeout_secs.map(Duration::from_secs),
        };
        let emitter = ProgressEmitter::new(&ctx);
        let leak_bridge = spawn_leak_warn_bridge_if_wired(&emitter, self.leak_watcher.as_ref());
        let use_case = self.use_cases.get_transfer_progress.as_ref();
        let transfers = use_case.transfers();
        let outcome = drive_with_transfer_progress(use_case, transfers, req, emitter).await;
        leak_bridge.shutdown().await;
        match outcome {
            Ok(result) => {
                let structured = render::sftp::transfer_progress_structured(&result);
                let body = render::sftp::transfer_progress_render(&result);
                Ok(ok_text_and_structured(body, structured))
            }
            Err(err) => Ok(render_tool_error_smart(
                "SSH_GET_TRANSFER_PROGRESS",
                &err,
                self.id_lister.as_ref(),
            )
            .await),
        }
    }

    // ---------- v4.7-step3 convenience + batch tools -----------------

    #[tool(
        title = "Run remote command (one-shot)",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        ),
        description = "Connect, execute a short command synchronously, and (by default) disconnect — all in one call.\n\nWhen to use:\n- Short atomic commands (uptime, hostname, cat /etc/release).\n- Smaller LLMs that prefer not to choreograph connect -> execute -> wait by hand.\n\nWorkflow:\n1. ssh_run mints (or reuses) a session via reuse=auto.\n2. Spawns the command and blocks until completion or `timeout_secs` fires.\n3. With `disconnect_after=true` (default) tears the session down.\n\nStatus values: COMPLETED, TIMEOUT, FAILED, CANCELLED.\n\nErrors: CONNECTION_FAILED, AUTH_FAILED, MAX_COMMANDS_EXCEEDED, TRANSPORT_ERROR.\n\nCost: 1 SSH handshake + 1 channel + (optional) disconnect. Returns when the command finishes or timeout_secs expires.",
        output_schema = schema_for_type::<SshRunResult>()
    )]
    async fn ssh_run(
        &self,
        Parameters(args): Parameters<SshRunArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_run",
            fingerprint_args(&args),
            || async {
                run_one_shot(
                    self.use_cases.connect.as_ref(),
                    self.use_cases.execute.as_ref(),
                    self.use_cases.get_command_output.as_ref(),
                    self.use_cases.disconnect.as_ref(),
                    args,
                )
                .await
            },
        )
        .await
    }

    #[tool(
        title = "Execute a batch of commands on one session",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        ),
        description = "Run up to 16 commands sequentially against a single session, with stop-on-failure semantics.\n\nWhen to use:\n- A small linear pipeline (`mkdir /tmp/foo`, `tar -xzf bundle.tgz -C /tmp/foo`, `chown -R svc /tmp/foo`).\n- Short bursts where the round-trip cost of one ssh_execute per command dominates.\n\nWorkflow:\n1. Each command runs synchronously with the per-command `timeout_secs_per_command` budget.\n2. With `stop_on_failure=true` (default) the loop halts on the first non-zero exit code; remaining slots surface as `skipped`.\n3. Each entry carries its own command_id, exit_code, stdout/stderr blocks.\n\nStatus values: OK, HALTED.\n\nErrors: SESSION_NOT_FOUND, MAX_COMMANDS_EXCEEDED, TRANSPORT_ERROR.\n\nCost: 1 SSH channel per command. Stops early on first non-zero exit by default.",
        output_schema = schema_for_type::<SshExecuteBatchResult>()
    )]
    async fn ssh_execute_batch(
        &self,
        Parameters(args): Parameters<SshExecuteBatchArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_execute_batch",
            fingerprint_args(&args),
            || async {
                run_execute_batch(
                    self.use_cases.execute.as_ref(),
                    self.use_cases.get_command_output.as_ref(),
                    args,
                )
                .await
            },
        )
        .await
    }

    #[tool(
        title = "Disconnect multiple SSH sessions",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        ),
        description = "Best-effort bulk disconnect of up to 64 sessions in a single call.\n\nWhen to use:\n- Cleaning up a fan-out of sessions when bulk-by-agent is not appropriate.\n- Per-id failures are reported in the response and do not abort the remaining disconnects.\n\nWorkflow:\n1. Pass the list of SESSION_IDs returned from prior ssh_connect calls.\n2. Inspect the per-id `results` array for any `error` entries.\n\nStatus values: OK.\n\nErrors: INVALID_ARGUMENT (empty / >64 ids).\n\nCost: O(N) disconnect calls. Best-effort: per-id failures do not abort the batch.",
        output_schema = schema_for_type::<SshDisconnectManyResult>()
    )]
    async fn ssh_disconnect_many(
        &self,
        Parameters(args): Parameters<SshDisconnectManyArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_disconnect_many",
            fingerprint_args(&args),
            || async { run_disconnect_many(self.use_cases.disconnect.as_ref(), args).await },
        )
        .await
    }

    // ---------- Forward domain --------------------------------------

    #[tool(
        title = "Forward TCP port",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        ),
        description = "Set up a TCP port forwarder backed by an SSH session.\n\nWhen to use:\n- Tunnelling local TCP traffic over the SSH transport to a remote host:port.\n- Available only when the `port_forward` Cargo feature is enabled.\n\nStatus values: OK.\n\nErrors: SESSION_NOT_FOUND, PORT_IN_USE.\n\nCost: 1 listener bind + SSH tcpip-forward. Subscribe to forward://<id>/events for the event log.",
        output_schema = schema_for_type::<SshForwardResult>()
    )]
    async fn ssh_forward(
        &self,
        Parameters(args): Parameters<SshForwardArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_forward",
            fingerprint_args(&args),
            || async {
                let req = ForwardPortRequest {
                    session_id: SessionId::new(args.session_id),
                    local_port: args.local_port,
                    remote_address: args.remote_address,
                    remote_port: args.remote_port,
                };
                match self.use_cases.forward_port.execute(req).await {
                    Ok(outcome) => {
                        let structured = render::forward::forward_structured(&outcome);
                        let body = render::forward::forward_render(outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => {
                        Ok(
                            render_tool_error_smart("SSH_FORWARD", &err, self.id_lister.as_ref())
                                .await,
                        )
                    }
                }
            },
        )
        .await
    }

    // ---------- Subscription administration (v5 Phase 3) -------------

    #[tool(
        title = "Subscribe to a resource lane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        ),
        description = "Open a Channel Mux lane for a ssh-mcp resource URI.\n\nWhen to use:\n- Push-first observation of a resource (shell://, command://, transfer://, session://, forward://) without polling.\n- Lifetime/lag/filter knobs let smaller LLMs match the resource budget.\n\nPush: events fan into the lane through the channel mux outbound sink.\n\nCleanup: ssh_unsubscribe sub_id=... when done. Skip and the lane becomes a zombie.\n\nCost: O(1) lane open + per-event mpsc.\n\nIdempotency: pass `_meta.idempotency_key` to dedup retries.\n\nHygiene: hold the SUB_ID; never re-open the same URI without first unsubscribing."
    )]
    async fn ssh_subscribe(
        &self,
        Parameters(args): Parameters<SshSubscribeArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_subscribe",
            fingerprint_args(&args),
            || async { run_sub_subscribe(self.use_cases.sub_subscribe.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "Unsubscribe from a resource lane",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        ),
        description = "Close a Channel Mux lane previously opened with ssh_subscribe.\n\nCleanup: this IS the cleanup tool — call it for every SUB_ID you obtained.\n\nCost: O(1).\n\nIdempotency: pass `_meta.idempotency_key` to dedup retries.\n\nHygiene: tolerate `SUB_NOT_FOUND` — lifetime auto-close may have closed the lane already."
    )]
    async fn ssh_unsubscribe(
        &self,
        Parameters(args): Parameters<SshUnsubscribeArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_unsubscribe",
            fingerprint_args(&args),
            || async { run_sub_unsubscribe(self.use_cases.sub_unsubscribe.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "Pause a subscription lane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Suspend lane drain. Subsequent events accumulate in the lane mpsc until ssh_sub_resume.\n\nCost: O(1)."
    )]
    async fn ssh_sub_pause(
        &self,
        Parameters(args): Parameters<SshSubPauseArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_sub_pause",
            fingerprint_args(&args),
            || async { run_sub_pause(self.use_cases.sub_pause.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "Resume a subscription lane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Resume lane drain after a previous ssh_sub_pause.\n\nCost: O(1)."
    )]
    async fn ssh_sub_resume(
        &self,
        Parameters(args): Parameters<SshSubResumeArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_sub_resume",
            fingerprint_args(&args),
            || async { run_sub_resume(self.use_cases.sub_resume.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "Hot-reload a subscription filter",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Replace the lane regex filter without re-opening the lane. Empty regex clears the filter.\n\nCost: 1 regex compile + atomic swap."
    )]
    async fn ssh_sub_filter(
        &self,
        Parameters(args): Parameters<SshSubFilterArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_sub_filter",
            fingerprint_args(&args),
            || async { run_sub_filter(self.use_cases.sub_filter.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "Replay a subscription lane from cursor",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Re-emit lane events from `from_cursor` (within the replay window).\n\nCost: O(window-bytes) snapshot rebuild."
    )]
    async fn ssh_sub_replay(
        &self,
        Parameters(args): Parameters<SshSubReplayArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_sub_replay",
            fingerprint_args(&args),
            || async { run_sub_replay(self.use_cases.sub_replay.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "List active subscription lanes",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Snapshot the per-`SubId` lane registry. Optional `uri_prefix` filter narrows the result.\n\nCost: O(N) over open lanes."
    )]
    async fn ssh_sub_list(
        &self,
        Parameters(args): Parameters<SshSubListArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(run_sub_list(self.use_cases.sub_list.as_ref(), args))
    }

    #[tool(
        title = "Snapshot subscription lane stats",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Per-lane atomic counter snapshot — events_sent, bytes_sent, lagged_*, queue depth/high-watermark.\n\nCost: O(1) atomic loads."
    )]
    async fn ssh_sub_stats(
        &self,
        Parameters(args): Parameters<SshSubStatsArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(run_sub_stats(self.use_cases.sub_stats.as_ref(), args))
    }

    #[tool(
        title = "Aggregate daemon-wide subscription stats",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Sum / max of every per-lane atomic counter across the whole channel mux.\n\nCost: O(N) over open lanes."
    )]
    async fn ssh_daemon_stats(
        &self,
        Parameters(_args): Parameters<SshDaemonStatsArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(run_daemon_stats(self.use_cases.daemon_stats.as_ref()))
    }
}

// ---------------------------------------------------------------------------
// `#[tool_router]` impl — `port_forward` disabled
// ---------------------------------------------------------------------------

#[cfg(not(feature = "port_forward"))]
#[tool_router]
impl<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg>
    McpSshServer<UseCases<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg>>
where
    S: SshClientPort + Send + Sync + 'static,
    F: SftpClientPort + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    CR: CommandRepository + Send + Sync + 'static,
    ShR: ShellRepository + Send + Sync + 'static,
    TR: TransferRepository + Send + Sync + 'static,
    N: NotifierPort + Send + Sync + 'static,
    AS: AuthStrategyPort + Send + Sync + 'static,
    OS: OutputStreamPort + Send + Sync + 'static,
    SubR: SubscriberRegistryPort + SubscriberRegistryAsync + Send + Sync + 'static,
    C: ClockPort + Send + Sync + 'static,
    Cfg: ConfigPort + Send + Sync + 'static,
    Idg: IdGeneratorPort + Send + Sync + 'static,
{
    /// Build an [`McpSshServer`] with the provided container, peer
    /// table, and shared idempotency cache.
    #[must_use]
    #[allow(
        clippy::type_complexity,
        reason = "the Arc<UseCases<...>> generic surface is the natural shape of the production wiring; the prod alias `ProdUseCases` collapses it at the call site"
    )]
    pub fn new(
        use_cases: Arc<UseCases<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg>>,
        peer_table: Arc<PeerTable>,
        idempotency: Arc<IdempotencyCache>,
    ) -> Self {
        Self::from_parts(use_cases, peer_table, idempotency)
    }

    // ---------- Connection domain ------------------------------------

    #[tool(
        title = "Connect to SSH server",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Connect to an SSH server and store the session.\n\nTip: pass `reuse=auto` to let the server pick the most recent healthy match in a single round-trip. Use `reuse=suggest` (default) when you want to inspect matches before reusing. Use `reuse=force_new` to bypass identity matching entirely.\nTip: pass `agent_id` so subsequent sessions are grouped and you can bulk-cleanup with `ssh_disconnect_agent`.\n\nCost: 1 SSH handshake (typical 200-2000ms). Cheap to retry with reuse=auto.\n\nIdempotency: pass `_meta.idempotency_key` to dedup retried calls within the v4.7-step5 cache TTL.",
        output_schema = schema_for_type::<SshConnectResult>()
    )]
    async fn ssh_connect(
        &self,
        Parameters(args): Parameters<SshConnectArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_connect",
            fingerprint_args(&args),
            || async { run_connect(self.use_cases.connect.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "Disconnect SSH session",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        ),
        description = "Disconnect an SSH session.\n\nCost: O(1). Always succeeds.",
        output_schema = schema_for_type::<SshDisconnectResult>()
    )]
    async fn ssh_disconnect(
        &self,
        Parameters(args): Parameters<SshDisconnectArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_disconnect",
            fingerprint_args(&args),
            || async {
                match self
                    .use_cases
                    .disconnect
                    .execute(DisconnectRequest {
                        session_id: SessionId::new(args.session_id),
                    })
                    .await
                {
                    Ok(outcome) => {
                        let structured = render::connection::disconnect_structured(&outcome);
                        let body = render::connection::disconnect_render(&outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error_smart(
                        "SSH_DISCONNECT",
                        &err,
                        self.id_lister.as_ref(),
                    )
                    .await),
                }
            },
        )
        .await
    }

    #[tool(
        title = "List SSH sessions",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "List active SSH sessions.\n\nCost: O(N) over current sessions. Cheap to call repeatedly.",
        output_schema = schema_for_type::<SshListSessionsResult>()
    )]
    async fn ssh_list_sessions(
        &self,
        Parameters(args): Parameters<SshListSessionsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let filter = args.agent_id.clone();
        match self
            .use_cases
            .list_sessions
            .execute(ListSessionsRequest {
                filter_agent_id: args.agent_id.map(AgentId::new),
                max_items: args.max_items,
            })
            .await
        {
            Ok(outcome) => {
                let alerts = self
                    .leak_probe
                    .as_ref()
                    .map(|p| p.current_alerts())
                    .unwrap_or_default();
                let structured = render::connection::list_sessions_structured_with_warnings(
                    &outcome,
                    filter.as_deref(),
                    &alerts,
                );
                let body = render::connection::list_sessions_render_with_warnings(outcome, &alerts);
                Ok(ok_text_and_structured(body, structured))
            }
            Err(err) => Ok(render_tool_error("SSH_LIST_SESSIONS", &err)),
        }
    }

    #[tool(
        title = "Disconnect all agent sessions",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        ),
        description = "Disconnect every session bound to a given agent.\n\nCost: O(N) over agent sessions. Tens of ms typical.",
        output_schema = schema_for_type::<SshDisconnectAgentResult>()
    )]
    async fn ssh_disconnect_agent(
        &self,
        Parameters(args): Parameters<SshDisconnectAgentArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_disconnect_agent",
            fingerprint_args(&args),
            || async {
                match self
                    .use_cases
                    .disconnect_agent
                    .execute(DisconnectAgentRequest {
                        agent_id: AgentId::new(args.agent_id),
                    })
                    .await
                {
                    Ok(outcome) => {
                        let structured = render::connection::disconnect_agent_structured(&outcome);
                        let body = render::connection::disconnect_agent_render(&outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error("SSH_DISCONNECT_AGENT", &err)),
                }
            },
        )
        .await
    }

    // ---------- Execute domain ---------------------------------------

    #[tool(
        title = "Run remote command",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        ),
        description = "Spawn an asynchronous command on an SSH session.\n\nCost: 1 SSH channel open. Returns immediately when wait=false (default async).",
        output_schema = schema_for_type::<SshExecuteResult>()
    )]
    async fn ssh_execute(
        &self,
        Parameters(args): Parameters<SshExecuteArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_execute",
            fingerprint_args(&args),
            || async {
                let req = ExecuteRequest {
                    session_id: SessionId::new(args.session_id),
                    command: args.command,
                    timeout: args.timeout_secs.map(Duration::from_secs),
                    use_pty: args.pty.unwrap_or(false),
                    lifecycle_policy: lifecycle_from_args(args.release_when_no_subs, args.grace_ms),
                };
                match self.use_cases.execute.execute(req).await {
                    Ok(outcome) => {
                        let structured = render::execute::execute_structured(&outcome);
                        let body = render::execute::execute_render(outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => {
                        Ok(
                            render_tool_error_smart("SSH_EXECUTE", &err, self.id_lister.as_ref())
                                .await,
                        )
                    }
                }
            },
        )
        .await
    }

    #[tool(
        title = "Get command output",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Fetch the current output of an asynchronous command.\n\nCost: O(buffer). Cheap with wait=false. With wait=true blocks up to wait_timeout_secs.\n\nProgress: when `wait=true` and `_meta.progressToken` is set, mid-flight `notifications/progress` updates fire every ~5s with the running stdout byte count (best-effort).",
        output_schema = schema_for_type::<SshGetCommandOutputResult>()
    )]
    async fn ssh_get_command_output(
        &self,
        Parameters(args): Parameters<SshGetCommandOutputArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let req = GetCommandOutputRequest {
            command_id: CommandId::new(args.command_id),
            wait: args.wait.unwrap_or(false),
            wait_timeout: args.wait_timeout_secs.map(Duration::from_secs),
            max_output_bytes: args.max_output_bytes,
        };
        let emitter = ProgressEmitter::new(&ctx);
        let leak_bridge = spawn_leak_warn_bridge_if_wired(&emitter, self.leak_watcher.as_ref());
        let use_case = self.use_cases.get_command_output.as_ref();
        let streams = use_case.streams();
        let outcome = drive_with_command_progress(use_case, streams, req, emitter).await;
        leak_bridge.shutdown().await;
        match outcome {
            Ok(result) => {
                let structured = render::execute::get_command_output_structured(&result);
                let body = render::execute::get_command_output_render(result);
                Ok(ok_text_and_structured(body, structured))
            }
            Err(err) => Ok(render_tool_error_smart(
                "SSH_GET_COMMAND_OUTPUT",
                &err,
                self.id_lister.as_ref(),
            )
            .await),
        }
    }

    #[tool(
        title = "List async commands",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "List asynchronous commands tracked on the server.\n\nCost: O(N) over async commands. Cheap.",
        output_schema = schema_for_type::<SshListCommandsResult>()
    )]
    async fn ssh_list_commands(
        &self,
        Parameters(args): Parameters<SshListCommandsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let req = ListCommandsRequest {
            filter_session_id: args.session_id.map(SessionId::new),
            filter_status: args
                .status
                .map(super::args::execute::CommandStatus::into_domain),
            max_items: args.max_items,
        };
        match self.use_cases.list_commands.execute(req).await {
            Ok(outcome) => {
                let alerts = self
                    .leak_probe
                    .as_ref()
                    .map(|p| p.current_alerts())
                    .unwrap_or_default();
                let structured =
                    render::execute::list_commands_structured_with_warnings(&outcome, &alerts);
                let body = render::execute::list_commands_render_with_warnings(outcome, &alerts);
                Ok(ok_text_and_structured(body, structured))
            }
            Err(err) => Ok(render_tool_error("SSH_LIST_COMMANDS", &err)),
        }
    }

    #[tool(
        title = "Cancel running command",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        ),
        description = "Cancel an asynchronous command.\n\nCost: O(1). Always succeeds (NOOP for already-finished commands).",
        output_schema = schema_for_type::<SshCancelCommandResult>()
    )]
    async fn ssh_cancel_command(
        &self,
        Parameters(args): Parameters<SshCancelCommandArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_cancel_command",
            fingerprint_args(&args),
            || async {
                let req = CancelCommandRequest {
                    command_id: CommandId::new(args.command_id),
                    max_output_bytes: args.max_output_bytes,
                };
                match self.use_cases.cancel_command.execute(req).await {
                    Ok(outcome) => {
                        let structured = render::execute::cancel_command_structured(&outcome);
                        let body = render::execute::cancel_command_render(outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error_smart(
                        "SSH_CANCEL_COMMAND",
                        &err,
                        self.id_lister.as_ref(),
                    )
                    .await),
                }
            },
        )
        .await
    }

    // ---------- Shell domain -----------------------------------------

    #[tool(
        title = "Open PTY shell",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        ),
        description = "Open an interactive PTY shell.\n\nCost: 1 SSH PTY allocation (typical 50-500ms). One PTY per shell_id.",
        output_schema = schema_for_type::<SshShellOpenResult>()
    )]
    async fn ssh_shell_open(
        &self,
        Parameters(args): Parameters<SshShellOpenArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_shell_open",
            fingerprint_args(&args),
            || async {
                let req = OpenShellRequest {
                    session_id: SessionId::new(args.session_id),
                    term: args.term,
                    cols: args.cols,
                    rows: args.rows,
                    inactivity_ttl_secs: args.inactivity_ttl,
                    max_buffer_size: parse_human_bytes(args.max_buffer_size.as_deref()),
                    lifecycle_policy: lifecycle_from_args(args.release_when_no_subs, args.grace_ms),
                };
                match self.use_cases.open_shell.execute(req).await {
                    Ok(outcome) => {
                        let streams = self.use_cases.read_shell.streams();
                        let peek =
                            peek_initial_shell_buffer(streams.as_ref(), &outcome.shell.id).await;
                        let bytes_ref = peek.as_ref().map(|p| p.bytes.as_slice());
                        let structured =
                            render::shell::shell_open_structured_with_initial(&outcome, bytes_ref);
                        let body =
                            render::shell::shell_open_render_with_initial(outcome, bytes_ref);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error_smart(
                        "SSH_SHELL_OPEN",
                        &err,
                        self.id_lister.as_ref(),
                    )
                    .await),
                }
            },
        )
        .await
    }

    #[tool(
        title = "Write to PTY shell",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        ),
        description = "Write raw bytes to a PTY shell.\n\nCost: O(input.len). Sub-ms typical. Subscribe to shell://<id>/output for response.",
        output_schema = schema_for_type::<SshShellWriteResult>()
    )]
    async fn ssh_shell_write(
        &self,
        Parameters(args): Parameters<SshShellWriteArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_shell_write",
            fingerprint_args(&args),
            || async {
                let req = WriteShellRequest {
                    shell_id: ShellId::new(args.shell_id),
                    bytes: Bytes::from(args.input.into_bytes()),
                };
                match self.use_cases.write_shell.execute(req).await {
                    Ok(outcome) => {
                        let structured = render::shell::shell_write_structured(&outcome);
                        let body = render::shell::shell_write_render(outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error_smart(
                        "SSH_SHELL_WRITE",
                        &err,
                        self.id_lister.as_ref(),
                    )
                    .await),
                }
            },
        )
        .await
    }

    #[tool(
        title = "Send keystroke to PTY",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        ),
        description = "Send a named keystroke (with optional modifiers) to a PTY shell.\n\nCost: O(repeat). Sub-ms typical. Subscribe to shell://<id>/output for response.",
        output_schema = schema_for_type::<SshShellSendKeyResult>()
    )]
    async fn ssh_shell_send_key(
        &self,
        Parameters(args): Parameters<SshShellSendKeyArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_shell_send_key",
            fingerprint_args(&args),
            || async {
                let req = SendKeyRequest {
                    shell_id: ShellId::new(args.shell_id),
                    key: args.key,
                    modifiers: pick_modifiers(args.shift, args.alt, args.ctrl),
                    repeat: args.repeat.unwrap_or(1),
                };
                match self.use_cases.send_key.execute(req).await {
                    Ok(outcome) => {
                        let structured = render::shell::shell_send_key_structured(&outcome);
                        let body = render::shell::shell_send_key_render(outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error_smart(
                        "SSH_SHELL_SEND_KEY",
                        &err,
                        self.id_lister.as_ref(),
                    )
                    .await),
                }
            },
        )
        .await
    }

    #[tool(
        title = "Read PTY buffer",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Read the buffered output of a PTY shell.\n\nCost: O(buffer). Cheap with wait=false. With wait=true blocks up to wait_timeout_secs.",
        output_schema = schema_for_type::<SshShellReadResult>()
    )]
    async fn ssh_shell_read(
        &self,
        Parameters(args): Parameters<SshShellReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        let cleared = args.clear.unwrap_or(true);
        let req = ReadShellRequest {
            shell_id: ShellId::new(args.shell_id),
            clear: cleared,
            max_output_bytes: args.max_output_bytes,
            wait: args.wait.unwrap_or(false),
            wait_timeout: args.wait_timeout_secs.map(Duration::from_secs),
            min_bytes: args.min_bytes,
        };
        match self.use_cases.read_shell.execute(req).await {
            Ok(outcome) => {
                let structured = render::shell::shell_read_structured(&outcome, cleared);
                let body = render::shell::shell_read_render(outcome);
                Ok(ok_text_and_structured(body, structured))
            }
            Err(err) => {
                Ok(render_tool_error_smart("SSH_SHELL_READ", &err, self.id_lister.as_ref()).await)
            }
        }
    }

    #[tool(
        title = "Wait for shell pattern",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Block until a substring pattern appears in the shell output.\n\nCost: blocks up to timeout_secs. Use for single-shot prompt gating.\n\nProgress: when `_meta.progressToken` is set, emits `notifications/progress` once per second carrying `(elapsed_secs, timeout_secs)` while the loop runs (best-effort).",
        output_schema = schema_for_type::<SshShellWaitForResult>()
    )]
    async fn ssh_shell_wait_for(
        &self,
        Parameters(args): Parameters<SshShellWaitForArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let req = WaitForPatternRequest {
            shell_id: ShellId::new(args.shell_id),
            patterns: args.patterns,
            timeout: args.timeout_secs.map(Duration::from_secs),
            max_output_bytes: args.max_output_bytes,
            clear: args.clear.unwrap_or(true),
        };
        let emitter = ProgressEmitter::new(&ctx);
        let leak_bridge = spawn_leak_warn_bridge_if_wired(&emitter, self.leak_watcher.as_ref());
        let outcome =
            drive_with_wait_for_progress(self.use_cases.wait_for_pattern.as_ref(), req, emitter)
                .await;
        leak_bridge.shutdown().await;
        match outcome {
            Ok(outcome) => {
                let structured = render::shell::shell_wait_for_structured(&outcome);
                let body = render::shell::shell_wait_for_render(&outcome);
                Ok(ok_text_and_structured(body, structured))
            }
            Err(err) => {
                Ok(
                    render_tool_error_smart("SSH_SHELL_WAIT_FOR", &err, self.id_lister.as_ref())
                        .await,
                )
            }
        }
    }

    #[tool(
        title = "Close PTY shell",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        ),
        description = "Close a PTY shell.\n\nCost: O(1). Always succeeds.",
        output_schema = schema_for_type::<SshShellCloseResult>()
    )]
    async fn ssh_shell_close(
        &self,
        Parameters(args): Parameters<SshShellCloseArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_shell_close",
            fingerprint_args(&args),
            || async {
                match self
                    .use_cases
                    .close_shell
                    .execute(CloseShellRequest {
                        shell_id: ShellId::new(args.shell_id),
                    })
                    .await
                {
                    Ok(outcome) => {
                        let structured = render::shell::shell_close_structured(&outcome);
                        let body = render::shell::shell_close_render(&outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => Ok(render_tool_error_smart(
                        "SSH_SHELL_CLOSE",
                        &err,
                        self.id_lister.as_ref(),
                    )
                    .await),
                }
            },
        )
        .await
    }

    // ---------- SFTP domain ------------------------------------------

    #[tool(
        title = "Upload file via SFTP",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        ),
        description = "Upload a local file to the remote host via SFTP.\n\nCost: O(file.size). Returns immediately, transfer runs async. Subscribe to transfer://<id>/progress.",
        output_schema = schema_for_type::<SshUploadResult>()
    )]
    async fn ssh_upload(
        &self,
        Parameters(args): Parameters<SshUploadArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_upload",
            fingerprint_args(&args),
            || async {
                let req = UploadRequest {
                    session_id: SessionId::new(args.session_id),
                    local_path: args.local_path,
                    remote_path: args.remote_path,
                    lifecycle_policy: lifecycle_from_args(args.release_when_no_subs, args.grace_ms),
                };
                match self.use_cases.upload_file.execute(req).await {
                    Ok(outcome) => {
                        let structured = render::sftp::upload_structured(&outcome);
                        let body = render::sftp::upload_render(outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => {
                        Ok(
                            render_tool_error_smart("SSH_UPLOAD", &err, self.id_lister.as_ref())
                                .await,
                        )
                    }
                }
            },
        )
        .await
    }

    #[tool(
        title = "Download file via SFTP",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        ),
        description = "Download a remote file via SFTP.\n\nCost: O(file.size). Returns immediately, transfer runs async. Subscribe to transfer://<id>/progress.",
        output_schema = schema_for_type::<SshDownloadResult>()
    )]
    async fn ssh_download(
        &self,
        Parameters(args): Parameters<SshDownloadArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_download",
            fingerprint_args(&args),
            || async {
                let req = DownloadRequest {
                    session_id: SessionId::new(args.session_id),
                    remote_path: args.remote_path,
                    local_path: args.local_path,
                    lifecycle_policy: lifecycle_from_args(args.release_when_no_subs, args.grace_ms),
                };
                match self.use_cases.download_file.execute(req).await {
                    Ok(outcome) => {
                        let structured = render::sftp::download_structured(&outcome);
                        let body = render::sftp::download_render(outcome);
                        Ok(ok_text_and_structured(body, structured))
                    }
                    Err(err) => {
                        Ok(
                            render_tool_error_smart("SSH_DOWNLOAD", &err, self.id_lister.as_ref())
                                .await,
                        )
                    }
                }
            },
        )
        .await
    }

    #[tool(
        title = "Get transfer progress",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Snapshot the progress of an SFTP transfer.\n\nCost: O(1). Cheap with wait=false. With wait=true blocks until done or wait_timeout_secs.\n\nProgress: when `wait=true` and `_meta.progressToken` is set, mid-flight `notifications/progress` updates fire every ~5s carrying `(bytes_transferred, total_bytes)` (best-effort).",
        output_schema = schema_for_type::<SshGetTransferProgressResult>()
    )]
    async fn ssh_get_transfer_progress(
        &self,
        Parameters(args): Parameters<SshGetTransferProgressArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let req = GetTransferProgressRequest {
            transfer_id: TransferId::new(args.transfer_id),
            wait: args.wait.unwrap_or(false),
            wait_timeout: args.wait_timeout_secs.map(Duration::from_secs),
        };
        let emitter = ProgressEmitter::new(&ctx);
        let leak_bridge = spawn_leak_warn_bridge_if_wired(&emitter, self.leak_watcher.as_ref());
        let use_case = self.use_cases.get_transfer_progress.as_ref();
        let transfers = use_case.transfers();
        let outcome = drive_with_transfer_progress(use_case, transfers, req, emitter).await;
        leak_bridge.shutdown().await;
        match outcome {
            Ok(result) => {
                let structured = render::sftp::transfer_progress_structured(&result);
                let body = render::sftp::transfer_progress_render(&result);
                Ok(ok_text_and_structured(body, structured))
            }
            Err(err) => Ok(render_tool_error_smart(
                "SSH_GET_TRANSFER_PROGRESS",
                &err,
                self.id_lister.as_ref(),
            )
            .await),
        }
    }

    // ---------- v4.7-step3 convenience + batch tools -----------------

    #[tool(
        title = "Run remote command (one-shot)",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        ),
        description = "Connect, execute a short command synchronously, and (by default) disconnect.\n\nCost: 1 SSH handshake + 1 channel + (optional) disconnect. Returns when the command finishes or timeout_secs expires.",
        output_schema = schema_for_type::<SshRunResult>()
    )]
    async fn ssh_run(
        &self,
        Parameters(args): Parameters<SshRunArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_run",
            fingerprint_args(&args),
            || async {
                run_one_shot(
                    self.use_cases.connect.as_ref(),
                    self.use_cases.execute.as_ref(),
                    self.use_cases.get_command_output.as_ref(),
                    self.use_cases.disconnect.as_ref(),
                    args,
                )
                .await
            },
        )
        .await
    }

    #[tool(
        title = "Execute a batch of commands on one session",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        ),
        description = "Run up to 16 commands sequentially on a single session.\n\nCost: 1 SSH channel per command. Stops early on first non-zero exit by default.",
        output_schema = schema_for_type::<SshExecuteBatchResult>()
    )]
    async fn ssh_execute_batch(
        &self,
        Parameters(args): Parameters<SshExecuteBatchArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_execute_batch",
            fingerprint_args(&args),
            || async {
                run_execute_batch(
                    self.use_cases.execute.as_ref(),
                    self.use_cases.get_command_output.as_ref(),
                    args,
                )
                .await
            },
        )
        .await
    }

    #[tool(
        title = "Disconnect multiple SSH sessions",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        ),
        description = "Best-effort bulk disconnect of up to 64 sessions.\n\nCost: O(N) disconnect calls. Best-effort: per-id failures do not abort the batch.",
        output_schema = schema_for_type::<SshDisconnectManyResult>()
    )]
    async fn ssh_disconnect_many(
        &self,
        Parameters(args): Parameters<SshDisconnectManyArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_disconnect_many",
            fingerprint_args(&args),
            || async { run_disconnect_many(self.use_cases.disconnect.as_ref(), args).await },
        )
        .await
    }

    // ---------- Subscription administration (v5 Phase 3) -------------

    #[tool(
        title = "Subscribe to a resource lane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        ),
        description = "Open a Channel Mux lane for a ssh-mcp resource URI.\n\nCleanup: ssh_unsubscribe sub_id=... when done. Skip and the lane becomes a zombie.\n\nCost: O(1) lane open + per-event mpsc.\n\nIdempotency: pass `_meta.idempotency_key` to dedup retries.\n\nHygiene: hold the SUB_ID; never re-open the same URI without first unsubscribing."
    )]
    async fn ssh_subscribe(
        &self,
        Parameters(args): Parameters<SshSubscribeArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_subscribe",
            fingerprint_args(&args),
            || async { run_sub_subscribe(self.use_cases.sub_subscribe.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "Unsubscribe from a resource lane",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        ),
        description = "Close a Channel Mux lane previously opened with ssh_subscribe.\n\nCost: O(1)."
    )]
    async fn ssh_unsubscribe(
        &self,
        Parameters(args): Parameters<SshUnsubscribeArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_unsubscribe",
            fingerprint_args(&args),
            || async { run_sub_unsubscribe(self.use_cases.sub_unsubscribe.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "Pause a subscription lane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Suspend lane drain.\n\nCost: O(1)."
    )]
    async fn ssh_sub_pause(
        &self,
        Parameters(args): Parameters<SshSubPauseArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_sub_pause",
            fingerprint_args(&args),
            || async { run_sub_pause(self.use_cases.sub_pause.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "Resume a subscription lane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Resume lane drain.\n\nCost: O(1)."
    )]
    async fn ssh_sub_resume(
        &self,
        Parameters(args): Parameters<SshSubResumeArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_sub_resume",
            fingerprint_args(&args),
            || async { run_sub_resume(self.use_cases.sub_resume.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "Hot-reload a subscription filter",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Replace the lane regex filter.\n\nCost: 1 regex compile + atomic swap."
    )]
    async fn ssh_sub_filter(
        &self,
        Parameters(args): Parameters<SshSubFilterArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_sub_filter",
            fingerprint_args(&args),
            || async { run_sub_filter(self.use_cases.sub_filter.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "Replay a subscription lane from cursor",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Re-emit lane events from `from_cursor`.\n\nCost: O(window-bytes)."
    )]
    async fn ssh_sub_replay(
        &self,
        Parameters(args): Parameters<SshSubReplayArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        with_idempotency(
            &self.idempotency,
            &ctx,
            "ssh_sub_replay",
            fingerprint_args(&args),
            || async { run_sub_replay(self.use_cases.sub_replay.as_ref(), args).await },
        )
        .await
    }

    #[tool(
        title = "List active subscription lanes",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Snapshot the per-`SubId` lane registry.\n\nCost: O(N) over open lanes."
    )]
    async fn ssh_sub_list(
        &self,
        Parameters(args): Parameters<SshSubListArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(run_sub_list(self.use_cases.sub_list.as_ref(), args))
    }

    #[tool(
        title = "Snapshot subscription lane stats",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Per-lane atomic counter snapshot.\n\nCost: O(1)."
    )]
    async fn ssh_sub_stats(
        &self,
        Parameters(args): Parameters<SshSubStatsArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(run_sub_stats(self.use_cases.sub_stats.as_ref(), args))
    }

    #[tool(
        title = "Aggregate daemon-wide subscription stats",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        ),
        description = "Sum / max of every per-lane atomic counter.\n\nCost: O(N) over open lanes."
    )]
    async fn ssh_daemon_stats(
        &self,
        Parameters(_args): Parameters<SshDaemonStatsArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(run_daemon_stats(self.use_cases.daemon_stats.as_ref()))
    }
}

// ---------------------------------------------------------------------------
// Free-standing connect helper shared between the two `tool_router` impls
// ---------------------------------------------------------------------------

/// Drive the connect-session use case from the rmcp `SshConnectArgs`.
/// Pulled out as a free function so both feature flavours of the
/// `#[tool_router]` impl can call it without duplicating the body.
async fn run_connect<S, R, C, I, Cfg>(
    use_case: &ConnectSessionUseCase<S, R, C, I, Cfg>,
    args: SshConnectArgs,
) -> Result<CallToolResult, McpError>
where
    S: SshClientPort + Send + Sync,
    R: SessionRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    I: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    let req = match build_connect_request(args) {
        Ok(r) => r,
        Err(err) => return Ok(render_tool_error("SSH_CONNECT", &err)),
    };
    match use_case.execute(req).await {
        Ok(outcome) => {
            let structured = render::connection::connect_structured(&outcome);
            let body = render::connection::connect_render(outcome);
            Ok(ok_text_and_structured(body, structured))
        }
        Err(err) => Ok(render_tool_error("SSH_CONNECT", &err)),
    }
}

/// Translate the rmcp `SshConnectArgs` payload into a domain
/// [`ConnectRequest`]. Address parsing failures surface as
/// [`DomainError::InvalidArgument`] so the caller renders the
/// canonical error block.
fn build_connect_request(args: SshConnectArgs) -> Result<ConnectRequest, DomainError> {
    let address = parse_address(&args.address)?;
    let credentials = pick_credentials(
        &args.username,
        args.password.as_deref(),
        args.key_path.as_deref(),
    );
    Ok(ConnectRequest {
        explicit_session_id: args.session_id.map(SessionId::new),
        address,
        username: args.username,
        credentials,
        timeout_secs: args.timeout_secs,
        max_retries: args.max_retries,
        retry_delay_ms: args.retry_delay_ms,
        compress: args.compress,
        name: args.name,
        persistent: args.persistent.unwrap_or(false),
        agent_id: args.agent_id.map(AgentId::new),
        reuse: args.reuse.map_or(
            DomainReusePolicy::Suggest,
            super::args::connection::ReusePolicy::into_domain,
        ),
    })
}

/// Parse a v3 human-byte string (`512k`, `10m`, `1g`, `1t`) into an
/// absolute byte count. Returns `None` for an unparseable input or
/// missing argument so the use case falls back to the configured
/// default.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "human-byte parser only operates on integer suffix tables; the cast surface is bounded by the multiplier table and never crosses 2^63"
)]
fn parse_human_bytes(input: Option<&str>) -> Option<u64> {
    let raw = input?.trim();
    if raw.is_empty() {
        return None;
    }
    let (digits, suffix): (String, String) =
        raw.chars().partition(|c| c.is_ascii_digit() || *c == '.');
    let multiplier: u64 = match suffix.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1024,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        "t" | "tb" => 1024_u64 * 1024 * 1024 * 1024,
        _ => return None,
    };
    let value: f64 = digits.parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some((value * multiplier as f64) as u64)
}

// ---------------------------------------------------------------------------
// v4.7-step3 — ssh_run / ssh_execute_batch / ssh_disconnect_many helpers
// ---------------------------------------------------------------------------

/// Hard cap on `ssh_run` / `ssh_execute_batch` per-command wait
/// timeout. Mirrors the documented "Cap: 300" line on the args
/// schema; outside the rmcp wrapper the use case has no notion of
/// caller intent so the budget is enforced here.
const RUN_TIMEOUT_CAP_SECS: u64 = 300;
/// Hard cap on the per-command output budget. Mirrors the documented
/// `SSH_MCP_OUTPUT_MAX_BYTES_CAP` ceiling for a single response.
const RUN_OUTPUT_BYTES_CAP: usize = 1_048_576;
/// Maximum number of commands accepted in a single
/// `ssh_execute_batch` call.
const EXECUTE_BATCH_MAX_COMMANDS: usize = 16;
/// Maximum number of session ids accepted in a single
/// `ssh_disconnect_many` call.
const DISCONNECT_MANY_MAX_IDS: usize = 64;

/// Clamp the caller-supplied `timeout_secs` to [`RUN_TIMEOUT_CAP_SECS`]
/// and return the resulting [`Duration`]. `None` defaults to 30s
/// (mirrors the schema default) and is also clamped.
fn clamp_run_timeout(secs: Option<u64>) -> Duration {
    let raw = secs.unwrap_or(30);
    Duration::from_secs(raw.min(RUN_TIMEOUT_CAP_SECS))
}

/// Clamp the caller-supplied `max_output_bytes` to
/// [`RUN_OUTPUT_BYTES_CAP`]. `None` defaults to 16384.
fn clamp_run_output_bytes(bytes: Option<usize>) -> usize {
    bytes.unwrap_or(16_384).min(RUN_OUTPUT_BYTES_CAP)
}

/// Build the `ConnectRequest` driving the implicit connect step of
/// `ssh_run`. Uses `reuse=Auto` so repeated calls converge on the
/// same long-lived session per host/user/agent triple.
fn build_run_connect_request(args: &SshRunArgs) -> Result<ConnectRequest, DomainError> {
    let address = parse_address(&args.address)?;
    let credentials = pick_credentials(
        &args.username,
        args.password.as_deref(),
        args.key_path.as_deref(),
    );
    Ok(ConnectRequest {
        explicit_session_id: None,
        address,
        username: args.username.clone(),
        credentials,
        timeout_secs: None,
        max_retries: None,
        retry_delay_ms: None,
        compress: None,
        name: None,
        persistent: false,
        agent_id: args.agent_id.clone().map(AgentId::new),
        reuse: DomainReusePolicy::Auto,
    })
}

/// Pull a usable `SessionId` out of the connect outcome. `Suggested`
/// is unreachable (`reuse=Auto` returns `Reused` instead); kept as a
/// defensive arm so a future use case change cannot silently break the
/// orchestration.
fn session_id_from_connect(outcome: ConnectOutcome) -> Result<SessionId, DomainError> {
    match outcome {
        ConnectOutcome::Connected { session, .. } | ConnectOutcome::Reused { session, .. } => {
            Ok(session.id)
        }
        ConnectOutcome::Suggested { .. } => Err(DomainError::Internal(
            "ssh_run: unexpected SUGGESTED outcome with reuse=Auto".to_string(),
        )),
    }
}

/// Drive the spawn -> wait pair backing both `ssh_run` and one
/// iteration of `ssh_execute_batch`. Returns the terminal command
/// snapshot the caller renders into the response body.
async fn spawn_and_wait_for_command<S, SR, CR, OS, C, Idg, Cfg, Sub>(
    execute: &ExecuteCommandUseCase<S, SR, CR, C, Idg, Cfg, Sub>,
    output: &GetCommandOutputUseCase<CR, OS>,
    session_id: &SessionId,
    command: String,
    pty: bool,
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<GetCommandOutputResult, DomainError>
where
    S: SshClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    C: ClockPort + Send + Sync,
    Idg: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
    Sub: SubscriberRegistryPort + Send + Sync,
{
    let exec_outcome = execute
        .execute(ExecuteRequest {
            session_id: session_id.clone(),
            command,
            timeout: Some(timeout),
            use_pty: pty,
            lifecycle_policy: None,
        })
        .await?;
    output
        .execute(GetCommandOutputRequest {
            command_id: exec_outcome.command_id,
            wait: true,
            wait_timeout: Some(timeout),
            max_output_bytes: Some(max_output_bytes),
        })
        .await
}

/// Resolve the `ssh_connect` step driven by `ssh_run`. Returns the
/// freshly minted (or reused) session id, mapping every error class
/// into the tool error body the orchestrator forwards to the caller.
async fn connect_for_run<S, SR, C, Idg, Cfg>(
    connect: &ConnectSessionUseCase<S, SR, C, Idg, Cfg>,
    args: &SshRunArgs,
) -> Result<SessionId, CallToolResult>
where
    S: SshClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    C: ClockPort + Send + Sync,
    Idg: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    let req = build_run_connect_request(args).map_err(|e| render_tool_error("SSH_RUN", &e))?;
    let outcome = connect
        .execute(req)
        .await
        .map_err(|e| render_tool_error("SSH_RUN", &e))?;
    session_id_from_connect(outcome).map_err(|e| render_tool_error("SSH_RUN", &e))
}

/// Drive the full `connect -> execute -> wait -> [disconnect]`
/// orchestration backing `ssh_run`.
async fn run_one_shot<S, SR, CR, ShR, TR, OS, C, Idg, Cfg, Sub>(
    connect: &ConnectSessionUseCase<S, SR, C, Idg, Cfg>,
    execute: &ExecuteCommandUseCase<S, SR, CR, C, Idg, Cfg, Sub>,
    output: &GetCommandOutputUseCase<CR, OS>,
    disconnect: &DisconnectSessionUseCase<S, SR, CR, ShR, TR>,
    args: SshRunArgs,
) -> Result<CallToolResult, McpError>
where
    S: SshClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    C: ClockPort + Send + Sync,
    Idg: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
    Sub: SubscriberRegistryPort + Send + Sync,
{
    let timeout = clamp_run_timeout(args.timeout_secs);
    let output_cap = clamp_run_output_bytes(args.max_output_bytes);
    let pty = args.pty.unwrap_or(false);
    let disconnect_after = args.disconnect_after.unwrap_or(true);
    let session_id = match connect_for_run(connect, &args).await {
        Ok(sid) => sid,
        Err(body) => return Ok(body),
    };
    let result = match spawn_and_wait_for_command(
        execute,
        output,
        &session_id,
        args.command,
        pty,
        timeout,
        output_cap,
    )
    .await
    {
        Ok(r) => r,
        Err(err) => return Ok(render_tool_error("SSH_RUN", &err)),
    };
    let disconnected = run_disconnect_after(disconnect, &session_id, disconnect_after).await;
    Ok(build_run_response(
        &result,
        session_id.as_str(),
        disconnected,
    ))
}

/// Disconnect the session if `disconnect_after` is true, otherwise
/// keep it open. Disconnect failures are tolerated — the command
/// already ran and the user-visible signal (the rendered result) is
/// the actionable signal.
async fn run_disconnect_after<S, SR, CR, ShR, TR>(
    disconnect: &DisconnectSessionUseCase<S, SR, CR, ShR, TR>,
    session_id: &SessionId,
    disconnect_after: bool,
) -> bool
where
    S: SshClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
{
    if !disconnect_after {
        return false;
    }
    let _ = disconnect
        .execute(DisconnectRequest {
            session_id: session_id.clone(),
        })
        .await;
    true
}

/// Compose the `ssh_run` success [`CallToolResult`] from the captured
/// snapshot. Pulled out so the orchestrator stays under the 30-line
/// cognitive threshold.
fn build_run_response(
    result: &GetCommandOutputResult,
    session_id: &str,
    disconnected: bool,
) -> CallToolResult {
    let body = render::execute::run_render(result, session_id, disconnected);
    let structured = render::execute::run_structured(result, session_id, disconnected);
    ok_text_and_structured(body, structured)
}

/// Validate the batch input. Returns the rendered error body when the
/// caller passed an empty list or more than [`EXECUTE_BATCH_MAX_COMMANDS`]
/// entries; otherwise `Ok(())` and the orchestrator continues.
fn validate_batch_args(args: &SshExecuteBatchArgs) -> Result<(), CallToolResult> {
    if args.commands.is_empty() {
        return Err(render_tool_error(
            "SSH_EXECUTE_BATCH",
            &DomainError::InvalidArgument("commands must contain at least one entry".to_string()),
        ));
    }
    if args.commands.len() > EXECUTE_BATCH_MAX_COMMANDS {
        return Err(render_tool_error(
            "SSH_EXECUTE_BATCH",
            &DomainError::InvalidArgument(format!(
                "commands accepts up to {EXECUTE_BATCH_MAX_COMMANDS} entries, got {}",
                args.commands.len()
            )),
        ));
    }
    Ok(())
}

/// Drive the sequential per-command loop backing `ssh_execute_batch`.
async fn run_execute_batch<S, SR, CR, OS, C, Idg, Cfg, Sub>(
    execute: &ExecuteCommandUseCase<S, SR, CR, C, Idg, Cfg, Sub>,
    output: &GetCommandOutputUseCase<CR, OS>,
    args: SshExecuteBatchArgs,
) -> Result<CallToolResult, McpError>
where
    S: SshClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    C: ClockPort + Send + Sync,
    Idg: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
    Sub: SubscriberRegistryPort + Send + Sync,
{
    if let Err(body) = validate_batch_args(&args) {
        return Ok(body);
    }
    let session_id = SessionId::new(args.session_id.clone());
    let outcome = drive_batch_loop(
        execute,
        output,
        &session_id,
        &args.commands,
        BatchSettings {
            stop_on_failure: args.stop_on_failure.unwrap_or(true),
            pty: args.pty.unwrap_or(false),
            timeout: clamp_run_timeout(args.timeout_secs_per_command),
            output_cap: clamp_run_output_bytes(args.max_output_bytes_per_command),
        },
    )
    .await;
    let body = render_batch_body(&args.session_id, &args.commands, &outcome);
    let structured = render_batch_structured(&args.session_id, &args.commands, &outcome);
    Ok(ok_text_and_structured(body, structured))
}

/// Per-iteration outcome captured during the batch loop. The caller
/// uses [`Self::failed`] to decide whether to halt and the renderer
/// converts each entry into the matching Markdown / structured shape.
struct BatchIteration {
    /// Index into the input `commands` array.
    index: usize,
    /// The terminal snapshot returned by `get_command_output`.
    result: GetCommandOutputResult,
}

impl BatchIteration {
    /// `true` when the command's terminal state is anything other than
    /// `Completed { exit_code: 0 }` — the canonical "failure" trigger
    /// for `stop_on_failure`.
    fn failed(&self) -> bool {
        if self.result.timed_out {
            return true;
        }
        match self.result.status {
            CommandStatus::Completed => self.result.exit_code != Some(0),
            CommandStatus::Cancelled | CommandStatus::Failed | CommandStatus::Running => true,
        }
    }
}

/// Aggregated loop result returned by [`drive_batch_loop`].
struct BatchOutcome {
    /// Successful per-command iterations, in order.
    iterations: Vec<BatchIteration>,
    /// `true` when the loop exited early because of a non-zero exit
    /// code under `stop_on_failure=true`.
    halted: bool,
    /// Optional fatal use case error surfaced before the loop ran (or
    /// before any iteration captured a snapshot).
    fatal: Option<DomainError>,
}

/// Caller-tunable knobs for the batch loop. Keeping the four flags +
/// caps grouped here lets [`drive_batch_loop`] respect the
/// `too_many_arguments` lint without losing any control surface.
#[derive(Debug, Clone, Copy)]
struct BatchSettings {
    /// Halt on the first non-zero exit code.
    stop_on_failure: bool,
    /// Allocate a PTY for each spawned command.
    pty: bool,
    /// Per-command wait budget.
    timeout: Duration,
    /// Per-command max output bytes.
    output_cap: usize,
}

/// Outcome of one iteration of the batch loop. `Halt` short-circuits
/// the loop under `stop_on_failure`; `Fatal` carries a use case error
/// surfaced before the iteration captured a snapshot.
enum BatchStep {
    Continue(BatchIteration),
    Halt(BatchIteration),
    Fatal(DomainError),
}

async fn drive_batch_loop<S, SR, CR, OS, C, Idg, Cfg, Sub>(
    execute: &ExecuteCommandUseCase<S, SR, CR, C, Idg, Cfg, Sub>,
    output: &GetCommandOutputUseCase<CR, OS>,
    session_id: &SessionId,
    commands: &[String],
    settings: BatchSettings,
) -> BatchOutcome
where
    S: SshClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    C: ClockPort + Send + Sync,
    Idg: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
    Sub: SubscriberRegistryPort + Send + Sync,
{
    let mut iterations: Vec<BatchIteration> = Vec::with_capacity(commands.len());
    for (index, command) in commands.iter().enumerate() {
        let step = run_batch_step(execute, output, session_id, &settings, index, command).await;
        match step {
            BatchStep::Continue(iter) => iterations.push(iter),
            BatchStep::Halt(iter) => {
                iterations.push(iter);
                return BatchOutcome {
                    iterations,
                    halted: true,
                    fatal: None,
                };
            }
            BatchStep::Fatal(err) => {
                return BatchOutcome {
                    iterations,
                    halted: false,
                    fatal: Some(err),
                };
            }
        }
    }
    BatchOutcome {
        iterations,
        halted: false,
        fatal: None,
    }
}

/// Drive one batch iteration: spawn + wait + classify the snapshot.
async fn run_batch_step<S, SR, CR, OS, C, Idg, Cfg, Sub>(
    execute: &ExecuteCommandUseCase<S, SR, CR, C, Idg, Cfg, Sub>,
    output: &GetCommandOutputUseCase<CR, OS>,
    session_id: &SessionId,
    settings: &BatchSettings,
    index: usize,
    command: &str,
) -> BatchStep
where
    S: SshClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    C: ClockPort + Send + Sync,
    Idg: IdGeneratorPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
    Sub: SubscriberRegistryPort + Send + Sync,
{
    let outcome = spawn_and_wait_for_command(
        execute,
        output,
        session_id,
        command.to_string(),
        settings.pty,
        settings.timeout,
        settings.output_cap,
    )
    .await;
    match outcome {
        Ok(result) => {
            let iteration = BatchIteration { index, result };
            if settings.stop_on_failure && iteration.failed() {
                BatchStep::Halt(iteration)
            } else {
                BatchStep::Continue(iteration)
            }
        }
        Err(err) => BatchStep::Fatal(err),
    }
}

fn render_batch_body(session_id: &str, commands: &[String], outcome: &BatchOutcome) -> String {
    if let Some(err) = outcome.fatal.as_ref() {
        return render_tool_error_body("SSH_EXECUTE_BATCH", err);
    }
    let entries = build_batch_views(commands, outcome);
    render::execute::batch_render(
        session_id,
        outcome.halted,
        outcome.iterations.len(),
        commands.len(),
        &entries,
    )
}

fn render_batch_structured(
    session_id: &str,
    commands: &[String],
    outcome: &BatchOutcome,
) -> serde_json::Value {
    if let Some(err) = outcome.fatal.as_ref() {
        let (code, reason, detail) = classify_error(err);
        return format_error_structured("SSH_EXECUTE_BATCH", code, &reason, detail.as_deref());
    }
    let results = build_batch_structured_entries(commands, outcome);
    let status = if outcome.halted { "halted" } else { "ok" };
    serde_json::json!({
        "tool":     "ssh_execute_batch",
        "status":   status,
        "session_id": session_id,
        "results":  results,
        "executed": outcome.iterations.len(),
        "total":    commands.len(),
    })
}

fn build_batch_structured_entries(
    commands: &[String],
    outcome: &BatchOutcome,
) -> Vec<serde_json::Value> {
    let mut results: Vec<serde_json::Value> = Vec::with_capacity(commands.len());
    let mut iter = outcome.iterations.iter();
    let mut next = iter.next();
    for (index, command) in commands.iter().enumerate() {
        if let Some(iteration) = next.filter(|i| i.index == index) {
            results.push(render::execute::batch_entry_structured(
                index,
                command,
                &iteration.result,
            ));
            next = iter.next();
        } else {
            results.push(render::execute::batch_skipped_entry(index, command));
        }
    }
    results
}

fn build_batch_views<'a>(
    commands: &'a [String],
    outcome: &'a BatchOutcome,
) -> Vec<render::execute::BatchEntryView<'a>> {
    let mut views: Vec<render::execute::BatchEntryView<'a>> = Vec::with_capacity(commands.len());
    let mut iter = outcome.iterations.iter();
    let mut next = iter.next();
    for (index, command) in commands.iter().enumerate() {
        if let Some(iteration) = next.filter(|i| i.index == index) {
            views.push(render::execute::BatchEntryView::Executed {
                index,
                command: command.as_str(),
                result: &iteration.result,
            });
            next = iter.next();
        } else {
            views.push(render::execute::BatchEntryView::Skipped {
                index,
                command: command.as_str(),
            });
        }
    }
    views
}

/// Render the canonical Markdown-only error body. Same shape as
/// [`render_tool_error`] without the structured channel — used inline
/// when the orchestrator hits a fatal use case error before producing
/// any per-command output.
fn render_tool_error_body(tool: &str, err: &DomainError) -> String {
    let (code, reason, detail) = classify_error(err);
    format_error(tool, code, &reason, detail.as_deref())
}

/// Validate the bulk disconnect input. Returns the rendered error
/// body when the caller passed an empty list or more than
/// [`DISCONNECT_MANY_MAX_IDS`] entries.
fn validate_disconnect_many_args(args: &SshDisconnectManyArgs) -> Result<(), CallToolResult> {
    if args.session_ids.is_empty() {
        return Err(render_tool_error(
            "SSH_DISCONNECT_MANY",
            &DomainError::InvalidArgument(
                "session_ids must contain at least one entry".to_string(),
            ),
        ));
    }
    if args.session_ids.len() > DISCONNECT_MANY_MAX_IDS {
        return Err(render_tool_error(
            "SSH_DISCONNECT_MANY",
            &DomainError::InvalidArgument(format!(
                "session_ids accepts up to {DISCONNECT_MANY_MAX_IDS} entries, got {}",
                args.session_ids.len()
            )),
        ));
    }
    Ok(())
}

/// Process one disconnect attempt and convert the use case outcome
/// into the per-id entry surfaced by both wire channels.
async fn disconnect_many_entry<S, SR, CR, ShR, TR>(
    disconnect: &DisconnectSessionUseCase<S, SR, CR, ShR, TR>,
    sid: String,
) -> render::connection::DisconnectManyEntry
where
    S: SshClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
{
    let result = disconnect
        .execute(DisconnectRequest {
            session_id: SessionId::new(sid.clone()),
        })
        .await;
    match result {
        Ok(_) => render::connection::DisconnectManyEntry::ok(sid),
        Err(err) => {
            let (code, reason, _) = classify_error(&err);
            render::connection::DisconnectManyEntry::error(sid, code.to_string(), reason)
        }
    }
}

/// Drive the best-effort bulk-disconnect loop backing
/// `ssh_disconnect_many`.
async fn run_disconnect_many<S, SR, CR, ShR, TR>(
    disconnect: &DisconnectSessionUseCase<S, SR, CR, ShR, TR>,
    args: SshDisconnectManyArgs,
) -> Result<CallToolResult, McpError>
where
    S: SshClientPort + Send + Sync,
    SR: SessionRepository + Send + Sync,
    CR: CommandRepository + Send + Sync,
    ShR: ShellRepository + Send + Sync,
    TR: TransferRepository + Send + Sync,
{
    if let Err(body) = validate_disconnect_many_args(&args) {
        return Ok(body);
    }
    let mut entries: Vec<render::connection::DisconnectManyEntry> =
        Vec::with_capacity(args.session_ids.len());
    for sid in args.session_ids {
        entries.push(disconnect_many_entry(disconnect, sid).await);
    }
    let body = render::connection::disconnect_many_render(&entries);
    let structured = render::connection::disconnect_many_structured(&entries);
    Ok(ok_text_and_structured(body, structured))
}

// ---------------------------------------------------------------------------
// Server identity + LLM bootstrap
// ---------------------------------------------------------------------------

/// Build the rmcp [`Implementation`] descriptor advertised on the
/// `initialize` handshake. Carries display title, free-form description,
/// and a public landing page so modern MCP hosts (Claude mobile / remote
/// clients) can render a humanised server card. Icons are intentionally
/// omitted for now — flipping them on requires a stable hosted asset URL
/// plus a tiny SVG.
//
// Field selection matches the rmcp 1.6 builder surface at
// `~/.cargo/registry/.../rmcp-1.6.0/src/model.rs:1009-1056`. The struct
// is `#[non_exhaustive]`, so we have to go through `Implementation::new`
// + the `with_*` setters.
fn build_implementation() -> Implementation {
    Implementation::new("ssh-mcp", env!("CARGO_PKG_VERSION"))
        .with_title("SSH Remote Shell")
        .with_description(
            "Run remote commands, drive PTY shells, transfer files via SFTP, \
             and forward TCP ports over SSH. Subscribe to shell, command, transfer, \
             session, and forward streams for push notifications.",
        )
        .with_website_url("https://github.com/farchanjo/ssh-mcp")
        .with_icons(vec![
            Icon::new("https://raw.githubusercontent.com/farchanjo/ssh-mcp/master/assets/icon.svg")
                .with_mime_type("image/svg+xml")
                .with_sizes(vec!["any".to_string()]),
        ])
}

/// Shared [`ServerCapabilities`] fingerprint advertised on the
/// `initialize` handshake — tools + resources + subscribe channels +
/// the v4.7-step3 prompts catalogue. Both feature flavours of the
/// server return the exact same capability set; only the tool
/// catalogue and instructions differ.
fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities::builder()
        .enable_tools()
        .enable_tool_list_changed()
        .enable_resources()
        .enable_resources_subscribe()
        .enable_resources_list_changed()
        .enable_prompts()
        .build()
}

/// Few-shot bootstrap text for the `port_forward` build (21 tools / 5
/// streams). Three canonical workflows steer 27B-class models away from
/// the most common failure modes (forgetting `wait=true`, leaking
/// sessions, polling instead of subscribing). v4.7 adds `ssh_run` +
/// batches and the `structured_content` channel.
#[cfg(feature = "port_forward")]
const INSTRUCTIONS_WITH_FORWARD: &str = "SSH MCP. 21 tools, 5 push streams \
(shell://, command://, transfer://, session://, forward://). All tools return \
block markdown (KEY: value, --- name [nonce] ---) + a typed JSON in \
structured_content. IDs end in _ID. NEXT: line lists successor tools.\n\
\n\
Happy paths:\n\
1) One-shot: ssh_run(address, username, command). Returns exit_code in one call.\n\
2) Run async: ssh_connect (agent_id, reuse=Auto). Then ssh_execute. Then \
ssh_get_command_output wait=true (subscribe command://<id>/output for push).\n\
3) Interactive shell: ssh_connect, ssh_shell_open (returns INITIAL_BUFFER if \
the prompt arrives within 100ms). Then resources/subscribe shell://<id>/output. \
Drive with ssh_shell_write or ssh_shell_send_key. Read deltas via \
resources/read?cursor=auto on each notification. ssh_shell_close, ssh_disconnect.\n\
4) Upload: ssh_upload. Then ssh_get_transfer_progress wait=true.\n\
\n\
Cleanup: agent_id on connect, ssh_disconnect_agent for bulk-close. Watch HINT \
lines and EXPIRES_AT. Pass _meta.idempotency_key on retries to dedup.";

/// Few-shot bootstrap text for the build without `port_forward`
/// (20 tools / 4 streams). Identical workflows minus the `forward://`
/// stream; the catalogue claim is dropped so callers do not look for
/// `ssh_forward`.
#[cfg(not(feature = "port_forward"))]
const INSTRUCTIONS_WITHOUT_FORWARD: &str = "SSH MCP. 20 tools, 4 push streams \
(shell://, command://, transfer://, session://). All tools return block \
markdown (KEY: value, --- name [nonce] ---) + a typed JSON in \
structured_content. IDs end in _ID. NEXT: line lists successor tools.\n\
\n\
Happy paths:\n\
1) One-shot: ssh_run(address, username, command). Returns exit_code in one call.\n\
2) Run async: ssh_connect (agent_id, reuse=Auto). Then ssh_execute. Then \
ssh_get_command_output wait=true (subscribe command://<id>/output for push).\n\
3) Interactive shell: ssh_connect, ssh_shell_open (returns INITIAL_BUFFER if \
prompt arrives within 100ms). Then resources/subscribe shell://<SHELL_ID>/output. \
Drive with ssh_shell_write or ssh_shell_send_key. Read deltas via \
resources/read?cursor=auto on each notification. ssh_shell_close, ssh_disconnect.\n\
4) Upload: ssh_upload. Then ssh_get_transfer_progress wait=true.\n\
\n\
Cleanup: agent_id on connect, ssh_disconnect_agent for bulk-close. Watch HINT \
lines and EXPIRES_AT. Pass _meta.idempotency_key on retries to dedup.";

// ---------------------------------------------------------------------------
// `#[tool_handler]` impl
// ---------------------------------------------------------------------------

#[cfg(feature = "port_forward")]
#[tool_handler]
impl<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg> ServerHandler
    for McpSshServer<UseCases<S, F, SR, CR, ShR, TR, FR, N, AS, OS, SubR, C, Cfg, Idg>>
where
    S: SshClientPort + Send + Sync + 'static,
    F: SftpClientPort + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    CR: CommandRepository + Send + Sync + 'static,
    ShR: ShellRepository + Send + Sync + 'static,
    TR: TransferRepository + Send + Sync + 'static,
    FR: ForwardRepository + Send + Sync + 'static,
    N: NotifierPort + Send + Sync + 'static,
    AS: AuthStrategyPort + Send + Sync + 'static,
    OS: OutputStreamPort + Send + Sync + 'static,
    SubR: SubscriberRegistryPort + SubscriberRegistryAsync + Send + Sync + 'static,
    C: ClockPort + Send + Sync + 'static,
    Cfg: ConfigPort + Send + Sync + 'static,
    Idg: IdGeneratorPort + Send + Sync + 'static,
{
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(server_capabilities())
            .with_protocol_version(ProtocolVersion::V_2025_06_18)
            .with_server_info(build_implementation())
            .with_instructions(INSTRUCTIONS_WITH_FORWARD)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        resource_handlers::list_resources_impl(
            &self.use_cases.list_resources,
            self.leak_probe.as_ref(),
        )
        .await
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult::with_all_items(
            resource_templates::build_list(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        resource_handlers::read_resource_impl(
            &self.use_cases.read_resource,
            request,
            &context,
            &self.peer_table,
        )
        .await
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        resource_handlers::subscribe_impl(
            &self.use_cases.subscribe_resource,
            request,
            &context,
            &self.peer_table,
        )
        .await
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        resource_handlers::unsubscribe_impl(
            &self.use_cases.unsubscribe_resource,
            request,
            &context,
            &self.peer_table,
        )
        .await
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::with_all_items(prompts::list_prompts()))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        let args = request.arguments.unwrap_or_default();
        prompts::get_prompt(&request.name, &args)
    }
}

#[cfg(not(feature = "port_forward"))]
#[tool_handler]
impl<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg> ServerHandler
    for McpSshServer<UseCases<S, F, SR, CR, ShR, TR, N, AS, OS, SubR, C, Cfg, Idg>>
where
    S: SshClientPort + Send + Sync + 'static,
    F: SftpClientPort + Send + Sync + 'static,
    SR: SessionRepository + Send + Sync + 'static,
    CR: CommandRepository + Send + Sync + 'static,
    ShR: ShellRepository + Send + Sync + 'static,
    TR: TransferRepository + Send + Sync + 'static,
    N: NotifierPort + Send + Sync + 'static,
    AS: AuthStrategyPort + Send + Sync + 'static,
    OS: OutputStreamPort + Send + Sync + 'static,
    SubR: SubscriberRegistryPort + SubscriberRegistryAsync + Send + Sync + 'static,
    C: ClockPort + Send + Sync + 'static,
    Cfg: ConfigPort + Send + Sync + 'static,
    Idg: IdGeneratorPort + Send + Sync + 'static,
{
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(server_capabilities())
            .with_protocol_version(ProtocolVersion::V_2025_06_18)
            .with_server_info(build_implementation())
            .with_instructions(INSTRUCTIONS_WITHOUT_FORWARD)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        resource_handlers::list_resources_impl(
            &self.use_cases.list_resources,
            self.leak_probe.as_ref(),
        )
        .await
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult::with_all_items(
            resource_templates::build_list(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        resource_handlers::read_resource_impl(
            &self.use_cases.read_resource,
            request,
            &context,
            &self.peer_table,
        )
        .await
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        resource_handlers::subscribe_impl(
            &self.use_cases.subscribe_resource,
            request,
            &context,
            &self.peer_table,
        )
        .await
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        resource_handlers::unsubscribe_impl(
            &self.use_cases.unsubscribe_resource,
            request,
            &context,
            &self.peer_table,
        )
        .await
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::with_all_items(prompts::list_prompts()))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        let args = request.arguments.unwrap_or_default();
        prompts::get_prompt(&request.name, &args)
    }
}

#[cfg(test)]
#[allow(
    unsafe_code,
    reason = "Rust 2024 requires unsafe for env::set_var; the env-mutating tests run --test-threads=1 so the global env stays serialised"
)]
mod tests {
    use super::{
        classify_error, filter_from_str, lifecycle_from_args, lifetime_from_args, parse_address,
        parse_human_bytes,
    };
    use crate::domain::error::DomainError;

    #[test]
    fn lifecycle_from_args_returns_none_when_both_unset() {
        assert!(lifecycle_from_args(None, None).is_none());
    }

    #[test]
    fn lifecycle_from_args_carries_release_when_no_subs() {
        let p = lifecycle_from_args(Some(true), None).expect("policy");
        assert!(p.release_when_no_subs);
        assert_eq!(p.grace_ms, 2_000);
        assert!(!p.cascade_session);
    }

    #[test]
    fn lifecycle_from_args_carries_grace_ms_only() {
        let p = lifecycle_from_args(None, Some(500)).expect("policy");
        // release_when_no_subs is false by default — caller must opt in.
        assert!(!p.release_when_no_subs);
        assert_eq!(p.grace_ms, 500);
    }

    #[test]
    fn lifecycle_from_args_carries_both_overrides() {
        let p = lifecycle_from_args(Some(true), Some(7_500)).expect("policy");
        assert!(p.release_when_no_subs);
        assert_eq!(p.grace_ms, 7_500);
    }

    #[test]
    fn filter_from_str_none_for_empty() {
        assert_eq!(
            filter_from_str(None),
            crate::domain::subscription::FilterRule::None
        );
        assert_eq!(
            filter_from_str(Some("")),
            crate::domain::subscription::FilterRule::None
        );
    }

    #[test]
    fn filter_from_str_regex_for_non_empty() {
        let r = filter_from_str(Some("abc"));
        assert_eq!(
            r,
            crate::domain::subscription::FilterRule::Regex("abc".to_string())
        );
    }

    #[test]
    fn lifetime_from_args_default_is_manual() {
        let lt = lifetime_from_args(None, None, None);
        assert_eq!(
            lt,
            crate::domain::subscription::SubscriptionLifetime::Manual
        );
    }

    #[test]
    fn lifetime_from_args_auto_close_uses_grace_ms() {
        let lt = lifetime_from_args(Some(super::LifetimeKind::AutoClose), Some(1_500), None);
        assert_eq!(
            lt,
            crate::domain::subscription::SubscriptionLifetime::AutoClose { grace_ms: 1_500 }
        );
    }

    #[test]
    fn lifetime_from_args_lease_uses_ttl_secs() {
        let lt = lifetime_from_args(Some(super::LifetimeKind::Lease), None, Some(60));
        assert_eq!(
            lt,
            crate::domain::subscription::SubscriptionLifetime::Lease { ttl_secs: 60 }
        );
    }

    #[test]
    fn parse_address_with_explicit_port() {
        let addr = parse_address("example.com:2222").expect("address");
        assert_eq!(addr.host(), "example.com");
        assert_eq!(addr.port(), 2222);
    }

    #[test]
    fn parse_address_defaults_to_22_when_no_port() {
        let addr = parse_address("example.com").expect("address");
        assert_eq!(addr.host(), "example.com");
        assert_eq!(addr.port(), 22);
    }

    #[test]
    fn parse_address_rejects_non_numeric_port() {
        let err = parse_address("example.com:abc").expect_err("non-numeric port");
        assert!(err.to_string().contains("invalid port"));
    }

    #[test]
    fn parse_human_bytes_handles_kb_mb_gb() {
        assert_eq!(parse_human_bytes(Some("512k")), Some(512 * 1024));
        assert_eq!(parse_human_bytes(Some("10m")), Some(10 * 1024 * 1024));
        assert_eq!(parse_human_bytes(Some("1g")), Some(1024_u64 * 1024 * 1024));
        assert_eq!(
            parse_human_bytes(Some("1t")),
            Some(1024_u64 * 1024 * 1024 * 1024)
        );
    }

    #[test]
    fn parse_human_bytes_rejects_garbage() {
        assert_eq!(parse_human_bytes(Some("nope")), None);
        assert_eq!(parse_human_bytes(Some("")), None);
        assert_eq!(parse_human_bytes(None), None);
    }

    // ---------------------------------------------------------------
    // v4.5 — granular wire error code dispatch via tag prefixes.
    //
    // Each documented tag must promote `InvalidArgument` / `Transport`
    // / `Sftp` payloads to the specific wire code, while untagged
    // payloads must keep the legacy flat code for backwards
    // compatibility.
    // ---------------------------------------------------------------

    #[test]
    fn classifies_invalid_argument_with_known_tag_to_specific_code() {
        let err = DomainError::InvalidArgument("EMPTY_PATTERNS: x".to_string());
        let (code, reason, detail) = classify_error(&err);
        assert_eq!(code, "EMPTY_PATTERNS");
        assert_eq!(reason, "x");
        assert!(detail.is_none());
    }

    #[test]
    fn classifies_invalid_argument_without_tag_to_generic_code() {
        let err = DomainError::InvalidArgument("plain reason".to_string());
        let (code, reason, _detail) = classify_error(&err);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(reason, "plain reason");
    }

    #[test]
    fn classifies_transport_with_known_tag_to_specific_code() {
        let err = DomainError::Transport("WRITE_FAILED: shell write blew up".to_string());
        let (code, reason, _detail) = classify_error(&err);
        assert_eq!(code, "WRITE_FAILED");
        assert_eq!(reason, "shell write blew up");
    }

    #[test]
    fn classifies_transport_without_tag_falls_back_to_transport_error() {
        let err = DomainError::Transport("kaput".to_string());
        let (code, reason, _detail) = classify_error(&err);
        assert_eq!(code, "TRANSPORT_ERROR");
        assert_eq!(reason, "kaput");
    }

    #[test]
    fn classifies_sftp_with_known_tag_to_specific_code() {
        let err = DomainError::Sftp(
            "LOCAL_FILE_ERROR: [IO_ERROR] stat local file '/tmp/x': I/O error (raw: ENOENT)"
                .to_string(),
        );
        let (code, reason, _detail) = classify_error(&err);
        assert_eq!(code, "LOCAL_FILE_ERROR");
        assert!(reason.starts_with("[IO_ERROR]"), "got {reason}");
    }

    #[test]
    fn classifies_sftp_without_tag_falls_back_to_sftp_error() {
        let err = DomainError::Sftp("[IO_ERROR] write to remote file '/x': ...".to_string());
        let (code, reason, _detail) = classify_error(&err);
        assert_eq!(code, "SFTP_ERROR");
        assert!(reason.starts_with("[IO_ERROR]"), "got {reason}");
    }

    #[test]
    fn classify_error_strips_tag_from_reason() {
        // The leading `TAG: ` prefix must be removed so the wire
        // `REASON:` line carries only the human message.
        let err = DomainError::InvalidArgument("INVALID_REPEAT: repeat must be 1..=64".to_string());
        let (code, reason, _detail) = classify_error(&err);
        assert_eq!(code, "INVALID_REPEAT");
        assert_eq!(reason, "repeat must be 1..=64");
    }

    #[test]
    fn classify_error_ignores_unknown_tags_in_invalid_argument() {
        let err = DomainError::InvalidArgument("UNKNOWN: blah".to_string());
        let (code, reason, _detail) = classify_error(&err);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(reason, "UNKNOWN: blah");
    }

    #[test]
    fn classify_error_promotes_feature_disabled_tag() {
        let err = DomainError::InvalidArgument(
            "FEATURE_DISABLED: forward:// resources require the port_forward Cargo feature"
                .to_string(),
        );
        let (code, _reason, _detail) = classify_error(&err);
        assert_eq!(code, "FEATURE_DISABLED");
    }

    /// `FORWARD_FAILED` is the v4.5 wire tag for non-`AddrInUse`
    /// pre-flight bind failures emitted by
    /// [`crate::application::forward_port::ForwardPortUseCase`]. The use
    /// case raises it via `DomainError::Transport`, so the dispatcher
    /// must promote it from the `TRANSPORT_ERROR` fallback to the
    /// dedicated wire code.
    #[test]
    fn classify_error_promotes_forward_failed_tag() {
        let err = DomainError::Transport(
            "FORWARD_FAILED: bind 0.0.0.0:80 failed: permission denied".to_string(),
        );
        let (code, reason, _detail) = classify_error(&err);
        assert_eq!(code, "FORWARD_FAILED");
        assert_eq!(reason, "bind 0.0.0.0:80 failed: permission denied");
    }

    /// `LOCAL_NOT_FILE` is the v4.5 wire tag for upload pre-flight
    /// failures where the local path resolves to a directory or other
    /// non-regular file. Emitted by
    /// [`crate::application::upload_file::UploadFileUseCase`] via
    /// `DomainError::Sftp`.
    #[test]
    fn classify_error_promotes_local_not_file_tag() {
        let err = DomainError::Sftp(
            "LOCAL_NOT_FILE: local path '/tmp' is not a regular file".to_string(),
        );
        let (code, reason, _detail) = classify_error(&err);
        assert_eq!(code, "LOCAL_NOT_FILE");
        assert_eq!(reason, "local path '/tmp' is not a regular file");
    }

    /// `REMOTE_METADATA_ERROR` is the v4.5 wire tag for download
    /// pre-flight failures where the remote stat call fails. Emitted by
    /// [`crate::application::download_file::DownloadFileUseCase`] via
    /// `DomainError::Sftp`.
    #[test]
    fn classify_error_promotes_remote_metadata_error_tag() {
        let err = DomainError::Sftp(
            "REMOTE_METADATA_ERROR: cannot stat remote path: permission denied".to_string(),
        );
        let (code, reason, _detail) = classify_error(&err);
        assert_eq!(code, "REMOTE_METADATA_ERROR");
        assert_eq!(reason, "cannot stat remote path: permission denied");
    }

    // ---------------------------------------------------------------
    // v4.7 — structured_content payload mirrors the Markdown body.
    //
    // Each tool now emits both the legacy block-style Markdown body
    // (byte-identical with v4.6) and a typed JSON object on
    // `CallToolResult::structured_content`. Smaller LLMs index by key;
    // the schema is exposed via `tools/list` for the six stretch-goal
    // tools (connect, execute, get_command_output, shell_open,
    // shell_read, get_transfer_progress).
    // ---------------------------------------------------------------

    use super::render_tool_error;
    use crate::application::connect_session::ConnectOutcome;
    use crate::application::execute_command::ExecuteOutcome;
    use crate::application::open_shell::OpenShellOutcome;
    use crate::domain::identity::Address;
    use crate::domain::ids::{CommandId, SessionId, ShellId};
    use crate::domain::session::SessionEntity;
    use crate::domain::shell::{ShellEntity, ShellTerminal};
    use crate::infra::mcp::helpers::structured::ok_text_and_structured;
    use crate::infra::mcp::render;
    use chrono::{TimeZone, Utc};
    use std::time::Duration;

    fn sample_session(id: &str) -> SessionEntity {
        SessionEntity {
            id: SessionId::new(id.to_string()),
            name: None,
            agent_id: None,
            address: Address::new("h.example.com".to_string(), 22).expect("address"),
            username: "alice".to_string(),
            connected_at: Utc.with_ymd_and_hms(2026, 5, 3, 12, 0, 0).unwrap(),
            default_timeout: Duration::from_secs(30),
            retry_attempts: 0,
            compression_enabled: true,
            last_health_check: None,
            healthy: Some(true),
        }
    }

    #[test]
    fn structured_content_mirrors_text_for_ssh_connect_ok() {
        let session = sample_session("sess-abc");
        let outcome = ConnectOutcome::Connected {
            session,
            replaced: 0,
            retries: 0,
            persistent: false,
            inactivity_timeout: Duration::from_secs(300),
        };
        let structured = render::connection::connect_structured(&outcome);
        let body = render::connection::connect_render(outcome);
        let result = ok_text_and_structured(body.clone(), structured.clone());
        // Markdown side stays byte-identical with v4.6.
        let text = result
            .content
            .first()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .expect("markdown body present");
        assert!(text.starts_with("SSH_CONNECT: OK"));
        // Structured side carries the discriminating keys.
        let json = result.structured_content.expect("structured present");
        assert_eq!(json["tool"], "ssh_connect");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["session_id"], "sess-abc");
        assert!(json["next"].is_array(), "next must be a list of tool names");
    }

    #[test]
    fn structured_content_mirrors_text_for_ssh_execute_started() {
        let outcome = ExecuteOutcome {
            command_id: CommandId::new("cmd-xyz".to_string()),
            session_id: SessionId::new("sess-1".to_string()),
            agent_id: None,
            started_at: "2026-04-18T10:30:00+00:00".to_string(),
        };
        let structured = render::execute::execute_structured(&outcome);
        let body = render::execute::execute_render(outcome);
        let result = ok_text_and_structured(body, structured);
        let json = result.structured_content.expect("structured present");
        assert_eq!(json["tool"], "ssh_execute");
        assert_eq!(json["status"], "started");
        assert_eq!(json["command_id"], "cmd-xyz");
        assert_eq!(json["session_id"], "sess-1");
        assert!(json["next"].is_array());
    }

    #[test]
    fn structured_content_mirrors_text_for_ssh_shell_open() {
        let shell = ShellEntity::new(
            ShellId::new("sh-1".to_string()),
            SessionId::new("sess-1".to_string()),
            ShellTerminal::new("xterm".to_string(), 80, 24),
            Utc::now(),
            Duration::from_secs(900),
            1024,
        );
        let outcome = OpenShellOutcome {
            shell,
            session_id: SessionId::new("sess-1".to_string()),
            agent_id: None,
        };
        let structured = render::shell::shell_open_structured(&outcome);
        let body = render::shell::shell_open_render(outcome);
        let result = ok_text_and_structured(body, structured);
        let json = result.structured_content.expect("structured present");
        assert_eq!(json["tool"], "ssh_shell_open");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["shell_id"], "sh-1");
        assert_eq!(json["term"], "xterm");
        assert_eq!(json["cols"], 80);
        assert_eq!(json["rows"], 24);
        assert!(json["next"].is_array());
    }

    #[test]
    fn structured_error_carries_code_and_reason() {
        let err = DomainError::InvalidArgument("EMPTY_PATTERNS: must contain >=1".to_string());
        let result = render_tool_error("SSH_SHELL_WAIT_FOR", &err);
        assert_eq!(result.is_error, Some(true));
        let json = result.structured_content.expect("structured present");
        assert_eq!(json["tool"], "ssh_shell_wait_for");
        assert_eq!(json["status"], "error");
        assert_eq!(json["code"], "EMPTY_PATTERNS");
        assert_eq!(json["reason"], "must contain >=1");
        // v5 Phase 3: every wire code carries a static DETAIL pedagogy
        // line via [`crate::infra::mcp::error_detail`]; assert the cure
        // surfaces even for codes without a per-call dynamic detail.
        let detail = json["detail"]
            .as_str()
            .expect("v5 DETAIL pedagogy must populate the structured detail");
        assert!(
            detail.contains("patterns must contain at least one entry"),
            "expected pedagogy line, got {detail}"
        );
    }

    // ---------------------------------------------------------------
    // v4.7-step6 — closest-match suggestions on NOT_FOUND
    // ---------------------------------------------------------------

    use super::{IdFuture, IdLister, NoopIdLister, render_tool_error_with_suggestions};
    use futures::executor::block_on;

    /// Test lister that returns a fixed list of session ids.
    struct FixedSessionLister(Vec<String>);

    impl IdLister for FixedSessionLister {
        fn list_sessions(&self) -> IdFuture<'_> {
            let v = self.0.clone();
            Box::pin(async move { v })
        }
        fn list_shells(&self) -> IdFuture<'_> {
            Box::pin(async { Vec::new() })
        }
        fn list_commands(&self) -> IdFuture<'_> {
            Box::pin(async { Vec::new() })
        }
        fn list_transfers(&self) -> IdFuture<'_> {
            Box::pin(async { Vec::new() })
        }
        #[cfg(feature = "port_forward")]
        fn list_forwards(&self) -> IdFuture<'_> {
            Box::pin(async { Vec::new() })
        }
    }

    #[test]
    fn not_found_error_includes_closest_matches() {
        let lister = FixedSessionLister(vec![
            "s-aaa".to_string(),
            "s-aab".to_string(),
            "s-aac".to_string(),
            "s-xyz".to_string(),
        ]);
        let err = DomainError::SessionNotFound(SessionId::new("s-aad".to_string()));
        let result = block_on(render_tool_error_with_suggestions(
            "SSH_DISCONNECT",
            &err,
            &lister,
        ));
        let body = result
            .content
            .first()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();
        assert!(body.contains("SSH_DISCONNECT: ERROR"));
        assert!(body.contains("closest matches:"), "body: {body}");
        let json = result.structured_content.expect("structured present");
        let detail = json["detail"]
            .as_str()
            .expect("detail must be present when suggestions fire");
        assert!(detail.contains("closest matches: s-aaa, s-aab, s-aac"));
    }

    #[test]
    fn not_found_error_omits_suggestions_when_repo_empty() {
        let lister = NoopIdLister;
        let err = DomainError::SessionNotFound(SessionId::new("s-missing".to_string()));
        let result = block_on(render_tool_error_with_suggestions(
            "SSH_DISCONNECT",
            &err,
            &lister,
        ));
        let body = result
            .content
            .first()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();
        assert!(!body.contains("closest matches:"), "body: {body}");
        let json = result.structured_content.expect("structured present");
        // v5 Phase 3: DETAIL pedagogy is appended ahead of the
        // missing-id context. The id is still present at the tail.
        let detail = json["detail"].as_str().expect("detail present");
        assert!(detail.contains("ssh_list_sessions"), "{detail}");
        assert!(detail.ends_with("s-missing"), "{detail}");
    }

    // ---------------------------------------------------------------
    // v4.7-step7 — INITIAL_BUFFER on ssh_shell_open
    // ---------------------------------------------------------------

    use super::peek_initial_shell_buffer;
    use crate::domain::error::DomainError as DomErr;
    use crate::ports::output_stream::{OutputSnapshot, OutputStreamPort};
    use bytes::Bytes;

    /// Output-stream fake that yields a fixed shell snapshot.
    struct FixedShellOutput(Bytes);

    impl OutputStreamPort for FixedShellOutput {
        async fn snapshot_command(&self, _id: &CommandId) -> Result<OutputSnapshot, DomErr> {
            Ok(OutputSnapshot {
                byte_cursor: 0,
                last_seq: 0,
                stdout: Bytes::new(),
                stderr: Bytes::new(),
            })
        }
        async fn snapshot_shell(&self, _id: &ShellId) -> Result<OutputSnapshot, DomErr> {
            Ok(OutputSnapshot {
                byte_cursor: 0,
                last_seq: 0,
                stdout: self.0.clone(),
                stderr: Bytes::new(),
            })
        }
    }

    #[test]
    fn shell_open_includes_initial_buffer_when_data_arrives() {
        let streams = FixedShellOutput(Bytes::from_static(b"$ "));
        let id = ShellId::new("sh-banner".to_string());
        let peek = block_on(peek_initial_shell_buffer(&streams, &id))
            .expect("peek must capture stdout when bytes are present");
        assert_eq!(peek.bytes, b"$ ".to_vec());
        assert!(!peek.truncated);
    }

    #[test]
    fn shell_open_omits_initial_buffer_when_silent() {
        // Drive on tokio's current_thread runtime so `tokio::time::sleep`
        // sees a live reactor (the peek loop sleeps between ticks).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("tokio runtime");
        // Override the peek window down to 30ms so the test stays fast.
        // SAFETY: env is process-global; the `mod tests` allow on
        // `unsafe_code` documents that the test mod runs serialised by
        // the harness (`--test-threads=1` per repo convention).
        unsafe {
            std::env::set_var("SSH_SHELL_OPEN_INITIAL_PEEK_MS", "30");
        }
        let peek = rt.block_on(async {
            let streams = FixedShellOutput(Bytes::new());
            let id = ShellId::new("sh-silent".to_string());
            peek_initial_shell_buffer(&streams, &id).await
        });
        unsafe {
            std::env::remove_var("SSH_SHELL_OPEN_INITIAL_PEEK_MS");
        }
        assert!(peek.is_none(), "silent shell must yield None");
    }

    #[test]
    fn shell_open_truncates_initial_buffer_at_max_bytes() {
        // Stub the cap at 8 bytes via env; keep the snapshot longer so
        // the truncation branch fires.
        unsafe {
            std::env::set_var("SSH_SHELL_OPEN_INITIAL_BUFFER_MAX_BYTES", "8");
        }
        let streams = FixedShellOutput(Bytes::from_static(b"abcdefghijklmnop"));
        let id = ShellId::new("sh-bigbanner".to_string());
        let peek = block_on(peek_initial_shell_buffer(&streams, &id))
            .expect("non-empty stdout must yield Some");
        unsafe {
            std::env::remove_var("SSH_SHELL_OPEN_INITIAL_BUFFER_MAX_BYTES");
        }
        assert_eq!(peek.bytes.len(), 8);
        assert!(peek.truncated);
    }

    // -------------------------------------------------------------------
    // v4.7.1 Bug #3 — idempotency replay must NOT invoke the use case.
    //
    // The `with_idempotency` wrapper is a wire-level dedup: on a cache
    // hit it must return the cached body verbatim and skip the
    // user-supplied closure entirely. The original execution is the
    // canonical writer of every side effect (subscriber notifications,
    // repo state, output stream pumps); a replay-induced re-run would
    // break the "exactly-once" contract LLMs rely on for retried calls.
    // The chaos battery's `cs12_idempotency_with_subscribe` reports the
    // post-replay notification count for context but cannot distinguish
    // a wrapper-induced republish from an unrelated debouncer
    // force-flush — these unit tests lock the wrapper purity contract
    // directly.
    // -------------------------------------------------------------------

    #[test]
    fn idempotency_replay_does_not_invoke_callback() {
        use crate::infra::mcp::idempotency::{IdempotencyCache, KeyOutcome};
        use rmcp::ErrorData as McpError;
        use rmcp::model::CallToolResult;
        use rmcp::model::Content;
        use serde_json::json;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        let rt = tokio::runtime::Runtime::new().expect("rt");
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        let calls = Arc::new(AtomicUsize::new(0));
        let key = "k-v471-bug3";

        let make_body = |calls: Arc<AtomicUsize>| async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let mut result = CallToolResult::success(vec![Content::text("BODY")]);
            result.structured_content = Some(json!({"tool": "ssh_test", "status": "ok"}));
            Ok::<_, McpError>(result)
        };

        // First call: cache miss, callback MUST run exactly once and
        // the response is cached.
        let calls1 = Arc::clone(&calls);
        let first = rt
            .block_on(super::with_idempotency_keyed(
                &cache,
                KeyOutcome::Present(key.to_string()),
                "ssh_test",
                String::new(),
                || make_body(Arc::clone(&calls1)),
            ))
            .expect("first call");
        assert_eq!(first.is_error, Some(false));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "first call must run f");

        // Second call with same key: cache hit, callback MUST NOT run.
        // This is the load-bearing assertion — locks the "replay is a
        // pure passthrough" contract documented above.
        let calls2 = Arc::clone(&calls);
        let second = rt
            .block_on(super::with_idempotency_keyed(
                &cache,
                KeyOutcome::Present(key.to_string()),
                "ssh_test",
                String::new(),
                || make_body(Arc::clone(&calls2)),
            ))
            .expect("second call");
        assert_eq!(second.is_error, Some(false));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "replay must NOT invoke the callback — counter must still be 1"
        );
        // Body must be replayed verbatim from the cached response.
        let body = second
            .content
            .first()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .unwrap_or_default();
        assert_eq!(body, "BODY", "replay must return cached body verbatim");
    }

    #[test]
    fn idempotency_absent_key_runs_callback_every_time() {
        use crate::infra::mcp::idempotency::{IdempotencyCache, KeyOutcome};
        use rmcp::ErrorData as McpError;
        use rmcp::model::CallToolResult;
        use rmcp::model::Content;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        let rt = tokio::runtime::Runtime::new().expect("rt");
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        let calls = Arc::new(AtomicUsize::new(0));

        for _ in 0..3 {
            let calls_inner = Arc::clone(&calls);
            let _ = rt
                .block_on(super::with_idempotency_keyed(
                    &cache,
                    KeyOutcome::Absent,
                    "ssh_test",
                    String::new(),
                    || async move {
                        calls_inner.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, McpError>(CallToolResult::success(vec![Content::text("x")]))
                    },
                ))
                .expect("call");
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "absent key must drive the callback on every call"
        );
    }

    #[test]
    fn idempotency_failed_response_is_not_cached() {
        // When the use case returns is_error=true, the response is NOT
        // cached so the LLM can retry after fixing the input. Lock that
        // contract here so an accidental "always cache" regression
        // surfaces as a unit-test failure.
        use crate::infra::mcp::idempotency::{IdempotencyCache, KeyOutcome};
        use rmcp::ErrorData as McpError;
        use rmcp::model::CallToolResult;
        use rmcp::model::Content;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        let rt = tokio::runtime::Runtime::new().expect("rt");
        let cache = IdempotencyCache::new(Duration::from_secs(60), 16);
        let calls = Arc::new(AtomicUsize::new(0));
        let key = "k-fail";

        for _ in 0..2 {
            let calls_inner = Arc::clone(&calls);
            let _ = rt
                .block_on(super::with_idempotency_keyed(
                    &cache,
                    KeyOutcome::Present(key.to_string()),
                    "ssh_test",
                    String::new(),
                    || async move {
                        calls_inner.fetch_add(1, Ordering::SeqCst);
                        let mut r = CallToolResult::success(vec![Content::text("nope")]);
                        r.is_error = Some(true);
                        Ok::<_, McpError>(r)
                    },
                ))
                .expect("call");
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "error responses must NOT be cached — both calls must run f"
        );
    }
}
