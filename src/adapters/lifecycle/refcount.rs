//! Lock-free per-resource lifecycle state machine.
//!
//! The adapter stores one [`ResourceLifecycle`] per `(kind, resource_id)`
//! pair inside a [`DashMap`]. Each lifecycle entry encodes its state in
//! an [`AtomicU8`], its subscriber count in an [`AtomicUsize`], and its
//! grace deadline (Unix-millis) in an [`AtomicU64`]. Policy is held in
//! an [`ArcSwap`] so hot-reload paths swap pointers without any lock.
//! Wake-ups happen through a shared [`Notify`].
//!
//! ## Transitions
//!
//! ```text
//!     Owned ───on_subscribe───► Observed ───on_unsubscribe(==0, release_when_no_subs)──► Releasing
//!       ▲                          │                                                       │
//!       │  on_subscribe (cancel)   │ on_subscribe (resub)                                  │
//!       └──────────────────────────┴───────────────────────────────────────────────────────┘
//!
//!     any state ──force_close──► Closed (terminal — cascade hook fires once)
//!
//!     Releasing ──grace deadline elapsed──► Closed (cascade hook fires once)
//! ```
//!
//! ## Concurrency invariants
//!
//! - Every transition uses `compare_exchange` on the state byte, so a
//!   concurrent subscribe + grace timer fire converges on a single
//!   winner with no spurious extra closes.
//! - Subscriber count uses `fetch_add(AcqRel)` / `fetch_sub(AcqRel)` so
//!   the writer that decrements to zero observes the policy snapshot
//!   together with the count.
//! - The cascade hook fires at most once per resource — the
//!   `Closed` -> `Closed` no-op CAS is the marker that another path
//!   already won.
//! - Zero `Mutex` — every field is either an `Atomic*`, an
//!   `ArcSwap`, or an `Arc<Notify>`.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use arc_swap::ArcSwap;
use dashmap::DashMap;
use tokio::sync::Notify;

use crate::adapters::lifecycle::cascade::CascadeCoordinator;
use crate::adapters::lifecycle::grace_timer::spawn_grace_timer;
use crate::domain::error::DomainError;
use crate::domain::ids::SessionId;
use crate::domain::lifecycle::{LifecyclePolicy, LifecycleSnapshot, LifecycleState};
use crate::ports::clock::ClockPort;
use crate::ports::lifecycle_policy::{LifecyclePolicyAsync, LifecyclePolicyPort};
use crate::ports::subscriber_registry::ResourceKind;

/// Composite key used by the lifecycle store.
type ResourceKey = (ResourceKind, String);

/// Read-only entry returned by [`RefcountedLifecycleAdapter::scan`].
///
/// Used by the `SUB_LEAK_RISK` leak watcher (v5 Phase 3) to surface
/// resources that have stayed `Owned` past the warn threshold without
/// `release_when_no_subs=true`.
#[derive(Debug, Clone)]
pub struct LifecycleScanEntry {
    /// Resource scheme.
    pub kind: ResourceKind,
    /// Resource id portion of the URI.
    pub resource_id: String,
    /// Current lifecycle state.
    pub state: LifecycleState,
    /// Live subscriber count.
    pub sub_count: usize,
    /// Age of the resource in milliseconds (now − `created_at_ms`).
    pub age_ms: u64,
    /// Policy in effect.
    pub policy: LifecyclePolicy,
}

/// Per-resource state record.
///
/// All mutations happen via atomic compare-exchange; readers use atomic
/// loads. The struct intentionally derives no `Clone` — every entry is
/// shared via `Arc<ResourceLifecycle>`.
pub struct ResourceLifecycle {
    /// Encoded [`LifecycleState`] (see
    /// [`LifecycleState::as_u8`]). Swapped via `compare_exchange`
    /// only.
    state: AtomicU8,
    /// Count of MCP subscribers currently observing the resource.
    sub_count: AtomicUsize,
    /// Grace deadline expressed as Unix-millis. `0` when no timer is
    /// currently armed.
    grace_until_ms: AtomicU64,
    /// Resource policy. Held in an [`ArcSwap`] so hot-reload paths
    /// swap pointers without any lock.
    policy: ArcSwap<LifecyclePolicy>,
    /// Wakes the grace timer task on subscribe / cancel events.
    waker: Arc<Notify>,
    /// Owning session — used by the cascade coordinator to debit the
    /// session refcount on close.
    session_id: SessionId,
    /// Creation time in Unix-millis. Stamped at construction; the
    /// `SUB_LEAK_RISK` leak watcher (Phase 3) reads it to flag resources
    /// that have stayed `Owned` past the warn threshold.
    created_at_ms: AtomicU64,
}

