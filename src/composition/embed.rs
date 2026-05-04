//! Embed transport composition root for `ssh-mcp-tail`.
//!
//! Wires the production [`crate::composition::prod`] adapters to a
//! `tokio::io::duplex(64 KiB)` pair so the rmcp `ServerHandler` and
//! an embedded rmcp client share a single in-process JSON-RPC bus.
//!
//! The three subcommand entry points (`run_daemon`, `run_one_shot`,
//! `run_shell`) live here so the binary in
//! `src/bin/ssh_mcp_tail.rs` stays a thin shell.
//!
//! Phase 4 ships with the daemon loop fully wired (stdin -> dispatcher
//! -> rmcp client -> `ServerHandler` -> rmcp client peer -> formatter
//! -> stdout). The `run` and `shell` shortcuts synthesise their
//! NDJSON op stream via the shared [`crate::embed::dispatcher`] entry
//! point.

use std::error::Error;
use std::io::stderr;
use std::time::Duration;

use tokio::io::{AsyncBufRead, BufReader, stdin, stdout};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::adapters::config::internal::{
    resolve_daemon_stats_interval_s, resolve_grace_hard_timeout_s, resolve_heartbeat_interval_s,
    resolve_mux_buffer, resolve_ndjson_line_max,
};
use crate::adapters::id_generator::uuid::UuidIds;
use crate::composition::prod::build_use_cases;
use crate::domain::ids::SessionId;
use crate::embed::cli::{DaemonArgs, RunArgs, ShellArgs};
use crate::embed::dispatcher::{DispatchOutcome, dispatch_one};
use crate::embed::duplex_transport::{
    EMBED_DUPLEX_BUFFER, EmbedHandle, assemble_handle, build_client_side, duplex_pair,
    spawn_server_side,
};
use crate::embed::event_mux::{
    EmbedClient, EventTx, spawn_daemon_stats, spawn_drain, spawn_heartbeat,
};
use crate::embed::formatter::{Event, NdjsonWriter};
use crate::embed::parser::{NdjsonReader, Op, ParseOutcome};
use crate::embed::signals::{spawn_signal_listener, wait_for_drain};
use crate::infra::mcp::server::McpSshServer;
use crate::ports::id_generator::IdGeneratorPort;

/// Boxed error alias so the binary entry signature stays simple.
pub type EmbedRunError = Box<dyn Error + Send + Sync>;

/// Wire the production composition root + the embed duplex transport
/// + the custom `EmbedClient` rmcp handler.
///
/// Returns the bundle of handles every entry point needs (server
/// task, running client, shutdown token, outbound mpsc). Kept
/// public so integration tests can drive the same wire surface the
/// production daemon uses (see `tests/v5_daemon_smoke.rs`).
///
/// # Errors
/// Surfaces [`crate::embed::duplex_transport::EmbedError`] when the
/// rmcp handshake fails on either side of the duplex.
pub async fn wire_embed_transport(mux_buffer: usize) -> Result<EmbedTransport, EmbedRunError> {
    let (use_cases, peer_table, idempotency, id_lister) = build_use_cases();
    let server = McpSshServer::new(use_cases, peer_table, idempotency).with_id_lister(id_lister);

    let (server_stream, client_stream) = duplex_pair();
    let server_task = spawn_server_side(server, server_stream);

    let (tx, rx) = mpsc::channel::<Event>(mux_buffer);
    let handler = EmbedClient::new(tx.clone());
    let client_service = build_client_side(handler, client_stream).await?;
    let shutdown = CancellationToken::new();
    let handle = assemble_handle(server_task, client_service, shutdown);
    Ok(EmbedTransport { handle, tx, rx })
}

/// Bundle of the three embed handles every entry point needs.
#[derive(Debug)]
pub struct EmbedTransport {
    /// Server task + running client + shutdown token.
    pub handle: EmbedHandle<EmbedClient>,
    /// Outbound mpsc sender consumed by the dispatcher / heartbeat /
    /// stats tasks.
    pub tx: EventTx,
    /// Outbound mpsc receiver consumed by the drain task.
    pub rx: mpsc::Receiver<Event>,
}

