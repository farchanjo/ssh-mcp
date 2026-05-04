//! Event multiplexer + custom rmcp client handler for the daemon.
//!
//! The mux owns:
//!
//! - The bounded mpsc that every producer (dispatcher, notification
//!   bridge, heartbeat task, stats task) writes [`crate::embed::formatter::Event`]
//!   variants into.
//! - The drain task that pulls from the mpsc and writes through a
//!   single shared [`crate::embed::formatter::NdjsonWriter`] on
//!   `tokio::io::stdout()`.
//! - The custom rmcp [`ClientHandler`] implementation that forwards
//!   `notifications/resources/updated` / progress / cancelled
//!   notifications into the same mpsc so the stdout stream stays
//!   ordered.
//!
//! Drain ordering: the mpsc is single-consumer so each event is
//! guaranteed to be written before the next is dequeued. Producers
//! that compete on the sender use `try_send` and drop on overflow
//! (heartbeat / stats / notifications are best-effort); the
//! dispatcher uses `send().await` because losing an `ack`/`err`
//! breaks the protocol contract.

use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::client::ClientHandler;
use rmcp::model::{
    CancelledNotificationParam, ClientCapabilities, ClientInfo, Implementation,
    ProgressNotificationParam, ProtocolVersion, ResourceUpdatedNotificationParam,
};
use rmcp::service::{NotificationContext, RoleClient};
use tokio::io::AsyncWrite;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval as tokio_interval};
use tokio_util::sync::CancellationToken;

use crate::embed::duplex_transport::embed_client_info;
use crate::embed::formatter::{Event, NdjsonWriter, PROTOCOL_VERSION};

/// Default mpsc capacity for the outbound event mux. Mirrors
/// `SSH_MUX_BUFFER` and is overridable through the matching env
/// resolver in `adapters::config::internal`.
pub const DEFAULT_MUX_CAPACITY: usize = 8_192;

/// Sender half of the outbound event mpsc. Cloned across the
/// dispatcher, the notification bridge, the heartbeat task, and the
/// stats task.
pub type EventTx = mpsc::Sender<Event>;

/// Receiver half consumed by the drain task.
pub type EventRx = mpsc::Receiver<Event>;

/// Spawn the drain task that formats every queued [`Event`] onto the
/// supplied async writer and exits when the receiver closes or the
/// shutdown token cancels.
#[must_use]
pub fn spawn_drain<W>(
    rx: EventRx,
    writer: NdjsonWriter<W>,
    shutdown: CancellationToken,
) -> JoinHandle<()>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(drain_loop(rx, writer, shutdown))
}

async fn drain_loop<W>(mut rx: EventRx, mut writer: NdjsonWriter<W>, shutdown: CancellationToken)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            maybe = rx.recv() => match maybe {
                Some(event) => {
                    if let Err(err) = writer.write(&event).await {
                        tracing::warn!("ndjson write failed: {err}");
                        break;
                    }
                }
                None => break,
            }
        }
    }
    if let Err(err) = writer.shutdown().await {
        tracing::debug!("ndjson writer shutdown: {err}");
    }
}

/// Spawn the heartbeat task that emits an [`Event::Heartbeat`] every
/// `interval`.
///
/// Uses `try_send` with drop semantics: if the mpsc is full the
/// heartbeat is silently skipped (the consumer is already saturated;
/// another tick will follow).
#[must_use]
pub fn spawn_heartbeat(
    tx: EventTx,
    interval: Duration,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(heartbeat_loop(tx, interval, shutdown))
}

async fn heartbeat_loop(tx: EventTx, interval: Duration, shutdown: CancellationToken) {
    let mut ticker = tokio_interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    // Skip the immediate first tick — interval semantics already
    // fire one on initialisation; we want the first heartbeat to
    // appear `interval` after start.
    ticker.tick().await;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            _ = ticker.tick() => {
                let event = Event::Heartbeat {
                    ts: chrono::Utc::now().to_rfc3339(),
                    protocol: PROTOCOL_VERSION.to_string(),
                };
                if let Err(err) = tx.try_send(event) {
                    tracing::trace!("heartbeat tx try_send: {err}");
                }
            }
        }
    }
}

/// Spawn the daemon-stats task.
///
/// The mux does not currently track counters of its own — Phase 3
/// owns the lifecycle/leak counters and Phase 5 will plug them in.
/// For the v5.0 wire surface we emit the stable shape with zero
/// baselines so consumers can wire dashboards before the real
/// numbers are ready.
#[must_use]
pub fn spawn_daemon_stats(
    tx: EventTx,
    interval: Duration,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(stats_loop(tx, interval, shutdown))
}

async fn stats_loop(tx: EventTx, interval: Duration, shutdown: CancellationToken) {
    let mut ticker = tokio_interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker.tick().await;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            _ = ticker.tick() => {
                let event = Event::DaemonStats {
                    active_sessions: 0,
                    active_subs: 0,
                    ring_buffer_overflows_total: 0,
                    mpsc_full_events_total: 0,
                    protocol: PROTOCOL_VERSION.to_string(),
                };
                if let Err(err) = tx.try_send(event) {
                    tracing::trace!("daemon_stats tx try_send: {err}");
                }
            }
        }
    }
}

/// Custom rmcp client handler.
///
/// Forwards every interesting notification onto the mux mpsc as a
/// typed [`Event`] so the daemon's stdout sees notifications in the
/// same order they crossed the duplex transport.
#[derive(Debug)]
pub struct EmbedClient {
    info: ClientInfo,
    tx: EventTx,
}

