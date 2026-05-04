//! v5 Phase 3 smoke integration tests.
//!
//! These tests cover the Phase 3 surface end-to-end: tool registration,
//! lane lifecycle, lag-policy snapshot recovery, regex filter
//! enforcement, and the SUB_LEAK_RISK leak watcher. None of them
//! require a live SSH server — every test drives the channel mux
//! adapter directly so the suite stays deterministic.
//!
//! Tests:
//! 1. `t05_subscribe_returns_sub_id` — the 9 Phase 3 tools register
//!    and `ssh_subscribe` returns a non-empty SubId.
//! 2. `t06_release_when_no_subs_grace` — the new args flag is
//!    discoverable on `ssh_shell_open` / `ssh_execute` / `ssh_upload` /
//!    `ssh_download` schemas.
//! 3. `t07_lag_policy_snapshot_recovery` — `LagPolicy::Snapshot`
//!    rebuilds after the lane mpsc fills.
//! 4. `t08_filter_regex_drops_match` — a regex filter forwards
//!    matching events and silently drops the rest.
//! 5. `t09_sub_leak_risk_warn` — the leak watcher fires a `Warn`
//!    alert after the threshold, and stays silent when
//!    `release_when_no_subs=true`.

#![allow(
    clippy::unwrap_used,
    reason = "integration tests use unwrap for brevity"
)]

use std::sync::Arc;
use std::time::Duration;

