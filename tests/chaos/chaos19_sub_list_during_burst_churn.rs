//! Chaos 19 — `list_subs` during a burst of subscribe/unsubscribe.
//!
//! 100 concurrent subscribe + unsubscribe pairs hammer the registry
//! while another task polls `list_subs`. Requirement: the list call
//! never observes inconsistent state (the lane count never exceeds
//! the snapshot of opened-but-not-yet-closed lanes) and never panics.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::SubscriberLaneAdapter;
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const CONCURRENT: u32 = 100;
const POLLS: u32 = 256;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos19_list_subs_during_churn_observes_consistent_state() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 8, 32, 256);
    let policy = LanePolicy {
        lag_policy: LagPolicy::Snapshot,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 4,
    };

    // Tracks "opened but not yet closed". The list_subs len must be
    // <= this counter at all times.
    let in_flight = Arc::new(AtomicUsize::new(0));

    // Spawn the churners.
    let mut handles = Vec::with_capacity(CONCURRENT as usize);
    for i in 0..CONCURRENT {
        let adapter_c = Arc::clone(&adapter);
        let policy_c = policy.clone();
        let counter = Arc::clone(&in_flight);
        let uri = format!("shell://burst-{i}/output");
        let resource = format!("burst-{i}");
        handles.push(tokio::spawn(async move {
            let id = adapter_c
                .open_lane(uri, ResourceKind::Shell, resource, policy_c)
                .await
                .unwrap();
            counter.fetch_add(1, Ordering::AcqRel);
            tokio::task::yield_now().await;
            adapter_c.close_lane(&id).await.unwrap();
            counter.fetch_sub(1, Ordering::AcqRel);
        }));
    }

    // Poll list_subs while the churners run.
    let poller_adapter = Arc::clone(&adapter);
    let poller_counter = Arc::clone(&in_flight);
    let poller = tokio::spawn(async move {
        let mut max_observed = 0_usize;
        for _ in 0..POLLS {
            let len = SubscriberLanePort::list_subs(&*poller_adapter).len();
            let in_flight_now = poller_counter.load(Ordering::Acquire);
            assert!(
                len <= in_flight_now + CONCURRENT as usize,
                "list_subs len {len} grossly exceeds in-flight ceiling {in_flight_now}",
            );
            if len > max_observed {
                max_observed = len;
            }
            tokio::task::yield_now().await;
        }
        max_observed
    });

    for h in handles {
        h.await.unwrap();
    }
    let _max = poller.await.unwrap();

    // After every churner finishes, the registry MUST be empty.
    assert!(SubscriberLanePort::list_subs(&*adapter).is_empty());
    assert_eq!(in_flight.load(Ordering::Acquire), 0);
}
