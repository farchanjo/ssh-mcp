//! In-process MCP transport for the daemon binary.
//!
//! `tokio::io::duplex(64 KiB)` produces a pair of bidirectional async
//! byte channels; we wrap one half in `rmcp::transport::AsyncRwTransport`
//! for the server side and feed the other half to a freshly-built rmcp
//! client. Both halves share the same tokio runtime so no IPC syscall
//! is involved — the protocol bytes never leave the process.
//!
//! The composition root in [`crate::composition::embed::wire`] returns
//! an [`EmbedHandle`] that owns the server task, the client service,
//! and a [`tokio_util::sync::CancellationToken`] for graceful shutdown.

use std::sync::Arc;

use rmcp::ServiceExt as _;
use rmcp::model::{
    ClientCapabilities, ClientInfo, ExperimentalCapabilities, Implementation, JsonObject,
    ProtocolVersion,
};
use rmcp::service::{RoleClient, RunningService, Service};
use rmcp::transport::async_rw::AsyncRwTransport;
use thiserror::Error;
use tokio::io::{DuplexStream, duplex, split};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::composition::prod::ProdUseCases;
use crate::infra::mcp::server::McpSshServer;

/// Errors surfaced when the embed transport cannot be wired.
#[derive(Debug, Error)]
pub enum EmbedError {
    /// rmcp server task failed to start.
    #[error("server task failed: {0}")]
    Server(String),
    /// rmcp client handshake failed.
    #[error("client handshake failed: {0}")]
    ClientInit(String),
}

/// Live handle returned by [`crate::composition::embed::wire`].
///
/// Owns the embedded server task plus the running client service so
/// the daemon main loop can drive `tools/call` / subscribe / read on
/// the client and shut everything down cooperatively through the
/// cancellation token.
///
/// Generic over the rmcp client `Service` so test harnesses can use the
/// default `ClientInfo` while production wiring uses the custom
/// [`crate::embed::event_mux::EmbedClient`] handler that forwards
/// notifications onto the formatter mpsc.
#[derive(Debug)]
pub struct EmbedHandle<S>
where
    S: Service<RoleClient>,
{
    /// Background task running the rmcp server side. Aborts on
    /// [`Self::shutdown`] cancel.
    server_task: JoinHandle<()>,
    /// Live rmcp client handshake holding the `Peer<RoleClient>` we
    /// dispatch ops onto.
    client_service: Arc<RunningService<RoleClient, S>>,
    /// Cooperative shutdown token. Cancel to drain everything.
    shutdown: CancellationToken,
}

impl<S> EmbedHandle<S>
where
    S: Service<RoleClient>,
{
    /// Borrow the live client service handle.
    #[must_use]
    pub const fn client(&self) -> &Arc<RunningService<RoleClient, S>> {
        &self.client_service
    }

    /// Borrow the cooperative shutdown token.
    #[must_use]
    pub const fn shutdown_token(&self) -> &CancellationToken {
        &self.shutdown
    }

    /// Cancel the cooperative shutdown token and join the server task.
    /// Returns once the server task has finished (or the task join
    /// itself fails — which is logged at warn level).
    pub async fn shutdown(self) {
        self.shutdown.cancel();
        let Self {
            server_task,
            client_service,
            ..
        } = self;
        // Drop the client first so its peer flushes any in-flight
        // requests through the duplex before the server task aborts.
        drop(client_service);
        if let Err(err) = server_task.await {
            tracing::warn!("embed server task join failed: {err}");
        }
    }
}

/// Experimental capability key advertised by the daemon client.
///
/// Echoed on `initialize` so the server-side ADR 0012 recorder activates
/// inline push for the in-process peer. Must match the key advertised by
/// the server side in
/// `crate::infra::mcp::tool_router::EXPERIMENTAL_INLINE_PUSH_KEY` so the
/// per-process registry observes the daemon as a capability-granting
/// peer.
pub const EMBED_CLIENT_EXPERIMENTAL_INLINE_PUSH: &str = "ssh_inline_push";

/// Build the standard `ClientInfo` advertised by the embedded daemon
/// client.
///
/// Capabilities are intentionally minimal — the embed client only
/// consumes `tools/call`, `resources/subscribe`, and the matching
/// notifications, so we leave roots / sampling / elicitation off.
/// `rmcp::model::{ClientInfo, Implementation}` are
/// `#[non_exhaustive]`, so the builder goes through the public
/// `::new` constructors and `with_protocol_version` chained setter
/// rather than struct literals.
///
/// ADR 0012 Phase 7 — the advertised capability map carries
/// `experimental.ssh_inline_push = {}` so the server-side recorder
/// installed at composition time flips the per-peer
/// `CapabilityRegistry::InlinePush` bit on the daemon's own internal
/// peer. With the bit set, `sub_open inline_push=true` lanes opened
/// through the daemon's NDJSON surface deliver
/// `notifications/ssh/output` events that the embedded client then
/// translates into NDJSON `inline_push` events (env-gated by
/// `SSH_INLINE_PUSH_DAEMON_RELAY`).
#[must_use]
pub fn embed_client_info() -> ClientInfo {
    let implementation = Implementation::new("ssh-mcp-tail (embed)", env!("CARGO_PKG_VERSION"));
    let mut capabilities = ClientCapabilities::default();
    let mut experimental = ExperimentalCapabilities::new();
    experimental.insert(
        EMBED_CLIENT_EXPERIMENTAL_INLINE_PUSH.to_string(),
        JsonObject::new(),
    );
    capabilities.experimental = Some(experimental);
    ClientInfo::new(capabilities, implementation).with_protocol_version(ProtocolVersion::default())
}

