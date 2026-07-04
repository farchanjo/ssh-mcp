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

/// Retention window (milliseconds) a [`LifecycleState::Closed`] entry is
/// kept in the store before the leak-watcher eviction sweep drops it.
///
/// The window preserves the `on_subscribe`-after-close
/// [`DomainError::ResourceGone`] contract: while the entry is present and
/// `Closed`, a late subscribe still reports the resource gone; only after
/// the entry is evicted does the call soften to the absent-entry no-op.
pub const CLOSED_RETENTION_MS: u64 = 60_000;

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
    /// Wall-clock time (Unix-millis) at which the resource reached
    /// [`LifecycleState::Closed`]. Stamped on every close transition
    /// (`force_close` and the grace-timer fire); `0` while the resource is
    /// still live. The leak-watcher eviction sweep reads it to enforce a
    /// short retention window ([`CLOSED_RETENTION_MS`]) before dropping the
    /// entry, keeping the `on_subscribe`-after-close `ResourceGone`
    /// contract intact.
    closed_at: AtomicU64,
    /// Records whether this resource contributed a session refcount
    /// increment at track time (i.e. `policy.cascade_session == true`).
    /// Frozen at construction and mirrored on close so the cascade
    /// inc/dec stays balanced even when a re-track swaps the policy — the
    /// close side never decrements a ref it never incremented.
    contributed_session_inc: bool,
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
            .field("closed_at", &self.closed_at.load(Ordering::Acquire))
            .field("contributed_session_inc", &self.contributed_session_inc)
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
            closed_at: AtomicU64::new(0),
            // `LifecyclePolicy` is `Copy`, so reading the flag after moving
            // `policy` into the `ArcSwap` is fine.
            contributed_session_inc: policy.cascade_session,
        })
    }

    /// Read the resource creation time (Unix-millis).
    ///
    /// Stamped on `track_resource` and never mutated thereafter.
    #[must_use]
    pub fn created_at_ms(&self) -> u64 {
        self.created_at_ms.load(Ordering::Acquire)
    }

    /// Read the close timestamp (Unix-millis). `0` while the resource is
    /// still live; stamped on the transition into
    /// [`LifecycleState::Closed`]. The eviction sweep reads it to enforce
    /// the [`CLOSED_RETENTION_MS`] retention window.
    #[must_use]
    pub fn closed_at(&self) -> u64 {
        self.closed_at.load(Ordering::Acquire)
    }

    /// Whether this resource contributed a session refcount increment at
    /// track time. Read on close so the cascade decrement mirrors the
    /// increment, keeping the session accounting balanced.
    #[must_use]
    pub const fn contributed_session_inc(&self) -> bool {
        self.contributed_session_inc
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

    /// Borrow the raw close-timestamp atomic. Used by the grace timer to
    /// stamp the close time when it fires the `Releasing -> Closed`
    /// transition, matching the stamp `force_close` writes on the manual
    /// close path.
    #[must_use]
    pub const fn closed_at_atomic(&self) -> &AtomicU64 {
        &self.closed_at
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

    /// Evict terminal resource entries and reaped-idle sessions to keep
    /// the lock-free maps bounded on long-running daemons (BUG #18).
    ///
    /// A [`LifecycleState::Closed`] entry is retained until it has been
    /// closed for longer than `retention_ms`. Within the window,
    /// `on_subscribe` after close keeps returning
    /// [`DomainError::ResourceGone`] (entry present + `Closed`); only once
    /// the entry is dropped does the call soften to the absent-entry
    /// no-op. Reaped, idle sessions are dropped from the cascade
    /// coordinator in the same pass.
    pub fn sweep_terminal(&self, retention_ms: u64) {
        let now = self.now_ms();
        self.resources.retain(|_key, entry| {
            if entry.current_state() != LifecycleState::Closed {
                return true;
            }
            now.saturating_sub(entry.closed_at()) <= retention_ms
        });
        self.cascade.sweep_reaped_sessions();
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
            // Compound-atomicity guard (B3 TOCTOU): a concurrent
            // `on_subscribe` may have taken the `Observed` fast path and
            // `fetch_add`ed AFTER our `fetch_sub` but BEFORE this CAS,
            // leaving `Releasing` with a live subscriber the grace timer
            // would wrongly close. Re-read the count now that we own
            // `Releasing`; if a subscriber raced in, revert to `Observed`
            // and clear the deadline so the timer never fires.
            if entry.sub_count.load(Ordering::Acquire) > 0
                && entry.cas_state(LifecycleState::Releasing, LifecycleState::Observed)
            {
                entry.grace_until_ms.store(0, Ordering::Release);
                entry.waker.notify_one();
            }
            // Otherwise fall through to spawn the timer task in the async slice.
        }
        entry.current_state()
    }

    /// B3 TOCTOU compensate applied right after the `on_subscribe`
    /// increment. The resource may have changed state between
    /// [`Self::promote_to_observed`] and the `fetch_add`; reconcile so a
    /// live subscriber is honored and a subscriber attached to a
    /// meanwhile-closed resource is undone.
    fn compensate_after_subscribe(entry: &Arc<ResourceLifecycle>) -> Result<(), DomainError> {
        match entry.current_state() {
            // A concurrent last-`on_unsubscribe` armed `Releasing`; our
            // subscription would otherwise be lost to the grace timer.
            // Revert so a live subscriber is honored (symmetric with
            // `maybe_arm_release`).
            LifecycleState::Releasing => {
                if entry.cas_state(LifecycleState::Releasing, LifecycleState::Observed) {
                    entry.grace_until_ms.store(0, Ordering::Release);
                    entry.waker.notify_one();
                }
                Ok(())
            }
            // A concurrent `force_close` / grace-timer fire closed the
            // resource under us. Undo the increment so a dead resource
            // never retains a phantom subscriber, and report it gone.
            LifecycleState::Closed => {
                entry.sub_count.fetch_sub(1, Ordering::AcqRel);
                Err(DomainError::ResourceGone(format!(
                    "resource already closed: {:?}",
                    entry.session_id
                )))
            }
            LifecycleState::Owned | LifecycleState::Observed => Ok(()),
        }
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
        // First-time tracking debits the session refcount ONLY when the
        // policy opts in to session cascade. `cascade_session == false`
        // (the v4 default) keeps the resource out of the session's
        // active-ref accounting so its close never triggers an
        // auto-disconnect. The entry's frozen `contributed_session_inc`
        // flag mirrors this decision on close so inc/dec stay balanced
        // across a re-track policy swap.
        if policy.cascade_session {
            self.cascade.inc_session(session_id);
        }
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
        // Compound-atomicity guard (B3 TOCTOU): the resource may have
        // changed state between `promote_to_observed` and this
        // `fetch_add`. Reconcile Releasing (revert) and Closed (undo +
        // report gone) so no phantom subscriber survives.
        Self::compensate_after_subscribe(&entry)
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
            // Stamp the close time so the eviction sweep can apply the
            // retention window before dropping the entry.
            entry.closed_at.store(self.now_ms(), Ordering::Release);
            entry.waker.notify_one();
            // Only decrement the session refcount when this resource
            // actually incremented it (frozen at track time), so the
            // cascade dec mirrors the inc and never underflows a sibling.
            if entry.contributed_session_inc() {
                self.cascade
                    .on_resource_closed(kind, resource_id, entry.session_id());
            }
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

    fn arm_release_timer(&self, kind: ResourceKind, resource_id: &str) {
        let Some(entry) = self.get(kind, resource_id) else {
            return;
        };
        // Only arm when the last unsubscribe actually drove the resource
        // into `Releasing` with a live deadline. A concurrent resubscribe
        // may have reverted it to `Observed` and cleared the deadline.
        // Spawning a second timer against the same resource is safe — the
        // `Releasing -> Closed` CAS in `grace_timer::fire_close` fires the
        // cascade at most once.
        if entry.current_state() != LifecycleState::Releasing || entry.grace_until_ms() == 0 {
            return;
        }
        spawn_grace_timer(
            kind,
            resource_id.to_string(),
            entry,
            Arc::clone(&self.cascade),
            Arc::clone(&self.clock),
        );
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

    // --- B3 TOCTOU compensate --------------------------------------

    #[test]
    fn maybe_arm_release_reverts_when_subscriber_present_b3() {
        // B3 TOCTOU compensate: if a subscriber is present when the
        // Observed->Releasing CAS fires (an on_subscribe that raced in
        // before the CAS), maybe_arm_release must revert to Observed and
        // clear the grace deadline so the timer never closes a live one.
        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s-1"),
            LifecyclePolicy::release_with_default_grace(),
        );
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        let entry = a.get(ResourceKind::Shell, "sh-1").expect("entry");
        assert_eq!(entry.current_state(), LifecycleState::Observed);
        assert_eq!(entry.sub_count(), 1);
        // Simulate the race outcome: arm release while the subscriber is
        // still counted.
        let deadline = a.now_ms().saturating_add(1_000);
        let state = RefcountedLifecycleAdapter::<FakeClock>::maybe_arm_release(
            &entry,
            entry.policy(),
            deadline,
        );
        assert_eq!(state, LifecycleState::Observed, "compensate must revert");
        assert_eq!(entry.current_state(), LifecycleState::Observed);
        assert_eq!(entry.grace_until_ms(), 0, "deadline cleared on revert");
    }

    #[test]
    fn on_subscribe_compensate_undoes_increment_when_resource_closed_b3() {
        // BUG #10: if the resource reaches Closed between
        // `promote_to_observed` and the `fetch_add`, the compensate must
        // undo the increment and report ResourceGone rather than leaving a
        // phantom subscriber counted on a dead resource.
        use std::sync::atomic::Ordering;

        let (a, _c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s-1"),
            LifecyclePolicy::default(),
        );
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        // Reproduce the post-`fetch_add` TOCTOU window: the subscriber is
        // already counted, but the resource was force-closed under us.
        entry.sub_count.fetch_add(1, Ordering::AcqRel);
        entry
            .state_atomic()
            .store(LifecycleState::Closed.as_u8(), Ordering::Release);
        let err = RefcountedLifecycleAdapter::<FakeClock>::compensate_after_subscribe(&entry)
            .expect_err("closed resource must report gone");
        match err {
            DomainError::ResourceGone(msg) => assert!(msg.contains("s-1"), "msg: {msg}"),
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(entry.sub_count(), 0, "phantom subscriber must be undone");
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
        // cascade_session=true so the inc is actually issued; the re-track
        // must be idempotent and not inc a second time.
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &s,
            LifecyclePolicy::release_with_cascade(),
        );
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &s,
            LifecyclePolicy::release_with_cascade(),
        );
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
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &s,
            LifecyclePolicy::release_with_cascade(),
        );
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
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &s,
            LifecyclePolicy::release_with_cascade(),
        );
        a.track_resource(
            ResourceKind::Command,
            "c-1",
            &s,
            LifecyclePolicy::release_with_cascade(),
        );
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
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &s1,
            LifecyclePolicy::release_with_cascade(),
        );
        a.track_resource(
            ResourceKind::Shell,
            "sh-2",
            &s2,
            LifecyclePolicy::release_with_cascade(),
        );
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

    // --- BUG #2: cascade_session gates session cascade ---------------

    #[test]
    fn default_policy_close_does_not_cascade_disconnect() {
        // BUG #2: cascade_session=false (the default) must keep the
        // resource out of the session's active-ref accounting so a close
        // never triggers the auto-disconnect hook.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (a, _c, cascade) = build();
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = Arc::clone(&calls);
        cascade.install_auto_disconnect_hook(Arc::new(move |_| {
            captured.fetch_add(1, Ordering::AcqRel);
        }));
        let s = sess("s-1");
        a.track_resource(ResourceKind::Shell, "sh-1", &s, LifecyclePolicy::default());
        // Default policy never inc'd the session.
        assert_eq!(cascade.session_active_refs(&s), 0);
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        assert_eq!(
            calls.load(Ordering::Acquire),
            0,
            "cascade_session=false must not auto-disconnect the session"
        );
    }

    #[test]
    fn cascade_policy_close_fires_disconnect_once() {
        // Complement: cascade_session=true inc's the session and its close
        // drives the auto-disconnect hook exactly once.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (a, _c, cascade) = build();
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = Arc::clone(&calls);
        cascade.install_auto_disconnect_hook(Arc::new(move |_| {
            captured.fetch_add(1, Ordering::AcqRel);
        }));
        let s = sess("s-1");
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &s,
            LifecyclePolicy::release_with_cascade(),
        );
        assert_eq!(cascade.session_active_refs(&s), 1);
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        assert_eq!(cascade.session_active_refs(&s), 0);
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn retrack_cascade_then_default_keeps_balance() {
        // Edge: track cascade_session=true (inc), re-track cascade_session
        // =false (policy swap). The contribution flag is frozen at track
        // time so close still decrements exactly once — never a dec
        // without a matching inc.
        let (a, _c, cascade) = build();
        let s = sess("s-1");
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &s,
            LifecyclePolicy::release_with_cascade(),
        );
        assert_eq!(cascade.session_active_refs(&s), 1);
        // Swap to a non-cascade policy — must not inc again and must not
        // drop the existing contribution.
        a.track_resource(ResourceKind::Shell, "sh-1", &s, LifecyclePolicy::default());
        assert_eq!(cascade.session_active_refs(&s), 1);
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        assert_eq!(
            cascade.session_active_refs(&s),
            0,
            "close mirrors the frozen inc"
        );
    }

    #[test]
    fn retrack_default_then_cascade_never_decs_a_siblings_ref() {
        // Reverse edge: track cascade_session=false (no inc), re-track
        // cascade_session=true. The frozen flag stays false so close does
        // NOT dec — no underflow, no phantom reap of a sibling's ref.
        let (a, _c, cascade) = build();
        let s = sess("s-1");
        // A sibling holds a real cascade ref so an erroneous dec would be
        // observable as a premature reap.
        a.track_resource(
            ResourceKind::Command,
            "c-sib",
            &s,
            LifecyclePolicy::release_with_cascade(),
        );
        assert_eq!(cascade.session_active_refs(&s), 1);
        a.track_resource(ResourceKind::Shell, "sh-1", &s, LifecyclePolicy::default());
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &s,
            LifecyclePolicy::release_with_cascade(),
        );
        // Still only the sibling contributed.
        assert_eq!(cascade.session_active_refs(&s), 1);
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        assert_eq!(
            cascade.session_active_refs(&s),
            1,
            "close must not dec a ref it never inc'd"
        );
    }

    // --- BUG #18: terminal-entry eviction sweep ---------------------

    #[test]
    fn sweep_terminal_retains_closed_within_window_then_evicts() {
        let (a, c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        // Within the retention window the Closed entry survives and a late
        // subscribe still reports ResourceGone.
        a.sweep_terminal(super::CLOSED_RETENTION_MS);
        assert!(a.entry(ResourceKind::Shell, "sh-1").is_some());
        match a.on_subscribe(ResourceKind::Shell, "sh-1") {
            Err(DomainError::ResourceGone(_)) => {}
            other => panic!("expected ResourceGone, got {other:?}"),
        }
        // Past the retention window the entry is evicted.
        c.advance(Duration::from_millis(super::CLOSED_RETENTION_MS + 1_000));
        a.sweep_terminal(super::CLOSED_RETENTION_MS);
        assert!(a.entry(ResourceKind::Shell, "sh-1").is_none());
        // on_subscribe now softens to the absent-entry no-op.
        assert!(a.on_subscribe(ResourceKind::Shell, "sh-1").is_ok());
    }

    #[test]
    fn sweep_terminal_keeps_live_resources_regardless_of_age() {
        let (a, c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "live",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        c.advance(Duration::from_millis(120_000));
        a.sweep_terminal(super::CLOSED_RETENTION_MS);
        assert!(a.entry(ResourceKind::Shell, "live").is_some());
    }

    #[test]
    fn sweep_terminal_drops_reaped_idle_sessions() {
        let (a, _c, cascade) = build();
        let s = sess("s-reap");
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &s,
            LifecyclePolicy::release_with_cascade(),
        );
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        assert!(cascade.is_session_reaped(&s));
        a.sweep_terminal(super::CLOSED_RETENTION_MS);
        // A brand-new inc after the drop starts from a fresh Active entry.
        a.track_resource(
            ResourceKind::Command,
            "c-1",
            &s,
            LifecyclePolicy::release_with_cascade(),
        );
        assert!(!cascade.is_session_reaped(&s));
        assert_eq!(cascade.session_active_refs(&s), 1);
    }
}
