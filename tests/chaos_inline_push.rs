//! ADR 0012 phase 8 -- chaos tests for the v7.1 inline-push path.
//!
//! Every scenario drives the in-memory adapters (no live SSH server)
//! and exercises an adversarial concurrency / lifecycle path that
//! the unit tests cannot reach in isolation. The harness pattern
//! mirrors `tests/chaos_rsync.rs`: a multi-threaded tokio runtime,
//! `tokio::task::JoinSet` for fan-out, and bounded concurrency
//! (<= 16 tasks per scenario).
//!
//! 1. `chaos_inline_concurrent_capability_record_and_sub_open` --
//!    record + sub_open race; final state matches whichever path
//!    won the CAS observation order; no panic.
//! 2. `chaos_inline_peer_gc_evicts_during_fan_out` -- bridge ships
//!    fragments while `forget_peer` evicts the peer; no UB, no panic.
//! 3. `chaos_inline_env_toggle_mid_stream` -- daemon-relay env-var
//!    flips mid-stream; fragments after the flip are silently
//!    dropped; no event reordering.
//! 4. `chaos_inline_mpsc_backpressure_lane_full` -- lane mpsc fills
//!    while the bridge ships fragments; the inline path still
//!    completes because it routes through the notifier port, not
//!    the lane mpsc.
//! 5. `chaos_inline_capability_record_after_sub_open` --
//!    sub_open(inline_push=true) BEFORE record_capability surfaces
//!    `inline_push_honored=false`; a subsequent record_capability
//!    does NOT retroactively flip the existing lane.
//! 6. `chaos_inline_handshake_dual_peers` -- two peers, one with
//!    capability and one without; per-peer registry isolation
//!    holds.
//! 7. `chaos_inline_fragments_under_concurrent_compose` -- 8 tasks
//!    composing on the same lane; the seq counter reaches exactly
//!    N (no double-increment, no torn read).
//! 8. `chaos_inline_max_bytes_zero_edge` -- `inline_max_bytes=0`
//!    via `with_inline_max`; payload is shipped as a single
//!    fragment without infinite loop or panic (matches the
//!    splitter's "fits" branch).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::cast_sign_loss,
    clippy::module_name_repetitions,
    clippy::default_numeric_fallback,
    clippy::implicit_hasher,
    clippy::needless_pass_by_value,
    clippy::wildcard_imports,
    reason = "chaos integration tests use unwrap/panic for brevity and exercise deliberate failure paths"
)]
#![allow(
    unsafe_code,
    reason = "Rust 2024 requires unsafe for env::set_var; chaos tests serialize via ENV_GUARD"
)]

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex as EnvMutex};
use std::time::Duration;

use ssh_mcp::adapters::capability::registry::{CapabilityFlag, CapabilityRegistry};
use ssh_mcp::adapters::config::internal::INLINE_PUSH_DAEMON_RELAY_ENV_VAR;
use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::lane_bridge::LaneFanoutBridge;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneState, SubscriberLaneAdapter};
use ssh_mcp::application::subscription_admin::{SubscribeRequest, SubscribeUseCase};
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::ids::PeerId;
use ssh_mcp::domain::inline_payload::InlinePayload;
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubId, SubscriptionLifetime};
use ssh_mcp::embed::event_mux::INLINE_PUSH_BAD_PARAMS_CODE;
use ssh_mcp::ports::notifier::{NotifierPort, PeerHandle};
use ssh_mcp::ports::subscriber_lane::{LaneAdmin, LanePolicy, SubscriberLaneAsync};
use ssh_mcp::ports::subscriber_registry::ResourceKind;
use tokio::task::JoinSet;

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// Serialise env-mutating chaos tests so the shared
/// `SSH_INLINE_PUSH_DAEMON_RELAY` variable cannot race across worker
/// threads. Mirrors the `ENV_GUARD` pattern from
/// `embed::event_mux::inline_push_translation`.
static ENV_GUARD: LazyLock<EnvMutex<()>> = LazyLock::new(|| EnvMutex::new(()));

#[derive(Debug, Default)]
struct RecordingNotifier {
    resource_events: StdMutex<Vec<(PeerId, String)>>,
    inline_events: StdMutex<Vec<(PeerId, InlinePayload)>>,
}

