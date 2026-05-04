//! Chaos 29 — replay cursor beyond the available ring buffer window.
//!
//! The replay validator (`validate_cursor`) is the canonical guard
//! between the use case and the lane adapter. A cursor past the
//! recorded `bytes_available` MUST surface `InvalidArgument` rather
//! than silently truncating or panicking; the adapter-level cursor
//! cap (`fetch_max`) handles the in-window case.

use std::sync::Arc;

use ssh_mcp::adapters::id_generator::uuid::UuidIds;
use ssh_mcp::adapters::subscription::replay::validate_cursor;
use ssh_mcp::adapters::subscription::subscriber_lane::SubscriberLaneAdapter;
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};
use ssh_mcp::ports::subscriber_lane::{LanePolicy, SubscriberLaneAsync, SubscriberLanePort};
use ssh_mcp::ports::subscriber_registry::ResourceKind;

const URI: &str = "shell://ring-overflow/output";
const RING_SIZE: u64 = 4_096;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos29_replay_beyond_window_returns_invalid_argument() {
    // Validator catches every out-of-window cursor.
    let err = validate_cursor(RING_SIZE + 1, RING_SIZE).unwrap_err();
    assert!(
        matches!(err, DomainError::InvalidArgument(_)),
        "expected InvalidArgument, got {err:?}",
    );
    let err2 = validate_cursor(u64::MAX, RING_SIZE).unwrap_err();
    assert!(matches!(err2, DomainError::InvalidArgument(_)));

    // In-window cursors pass through.
    validate_cursor(0, RING_SIZE).unwrap();
    validate_cursor(RING_SIZE / 2, RING_SIZE).unwrap();
    validate_cursor(RING_SIZE, RING_SIZE).unwrap();

    // The lane adapter's replay path advances the cursor via fetch_max,
    // so a past-end cursor never moves backwards even when the
    // validator would reject it. Confirm that monotonicity is intact
    // even after a "would-be-invalid" advance attempt.
    let adapter = SubscriberLaneAdapter::new(Arc::new(UuidIds), 8, 8, 64);
    let policy = LanePolicy {
        lag_policy: LagPolicy::Snapshot,
        lifetime: SubscriptionLifetime::Manual,
        filter: FilterRule::None,
        buffer_size: 4,
        peer: None,
    };
    let sub_id = adapter
        .open_lane(
            URI.to_string(),
            ResourceKind::Shell,
            "ring-overflow".to_string(),
            policy,
        )
        .await
        .unwrap();
    let target = RING_SIZE * 4;
    adapter.replay_from_cursor(&sub_id, target).await.unwrap();
    let observed = adapter.current_cursor(&sub_id, URI);
    assert_eq!(observed, target, "fetch_max must accept the bigger cursor");

    // A subsequent smaller cursor does not regress.
    adapter
        .replay_from_cursor(&sub_id, target / 2)
        .await
        .unwrap();
    assert_eq!(adapter.current_cursor(&sub_id, URI), target);
}