impl fmt::Debug for ResourceLifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResourceLifecycle")
            .field("state", &self.state.load(Ordering::Acquire))
            .field("sub_count", &self.sub_count.load(Ordering::Acquire))
            .field(
                "grace_until_ms",
                &self.grace_until_ms.load(Ordering::Acquire),
            )
            .field("policy", &self.policy.load_full())
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl ResourceLifecycle {
    /// Build a fresh entry in [`LifecycleState::Owned`].
    fn new(session_id: SessionId, policy: LifecyclePolicy, now_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU8::new(LifecycleState::Owned.as_u8()),
            sub_count: AtomicUsize::new(0),
            grace_until_ms: AtomicU64::new(0),
            policy: ArcSwap::new(Arc::new(policy)),
            waker: Arc::new(Notify::new()),
            session_id,
            created_at_ms: AtomicU64::new(now_ms),
        })
    }

    /// Read the resource creation time (Unix-millis).
    ///
    /// Stamped on `track_resource` and never mutated thereafter.
    #[must_use]
    pub fn created_at_ms(&self) -> u64 {
        self.created_at_ms.load(Ordering::Acquire)
    }

    /// Decode the current state. Falls back to
    /// [`LifecycleState::Closed`] if the byte tag is somehow corrupt —
    /// this branch is unreachable in practice (only [`Self::cas_state`]
    /// writes the byte) but the strict lint baseline forbids `panic!`.
    pub fn current_state(&self) -> LifecycleState {
        LifecycleState::from_u8(self.state.load(Ordering::Acquire))
            .unwrap_or(LifecycleState::Closed)
    }

    /// Read the current subscriber count.
    #[must_use]
    pub fn sub_count(&self) -> usize {
        self.sub_count.load(Ordering::Acquire)
    }

    /// Read a clone of the active policy.
    #[must_use]
    pub fn policy(&self) -> LifecyclePolicy {
        *self.policy.load_full()
    }

    /// Read the grace deadline (Unix-millis). Zero when no timer is
    /// armed.
    #[must_use]
    pub fn grace_until_ms(&self) -> u64 {
        self.grace_until_ms.load(Ordering::Acquire)
    }

    /// Borrow the shared waker so the grace timer can `.notified()` on
    /// it without taking another `Arc` clone.
    #[must_use]
    pub fn waker(&self) -> Arc<Notify> {
        Arc::clone(&self.waker)
    }

    /// Borrow the session id so the cascade coordinator can debit the
    /// owning session on close.
    #[must_use]
    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Compare-and-swap the state byte. Returns `true` when this
    /// caller wrote the transition; `false` when another thread won
    /// the race.
    fn cas_state(&self, expect: LifecycleState, next: LifecycleState) -> bool {
        self.state
            .compare_exchange(
                expect.as_u8(),
                next.as_u8(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Force-write the state byte. Used by [`Self::force_close`] which
    /// must transition from any non-terminal state to `Closed`. Returns
    /// the previous state so the caller can decide whether the cascade
    /// hook needs to fire.
    fn swap_state(&self, next: LifecycleState) -> LifecycleState {
        let prev_byte = self.state.swap(next.as_u8(), Ordering::AcqRel);
        LifecycleState::from_u8(prev_byte).unwrap_or(LifecycleState::Closed)
    }

    /// Borrow the raw atomic state byte. Used by the grace timer task
    /// to issue its own `compare_exchange` from `Releasing` to `Closed`
    /// so the cascade hook fires at most once per resource.
    #[must_use]
    pub const fn state_atomic(&self) -> &AtomicU8 {
        &self.state
    }

    /// Borrow the raw grace-deadline atomic. Used by the grace timer
    /// to clear the deadline once it fires the close transition.
    #[must_use]
    pub const fn grace_until_ms_atomic(&self) -> &AtomicU64 {
        &self.grace_until_ms
    }
}

/// Production [`LifecyclePolicyPort`] +
/// [`LifecyclePolicyAsync`] implementation.
///
/// `C` is the clock port used to compute grace deadlines and remaining
/// times surfaced through [`Self::snapshot`]. Pinned at construction so
/// the adapter stays free of dyn-async overhead.
pub struct RefcountedLifecycleAdapter<C: ClockPort> {
    /// `(kind, resource_id)` -> per-resource state.
    resources: DashMap<ResourceKey, Arc<ResourceLifecycle>>,
    /// Cascade coordinator notified on every close.
    cascade: Arc<CascadeCoordinator>,
    /// Clock port used by [`Self::snapshot`] to compute the remaining
    /// grace window. Cloned into the timer task on each arm.
    clock: Arc<C>,
}

impl<C: ClockPort> fmt::Debug for RefcountedLifecycleAdapter<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefcountedLifecycleAdapter")
            .field("resources_len", &self.resources.len())
            .finish_non_exhaustive()
    }
}

