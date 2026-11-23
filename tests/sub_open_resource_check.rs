//! The production composition refuses a missing resource before any lane.

#![allow(
    clippy::unwrap_used,
    reason = "tests use unwrap for brevity per AGENTS.md test policy"
)]

use ssh_mcp::application::subscription_admin::SubscribeRequest;
use ssh_mcp::domain::error::DomainError;
use ssh_mcp::domain::subscription::{FilterRule, LagPolicy, SubscriptionLifetime};

fn req(uri: &str) -> SubscribeRequest {
    SubscribeRequest {
        uri: uri.to_string(),
        lifetime: SubscriptionLifetime::Manual,
        lag_policy: LagPolicy::Snapshot,
        filter: FilterRule::None,
        peer: None,
        inline_push: false,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn production_sub_subscribe_refuses_missing_resources_and_opens_forward() {
    let (use_cases, ..) = ssh_mcp::composition::prod::build_use_cases();
    let uc = &use_cases.sub_subscribe;
    assert!(matches!(
        uc.execute(req("shell://missing/output")).await,
        Err(DomainError::ShellNotFound(_))
    ));
    assert!(matches!(
        uc.execute(req("command://missing/output")).await,
        Err(DomainError::CommandNotFound(_))
    ));
    assert!(matches!(
        uc.execute(req("transfer://missing/progress")).await,
        Err(DomainError::TransferNotFound(_))
    ));
    assert!(matches!(
        uc.execute(req("session://missing/health")).await,
        Err(DomainError::SessionNotFound(_))
    ));
    uc.execute(req("forward://f/events"))
        .await
        .expect("forward stays permissive");
}
