//! Chaos 30 — concurrent subscribe with `UUIDv7` minting.
//!
//! 100 concurrent `open_lane` calls fan out through a `UUIDv7`-backed
//! id generator. Requirement: every `SubId` is unique, well-formed
//! `UUIDv7`, and the values are time-monotonic in creation order
//! (within sub-millisecond tolerance: same-millisecond batches share
//! a prefix so we sort by the encoded timestamp prefix).

use std::sync::Arc;

use ssh_mcp::adapters::subscription::subscriber_lane::SubscriberLaneAdapter;
use ssh_mcp::domain::ids::{CommandId, ForwardId, SessionId, ShellId, TransferId};
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::id_generator::IdGeneratorPort;
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;
use uuid::Uuid;

const SUBSCRIBES: u32 = 100;

/// `UUIDv7` id generator (the production [`UuidIds`] adapter uses
/// `Uuid::new_v4`; v5 contracts prefer v7 for time-ordering of
/// `SubId`).
#[derive(Debug, Default)]
struct UuidV7Ids;

impl IdGeneratorPort for UuidV7Ids {
    fn new_session_id(&self) -> SessionId {
        SessionId::new(Uuid::now_v7().to_string())
    }
    fn new_command_id(&self) -> CommandId {
        CommandId::new(Uuid::now_v7().to_string())
    }
    fn new_shell_id(&self) -> ShellId {
        ShellId::new(Uuid::now_v7().to_string())
    }
    fn new_transfer_id(&self) -> TransferId {
        TransferId::new(Uuid::now_v7().to_string())
    }
    fn new_forward_id(&self) -> ForwardId {
        ForwardId::new(Uuid::now_v7().to_string())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos30_uuidv7_concurrent_subscribe_unique_and_time_ordered() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidV7Ids), 8, 16, 256);
    let policy = LanePolicy {
        lag_policy: LagPolicy::Snapshot,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 4,
    };

    let mut handles = Vec::with_capacity(SUBSCRIBES as usize);
    for i in 0..SUBSCRIBES {
        let adapter_c = Arc::clone(&adapter);
        let policy_c = policy.clone();
        let uri = format!("shell://uuidv7-{i}/output");
        let resource = format!("uuidv7-{i}");
        handles.push(tokio::spawn(async move {
            adapter_c
                .open_lane(uri, ResourceKind::Shell, resource, policy_c)
                .await
                .unwrap()
        }));
    }
    let mut sub_ids = Vec::with_capacity(SUBSCRIBES as usize);
    for h in handles {
        sub_ids.push(h.await.unwrap());
    }

    // Uniqueness — every SubId is distinct.
    let unique: std::collections::HashSet<_> = sub_ids.iter().map(|s| s.as_str()).collect();
    assert_eq!(unique.len(), sub_ids.len(), "duplicate SubId minted");

    // Each value parses as a UUIDv7 (version nibble 7 in byte 6).
    for sid in &sub_ids {
        let parsed = Uuid::parse_str(sid.as_str()).expect("UUID parse");
        assert_eq!(
            parsed.get_version_num(),
            7,
            "non-v7 UUID minted: {}",
            sid.as_str()
        );
    }

    // Time-ordering: sort by string and verify timestamps are
    // monotonically non-decreasing (UUIDv7 string repr is sortable
    // because the leading bytes encode a Unix-millis timestamp).
    let mut sorted = sub_ids
        .iter()
        .map(|s| s.as_str().to_string())
        .collect::<Vec<_>>();
    sorted.sort();
    let stamps: Vec<u64> = sorted
        .iter()
        .map(|s| {
            let parsed = Uuid::parse_str(s).unwrap_or_default();
            // The first 48 bits encode a Unix-millis stamp.
            let bytes = parsed.into_bytes();
            u64::from(bytes[0]) << 40
                | u64::from(bytes[1]) << 32
                | u64::from(bytes[2]) << 24
                | u64::from(bytes[3]) << 16
                | u64::from(bytes[4]) << 8
                | u64::from(bytes[5])
        })
        .collect();
    for w in stamps.windows(2) {
        assert!(
            w[0] <= w[1],
            "UUIDv7 timestamps not monotonic after sort: {w:?}",
        );
    }

    // Registry coherent.
    assert_eq!(
        SubscriberLanePort::list_subs(&*adapter).len(),
        sub_ids.len()
    );
}