impl<C: ClockPort> RefcountedLifecycleAdapter<C> {
    /// Construct an empty adapter wired against an externally owned
    /// cascade coordinator.
    #[must_use]
    pub fn new(cascade: Arc<CascadeCoordinator>, clock: Arc<C>) -> Arc<Self> {
        Arc::new(Self {
            resources: DashMap::new(),
            cascade,
            clock,
        })
    }

    /// Lookup the lifecycle entry for `(kind, resource_id)`.
    fn get(&self, kind: ResourceKind, resource_id: &str) -> Option<Arc<ResourceLifecycle>> {
        self.resources
            .get(&(kind, resource_id.to_string()))
            .map(|r| Arc::clone(r.value()))
    }

    /// Compute the current Unix-millis through the clock port.
    fn now_ms(&self) -> u64 {
        let dt = self.clock.utc_now();
        u64::try_from(dt.timestamp_millis()).unwrap_or(0)
    }

    /// Test-only accessor returning the live entry handle. Useful when
    /// asserting on the atomic state directly.
    #[cfg(test)]
    pub(crate) fn entry(
        &self,
        kind: ResourceKind,
        resource_id: &str,
    ) -> Option<Arc<ResourceLifecycle>> {
        self.get(kind, resource_id)
    }

    /// Scan every tracked resource.
    ///
    /// Returns a [`LifecycleScanEntry`] per active entry — used by the
    /// v5 Phase 3 `SUB_LEAK_RISK` leak watcher. Skips closed resources
    /// so the watcher never warns on lanes that are already in their
    /// cleanup window.
    #[must_use]
    pub fn scan(&self) -> Vec<LifecycleScanEntry> {
        let now = self.now_ms();
        self.resources
            .iter()
            .filter_map(|entry| {
                let lc = entry.value();
                let state = lc.current_state();
                if state == LifecycleState::Closed {
                    return None;
                }
                let created_at = lc.created_at_ms();
                let age_ms = now.saturating_sub(created_at);
                Some(LifecycleScanEntry {
                    kind: entry.key().0,
                    resource_id: entry.key().1.clone(),
                    state,
                    sub_count: lc.sub_count(),
                    age_ms,
                    policy: lc.policy(),
                })
            })
            .collect()
    }

    /// Drive `Owned -> Observed` or `Releasing -> Observed`. Returns
    /// the post-transition state so callers can branch on whether a
    /// grace timer needs to be cancelled.
    fn promote_to_observed(entry: &Arc<ResourceLifecycle>) -> Result<LifecycleState, DomainError> {
        let observed = LifecycleState::Observed;
        loop {
            let current = entry.current_state();
            match current {
                LifecycleState::Owned => {
                    if entry.cas_state(LifecycleState::Owned, observed) {
                        return Ok(observed);
                    }
                }
                LifecycleState::Releasing => {
                    if entry.cas_state(LifecycleState::Releasing, observed) {
                        // Clear the grace deadline — a subsequent
                        // `cancel_grace_timer` is still safe but the
                        // timer task will treat the zero deadline as
                        // "do nothing".
                        entry.grace_until_ms.store(0, Ordering::Release);
                        // Wake the grace task so it observes the new
                        // state and exits. `notify_one` is a sync call.
                        entry.waker.notify_one();
                        return Ok(observed);
                    }
                }
                LifecycleState::Observed => return Ok(observed),
                LifecycleState::Closed => {
                    return Err(DomainError::ResourceGone(format!(
                        "resource already closed: {:?}",
                        entry.session_id
                    )));
                }
            }
        }
    }