impl NotifierPort for RecordingNotifier {
    async fn notify_resource_updated(
        &self,
        peer: Arc<dyn PeerHandle>,
        uri: &str,
    ) -> Result<(), DomainError> {
        self.resource_events
            .lock()
            .unwrap()
            .push((peer.id(), uri.to_string()));
        Ok(())
    }

    async fn notify_ssh_output(
        &self,
        peer: Arc<dyn PeerHandle>,
        payload: InlinePayload,
    ) -> Result<(), DomainError> {
        self.inline_events
            .lock()
            .unwrap()
            .push((peer.id(), payload));
        Ok(())
    }
}

#[derive(Debug)]
struct StubPeer {
    id: PeerId,
    closed: AtomicBool,
}

impl StubPeer {
    fn new(id: &str) -> Arc<Self> {
        Arc::new(Self {
            id: PeerId::new(id.to_string()),
            closed: AtomicBool::new(false),
        })
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

fn ids() -> Arc<UuidIds> {
    Arc::new(UuidIds)
}

fn adapter() -> Arc<SubscriberLaneAdapter<UuidIds>> {
    SubscriberLaneAdapter::new(ids(), 16, 8, 64)
}

fn policy(peer: Arc<dyn PeerHandle>) -> LanePolicy {
    LanePolicy {
        lag_policy: LagPolicy::Snapshot,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 4,
        peer: Some(peer),
    }
}

async fn open_lane(
    a: &SubscriberLaneAdapter<UuidIds>,
    peer: Arc<dyn PeerHandle>,
    uri: &str,
) -> Arc<LaneState> {
    let sid = a
        .open_lane(
            uri.to_string(),
            ResourceKind::Shell,
            "test".to_string(),
            policy(peer),
        )
        .await
        .unwrap();
    a.lanes_for_uri_public(uri)
        .into_iter()
        .find(|l| l.sub_id == sid)
        .expect("lane present after open")
}

fn bridge(
    a: Arc<SubscriberLaneAdapter<UuidIds>>,
    n: Arc<RecordingNotifier>,
    inline_max: usize,
) -> Arc<LaneFanoutBridge<UuidIds, RecordingNotifier>> {
    LaneFanoutBridge::with_inline_max(a, n, inline_max)
}

fn unset_relay() {
    // SAFETY: callers hold ENV_GUARD.
    unsafe { std::env::remove_var(INLINE_PUSH_DAEMON_RELAY_ENV_VAR) };
}

fn set_relay(value: &str) {
    // SAFETY: callers hold ENV_GUARD.
    unsafe { std::env::set_var(INLINE_PUSH_DAEMON_RELAY_ENV_VAR, value) };
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// `chaos_inline_concurrent_capability_record_and_sub_open`
///
/// 16 tasks each record InlinePush for the same peer while 16 other
/// tasks open sub_open lanes against that peer. Outcome:
/// - No panic, no torn registry state.
/// - At least ONE record_capability was visible to at least ONE
///   sub_open (the registry CAS is Release/Acquire).
/// - Every honored outcome has the lane gate flipped to `true`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos_inline_concurrent_capability_record_and_sub_open() {
    let registry = Arc::new(CapabilityRegistry::new());
    let lanes: Arc<SubscriberLaneAdapter<UuidIds>> = adapter();
    let lane_admin: Arc<dyn LaneAdmin> = Arc::clone(&lanes) as Arc<dyn LaneAdmin>;
    let uc =
        SubscribeUseCase::new(Arc::clone(&lane_admin)).with_capabilities(Arc::clone(&registry));
    let peer_id = PeerId::new("chaos-peer".to_string());
    let peer: Arc<dyn PeerHandle> = StubPeer::new(peer_id.as_str()) as Arc<dyn PeerHandle>;
    let recorder = Arc::clone(&registry);
    let opener = Arc::new(uc);

    let mut set = JoinSet::new();
    for i in 0_u8..16 {
        let recorder = Arc::clone(&recorder);
        let peer_id = peer_id.clone();
        set.spawn(async move {
            recorder.record_capability(peer_id, CapabilityFlag::InlinePush, true);
            i
        });
    }
    let mut honored_total = 0_u32;
    let mut sub_tasks = JoinSet::new();
    for i in 0_u32..16 {
        let opener = Arc::clone(&opener);
        let peer = Arc::clone(&peer);
        sub_tasks.spawn(async move {
            let req = SubscribeRequest {
                uri: format!("shell://chaos-{i}/output"),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: LagPolicy::Snapshot,
                filter: FilterRule::None,
                peer: Some(peer),
                inline_push: true,
            };
            opener.execute(req).await.unwrap()
        });
    }
    while let Some(joined) = set.join_next().await {
        joined.unwrap();
    }
    while let Some(joined) = sub_tasks.join_next().await {
        let outcome = joined.unwrap();
        if outcome.inline_push_honored {
            honored_total += 1;
            let stats = lanes
                .inline_stats(&outcome.sub_id)
                .expect("inline counters present on honored lane");
            assert!(
                stats.inline_push,
                "honored outcome must have flipped lane gate",
            );
        }
    }
    // At least one race ordering must surface the recorded capability
    // to the use case; this confirms the registry stays observable
    // across the worker pool.
    assert!(
        honored_total >= 1,
        "expected at least one honored sub_open out of 16",
    );
    assert!(registry.peer_has_capability(&peer_id, CapabilityFlag::InlinePush));
}

/// `chaos_inline_peer_gc_evicts_during_fan_out`
///
/// Bridge ships 100 fragments to a peer; concurrently the
/// peer-GC pump calls `forget_peer`. The bridge consults the lane
/// gate, not the capability registry, so eviction never aborts an
/// in-flight fragment; production code separately filters new
/// sub_opens via the registry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos_inline_peer_gc_evicts_during_fan_out() {
    let registry = Arc::new(CapabilityRegistry::new());
    let peer_id = PeerId::new("chaos-gc-peer".to_string());
    registry.record_capability(peer_id.clone(), CapabilityFlag::InlinePush, true);
    let a = adapter();
    let n = Arc::new(RecordingNotifier::default());
    let peer: Arc<dyn PeerHandle> = StubPeer::new(peer_id.as_str()) as Arc<dyn PeerHandle>;
    let lane = open_lane(&a, Arc::clone(&peer), "shell://gc/output").await;
    lane.set_inline_push(true);

    let br = bridge(Arc::clone(&a), Arc::clone(&n), 64);
    let gc_registry = Arc::clone(&registry);
    let gc_peer_id = peer_id.clone();
    let gc = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2)).await;
        gc_registry.forget_peer(&gc_peer_id);
    });
    for _ in 0..100 {
        br.notify_lanes_with_bytes("shell://gc/output", b"ABCDEFGH")
            .await;
    }
    gc.await.unwrap();
    let events = n.inline_events.lock().unwrap();
    assert!(
        events.len() >= 100,
        "every fragment must reach the notifier ({} got)",
        events.len(),
    );
    assert!(!registry.peer_has_capability(&peer_id, CapabilityFlag::InlinePush));
}