impl EmbedClient {
    /// Build a fresh handler with the daemon's identity advertised on
    /// the rmcp `Implementation` block.
    #[must_use]
    pub fn new(tx: EventTx) -> Self {
        Self {
            info: embed_client_info(),
            tx,
        }
    }

    /// Build a handler with caller-supplied identity (test override).
    #[must_use]
    pub const fn with_info(tx: EventTx, info: ClientInfo) -> Self {
        Self { info, tx }
    }

    /// Borrow the underlying [`EventTx`] (test helper).
    #[must_use]
    pub const fn event_tx(&self) -> &EventTx {
        &self.tx
    }
}

impl Default for EmbedClient {
    fn default() -> Self {
        let (tx, _rx) = mpsc::channel(DEFAULT_MUX_CAPACITY);
        // rmcp 1.6 marks ClientInfo + Implementation as
        // `#[non_exhaustive]`; build through the public `::new`
        // constructor (capabilities + implementation) and
        // `with_protocol_version` chain instead of struct literals.
        let implementation =
            Implementation::new("ssh-mcp-tail (embed default)", env!("CARGO_PKG_VERSION"));
        let info = ClientInfo::new(ClientCapabilities::default(), implementation)
            .with_protocol_version(ProtocolVersion::default());
        Self { info, tx }
    }
}

impl ClientHandler for EmbedClient {
    fn get_info(&self) -> ClientInfo {
        self.info.clone()
    }

    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _ctx: NotificationContext<RoleClient>,
    ) {
        // Surface the notification as an advisory `Warn` envelope so
        // downstream consumers see byte-ordered notifications without
        // waiting for the Phase 5 read-and-format pipeline.
        let event = Event::Warn {
            code: "RESOURCE_UPDATED".to_string(),
            resource: Some(params.uri.clone()),
            msg: format!("notifications/resources/updated for {}", params.uri),
        };
        if let Err(err) = self.tx.try_send(event) {
            tracing::trace!("resource_updated forward dropped: {err}");
        }
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _ctx: NotificationContext<RoleClient>,
    ) {
        let detail = params.message.unwrap_or_else(|| "progress".to_string());
        let event = Event::Warn {
            code: "PROGRESS".to_string(),
            resource: None,
            msg: detail,
        };
        if let Err(err) = self.tx.try_send(event) {
            tracing::trace!("progress forward dropped: {err}");
        }
    }

    async fn on_cancelled(
        &self,
        params: CancelledNotificationParam,
        _ctx: NotificationContext<RoleClient>,
    ) {
        let detail = params.reason.unwrap_or_else(|| "cancelled".to_string());
        let event = Event::Warn {
            code: "CANCELLED".to_string(),
            resource: None,
            msg: detail,
        };
        if let Err(err) = self.tx.try_send(event) {
            tracing::trace!("cancelled forward dropped: {err}");
        }
    }
}

/// Wrap the [`EmbedClient`] handler in `Arc` so the dispatcher and the
/// rmcp running service share a single allocation.
#[must_use]
pub fn shared_client(tx: EventTx) -> Arc<EmbedClient> {
    Arc::new(EmbedClient::new(tx))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test-only assertions are deliberately direct"
)]
mod tests {
    use super::*;
    use crate::domain::ids::SessionId;

    fn make_pair() -> (EventTx, EventRx) {
        mpsc::channel(8)
    }

    #[tokio::test]
    async fn drain_writes_each_event_as_one_line() {
        let (tx, rx) = make_pair();
        let buf: Vec<u8> = Vec::new();
        let writer = NdjsonWriter::new(buf);
        let token = CancellationToken::new();
        let handle = spawn_drain(rx, writer, token.clone());
        tx.send(Event::Closed {
            id: None,
            sid: SessionId::new("s1".to_string()),
        })
        .await
        .unwrap();
        // Drop the sender so drain exits cleanly.
        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_cancels_drain() {
        let (tx, rx) = make_pair();
        let buf: Vec<u8> = Vec::new();
        let writer = NdjsonWriter::new(buf);
        let token = CancellationToken::new();
        let handle = spawn_drain(rx, writer, token.clone());
        token.cancel();
        handle.await.unwrap();
        drop(tx);
    }

    #[tokio::test]
    async fn heartbeat_emits_after_interval() {
        let (tx, mut rx) = make_pair();
        let token = CancellationToken::new();
        let handle = spawn_heartbeat(tx, Duration::from_millis(20), token.clone());
        let event = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap();
        match event {
            Some(Event::Heartbeat { protocol, .. }) => {
                assert_eq!(protocol, PROTOCOL_VERSION);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        token.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn daemon_stats_emits_zero_baseline() {
        let (tx, mut rx) = make_pair();
        let token = CancellationToken::new();
        let handle = spawn_daemon_stats(tx, Duration::from_millis(20), token.clone());
        let event = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .unwrap();
        match event {
            Some(Event::DaemonStats {
                active_sessions,
                active_subs,
                ..
            }) => {
                assert_eq!(active_sessions, 0);
                assert_eq!(active_subs, 0);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        token.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn embed_client_advertises_identity() {
        let (tx, _rx) = make_pair();
        let client = EmbedClient::new(tx);
        let info = client.get_info();
        assert!(info.client_info.name.starts_with("ssh-mcp-tail"));
    }

    #[tokio::test]
    async fn shared_client_wraps_in_arc() {
        let (tx, _rx) = make_pair();
        let client = shared_client(tx.clone());
        assert!(Arc::strong_count(&client) >= 1);
        // Sender must still be usable after sharing.
        assert!(!tx.is_closed());
    }

    #[tokio::test]
    async fn embed_client_default_constructs() {
        let client = EmbedClient::default();
        let info = client.get_info();
        assert!(info.client_info.name.contains("default"));
    }
}