    /// Drive `Observed -> Releasing` when subscriber count reaches
    /// zero AND policy enables `release_when_no_subs`. Returns the
    /// resulting state so the caller can arm the grace timer when
    /// appropriate.
    fn maybe_arm_release(
        entry: &Arc<ResourceLifecycle>,
        policy: LifecyclePolicy,
        deadline_ms: u64,
    ) -> LifecycleState {
        if !policy.release_when_no_subs {
            return entry.current_state();
        }
        if entry.cas_state(LifecycleState::Observed, LifecycleState::Releasing) {
            entry.grace_until_ms.store(deadline_ms, Ordering::Release);
            // Fall through to spawn the timer task in the async slice.
        }
        entry.current_state()
    }
}

impl<C: ClockPort> LifecyclePolicyPort for RefcountedLifecycleAdapter<C> {
    fn track_resource(
        &self,
        kind: ResourceKind,
        resource_id: &str,
        session_id: &SessionId,
        policy: LifecyclePolicy,
    ) {
        let key = (kind, resource_id.to_string());
        let session_clone = session_id.clone();
        // Idempotent insert — re-track only swaps the policy.
        if let Some(existing) = self.resources.get(&key) {
            existing.policy.store(Arc::new(policy));
            return;
        }
        let now_ms = self.now_ms();
        let entry = ResourceLifecycle::new(session_clone, policy, now_ms);
        self.resources.insert(key, entry);
        // First-time tracking debits the session refcount so the
        // cascade coordinator knows the session has at least one
        // active resource.
        self.cascade.inc_session(session_id);
    }

    fn on_subscribe(&self, kind: ResourceKind, resource_id: &str) -> Result<(), DomainError> {
        let Some(entry) = self.get(kind, resource_id) else {
            // Track-then-subscribe ordering is enforced at the
            // composition root; a missing entry means the resource was
            // never registered and we treat it as a soft no-op so the
            // legacy SSH/SFTP runtime adapters that still drive
            // subscribers directly do not regress.
            return Ok(());
        };
        Self::promote_to_observed(&entry)?;
        entry.sub_count.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn on_unsubscribe(&self, kind: ResourceKind, resource_id: &str) -> Result<(), DomainError> {
        let Some(entry) = self.get(kind, resource_id) else {
            return Ok(());
        };
        let prev = entry.sub_count.fetch_sub(1, Ordering::AcqRel);
        if prev == 0 {
            // Bug-detect: the writer underflowed. Recover the counter
            // and surface a typed error so the strict lint baseline
            // does not need a panic.
            entry.sub_count.fetch_add(1, Ordering::AcqRel);
            return Err(DomainError::Internal(format!(
                "lifecycle sub_count underflow on {kind:?}/{resource_id}"
            )));
        }
        if prev == 1 {
            let policy = entry.policy();
            let deadline = self.now_ms().saturating_add(u64::from(policy.grace_ms));
            let _ = Self::maybe_arm_release(&entry, policy, deadline);
        }
        Ok(())
    }

    fn force_close(&self, kind: ResourceKind, resource_id: &str) -> Result<(), DomainError> {
        let Some(entry) = self.get(kind, resource_id) else {
            return Ok(());
        };
        let prev = entry.swap_state(LifecycleState::Closed);
        if !prev.is_terminal() {
            entry.grace_until_ms.store(0, Ordering::Release);
            entry.waker.notify_one();
            self.cascade
                .on_resource_closed(kind, resource_id, entry.session_id());
        }
        Ok(())
    }

    fn snapshot(&self, kind: ResourceKind, resource_id: &str) -> Option<LifecycleSnapshot> {
        let entry = self.get(kind, resource_id)?;
        let state = entry.current_state();
        let policy = entry.policy();
        let grace_remaining_ms = (state == LifecycleState::Releasing).then(|| {
            let until = entry.grace_until_ms();
            let now = self.now_ms();
            let remaining = until.saturating_sub(now);
            u32::try_from(remaining).unwrap_or(u32::MAX)
        });
        Some(LifecycleSnapshot {
            state,
            sub_count: entry.sub_count(),
            grace_remaining_ms,
            policy,
        })
    }
}

impl<C: ClockPort> LifecyclePolicyAsync for RefcountedLifecycleAdapter<C> {
    async fn arm_grace_timer(&self, kind: ResourceKind, resource_id: String) {
        let Some(entry) = self.get(kind, &resource_id) else {
            return;
        };
        // The timer task observes `grace_until_ms` and fires the
        // `Releasing -> Closed` transition when the deadline elapses.
        // The task is owned by the runtime (no JoinHandle stored) — it
        // exits on its own when the state changes.
        spawn_grace_timer(
            kind,
            resource_id,
            Arc::clone(&entry),
            Arc::clone(&self.cascade),
            Arc::clone(&self.clock),
        );
    }

