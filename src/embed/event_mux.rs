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
    CancelledNotificationParam, ClientCapabilities, ClientInfo, CustomNotification, Implementation,
    ProgressNotificationParam, ProtocolVersion, ResourceUpdatedNotificationParam,
};
use rmcp::service::{NotificationContext, RoleClient};
use tokio::io::AsyncWrite;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval as tokio_interval};
use tokio_util::sync::CancellationToken;

use crate::adapters::config::internal::resolve_inline_push_daemon_relay;
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
/// The mux does not currently track counters of its own — the
/// lifecycle / leak counters live on the Phase 3 `LeakWatcher` and
/// per-lane `SubscriberStats` (`adapters::subscription`). The daemon
/// emits the stable shape with zero baselines for fields the embed
/// layer does not currently observe so consumers can wire dashboards
/// against a forward-compatible schema.
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
        // waiting for a dedicated read-and-format pipeline.
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

    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _ctx: NotificationContext<RoleClient>,
    ) {
        if notification.method.as_str() != NOTIFICATIONS_SSH_OUTPUT {
            tracing::trace!(
                "custom notification ignored (method={})",
                notification.method
            );
            return;
        }
        translate_inline_push(&self.tx, &notification);
    }
}

/// MCP notification method advertised by ADR 0012 for inline-push
/// delivery. Matches the constant on the server-side notifier
/// adapter — the wire key must never drift across sites.
const NOTIFICATIONS_SSH_OUTPUT: &str = "notifications/ssh/output";

/// ADR 0012 Phase 7 wire-error code carried by the daemon's
/// `INLINE_PUSH_BAD_PARAMS` synthetic `Event::Err`. Keeps the
/// wire-resilience contract auditable from a single name.
pub const INLINE_PUSH_BAD_PARAMS_CODE: &str = "INLINE_PUSH_BAD_PARAMS";

/// Translate a `notifications/ssh/output` custom notification into a
/// [`Event::InlinePush`] NDJSON line and push it onto the outbound
/// mux.
///
/// Gated by [`resolve_inline_push_daemon_relay`]: when the env var
/// is not truthy the function returns silently so v7.0.x NDJSON
/// consumers see no behavioural change.
///
/// Parses the JSON-RPC params object without decoding the
/// `bytes_b64` string — ADR 0012 mandates verbatim pass-through so
/// the daemon never round-trips through base64. Missing or mistyped
/// params surface as an `Event::Err{code: INLINE_PUSH_BAD_PARAMS}`
/// so the daemon stays resilient (no panic, no abort).
fn translate_inline_push(tx: &EventTx, notification: &CustomNotification) {
    if !resolve_inline_push_daemon_relay() {
        tracing::trace!("inline-push relay disabled; dropping notifications/ssh/output silently");
        return;
    }
    let event = build_inline_push_event(notification.params.as_ref());
    if let Err(err) = tx.try_send(event) {
        tracing::trace!("inline_push forward dropped: {err}");
    }
}

/// Pure helper — convert the JSON-RPC params blob carried by a
/// `notifications/ssh/output` notification into either an
/// [`Event::InlinePush`] or an [`Event::Err`] when the params are
/// missing / mistyped. Split off the async path so unit tests can
/// drive the translation with a synthetic params blob.
fn build_inline_push_event(params: Option<&serde_json::Value>) -> Event {
    params.map_or_else(missing_params_event, |value| {
        parse_inline_push_params(value).unwrap_or_else(malformed_params_event)
    })
}

fn missing_params_event() -> Event {
    Event::Err {
        id: None,
        code: INLINE_PUSH_BAD_PARAMS_CODE.to_string(),
        reason: "notifications/ssh/output missing params".to_string(),
        detail: Some(
            "Server must send a JSON object carrying sub_id/uri/seq/cursor_after/len/bytes_b64/truncated."
                .to_string(),
        ),
    }
}

fn malformed_params_event(missing: &'static str) -> Event {
    Event::Err {
        id: None,
        code: INLINE_PUSH_BAD_PARAMS_CODE.to_string(),
        reason: "notifications/ssh/output params malformed".to_string(),
        detail: Some(format!(
            "Missing or mistyped param: {missing}. Expected sub_id/uri/seq/cursor_after/len/bytes_b64/truncated."
        )),
    }
}

/// Decode the `notifications/ssh/output` params object into an
/// [`Event::InlinePush`]. Returns the missing / mistyped field name
/// on failure so the caller can surface a precise error event.
fn parse_inline_push_params(value: &serde_json::Value) -> Result<Event, &'static str> {
    let obj = value.as_object().ok_or("<root-not-object>")?;
    let sub_id = take_string(obj, "sub_id")?;
    let uri = take_string(obj, "uri")?;
    let seq = take_u64(obj, "seq")?;
    let cursor_after = take_u64(obj, "cursor_after")?;
    let len = take_u64(obj, "len")?;
    let bytes_b64 = take_string(obj, "bytes_b64")?;
    let truncated = take_bool(obj, "truncated")?;
    Ok(Event::InlinePush {
        sub_id,
        uri,
        seq,
        cursor_after,
        len,
        bytes_b64,
        truncated,
    })
}

fn take_string(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<String, &'static str> {
    obj.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or(key)
}

fn take_u64(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<u64, &'static str> {
    obj.get(key).and_then(serde_json::Value::as_u64).ok_or(key)
}

