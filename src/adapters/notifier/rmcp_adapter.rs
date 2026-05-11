//! Production [`NotifierPort`] adapter that fans out
//! `notifications/resources/updated` and (since ADR 0012, v7.1)
//! `notifications/ssh/output` over rmcp.
//!
//! The adapter receives the type-erased subscriber as
//! `Arc<dyn PeerHandle>` (per the port). It resolves it to the live
//! [`rmcp::Peer<RoleServer>`] through the shared
//! [`crate::adapters::notifier::rmcp_peer::PeerTable`] populated by
//! [`crate::adapters::notifier::rmcp_peer::RmcpPeerHandle`]. A miss means
//! the peer's [`crate::adapters::notifier::rmcp_peer::RmcpPeerHandle`]
//! has already been dropped (transport closed); we treat that as an
//! at-most-once delivery and short-circuit with [`Ok`].

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use rmcp::model::{CustomNotification, ResourceUpdatedNotificationParam, ServerNotification};
use serde_json::json;

use crate::adapters::config::internal::resolve_inline_push_max_bytes_per_notify;
use crate::domain::error::DomainError;
use crate::domain::inline_payload::InlinePayload;
use crate::ports::notifier::{NotifierPort, PeerHandle};

use super::rmcp_peer::PeerTable;

/// MCP notification method ADR 0012 reserves for v7.1 inline-push
/// delivery. Hosts that signed the matching capability flag at
/// `initialize` time route this method onto the originating subscription.
const NOTIFICATIONS_SSH_OUTPUT: &str = "notifications/ssh/output";

/// Production [`crate::ports::notifier::NotifierPort`] adapter backed by rmcp.
///
/// Constructed once at the composition root with the shared
/// [`PeerTable`] and cloned freely (the inner [`Arc`] is cheap to clone).
#[derive(Debug, Clone)]
pub struct RmcpNotifier {
    /// Shared lookup [`crate::domain::ids::PeerId`] -> live rmcp peer.
    /// Written by [`crate::adapters::notifier::rmcp_peer::RmcpPeerHandle::new`].
    peers: Arc<PeerTable>,
}

impl RmcpNotifier {
    /// Build a notifier that resolves peers through the shared `peers`
    /// table.
    #[must_use]
    pub const fn new(peers: Arc<PeerTable>) -> Self {
        Self { peers }
    }
}

impl NotifierPort for RmcpNotifier {
    async fn notify_resource_updated(
        &self,
        peer: Arc<dyn PeerHandle>,
        uri: &str,
    ) -> Result<(), DomainError> {
        let peer_id = peer.id();
        // Snapshot the rmcp peer out of the shared table BEFORE awaiting so
        // we never hold a DashMap shard guard across `.await`. `Peer` is
        // cheap to clone (mpsc::Sender + Arc).
        let Some(rmcp_peer) = self.peers.lookup_peer(&peer_id) else {
            // The matching peer has been GC-collected (transport closed).
            // Resource notifications are at-most-once, so a miss is not
            // an error.
            return Ok(());
        };
        let params = ResourceUpdatedNotificationParam {
            uri: uri.to_string(),
        };
        rmcp_peer
            .notify_resource_updated(params)
            .await
            .map_err(|err| DomainError::Transport(err.to_string()))
    }

    async fn notify_ssh_output(
        &self,
        peer: Arc<dyn PeerHandle>,
        payload: InlinePayload,
    ) -> Result<(), DomainError> {
        // ADR 0012 phase 10 — defensive port-boundary guard. The
        // production `LaneFanoutBridge::ship_inline_fragments` path
        // splits every payload through `InlinePayload::split(cap, ..)`
        // so each fragment is under the cap; this branch fires only
        // when a direct port caller (test harness, future SDK consumer)
        // bypasses the splitter and hands a too-large payload.
        let cap_bytes = resolve_inline_push_max_bytes_per_notify();
        if payload.bytes.len() > cap_bytes {
            return Err(DomainError::InlinePushOversize {
                payload_bytes: payload.bytes.len(),
                cap_bytes,
            });
        }
        // Snapshot the rmcp peer BEFORE awaiting, identical to the
        // `notify_resource_updated` path. ADR 0012 reuses the at-most-once
        // semantics: a missing peer means the transport already closed.
        let Some(rmcp_peer) = self.peers.lookup_peer(&peer.id()) else {
            return Ok(());
        };
        let notification = ServerNotification::CustomNotification(CustomNotification::new(
            NOTIFICATIONS_SSH_OUTPUT,
            Some(inline_payload_params(&payload)),
        ));
        rmcp_peer
            .send_notification(notification)
            .await
            .map_err(|err| DomainError::Transport(err.to_string()))
    }
}

