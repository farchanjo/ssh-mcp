//! Resource lifecycle port (v5 Phase 1).
//!
//! Decouples use cases from the concrete state machine that tracks
//! subscriber counts and arms grace timers. The port has two slices:
//!
//! - [`LifecyclePolicyPort`] — sync, dyn-safe surface for refcount
//!   bookkeeping and snapshot reads. Sequence allocation, transition
//!   CAS, and snapshot inspection happen synchronously so use cases
//!   that must remain cancel-safe never `.await` while holding a
//!   lifecycle reference. Mirrors the
//!   [`crate::ports::subscriber_registry::SubscriberRegistryPort`]
//!   pattern.
//! - [`LifecyclePolicyAsync`] — async slice that may spawn the grace
//!   timer task on the runtime. Built via the
//!   `#[trait_variant::make]` attribute so callers get a Send-bounded
//!   alias for static-dispatch wiring at the composition root.
//!
//! Use cases consume the sync slice through a generic parameter; only
//! the composition root and the adapter implement the async slice.
//!
//! # Phase 1 scope
//!
//! - `track_resource` — register a freshly created shell / command /
//!   transfer / forward.
//! - `on_subscribe` / `on_unsubscribe` — refcount manipulation, drives
//!   the Owned -> Observed -> Releasing transitions.
//! - `force_close` — explicit close path used by `ssh_shell_close`,
//!   `ssh_exec_cancel`, etc.
//! - `snapshot` — read-only inspection for diagnostics.
//! - `arm_grace_timer` / `cancel_grace_timer` — async helpers consumed
//!   by the adapter's grace-timer task.
//!
//! Phase 2 (v5) will layer SubId-based addressing on top of the same
//! port surface; this layer therefore stays peer-keyed and only
//! exposes resource-keyed operations.

use std::fmt;

use crate::domain::error::DomainError;
use crate::domain::ids::SessionId;
use crate::domain::lifecycle::{LifecyclePolicy, LifecycleSnapshot};
use crate::ports::subscriber_registry::ResourceKind;

/// Sync, dyn-safe slice of the lifecycle adapter.
///
/// Mirrors [`crate::ports::subscriber_registry::SubscriberRegistryPort`]:
/// every state mutation that does not need to spawn a runtime task is
/// expressed as a sync method so use cases never hold a guard across a
/// `.await` boundary.
///
/// `Debug` is a supertrait so the [`crate::composition::UseCases`]
/// container can derive `Debug` even when the lifecycle handle is
/// erased behind `Arc<dyn LifecyclePolicyPort>`.
pub trait LifecyclePolicyPort: fmt::Debug + Send + Sync + 'static {
    /// Register a new resource and pin its policy. Initial state is
    /// [`crate::domain::lifecycle::LifecycleState::Owned`].
    ///
    /// Idempotent: re-registering the same `(kind, resource_id)` pair
    /// replaces the policy without touching the current state — useful
    /// for hot-reload paths where the inbound adapter rebuilds the
    /// resource without a teardown.
    fn track_resource(
        &self,
        kind: ResourceKind,
        resource_id: &str,
        session_id: &SessionId,
        policy: LifecyclePolicy,
    );

    /// Increment the subscriber count.
    ///
    /// Transitions:
    /// - [`crate::domain::lifecycle::LifecycleState::Owned`] ->
    ///   [`crate::domain::lifecycle::LifecycleState::Observed`].
    /// - [`crate::domain::lifecycle::LifecycleState::Releasing`] ->
    ///   [`crate::domain::lifecycle::LifecycleState::Observed`] (cancels
    ///   any in-flight grace timer).
    ///
    /// # Errors
    ///
    /// - [`DomainError::ResourceGone`] when the resource has already
    ///   transitioned to [`crate::domain::lifecycle::LifecycleState::Closed`].
    fn on_subscribe(&self, kind: ResourceKind, resource_id: &str) -> Result<(), DomainError>;

    /// Decrement the subscriber count.
    ///
    /// When the count reaches zero AND the policy enables
    /// `release_when_no_subs`, transitions
    /// [`crate::domain::lifecycle::LifecycleState::Observed`] ->
    /// [`crate::domain::lifecycle::LifecycleState::Releasing`] and arms
    /// the grace timer.
    ///
    /// Idempotent for unknown `(kind, resource_id)` pairs (no-op),
    /// matching the registry's permissive `unsubscribe` semantics.
    ///
    /// # Errors
    ///
    /// - [`DomainError::Internal`] when the refcount is already zero
    ///   (caller bug — surfaces as a typed error so the strict lint
    ///   baseline can keep `forbid panic` intact).
    fn on_unsubscribe(&self, kind: ResourceKind, resource_id: &str) -> Result<(), DomainError>;

    /// Force-close the resource (manual close path). Transitions any
    /// non-terminal state -> [`crate::domain::lifecycle::LifecycleState::Closed`]
    /// and signals the cascade coordinator so the owning session can
    /// reap itself once its active-resource refcount hits zero.
    ///
    /// Idempotent: closing an already-closed resource is a no-op.
    ///
    /// # Errors
    ///
    /// Currently always returns `Ok(())` — the result is wrapped in
    /// [`Result`] to preserve API symmetry with [`Self::on_subscribe`]
    /// and to leave room for Phase 2 / Phase 3 enrichments.
    fn force_close(&self, kind: ResourceKind, resource_id: &str) -> Result<(), DomainError>;

    /// Read-only snapshot for diagnostics, MCP `sub_stats` rendering,
    /// and integration tests. Returns `None` when the resource has
    /// never been tracked.
    fn snapshot(&self, kind: ResourceKind, resource_id: &str) -> Option<LifecycleSnapshot>;
}