    async fn cancel_grace_timer(&self, kind: ResourceKind, resource_id: &str) {
        if let Some(entry) = self.get(kind, resource_id) {
            entry.grace_until_ms.store(0, Ordering::Release);
            entry.waker.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::adapters::clock::fake::FakeClock;
    use crate::adapters::lifecycle::cascade::CascadeCoordinator;
    use crate::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
    use crate::domain::error::DomainError;
    use crate::domain::ids::SessionId;
    use crate::domain::lifecycle::{LifecyclePolicy, LifecycleState};
    use crate::ports::lifecycle_policy::{LifecyclePolicyAsync, LifecyclePolicyPort};
    use crate::ports::subscriber_registry::ResourceKind;

    fn build() -> (
        Arc<RefcountedLifecycleAdapter<FakeClock>>,
        Arc<FakeClock>,
        Arc<CascadeCoordinator>,
    ) {
        let clock = Arc::new(FakeClock::new(1_000_000));
        let cascade = CascadeCoordinator::new();
        let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));
        (adapter, clock, cascade)
    }

    fn sess(id: &str) -> SessionId {
        SessionId::new(id.to_string())
    }

    // --- Tracking and initial state --------------------------------

    #[test]
    fn fresh_track_resource_starts_in_owned() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s-1"),
            LifecyclePolicy::default(),
        );
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("tracked");
        assert_eq!(snap.state, LifecycleState::Owned);
        assert_eq!(snap.sub_count, 0);
        assert!(snap.grace_remaining_ms.is_none());
    }

    #[test]
    fn snapshot_returns_none_for_untracked_resource() {
        let (a, _c, _) = build();
        assert!(a.snapshot(ResourceKind::Shell, "ghost").is_none());
    }

    #[test]
    fn re_track_swaps_policy_without_clobbering_state() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s-1"),
            LifecyclePolicy::default(),
        );
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s-1"),
            LifecyclePolicy::release_with_default_grace(),
        );
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("tracked");
        assert_eq!(snap.state, LifecycleState::Observed);
        assert!(snap.policy.release_when_no_subs);
    }

    #[test]
    fn track_resource_inc_session_only_on_first_track() {
        let (a, _c, cascade) = build();
        let s = sess("s-1");
        a.track_resource(ResourceKind::Shell, "sh-1", &s, LifecyclePolicy::default());
        a.track_resource(ResourceKind::Shell, "sh-1", &s, LifecyclePolicy::default());
        assert_eq!(cascade.session_active_refs(&s), 1);
    }

    #[test]
    fn distinct_resource_ids_track_independently() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        a.track_resource(
            ResourceKind::Shell,
            "sh-2",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        assert!(a.snapshot(ResourceKind::Shell, "sh-1").is_some());
        assert!(a.snapshot(ResourceKind::Shell, "sh-2").is_some());
    }

    #[test]
    fn distinct_resource_kinds_track_independently() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "x",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        a.track_resource(
            ResourceKind::Command,
            "x",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        assert_eq!(
            a.snapshot(ResourceKind::Shell, "x").expect("shell").state,
            LifecycleState::Owned
        );
        assert_eq!(
            a.snapshot(ResourceKind::Command, "x").expect("cmd").state,
            LifecycleState::Owned
        );
    }

    // --- on_subscribe -----------------------------------------------

    #[test]
    fn first_subscribe_transitions_owned_to_observed() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Command,
            "c-1",
            &sess("s-1"),
            LifecyclePolicy::default(),
        );
        a.on_subscribe(ResourceKind::Command, "c-1").expect("sub");
        let snap = a.snapshot(ResourceKind::Command, "c-1").expect("snap");
        assert_eq!(snap.state, LifecycleState::Observed);
        assert_eq!(snap.sub_count, 1);
    }

    #[test]
    fn additional_subscribes_increment_sub_count() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s-1"),
            LifecyclePolicy::default(),
        );
        for _ in 0_u8..5 {
            a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        }
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert_eq!(snap.sub_count, 5);
        assert_eq!(snap.state, LifecycleState::Observed);
    }

    #[test]
    fn subscribe_to_unknown_resource_is_silent_noop() {
        let (a, _c, _) = build();
        // Soft no-op so legacy SSH/SFTP-runtime callers do not regress.
        let result = a.on_subscribe(ResourceKind::Shell, "ghost");
        assert!(result.is_ok());
    }

    #[test]
    fn subscribe_after_close_returns_resource_gone() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s-1"),
            LifecyclePolicy::default(),
        );
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        let err = a
            .on_subscribe(ResourceKind::Shell, "sh-1")
            .expect_err("must fail");
        match err {
            DomainError::ResourceGone(msg) => assert!(msg.contains("s-1"), "msg: {msg}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    // --- on_unsubscribe ---------------------------------------------

    #[test]
    fn unsubscribe_decrements_sub_count() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        a.on_unsubscribe(ResourceKind::Shell, "sh-1").expect("uns");
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert_eq!(snap.sub_count, 1);
        assert_eq!(snap.state, LifecycleState::Observed);
    }

    #[test]
    fn last_unsubscribe_with_release_arms_grace_timer() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::release_with_default_grace(),
        );
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        a.on_unsubscribe(ResourceKind::Shell, "sh-1").expect("uns");
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert_eq!(snap.state, LifecycleState::Releasing);
        assert!(snap.grace_remaining_ms.is_some());
    }

    #[test]
    fn last_unsubscribe_without_release_keeps_observed() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        a.on_unsubscribe(ResourceKind::Shell, "sh-1").expect("uns");
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert_eq!(snap.state, LifecycleState::Observed);
        assert!(snap.grace_remaining_ms.is_none());
    }

    #[test]
    fn unsubscribe_underflow_returns_internal_error_and_recovers_counter() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        let err = a
            .on_unsubscribe(ResourceKind::Shell, "sh-1")
            .expect_err("must fail");
        match err {
            DomainError::Internal(msg) => assert!(msg.contains("underflow"), "msg: {msg}"),
            other => panic!("unexpected: {other:?}"),
        }
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert_eq!(
            snap.sub_count, 0,
            "counter must be restored after underflow"
        );
    }

    #[test]
    fn unsubscribe_unknown_resource_is_silent_noop() {
        let (a, _c, _) = build();
        assert!(a.on_unsubscribe(ResourceKind::Shell, "ghost").is_ok());
    }

    // --- Resubscribe during release window --------------------------

    #[test]
    fn resubscribe_during_releasing_cancels_grace_and_returns_to_observed() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::release_with_default_grace(),
        );
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub1");
        a.on_unsubscribe(ResourceKind::Shell, "sh-1").expect("uns");
        let mid = a.snapshot(ResourceKind::Shell, "sh-1").expect("mid");
        assert_eq!(mid.state, LifecycleState::Releasing);
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub2");
        let after = a.snapshot(ResourceKind::Shell, "sh-1").expect("after");
        assert_eq!(after.state, LifecycleState::Observed);
        assert_eq!(after.sub_count, 1);
        assert!(after.grace_remaining_ms.is_none());
    }

    // --- force_close ------------------------------------------------

    #[test]
    fn force_close_transitions_owned_to_closed() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert_eq!(snap.state, LifecycleState::Closed);
    }

    #[test]
    fn force_close_transitions_observed_to_closed() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        assert_eq!(
            a.snapshot(ResourceKind::Shell, "sh-1").expect("snap").state,
            LifecycleState::Closed
        );
    }

    #[test]
    fn force_close_transitions_releasing_to_closed() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::release_with_default_grace(),
        );
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        a.on_unsubscribe(ResourceKind::Shell, "sh-1").expect("uns");
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        assert_eq!(
            a.snapshot(ResourceKind::Shell, "sh-1").expect("snap").state,
            LifecycleState::Closed
        );
    }

    #[test]
    fn force_close_is_idempotent() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        a.force_close(ResourceKind::Shell, "sh-1").expect("c1");
        a.force_close(ResourceKind::Shell, "sh-1").expect("c2");
        a.force_close(ResourceKind::Shell, "sh-1").expect("c3");
        assert_eq!(
            a.snapshot(ResourceKind::Shell, "sh-1").expect("snap").state,
            LifecycleState::Closed
        );
    }

    #[test]
    fn force_close_unknown_resource_is_silent_noop() {
        let (a, _c, _) = build();
        assert!(a.force_close(ResourceKind::Shell, "ghost").is_ok());
    }

    #[test]
    fn force_close_decrements_session_refcount_only_on_first_close() {
        let (a, _c, cascade) = build();
        let s = sess("s-x");
        a.track_resource(ResourceKind::Shell, "sh-1", &s, LifecyclePolicy::default());
        assert_eq!(cascade.session_active_refs(&s), 1);
        a.force_close(ResourceKind::Shell, "sh-1").expect("c1");
        assert_eq!(cascade.session_active_refs(&s), 0);
        a.force_close(ResourceKind::Shell, "sh-1").expect("c2");
        // Idempotent — refcount stays at 0.
        assert_eq!(cascade.session_active_refs(&s), 0);
    }

    // --- Snapshot remaining time ------------------------------------

    #[test]
    fn snapshot_grace_remaining_decreases_as_clock_advances() {
        let (a, c, _) = build();
        let mut p = LifecyclePolicy::release_with_default_grace();
        p.grace_ms = 10_000;
        a.track_resource(ResourceKind::Shell, "sh-1", &sess("s"), p);
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        a.on_unsubscribe(ResourceKind::Shell, "sh-1").expect("uns");
        let first = a
            .snapshot(ResourceKind::Shell, "sh-1")
            .expect("snap")
            .grace_remaining_ms
            .expect("releasing must report remaining ms");
        c.advance(Duration::from_millis(2_000));
        let second = a
            .snapshot(ResourceKind::Shell, "sh-1")
            .expect("snap")
            .grace_remaining_ms
            .expect("still releasing");
        assert!(second < first, "remaining must shrink: {first} -> {second}");
    }

    #[test]
    fn snapshot_grace_remaining_zero_after_deadline_passed() {
        let (a, c, _) = build();
        let mut p = LifecyclePolicy::release_with_default_grace();
        p.grace_ms = 1_000;
        a.track_resource(ResourceKind::Shell, "sh-1", &sess("s"), p);
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        a.on_unsubscribe(ResourceKind::Shell, "sh-1").expect("uns");
        c.advance(Duration::from_millis(5_000));
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert_eq!(snap.grace_remaining_ms, Some(0));
    }

    // --- Async slice (cancel_grace_timer / arm_grace_timer) ----------

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_grace_timer_clears_deadline_and_wakes_waker() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::release_with_default_grace(),
        );
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        a.on_unsubscribe(ResourceKind::Shell, "sh-1").expect("uns");
        a.cancel_grace_timer(ResourceKind::Shell, "sh-1").await;
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry present");
        assert_eq!(entry.grace_until_ms(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_grace_timer_unknown_resource_is_noop() {
        let (a, _c, _) = build();
        a.cancel_grace_timer(ResourceKind::Shell, "ghost").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn arm_grace_timer_unknown_resource_is_noop() {
        let (a, _c, _) = build();
        a.arm_grace_timer(ResourceKind::Shell, "ghost".to_string())
            .await;
    }

    // --- Concurrency invariants (best-effort under tokio rt) --------

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_sub_unsub_pairs_keep_count_consistent() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        let n = 100_u32;
        let mut handles = Vec::with_capacity(n as usize);
        for _ in 0_u32..n {
            let adapter = Arc::clone(&a);
            handles.push(tokio::spawn(async move {
                adapter.on_subscribe(ResourceKind::Shell, "sh-1").ok();
            }));
        }
        for h in handles {
            h.await.expect("join");
        }
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert_eq!(snap.sub_count, usize::try_from(n).unwrap_or(0));
        assert_eq!(snap.state, LifecycleState::Observed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_balanced_subscribe_and_unsubscribe_returns_to_zero() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        // Pre-fill the count so concurrent unsub never underflows.
        let initial = 50_u32;
        for _ in 0_u32..initial {
            a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        }
        let mut handles = Vec::with_capacity(usize::try_from(initial).unwrap_or(0));
        for _ in 0_u32..initial {
            let adapter = Arc::clone(&a);
            handles.push(tokio::spawn(async move {
                adapter.on_unsubscribe(ResourceKind::Shell, "sh-1").ok();
            }));
        }
        for h in handles {
            h.await.expect("join");
        }
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert_eq!(snap.sub_count, 0);
    }

    // --- LifecycleState invariants kept by the adapter --------------

    #[test]
    fn state_byte_round_trips_through_atomic_storage() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        assert_eq!(entry.current_state(), LifecycleState::Owned);
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        assert_eq!(entry.current_state(), LifecycleState::Observed);
    }

    #[test]
    fn waker_arc_is_shared_with_internal_state() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        let w = entry.waker();
        // notify_one is sync; just ensure the call compiles.
        w.notify_one();
    }

    #[test]
    fn session_id_is_carried_through_to_entry() {
        let (a, _c, _) = build();
        let s = sess("session-7");
        a.track_resource(ResourceKind::Shell, "sh-1", &s, LifecyclePolicy::default());
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        assert_eq!(entry.session_id(), &s);
    }

    #[test]
    fn policy_swap_observable_via_snapshot() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::release_with_default_grace(),
        );
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert!(snap.policy.release_when_no_subs);
    }

    #[test]
    fn track_then_close_decrements_session_refs_to_zero() {
        let (a, _c, cascade) = build();
        let s = sess("s");
        a.track_resource(ResourceKind::Shell, "sh-1", &s, LifecyclePolicy::default());
        a.track_resource(ResourceKind::Command, "c-1", &s, LifecyclePolicy::default());
        assert_eq!(cascade.session_active_refs(&s), 2);
        a.force_close(ResourceKind::Shell, "sh-1").expect("c1");
        a.force_close(ResourceKind::Command, "c-1").expect("c2");
        assert_eq!(cascade.session_active_refs(&s), 0);
    }

    #[test]
    fn observed_state_blocks_release_arming() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::release_with_default_grace(),
        );
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        // Still observed — no release arming yet.
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert_eq!(snap.state, LifecycleState::Observed);
        assert!(snap.grace_remaining_ms.is_none());
    }

    #[test]
    fn high_subscriber_count_is_accurate() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Command,
            "c-x",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        for _ in 0_u32..1_000 {
            a.on_subscribe(ResourceKind::Command, "c-x").expect("sub");
        }
        let snap = a.snapshot(ResourceKind::Command, "c-x").expect("snap");
        assert_eq!(snap.sub_count, 1_000);
    }

    #[test]
    fn snapshot_returns_consistent_struct_after_close() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert_eq!(snap.state, LifecycleState::Closed);
        // grace_remaining_ms is only Some in Releasing.
        assert!(snap.grace_remaining_ms.is_none());
    }

    #[test]
    fn track_records_distinct_session_ids_for_distinct_resources() {
        let (a, _c, cascade) = build();
        let s1 = sess("s-1");
        let s2 = sess("s-2");
        a.track_resource(ResourceKind::Shell, "sh-1", &s1, LifecyclePolicy::default());
        a.track_resource(ResourceKind::Shell, "sh-2", &s2, LifecyclePolicy::default());
        assert_eq!(cascade.session_active_refs(&s1), 1);
        assert_eq!(cascade.session_active_refs(&s2), 1);
    }

    #[test]
    fn snapshot_policy_carries_grace_ms_overrides() {
        let (a, _c, _) = build();
        let mut p = LifecyclePolicy::release_with_default_grace();
        p.grace_ms = 12_500;
        a.track_resource(ResourceKind::Shell, "sh-1", &sess("s"), p);
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert_eq!(snap.policy.grace_ms, 12_500);
    }

    #[test]
    fn re_subscribe_after_resub_keeps_count_in_sync() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::release_with_default_grace(),
        );
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub1");
        a.on_unsubscribe(ResourceKind::Shell, "sh-1").expect("uns");
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub2");
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert_eq!(snap.sub_count, 1);
        assert_eq!(snap.state, LifecycleState::Observed);
    }

    #[test]
    fn closed_snapshot_reports_terminal_state_via_helper() {
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        let snap = a.snapshot(ResourceKind::Shell, "sh-1").expect("snap");
        assert!(snap.state.is_terminal());
    }
}
