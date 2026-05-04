//! Resource lifecycle adapter (v5 Phase 1).
//!
//! Implements [`crate::ports::lifecycle_policy::LifecyclePolicyPort`] +
//! [`crate::ports::lifecycle_policy::LifecyclePolicyAsync`] using a
//! lock-free state machine: an `AtomicU8` per resource encodes the
//! [`crate::domain::lifecycle::LifecycleState`], an `AtomicUsize`
//! tracks the subscriber count, an `AtomicU64` carries the grace
//! deadline, and an [`arc_swap::ArcSwap`] holds the policy so hot
//! reloads stay race-free. Wakeups happen through
//! [`tokio::sync::Notify`].
//!
//! ## Module map
//!
//! - [`refcount`] — per-resource state machine and the
//!   [`RefcountedLifecycleAdapter`] that bundles the
//!   `DashMap<ResourceKey, Arc<ResourceLifecycle>>` storage.
//! - [`grace_timer`] — async grace timer task that fires
//!   `Releasing -> Closed` once the deadline elapses.
//! - [`cascade`] — session-level refcount and the cascade hook fired
//!   when a tracked resource transitions to
//!   [`crate::domain::lifecycle::LifecycleState::Closed`].
//!
//! Wire the adapter at the composition root via
//! [`RefcountedLifecycleAdapter::new`] and inject it as
//! `Arc<dyn LifecyclePolicyPort>` into the use case container.

pub mod cascade;
pub mod grace_timer;
pub mod refcount;