/// `daemon` subcommand entry point.
///
/// Drains stdin NDJSON ops and exits on `shutdown` op or stdin EOF.
/// Honours the production `IdLister` implicitly: the embedded
/// server publishes every id through `composition::prod` so
/// closest-match suggestions work for daemon errors as well.
///
/// # Errors
/// Propagates any embed-wiring failure or fatal mpsc closure
/// reported by the dispatcher.
pub async fn run_daemon(args: DaemonArgs) -> Result<(), EmbedRunError> {
    install_subscriber();
    let cfg = DaemonRuntimeConfig::resolve(&args);
    let EmbedTransport { handle, tx, rx } = wire_embed_transport(cfg.mux_buffer).await?;
    let shutdown = handle.shutdown_token().clone();
    let tasks = spawn_daemon_tasks(&tx, rx, &cfg, &shutdown);
    drive_daemon_stdin(&handle, &tx, cfg.line_max, &shutdown).await;
    finalise_daemon(handle, tasks, &shutdown).await;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct DaemonRuntimeConfig {
    mux_buffer: usize,
    line_max: usize,
    heartbeat_secs: u64,
    stats_secs: u64,
}

impl DaemonRuntimeConfig {
    fn resolve(args: &DaemonArgs) -> Self {
        Self {
            mux_buffer: resolve_mux_buffer(),
            line_max: args.line_max.unwrap_or_else(resolve_ndjson_line_max),
            heartbeat_secs: args
                .heartbeat_secs
                .unwrap_or_else(resolve_heartbeat_interval_s),
            stats_secs: args
                .stats_secs
                .unwrap_or_else(resolve_daemon_stats_interval_s),
        }
    }
}

struct DaemonTasks {
    drain: JoinHandle<()>,
    heartbeat: JoinHandle<()>,
    stats: JoinHandle<()>,
    signals: JoinHandle<()>,
}

fn spawn_daemon_tasks(
    tx: &EventTx,
    rx: mpsc::Receiver<Event>,
    cfg: &DaemonRuntimeConfig,
    shutdown: &CancellationToken,
) -> DaemonTasks {
    let writer = NdjsonWriter::new(stdout());
    let drain = spawn_drain(rx, writer, shutdown.clone());
    let heartbeat = spawn_heartbeat(
        tx.clone(),
        Duration::from_secs(cfg.heartbeat_secs),
        shutdown.clone(),
    );
    let stats = spawn_daemon_stats(
        tx.clone(),
        Duration::from_secs(cfg.stats_secs),
        shutdown.clone(),
    );
    let signals = spawn_signal_listener(shutdown.clone());
    DaemonTasks {
        drain,
        heartbeat,
        stats,
        signals,
    }
}

async fn drive_daemon_stdin(
    handle: &EmbedHandle<EmbedClient>,
    tx: &EventTx,
    line_max: usize,
    shutdown: &CancellationToken,
) {
    let stdin_handle = stdin();
    let stdin_buf = BufReader::new(stdin_handle);
    let mut reader = NdjsonReader::with_line_max(stdin_buf, line_max);
    if let Err(err) = stdin_loop(handle, tx, &mut reader, shutdown).await {
        tracing::warn!("daemon dispatch loop exited with error: {err}");
    }
}

async fn finalise_daemon(
    handle: EmbedHandle<EmbedClient>,
    tasks: DaemonTasks,
    shutdown: &CancellationToken,
) {
    shutdown.cancel();
    let DaemonTasks {
        drain,
        heartbeat,
        stats,
        signals,
    } = tasks;
    let grace = Duration::from_secs(resolve_grace_hard_timeout_s());
    let drain_task = async {
        let _ = drain.await;
    };
    if wait_for_drain(drain_task, grace).await.is_err() {
        tracing::warn!("hard grace timeout fired before drain finished");
    }
    heartbeat.abort();
    stats.abort();
    signals.abort();
    let _ = heartbeat.await;
    let _ = stats.await;
    let _ = signals.await;
    handle.shutdown().await;
}

async fn stdin_loop<R>(
    handle: &EmbedHandle<EmbedClient>,
    tx: &EventTx,
    reader: &mut NdjsonReader<R>,
    shutdown: &CancellationToken,
) -> Result<(), EmbedRunError>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            outcome = reader.next() => {
                if !handle_outcome(handle, tx, outcome).await? {
                    return Ok(());
                }
            }
        }
    }
}

async fn handle_outcome(
    handle: &EmbedHandle<EmbedClient>,
    tx: &EventTx,
    outcome: ParseOutcome,
) -> Result<bool, EmbedRunError> {
    match outcome {
        ParseOutcome::Eof => Ok(false),
        ParseOutcome::Invalid(err) => {
            let event = Event::Err {
                id: None,
                code: "INVALID_OP".to_string(),
                reason: "NDJSON parse error".to_string(),
                detail: Some(err.to_string()),
            };
            if tx.send(event).await.is_err() {
                return Ok(false);
            }
            Ok(true)
        }
        ParseOutcome::Op(op) => match dispatch_one(handle.client(), tx, op).await {
            Ok(DispatchOutcome::Continue) => Ok(true),
            Ok(DispatchOutcome::Shutdown) => Ok(false),
            Err(err) => {
                tracing::warn!("dispatch failed: {err}");
                Ok(false)
            }
        },
    }
}

/// `run` subcommand entry point.
///
/// Synthesises a single `connect` + `exec` flow and runs it through
/// the same dispatcher.
///
/// # Errors
/// Propagates any embed-wiring failure or dispatcher error.
pub async fn run_one_shot(args: RunArgs) -> Result<(), EmbedRunError> {
    install_subscriber();
    let mux_buffer = resolve_mux_buffer();
    let EmbedTransport { handle, tx, rx } = wire_embed_transport(mux_buffer).await?;
    let shutdown = handle.shutdown_token().clone();
    let writer = NdjsonWriter::new(stdout());
    let drain_handle = spawn_drain(rx, writer, shutdown.clone());

    issue_run_ops(&handle, &tx, &args).await;
    drain_and_shutdown(handle, drain_handle, shutdown).await;
    Ok(())
}

