//! Chaos 10 — too many subscribers per URI.
//!
//! The lane adapter enforces both `max_per_uri` and `max_total`
//! caps. Once the per-URI cap is exhausted, every additional
//! `open_lane` MUST surface a typed `MaxSubsPerUriExceeded` error —
//! never a panic, never a silent overflow.

use std::sync::Arc;

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::subscriber_lane::SubscriberLaneAdapter;
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const MAX_PER_URI: u16 = 4;
const URI: &str = "shell://saturated/output";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos10_too_many_subs_per_uri_surfaces_typed_error() {
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 8, MAX_PER_URI, 1024);
    let policy = LanePolicy {
        lag_policy: LagPolicy::Snapshot,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 4, peer: None,
    };

    // Saturate the URI cap.
    let mut ids = Vec::new();
    for _ in 0..MAX_PER_URI {
        let id = adapter
            .open_lane(
                URI.to_string(),
                ResourceKind::Shell,
                "saturated".to_string(),
                policy.clone(),
            )
            .await
            .unwrap();
        ids.push(id);
    }
    // The next open MUST fail with the typed error.
    let err = adapter
        .open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "saturated".to_string(),
            policy.clone(),
        )
        .await
        .unwrap_err();
    match err {
        DomainError::MaxSubsPerUriExceeded { uri, limit } => {
            assert_eq!(uri, URI);
            assert_eq!(limit, MAX_PER_URI);
        }
        other => panic!("unexpected error: {other:?}"),
    }

    // After freeing one lane, the next open succeeds — the adapter
    // re-fills the cap deterministically.
    adapter.close_lane(&ids[0]).await.unwrap();
    let _next = adapter
        .open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "saturated".to_string(),
            policy,
        )
        .await
        .unwrap();
}