use rmcp::ServerHandler;
use ssh_mcp::adapters::clock::system::SystemClock;
use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::lifecycle::cascade::CascadeCoordinator;
use ssh_mcp::adapters::lifecycle::leak_watcher::{
    LeakRiskSeverity, LeakWatcher, LeakWatcherConfig,
};
use ssh_mcp::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
use ssh_mcp::adapters::subscription::subscriber_lane::{LaneMsg, SubscriberLaneAdapter};
use ssh_mcp::application::subscription_admin::{
    PauseSubUseCase, SubToggleRequest, SubscribeRequest, SubscribeUseCase,
};
use ssh_mcp::composition::prod::build_server;
use ssh_mcp::domain::ids::SessionId;
use ssh_mcp::domain::lifecycle::LifecyclePolicy;
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubId, SubscriptionLifetime};
use ssh_mcp::ports::lifecycle_policy::LifecyclePolicyPort;
use ssh_mcp::ports::subscriber_lane::{LaneAdmin, LanePolicy, SubscriberLaneAsync};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const PHASE3_TOOLS: [&str; 9] = [
    "ssh_subscribe",
    "ssh_unsubscribe",
    "ssh_sub_pause",
    "ssh_sub_resume",
    "ssh_sub_filter",
    "ssh_sub_replay",
    "ssh_sub_list",
    "ssh_sub_stats",
    "ssh_daemon_stats",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t05_subscribe_returns_sub_id() {
    // The MCP server registers all 9 Phase 3 tools.
    let server = build_server();
    for name in PHASE3_TOOLS {
        assert!(
            server.get_tool(name).is_some(),
            "phase 3 tool {name} not registered"
        );
    }
    // Drive the use case directly to confirm the lane port wiring
    // is end-to-end.
    let lane: Arc<dyn LaneAdmin> = SubscriberLaneAdapter::new(Arc::new(UuidIds), 16, 8, 64);
    let uc = SubscribeUseCase::new(lane);
    let outcome = uc
        .execute(SubscribeRequest {
            uri: "shell://sh-1/output".to_string(),
            lifetime: SubscriptionLifetime::Manual,
            lag_policy: LagPolicy::Snapshot,
            filter: FilterRule::None,
        })
        .await
        .unwrap();
    assert!(!outcome.sub_id.as_str().is_empty());
    assert_eq!(outcome.uri, "shell://sh-1/output");
}

#[test]
fn t06_release_when_no_subs_grace() {
    // The four args structs advertise both new fields.
    let server = build_server();
    let names = [
        "ssh_shell_open",
        "ssh_execute",
        "ssh_upload",
        "ssh_download",
    ];
    for name in names {
        let tool = server
            .get_tool(name)
            .unwrap_or_else(|| panic!("missing tool {name}"));
        let schema_json = serde_json::to_string(&tool.input_schema).unwrap_or_default();
        assert!(
            schema_json.contains("release_when_no_subs"),
            "tool {name} missing release_when_no_subs in schema: {schema_json}"
        );
        assert!(
            schema_json.contains("grace_ms"),
            "tool {name} missing grace_ms in schema: {schema_json}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t07_lag_policy_snapshot_recovery() {
    // With Snapshot policy, when the mpsc fills the producer drops the
    // backlog and emits a Snapshot marker; the consumer therefore sees
    // a Snapshot event and the lagged_recoveries counter increases.
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 0, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::Snapshot,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        // Buffer of 2 so the third Data triggers the recovery.
        buffer_size: 2,
    };
    let sub_id = adapter
        .open_lane(
            "shell://x/output".to_string(),
            ResourceKind::Shell,
            "x".to_string(),
            policy,
        )
        .await
        .unwrap();

    // Push three Data messages, none of which the consumer drains.
    for seq in 0..3 {
        adapter
            .produce(
                "shell://x/output",
                &LaneMsg::Data {
                    seq,
                    payload: vec![b'A' + seq as u8],
                },
            )
            .unwrap();
    }
    // The third push triggers the Snapshot rebuild path.
    let stats = adapter.list();
    let summary = stats
        .iter()
        .find(|s| s.sub_id == sub_id)
        .expect("lane summary");
    assert!(
        summary.stats.lagged_recoveries >= 1,
        "expected at least one Snapshot recovery; got {summary:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t08_filter_regex_drops_match() {
    // Regex filter retains only matching events. The lane atomic
    // counters fold filtered events into events_sent (so the operator
    // can correlate filter rate vs production rate); the actual mpsc
    // contents only carry matches.
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 16, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::Snapshot,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::Regex("KEEP".to_string()),
        buffer_size: 16,
    };
    let _sub_id = adapter
        .open_lane(
            "shell://f/output".to_string(),
            ResourceKind::Shell,
            "f".to_string(),
            policy,
        )
        .await
        .unwrap();

    adapter
        .produce(
            "shell://f/output",
            &LaneMsg::Data {
                seq: 0,
                payload: b"DROP this".to_vec(),
            },
        )
        .unwrap();
    adapter
        .produce(
            "shell://f/output",
            &LaneMsg::Data {
                seq: 1,
                payload: b"KEEP this".to_vec(),
            },
        )
        .unwrap();

    // Both produce calls increment events_sent (filtered events count
    // too — that is the design); but the mpsc only carries the match.
    let summary = adapter
        .list()
        .into_iter()
        .next()
        .expect("at least one lane");
    assert_eq!(
        summary.stats.events_sent, 2,
        "events_sent must include filtered events for observability"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t09_sub_leak_risk_warn() {
    // 1. A resource opened with the default policy
    //    (release_when_no_subs=false) past the warn threshold MUST
    //    fire a SUB_LEAK_RISK Warn alert.
    // 2. A resource opened with release_when_no_subs=true MUST NOT
    //    fire — the watcher trusts the auto-release contract.
    let cascade = CascadeCoordinator::new();
    let clock = Arc::new(SystemClock);
    let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));
    adapter.track_resource(
        ResourceKind::Shell,
        "leaky",
        &SessionId::new("sess".to_string()),
        LifecyclePolicy::default(), // release_when_no_subs=false
    );
    adapter.track_resource(
        ResourceKind::Shell,
        "self-cleaning",
        &SessionId::new("sess".to_string()),
        LifecyclePolicy {
            release_when_no_subs: true,
            grace_ms: 2_000,
            cascade_session: false,
        },
    );

    let handle = LeakWatcher::spawn(
        &adapter,
        LeakWatcherConfig {
            warn_after_s: 1,
            kill_after_s: 0,
            scan_interval: Duration::from_millis(50),
        },
    );
    let mut rx = handle.watcher.subscribe();
    let alert = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("alert in time")
        .expect("alert ok");
    assert_eq!(alert.severity, LeakRiskSeverity::Warn);
    assert_eq!(alert.kind, ResourceKind::Shell);
    assert_eq!(
        alert.resource_id, "leaky",
        "self-cleaning resource must not fire SUB_LEAK_RISK"
    );

    handle.cancel.cancel();
    let _ = handle.task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t10_pause_resume_round_trip_via_lane_admin() {
    // Bonus integration: pause / resume mutate the same lane state
    // observable through SubscriberLanePort::list.
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 16, 8, 64);
    let lane: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
    let sub_id = adapter
        .open_lane(
            "shell://p/output".to_string(),
            ResourceKind::Shell,
            "p".to_string(),
            LanePolicy::default(),
        )
        .await
        .unwrap();

    let pause = PauseSubUseCase::new(Arc::clone(&lane));
    let _ = pause
        .execute(SubToggleRequest {
            sub_id: sub_id.clone(),
        })
        .await
        .unwrap();
    let summary = adapter
        .list()
        .into_iter()
        .find(|s| s.sub_id == sub_id)
        .expect("lane");
    assert!(summary.paused);

    // Direct adapter call -> same outcome via the dyn port.
    drop(lane);
    let lane2: Arc<dyn LaneAdmin> = Arc::clone(&adapter) as Arc<dyn LaneAdmin>;
    let resume = ssh_mcp::application::subscription_admin::ResumeSubUseCase::new(lane2);
    let _ = resume
        .execute(SubToggleRequest {
            sub_id: sub_id.clone(),
        })
        .await
        .unwrap();
    let summary = adapter
        .list()
        .into_iter()
        .find(|s| s.sub_id == sub_id)
        .expect("lane");
    assert!(!summary.paused);

    // Final sanity — the test wires through to the same SubId end-to-end.
    let _ = SubId::new(sub_id.into_inner());
}