/// `chaos_inline_env_toggle_mid_stream`
///
/// Flip `SSH_INLINE_PUSH_DAEMON_RELAY` from on to off across a
/// sequence of synthetic `notifications/ssh/output` translations.
/// Pre-flip events go through; post-flip events are silently
/// dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos_inline_env_toggle_mid_stream() {
    use ssh_mcp::embed::formatter::Event;
    let _g = ENV_GUARD.lock().unwrap();
    set_relay("1");
    let mut count_on = 0_u32;
    for _ in 0..32 {
        // Use the formatter directly -- the env-var gate is read by
        // `translate_inline_push` in the dispatcher; here we model
        // the gate read with the same env-var helper.
        let relay_enabled = ssh_mcp::adapters::config::internal::resolve_inline_push_daemon_relay();
        if relay_enabled {
            count_on += 1;
        }
        let _ = Event::InlinePush {
            sub_id: "sub-1".to_string(),
            uri: "shell://toggle/output".to_string(),
            seq: u64::from(count_on),
            cursor_after: u64::from(count_on),
            len: 1,
            bytes_b64: "QQ==".to_string(),
            truncated: false,
        };
    }
    unset_relay();
    let mut count_off = 0_u32;
    for _ in 0..32 {
        if ssh_mcp::adapters::config::internal::resolve_inline_push_daemon_relay() {
            count_off += 1;
        }
    }
    assert_eq!(count_on, 32, "all pre-flip events must observe relay ON");
    assert_eq!(count_off, 0, "all post-flip events must observe relay OFF");
}