/// Wire one half of a `tokio::io::duplex` pair as the rmcp server side
/// of the embedded transport. The server is consumed by the spawned
/// task; the returned `JoinHandle` aborts when the duplex closes.
#[must_use]
pub fn spawn_server_side(
    server: McpSshServer<ProdUseCases>,
    stream: DuplexStream,
) -> JoinHandle<()> {
    let (read, write) = split(stream);
    tokio::spawn(async move {
        let transport = AsyncRwTransport::new_server(read, write);
        match server.serve(transport).await {
            Ok(running) => {
                if let Err(err) = running.waiting().await {
                    tracing::warn!("embed server waiting() returned: {err}");
                }
            }
            Err(err) => {
                tracing::warn!("embed server failed to start: {err}");
            }
        }
    })
}

/// Wire the other half as the rmcp client and drive the initialisation
/// handshake using the supplied service handler.
///
/// Returns the running service the dispatcher uses to issue
/// `tools/call` and `resources/subscribe` requests. Production wiring
/// passes an [`crate::embed::event_mux::EmbedClient`] so
/// notifications surface on the daemon's NDJSON stdout; tests pass
/// the rmcp default `ClientInfo` when notification capture isn't
/// needed.
///
/// # Errors
/// Returns [`EmbedError::ClientInit`] if the rmcp handshake fails.
pub async fn build_client_side<S>(
    handler: S,
    stream: DuplexStream,
) -> Result<RunningService<RoleClient, S>, EmbedError>
where
    S: Service<RoleClient>,
{
    let (read, write) = split(stream);
    let transport = AsyncRwTransport::new_client(read, write);
    handler
        .serve(transport)
        .await
        .map_err(|err| EmbedError::ClientInit(err.to_string()))
}

/// Buffer size of the in-process duplex pair. 64 KiB matches the
/// `tokio::io::duplex` example in ADR 0008 and is large enough that
/// realistic JSON-RPC requests (typically a few KiB) flow without
/// stalling.
pub const EMBED_DUPLEX_BUFFER: usize = 64 * 1_024;

/// Build a duplex pair sized for the embed transport. Convenience
/// wrapper around `tokio::io::duplex` — kept here so the buffer size
/// is documented in one place.
#[must_use]
pub fn duplex_pair() -> (DuplexStream, DuplexStream) {
    duplex(EMBED_DUPLEX_BUFFER)
}

/// Wrap an already-spawned server task and an already-handshook
/// client into a fresh [`EmbedHandle`]. Used by both the production
/// composition root and the integration tests.
#[must_use]
pub fn assemble_handle<S>(
    server_task: JoinHandle<()>,
    client_service: RunningService<RoleClient, S>,
    shutdown: CancellationToken,
) -> EmbedHandle<S>
where
    S: Service<RoleClient>,
{
    EmbedHandle {
        server_task,
        client_service: Arc::new(client_service),
        shutdown,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test-only assertions are deliberately direct"
)]
mod tests {
    use super::*;

    #[test]
    fn embed_client_info_has_implementation_name() {
        let info = embed_client_info();
        assert_eq!(info.client_info.name, "ssh-mcp-tail (embed)");
        assert!(!info.client_info.version.is_empty());
    }

    #[test]
    fn client_capabilities_advertise_ssh_inline_push() {
        let info = embed_client_info();
        let experimental = info
            .capabilities
            .experimental
            .as_ref()
            .expect("daemon client must advertise an experimental map (ADR 0012 phase 7)");
        assert!(
            experimental.contains_key(EMBED_CLIENT_EXPERIMENTAL_INLINE_PUSH),
            "experimental map must include ssh_inline_push so the in-process recorder activates"
        );
    }

    #[test]
    fn duplex_pair_has_documented_buffer() {
        let (a, b) = duplex_pair();
        // The duplex returns two halves wired together — drop them
        // immediately to avoid pinning resources in the test runtime.
        drop(a);
        drop(b);
        assert_eq!(EMBED_DUPLEX_BUFFER, 64 * 1_024);
    }
}