/// Serialize an [`InlinePayload`] into the JSON-RPC params object
/// described by ADR 0012 for `notifications/ssh/output`. The wire shape
/// is `{sub_id, uri, seq, cursor_after, bytes_b64, len, truncated}` —
/// `len` is derived from the raw byte slice to keep the domain
/// `bytes.len()` invariant authoritative and avoid carrying a cached
/// length on the wire.
fn inline_payload_params(payload: &InlinePayload) -> serde_json::Value {
    let len = payload.bytes.len();
    let bytes_b64 = BASE64_STANDARD.encode(&payload.bytes);
    json!({
        "sub_id": payload.sub_id.as_str(),
        "uri": payload.uri,
        "seq": payload.seq,
        "cursor_after": payload.cursor_after,
        "bytes_b64": bytes_b64,
        "len": len,
        "truncated": payload.truncated,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests use unwrap for brevity per CLAUDE.md test policy"
)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::RmcpNotifier;
    use crate::adapters::notifier::rmcp_peer::new_peer_table;
    use crate::domain::ids::PeerId;
    use crate::ports::notifier::{NotifierPort, PeerHandle};
    // `NotifierPort` is the `Send`-bounded alias minted by `trait_variant`;
    // `notify_resource_updated` lives on it.

    fn _assert_send_sync<T: Send + Sync>() {}

    #[derive(Debug)]
    struct StubPeer {
        id: PeerId,
        closed: AtomicBool,
    }

    impl StubPeer {
        fn new(id: &str) -> Self {
            Self {
                id: PeerId::new(id.to_string()),
                closed: AtomicBool::new(false),
            }
        }
    }

    impl PeerHandle for StubPeer {
        fn id(&self) -> PeerId {
            self.id.clone()
        }
        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::Relaxed)
        }
    }

    #[test]
    fn rmcp_notifier_is_send_sync() {
        _assert_send_sync::<RmcpNotifier>();
    }

    #[test]
    fn rmcp_notifier_compiles_as_notifier_port() {
        // Trait bound check: `RmcpNotifier` must satisfy `NotifierPort`
        // (the public `Send`-bounded alias minted by `trait_variant`).
        fn _accepts<T: NotifierPort + Send + Sync>(_t: T) {}
        let table = new_peer_table();
        let notifier = RmcpNotifier::new(table);
        _accepts(notifier);
    }

    #[tokio::test]
    async fn notify_unknown_peer_returns_ok_silently() {
        // Peer is not registered in the shared table — the adapter must
        // treat the miss as at-most-once and return Ok.
        let table = new_peer_table();
        let notifier = RmcpNotifier::new(table);
        let peer: Arc<dyn PeerHandle> = Arc::new(StubPeer::new("ghost"));
        let res = notifier
            .notify_resource_updated(peer, "shell://abc/output")
            .await;
        assert!(res.is_ok(), "missing peer must not error: {res:?}");
    }

    #[tokio::test]
    async fn notify_ssh_output_unknown_peer_returns_ok_silently() {
        // Phase-2 mirror of the resource-updated at-most-once contract:
        // the inline-push path must short-circuit with Ok when the
        // matching rmcp peer has already been GC-collected.
        use crate::domain::inline_payload::InlinePayload;
        use crate::domain::subscription::SubId;

        let table = new_peer_table();
        let notifier = RmcpNotifier::new(table);
        let peer: Arc<dyn PeerHandle> = Arc::new(StubPeer::new("ghost"));
        let payload = InlinePayload::new(
            SubId::new("0193f04e-3a2b-7c12-8d11-1f1f04ab92e1".to_string()),
            "shell://abc/output".to_string(),
            1,
            64,
            b"hello".to_vec(),
            false,
        );
        let res = notifier.notify_ssh_output(peer, payload).await;
        assert!(res.is_ok(), "missing peer must not error: {res:?}");
    }

    #[tokio::test]
    async fn notify_ssh_output_returns_oversize_when_payload_exceeds_cap() {
        // ADR 0012 phase 10 — defensive guard at the port boundary.
        // Construct a payload larger than the server cap and confirm
        // the adapter short-circuits with `INLINE_PUSH_OVERSIZE`
        // BEFORE attempting the peer lookup. The lookup table is
        // empty here; if the guard were missing, the function would
        // return `Ok(())` via the at-most-once short-circuit, masking
        // the misuse.
        use crate::adapters::config::internal::{
            INLINE_PUSH_MAX_BYTES_PER_NOTIFY_ENV_VAR, resolve_inline_push_max_bytes_per_notify,
        };
        use crate::domain::error::DomainError;
        use crate::domain::inline_payload::InlinePayload;
        use crate::domain::subscription::SubId;

        // Force the cap to its default by clearing the env var; the
        // resolver returns 32 KiB. A `cap + 1024` byte payload trips
        // the guard. The env removal is single-line, immediately
        // followed by the resolver read; no other thread observes the
        // transient unset state.
        // SAFETY: env mutation is consistent with the other env tests
        // in this crate (see `inline_push_max_bytes_per_notify` tests
        // in `adapters/config/internal/mod.rs`).
        #[allow(
            unsafe_code,
            reason = "test-only env mutation paired with immediate read; same shape as the existing env tests in adapters/config/internal/mod.rs"
        )]
        unsafe {
            std::env::remove_var(INLINE_PUSH_MAX_BYTES_PER_NOTIFY_ENV_VAR);
        }
        let cap = resolve_inline_push_max_bytes_per_notify();
        let payload_size = cap + 1024;

        let table = new_peer_table();
        let notifier = RmcpNotifier::new(table);
        let peer: Arc<dyn PeerHandle> = Arc::new(StubPeer::new("p1"));
        let payload = InlinePayload::new(
            SubId::new("0193f04e-3a2b-7c12-8d11-1f1f04ab92e2".to_string()),
            "shell://abc/output".to_string(),
            1,
            payload_size as u64,
            vec![0_u8; payload_size],
            false,
        );
        let res = notifier.notify_ssh_output(peer, payload).await;
        match res {
            Err(DomainError::InlinePushOversize {
                payload_bytes,
                cap_bytes,
            }) => {
                assert_eq!(payload_bytes, payload_size);
                assert_eq!(cap_bytes, cap);
            }
            other => panic!("expected InlinePushOversize, got {other:?}"),
        }
    }

    #[test]
    fn inline_payload_params_shape_matches_adr0012() {
        // ADR 0012 wire shape: `{sub_id, uri, seq, cursor_after,
        // bytes_b64, len, truncated}`. `len` is derived from
        // `bytes.len()` and `bytes_b64` is standard base64.
        use crate::domain::inline_payload::InlinePayload;
        use crate::domain::subscription::SubId;

        let payload = InlinePayload::new(
            SubId::new("0193f04e-3a2b-7c12-8d11-1f1f04ab92e1".to_string()),
            "shell://abc/output".to_string(),
            7,
            128,
            b"hello".to_vec(),
            true,
        );
        let params = super::inline_payload_params(&payload);
        assert_eq!(params["sub_id"], "0193f04e-3a2b-7c12-8d11-1f1f04ab92e1");
        assert_eq!(params["uri"], "shell://abc/output");
        assert_eq!(params["seq"], 7);
        assert_eq!(params["cursor_after"], 128);
        assert_eq!(params["len"], 5);
        assert_eq!(params["truncated"], true);
        // "hello" -> base64 standard `aGVsbG8=`
        assert_eq!(params["bytes_b64"], "aGVsbG8=");
    }
}