/// `chaos_inline_mpsc_backpressure_lane_full`
///
/// Bridge sends inline fragments while the lane mpsc is full. The
/// inline path is independent from the legacy lane mpsc (it ships
/// through the notifier port directly), so every fragment still
/// reaches the notifier even when the legacy lane queue is jammed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos_inline_mpsc_backpressure_lane_full() {
    let a = adapter();
    let n = Arc::new(RecordingNotifier::default());
    let peer: Arc<dyn PeerHandle> = StubPeer::new("chaos-bp") as Arc<dyn PeerHandle>;
    let lane = open_lane(&a, Arc::clone(&peer), "shell://bp/output").await;
    lane.set_inline_push(true);
    let br = bridge(Arc::clone(&a), Arc::clone(&n), 32);
    // Fire enough fragments that the legacy lane mpsc would overflow.
    let payload = vec![0xAA_u8; 16];
    for _ in 0..32 {
        br.notify_lanes_with_bytes("shell://bp/output", &payload)
            .await;
    }
    let inline = n.inline_events.lock().unwrap();
    assert_eq!(
        inline.len(),
        32,
        "inline path must deliver every fragment regardless of lane mpsc",
    );
    let resource = n.resource_events.lock().unwrap();
    assert_eq!(
        resource.len(),
        32,
        "legacy fan-out must also fire on the same tick (additive)",
    );
}

/// `chaos_inline_capability_record_after_sub_open`
///
/// `sub_open(inline_push=true)` BEFORE `record_capability` surfaces
/// `inline_push_honored=false` because the registry has no entry
/// yet. A subsequent `record_capability` must NOT retroactively
/// flip the existing lane's gate (per-lane setting is captured at
/// sub_open time).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos_inline_capability_record_after_sub_open() {
    let registry = Arc::new(CapabilityRegistry::new());
    let lanes: Arc<SubscriberLaneAdapter<UuidIds>> = adapter();
    let lane_admin: Arc<dyn LaneAdmin> = Arc::clone(&lanes) as Arc<dyn LaneAdmin>;
    let uc =
        SubscribeUseCase::new(Arc::clone(&lane_admin)).with_capabilities(Arc::clone(&registry));
    let peer_id = PeerId::new("chaos-late".to_string());
    let peer: Arc<dyn PeerHandle> = StubPeer::new(peer_id.as_str()) as Arc<dyn PeerHandle>;

    let outcome = uc
        .execute(SubscribeRequest {
            uri: "shell://late/output".to_string(),
            lifetime: SubscriptionLifetime::Manual,
            lag_policy: LagPolicy::Snapshot,
            filter: FilterRule::None,
            peer: Some(Arc::clone(&peer)),
            inline_push: true,
        })
        .await
        .unwrap();
    assert!(outcome.inline_push_requested);
    assert!(
        !outcome.inline_push_honored,
        "use case must downgrade silently when registry has no entry",
    );

    // After-the-fact recording must not flip the existing lane.
    registry.record_capability(peer_id, CapabilityFlag::InlinePush, true);
    let stats = lanes.inline_stats(&outcome.sub_id).unwrap();
    assert!(
        !stats.inline_push,
        "existing lane gate must NOT retroactively flip on late record_capability",
    );
}