fn take_bool(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<bool, &'static str> {
    obj.get(key).and_then(serde_json::Value::as_bool).ok_or(key)
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

    // ADR 0012 Phase 7 — daemon inline-push relay.
    #[allow(
        unsafe_code,
        reason = "Rust 2024 requires unsafe for env::set_var; tests serialize via ENV_GUARD"
    )]
    mod inline_push_translation {
        use super::*;
        use crate::adapters::config::internal::INLINE_PUSH_DAEMON_RELAY_ENV_VAR;
        use serde_json::json;
        use std::sync::{LazyLock, Mutex as StdMutex};

        /// Serialise env-mutating tests so the shared `SSH_INLINE_PUSH_DAEMON_RELAY`
        /// variable cannot race across `cargo test` worker threads. Mirrors the
        /// `ENV_TEST_MUTEX` pattern used in `adapters::config::internal`.
        static ENV_GUARD: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

        fn sample_params() -> serde_json::Value {
            json!({
                "sub_id": "0193f04e-3a2b-7c12-8d11-1f1f04ab92e1",
                "uri": "shell://abc/output",
                "seq": 7_u64,
                "cursor_after": 42_u64,
                "len": 5_u64,
                "bytes_b64": "aGVsbG8=",
                "truncated": false,
            })
        }

        fn sample_notification() -> CustomNotification {
            CustomNotification::new(NOTIFICATIONS_SSH_OUTPUT, Some(sample_params()))
        }

        fn unset_relay() {
            // SAFETY: ENV_GUARD held by every caller of this helper.
            unsafe { std::env::remove_var(INLINE_PUSH_DAEMON_RELAY_ENV_VAR) };
        }

        fn set_relay(value: &str) {
            // SAFETY: ENV_GUARD held by every caller of this helper.
            unsafe { std::env::set_var(INLINE_PUSH_DAEMON_RELAY_ENV_VAR, value) };
        }

        #[test]
        fn build_inline_push_event_parses_valid_params() {
            let event = build_inline_push_event(Some(&sample_params()));
            match event {
                Event::InlinePush {
                    sub_id,
                    uri,
                    seq,
                    cursor_after,
                    len,
                    bytes_b64,
                    truncated,
                } => {
                    assert_eq!(sub_id, "0193f04e-3a2b-7c12-8d11-1f1f04ab92e1");
                    assert_eq!(uri, "shell://abc/output");
                    assert_eq!(seq, 7);
                    assert_eq!(cursor_after, 42);
                    assert_eq!(len, 5);
                    assert_eq!(bytes_b64, "aGVsbG8=");
                    assert!(!truncated);
                }
                other => panic!("expected Event::InlinePush, got {other:?}"),
            }
        }

        #[test]
        fn build_inline_push_event_emits_err_on_missing_param() {
            let mut params = sample_params();
            let obj = params.as_object_mut().unwrap();
            obj.remove("bytes_b64");
            let event = build_inline_push_event(Some(&params));
            match event {
                Event::Err { code, detail, .. } => {
                    assert_eq!(code, INLINE_PUSH_BAD_PARAMS_CODE);
                    let detail = detail.unwrap_or_default();
                    assert!(
                        detail.contains("bytes_b64"),
                        "detail must surface the missing field; got {detail}"
                    );
                }
                other => panic!("expected Event::Err on missing param, got {other:?}"),
            }
        }

        #[test]
        fn build_inline_push_event_emits_err_when_params_absent() {
            let event = build_inline_push_event(None);
            match event {
                Event::Err { code, .. } => {
                    assert_eq!(code, INLINE_PUSH_BAD_PARAMS_CODE);
                }
                other => panic!("expected Event::Err when params absent, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn translates_ssh_output_to_inline_push_event_when_relay_enabled() {
            let _g = ENV_GUARD.lock().unwrap();
            set_relay("1");
            let (tx, mut rx) = make_pair();
            translate_inline_push(&tx, &sample_notification());
            unset_relay();
            let event = rx.try_recv().expect("relay must emit an event");
            match event {
                Event::InlinePush { seq, len, .. } => {
                    assert_eq!(seq, 7);
                    assert_eq!(len, 5);
                }
                other => panic!("expected InlinePush, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn drops_ssh_output_silently_when_relay_disabled() {
            let _g = ENV_GUARD.lock().unwrap();
            unset_relay();
            let (tx, mut rx) = make_pair();
            translate_inline_push(&tx, &sample_notification());
            assert!(
                rx.try_recv().is_err(),
                "relay disabled: no event must be queued"
            );
        }

        #[tokio::test]
        async fn emits_error_event_on_missing_param_via_translate() {
            let _g = ENV_GUARD.lock().unwrap();
            set_relay("yes");
            let (tx, mut rx) = make_pair();
            let mut bad_params = sample_params();
            bad_params.as_object_mut().unwrap().remove("bytes_b64");
            let bad_notification =
                CustomNotification::new(NOTIFICATIONS_SSH_OUTPUT, Some(bad_params));
            translate_inline_push(&tx, &bad_notification);
            unset_relay();
            let event = rx.try_recv().expect("error event must be queued");
            match event {
                Event::Err { code, .. } => assert_eq!(code, INLINE_PUSH_BAD_PARAMS_CODE),
                other => panic!("expected Event::Err, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn env_truthy_parsing_matches_existing_convention() {
            let _g = ENV_GUARD.lock().unwrap();
            for raw in ["true", "TRUE", "1", "yes", "Yes", "on", "ON"] {
                set_relay(raw);
                let (tx, mut rx) = make_pair();
                translate_inline_push(&tx, &sample_notification());
                assert!(
                    rx.try_recv().is_ok(),
                    "relay must be ENABLED for env value {raw:?}"
                );
            }
            for raw in ["false", "FALSE", "0", "no", "off", "", "garbage"] {
                set_relay(raw);
                let (tx, mut rx) = make_pair();
                translate_inline_push(&tx, &sample_notification());
                assert!(
                    rx.try_recv().is_err(),
                    "relay must be DISABLED for env value {raw:?}"
                );
            }
            unset_relay();
        }
    }
}