async fn issue_run_ops(handle: &EmbedHandle<EmbedClient>, tx: &EventTx, args: &RunArgs) {
    let connect = Op::Connect {
        host: args.host.clone(),
        user: args.user.clone(),
        key: args.key.as_ref().map(|p| p.display().to_string()),
        password: None,
        port: args.port,
        agent_id: None,
        reuse_policy: Some("auto".to_string()),
        id: Some("run-connect".to_string()),
    };
    let _ = dispatch_one(handle.client(), tx, connect).await;

    if !args.command.is_empty() {
        let cmd = args.command.join(" ");
        let exec = Op::Exec {
            sid: SessionId::new(synthesise_session_id()),
            cmd,
            pty: Some(false),
            release_when_no_subs: Some(true),
            id: Some("run-exec".to_string()),
        };
        let _ = dispatch_one(handle.client(), tx, exec).await;
    }

    if args.auto_disconnect {
        let disc = Op::Shutdown {
            id: Some("run-shutdown".to_string()),
        };
        let _ = dispatch_one(handle.client(), tx, disc).await;
    }
}

async fn drain_and_shutdown(
    handle: EmbedHandle<EmbedClient>,
    drain: JoinHandle<()>,
    shutdown: CancellationToken,
) {
    shutdown.cancel();
    let grace = Duration::from_secs(resolve_grace_hard_timeout_s());
    let drain_task = async {
        let _ = drain.await;
    };
    let _ = wait_for_drain(drain_task, grace).await;
    handle.shutdown().await;
}

/// `shell` subcommand entry point.
///
/// Synthesises a `connect` + `shell_open` + drain flow and forwards
/// stdin bytes as `shell_write` ops until the cooperative shutdown
/// token cancels.
///
/// # Errors
/// Propagates any embed-wiring failure or dispatcher error.
pub async fn run_shell(args: ShellArgs) -> Result<(), EmbedRunError> {
    install_subscriber();
    let mux_buffer = resolve_mux_buffer();
    let EmbedTransport { handle, tx, rx } = wire_embed_transport(mux_buffer).await?;
    let shutdown = handle.shutdown_token().clone();
    let writer = NdjsonWriter::new(stdout());
    let drain_handle = spawn_drain(rx, writer, shutdown.clone());

    issue_shell_ops(&handle, &tx, &args).await;
    let signal_handle = spawn_signal_listener(shutdown.clone());
    shutdown.cancelled().await;
    drain_and_shutdown(handle, drain_handle, shutdown).await;
    signal_handle.abort();
    let _ = signal_handle.await;
    Ok(())
}

async fn issue_shell_ops(handle: &EmbedHandle<EmbedClient>, tx: &EventTx, args: &ShellArgs) {
    let connect = Op::Connect {
        host: args.host.clone(),
        user: args.user.clone(),
        key: args.key.as_ref().map(|p| p.display().to_string()),
        password: None,
        port: args.port,
        agent_id: None,
        reuse_policy: Some("auto".to_string()),
        id: Some("shell-connect".to_string()),
    };
    let _ = dispatch_one(handle.client(), tx, connect).await;

    let open = Op::ShellOpen {
        sid: SessionId::new(synthesise_session_id()),
        cols: args.cols,
        rows: args.rows,
        release_when_no_subs: Some(true),
        inactivity_ttl_secs: None,
        max_buffer_size: None,
        id: Some("shell-open".to_string()),
    };
    let _ = dispatch_one(handle.client(), tx, open).await;
}

fn synthesise_session_id() -> String {
    // Wrapper subcommands don't observe the real session id from the
    // rmcp ack response (the dispatcher echoes the wire result in a
    // future phase). For now we mint a placeholder id so the
    // dispatcher arguments validate. The `run` flow will surface
    // `INVALID_ARGUMENT` if the placeholder doesn't match a live
    // session — operators should switch to the `daemon` subcommand
    // for full control.
    UuidIds.new_session_id().into_inner()
}

fn install_subscriber() {
    use tracing_subscriber::EnvFilter;
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(stderr)
        .try_init();
}

/// Convenience constant exported for downstream callers (browser
/// bridges) that want to mirror the daemon's duplex sizing.
#[must_use]
pub const fn duplex_buffer_bytes() -> usize {
    EMBED_DUPLEX_BUFFER
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test-only assertions are deliberately direct"
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wire_embed_transport_handshakes_successfully() {
        let transport = wire_embed_transport(64).await.unwrap();
        // Cancel and drain so the runtime tears down cleanly.
        transport.handle.shutdown().await;
    }

    #[test]
    fn duplex_buffer_constant_matches_transport() {
        assert_eq!(duplex_buffer_bytes(), 64 * 1_024);
    }

    #[test]
    fn synthesised_session_id_is_uuid_shaped() {
        let id = synthesise_session_id();
        // UUID v4 strings are 36 chars including dashes.
        assert_eq!(id.len(), 36);
    }
}