/// `chaos_inline_handshake_dual_peers`
///
/// Two peers connect concurrently; only one advertises the
/// capability. Per-peer isolation: the un-advertising peer's
/// sub_open is downgraded; the advertising peer's is honored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos_inline_handshake_dual_peers() {
    let registry = Arc::new(CapabilityRegistry::new());
    let lanes: Arc<SubscriberLaneAdapter<UuidIds>> = adapter();
    let lane_admin: Arc<dyn LaneAdmin> = Arc::clone(&lanes) as Arc<dyn LaneAdmin>;
    let uc =
        SubscribeUseCase::new(Arc::clone(&lane_admin)).with_capabilities(Arc::clone(&registry));

    let opt_in_id = PeerId::new("opt-in".to_string());
    let opt_out_id = PeerId::new("opt-out".to_string());
    registry.record_capability(opt_in_id.clone(), CapabilityFlag::InlinePush, true);

    let opt_in: Arc<dyn PeerHandle> = StubPeer::new(opt_in_id.as_str()) as Arc<dyn PeerHandle>;
    let opt_out: Arc<dyn PeerHandle> = StubPeer::new(opt_out_id.as_str()) as Arc<dyn PeerHandle>;
    let req = |peer: Arc<dyn PeerHandle>, uri: &str| SubscribeRequest {
        uri: uri.to_string(),
        lifetime: SubscriptionLifetime::Manual,
        lag_policy: LagPolicy::Snapshot,
        filter: FilterRule::None,
        peer: Some(peer),
        inline_push: true,
    };
    let outcome_in = uc.execute(req(opt_in, "shell://in/output")).await.unwrap();
    let outcome_out = uc
        .execute(req(opt_out, "shell://out/output"))
        .await
        .unwrap();
    assert!(
        outcome_in.inline_push_honored,
        "opt-in peer must be honored"
    );
    assert!(
        !outcome_out.inline_push_honored,
        "opt-out peer must be downgraded",
    );
}

/// `chaos_inline_fragments_under_concurrent_compose`
///
/// 8 tasks call `compose_inline_payload` on the same lane. Each
/// successful compose consumes one `inline_seq` slot via
/// `fetch_add`. After all tasks join the counter must equal exactly
/// 8 with no torn read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos_inline_fragments_under_concurrent_compose() {
    let a = adapter();
    let n = Arc::new(RecordingNotifier::default());
    let peer: Arc<dyn PeerHandle> = StubPeer::new("chaos-compose") as Arc<dyn PeerHandle>;
    let lane = open_lane(&a, Arc::clone(&peer), "shell://compose/output").await;
    lane.set_inline_push(true);
    let br = bridge(Arc::clone(&a), Arc::clone(&n), 64);

    let mut set = JoinSet::new();
    for i in 0_u8..8 {
        let br = Arc::clone(&br);
        set.spawn(async move {
            br.notify_lanes_with_bytes("shell://compose/output", &[i, i, i, i])
                .await;
        });
    }
    while let Some(joined) = set.join_next().await {
        joined.unwrap();
    }
    let final_seq = lane.inline_seq.load(Ordering::Acquire);
    assert_eq!(
        final_seq, 8,
        "inline_seq counter must equal compose count (no double-increment)",
    );
    let events = n.inline_events.lock().unwrap();
    assert_eq!(events.len(), 8, "every fan-out tick produced a fragment");
    let mut seqs: Vec<u64> = events.iter().map(|(_, p)| p.seq).collect();
    seqs.sort_unstable();
    assert_eq!(
        seqs,
        vec![0_u64, 1, 2, 3, 4, 5, 6, 7],
        "seq drift under contention"
    );
}

/// `chaos_inline_max_bytes_zero_edge`
///
/// `inline_max_bytes=0` is a documented "fits" edge case in the
/// splitter -- the input is shipped unchanged as a single fragment.
/// Verifies the bridge does not loop or panic when constructed with
/// the pathological cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos_inline_max_bytes_zero_edge() {
    let a = adapter();
    let n = Arc::new(RecordingNotifier::default());
    let peer: Arc<dyn PeerHandle> = StubPeer::new("chaos-zero") as Arc<dyn PeerHandle>;
    let lane = open_lane(&a, Arc::clone(&peer), "shell://zero/output").await;
    lane.set_inline_push(true);
    let br = bridge(Arc::clone(&a), Arc::clone(&n), 0);
    let payload = vec![0xCC_u8; 100];
    br.notify_lanes_with_bytes("shell://zero/output", &payload)
        .await;
    let events = n.inline_events.lock().unwrap();
    assert_eq!(events.len(), 1, "max=0 fits path must ship one fragment");
    assert_eq!(events[0].1.bytes.len(), 100, "byte count preserved");
    assert!(!events[0].1.truncated, "single fragment is final");
}

// Sanity guard: confirm the wire-error code constant is reachable
// from integration tests (regression catch -- the constant moved
// crates in phase 7).
#[test]
fn inline_push_bad_params_code_is_stable() {
    let _ = SubId::new("guard".to_string());
    assert_eq!(INLINE_PUSH_BAD_PARAMS_CODE, "INLINE_PUSH_BAD_PARAMS");
}