/// Async slice of the lifecycle adapter.
///
/// `arm_grace_timer` may spawn a runtime task; `cancel_grace_timer`
/// notifies the timer's `tokio::sync::Notify` waker. Both are async to
/// keep the spawn point inside the adapter — use cases never reach for
/// `tokio::spawn` directly.
#[trait_variant::make(LifecyclePolicyAsync: Send)]
pub trait LocalLifecyclePolicyAsync: Sync {
    /// Spawn the grace timer task for `resource_id`. Idempotent — no-op
    /// when a timer is already armed.
    async fn arm_grace_timer(&self, kind: ResourceKind, resource_id: String);

    /// Cancel the grace timer for `resource_id`. Idempotent.
    async fn cancel_grace_timer(&self, kind: ResourceKind, resource_id: &str);
}

/// No-op [`LifecyclePolicyPort`] adapter.
///
/// Used as the constructor default by use cases that opt in to the v5
/// lifecycle binding via the `with_lifecycle` builder. Keeps every
/// call a silent success so callers that have not migrated to the v5
/// wiring observe the same behaviour as v4.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopLifecycle;

impl LifecyclePolicyPort for NoopLifecycle {
    fn track_resource(
        &self,
        _kind: ResourceKind,
        _resource_id: &str,
        _session_id: &SessionId,
        _policy: LifecyclePolicy,
    ) {
    }

    fn on_subscribe(&self, _kind: ResourceKind, _resource_id: &str) -> Result<(), DomainError> {
        Ok(())
    }

    fn on_unsubscribe(&self, _kind: ResourceKind, _resource_id: &str) -> Result<(), DomainError> {
        Ok(())
    }

    fn force_close(&self, _kind: ResourceKind, _resource_id: &str) -> Result<(), DomainError> {
        Ok(())
    }

    fn snapshot(&self, _kind: ResourceKind, _resource_id: &str) -> Option<LifecycleSnapshot> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{LifecyclePolicyAsync, LifecyclePolicyPort, NoopLifecycle};
    use crate::domain::ids::SessionId;
    use crate::domain::lifecycle::LifecyclePolicy;
    use crate::ports::subscriber_registry::ResourceKind;

    fn _assert_sync_dyn_safe(_p: &dyn LifecyclePolicyPort) {}

    fn _assert_async_port<T: LifecyclePolicyAsync>() {}

    #[test]
    fn noop_lifecycle_returns_ok_on_every_call() {
        let n = NoopLifecycle;
        n.track_resource(
            ResourceKind::Shell,
            "x",
            &SessionId::new("s".to_string()),
            LifecyclePolicy::default(),
        );
        assert!(n.on_subscribe(ResourceKind::Shell, "x").is_ok());
        assert!(n.on_unsubscribe(ResourceKind::Shell, "x").is_ok());
        assert!(n.force_close(ResourceKind::Shell, "x").is_ok());
        assert!(n.snapshot(ResourceKind::Shell, "x").is_none());
    }

    #[test]
    fn noop_lifecycle_implements_dyn_safe_trait() {
        let n: Arc<dyn LifecyclePolicyPort> = Arc::new(NoopLifecycle);
        assert!(n.snapshot(ResourceKind::Shell, "x").is_none());
    }
}
