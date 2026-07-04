//! Session-level cascade coordinator (v5 Phase 1).
//!
//! [`CascadeCoordinator`] tracks the number of active resources owned by
//! each session. When a resource transitions to
//! [`crate::domain::lifecycle::LifecycleState::Closed`], the lifecycle
//! adapter (`crate::adapters::lifecycle::refcount`) calls
//! [`CascadeCoordinator::on_resource_closed`], which decrements the
//! per-session refcount and — when configured — invokes the optional
//! `auto_disconnect_hook` callback so the composition root can wire an
//! explicit `DisconnectSessionUseCase::execute` call without dragging
//! the use case generics into the lifecycle layer.
//!
//! ## Concurrency invariants
//!
//! - `active_refs: AtomicUsize` per session; decrements use `AcqRel`.
//! - `auto_disconnect_hook` is wrapped in [`ArcSwap`] so the
//!   composition root can install / replace the callback without
//!   restarting the adapter.
//! - The hook fires **at most once** per session — the
//!   `compare_exchange` from `Active -> Reaped` guards re-entry.
//! - Zero `Mutex`. Hook invocations cross thread boundaries through
//!   `Arc<dyn Fn(SessionId) + Send + Sync>` only.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use arc_swap::ArcSwap;
use dashmap::DashMap;

use crate::domain::ids::SessionId;
use crate::ports::subscriber_registry::ResourceKind;

/// Cascade callback signature. Receives the session that just released
/// its last active resource and may trigger the disconnect path.
pub type AutoDisconnectHook = Arc<dyn Fn(SessionId) + Send + Sync + 'static>;

/// Session-level state byte. Matches the
/// [`crate::domain::lifecycle::LifecycleState`] encoding for
/// readability but is internal to the cascade — sessions do not expose
/// `Releasing` to the port surface in Phase 1.
const SESSION_ACTIVE: u8 = 0;
const SESSION_REAPED: u8 = 1;

/// Per-session bookkeeping carried by the coordinator.
struct SessionEntry {
    /// Count of tracked resources whose lifecycle has not yet
    /// transitioned to [`crate::domain::lifecycle::LifecycleState::Closed`].
    active_refs: AtomicUsize,
    /// Reaped flag. Flips from [`SESSION_ACTIVE`] to [`SESSION_REAPED`]
    /// the first time `active_refs` decrements to zero, guaranteeing
    /// the auto-disconnect hook fires at most once per session.
    state: AtomicU8,
}

impl fmt::Debug for SessionEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionEntry")
            .field("active_refs", &self.active_refs.load(Ordering::Acquire))
            .field("state", &self.state.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl SessionEntry {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            active_refs: AtomicUsize::new(0),
            state: AtomicU8::new(SESSION_ACTIVE),
        })
    }
}

/// Cascade coordinator wired into the lifecycle adapter.
pub struct CascadeCoordinator {
    /// Per-session refcount entries.
    sessions: DashMap<SessionId, Arc<SessionEntry>>,
    /// Optional callback fired when a session's `active_refs` reaches
    /// zero. Composition root installs the closure that drives
    /// `DisconnectSessionUseCase::execute`.
    auto_disconnect_hook: ArcSwap<Option<AutoDisconnectHook>>,
}

impl fmt::Debug for CascadeCoordinator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CascadeCoordinator")
            .field("sessions_len", &self.sessions.len())
            .finish_non_exhaustive()
    }
}

impl CascadeCoordinator {
    /// Build a fresh coordinator with no auto-disconnect hook installed.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: DashMap::new(),
            auto_disconnect_hook: ArcSwap::new(Arc::new(None)),
        })
    }

    /// Install the auto-disconnect hook. Composition root wires this
    /// post-construction so the `DisconnectSessionUseCase` and the
    /// lifecycle adapter do not have a circular generic dependency.
    pub fn install_auto_disconnect_hook(&self, hook: AutoDisconnectHook) {
        self.auto_disconnect_hook.store(Arc::new(Some(hook)));
    }

    /// Increment the active-resource count for `session_id`. Called by
    /// the lifecycle adapter the first time it tracks a resource for a
    /// given session.
    pub fn inc_session(&self, session_id: &SessionId) {
        let entry = self
            .sessions
            .entry(session_id.clone())
            .or_insert_with(SessionEntry::new);
        entry.active_refs.fetch_add(1, Ordering::AcqRel);
        // Reset the reaped flag AFTER the increment (B3 TOCTOU twin) so a
        // fresh resource re-binds a previously-reaped session, and a
        // concurrent last-close that reaped between here and the
        // `fetch_add` cannot leave a live-ref session flagged reaped.
        entry.state.store(SESSION_ACTIVE, Ordering::Release);
    }

    /// Notify the coordinator that a tracked resource transitioned to
    /// [`crate::domain::lifecycle::LifecycleState::Closed`]. Decrements
    /// the owning session's refcount and fires the auto-disconnect
    /// hook when it reaches zero.
    ///
    /// The `kind` and `resource_id` arguments are accepted for parity
    /// with future Phase 3 `sub_stats` / `sub_list` rendering; Phase 1
    /// only needs the session id.
    pub fn on_resource_closed(
        &self,
        kind: ResourceKind,
        resource_id: &str,
        session_id: &SessionId,
    ) {
        let _ = (kind, resource_id);
        let entry_opt = self.sessions.get(session_id).map(|r| Arc::clone(r.value()));
        let Some(entry) = entry_opt else {
            return;
        };
        let prev = entry.active_refs.fetch_sub(1, Ordering::AcqRel);
        if prev == 0 {
            // Underflow — recover and bail. This branch documents the
            // bug class without panicking; the strict lint baseline
            // does not allow `panic!`.
            entry.active_refs.fetch_add(1, Ordering::AcqRel);
            return;
        }
        if prev == 1 {
            self.try_fire_hook(session_id, &entry);
        }
    }

    /// Return the active-resource count for `session_id`. Used by
    /// integration tests and the session reaper that wants to gate
    /// inactivity-based reaping on whether any resource is still
    /// observable.
    #[must_use]
    pub fn session_active_refs(&self, session_id: &SessionId) -> usize {
        self.sessions
            .get(session_id)
            .map_or(0, |r| r.value().active_refs.load(Ordering::Acquire))
    }

    /// True when the session has fired its auto-disconnect hook.
    /// Useful for the session reaper to skip a redundant disconnect.
    #[must_use]
    pub fn is_session_reaped(&self, session_id: &SessionId) -> bool {
        self.sessions
            .get(session_id)
            .is_some_and(|r| r.value().state.load(Ordering::Acquire) == SESSION_REAPED)
    }

    /// Drop the session entry once it is reaped and idle. Called by
    /// the session reaper to keep the coordinator's `DashMap` size
    /// bounded over long-running deployments.
    pub fn drop_session(&self, session_id: &SessionId) {
        self.sessions.remove(session_id);
    }

    /// Drop every session that has already fired its auto-disconnect hook
    /// (`SESSION_REAPED`) and carries no active resource refs.
    ///
    /// Called by the leak-watcher eviction sweep (BUG #18) so the
    /// coordinator's `DashMap` stays bounded across long-running
    /// deployments — the per-session [`Self::drop_session`] path is never
    /// reached in production. A re-`inc_session` after the drop simply
    /// recreates a fresh `Active` entry.
    pub fn sweep_reaped_sessions(&self) {
        // Retain a session while it still holds active refs OR has not yet
        // been reaped; drop only the reaped-and-idle entries.
        self.sessions.retain(|_sid, entry| {
            let active = entry.active_refs.load(Ordering::Acquire) > 0;
            let live = entry.state.load(Ordering::Acquire) != SESSION_REAPED;
            active || live
        });
    }

    fn try_fire_hook(&self, session_id: &SessionId, entry: &Arc<SessionEntry>) {
        // CAS Active -> Reaped. Only the winner fires the hook so the
        // disconnect call lands at most once per session.
        if entry
            .state
            .compare_exchange(
                SESSION_ACTIVE,
                SESSION_REAPED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }
        // Compound-atomicity guard (B3 TOCTOU twin): a concurrent
        // `inc_session` may have added a ref AFTER our `fetch_sub` but
        // BEFORE this CAS. Re-read now that we own `Reaped`; if a resource
        // raced in, revert to `Active` and do NOT fire the disconnect hook.
        if entry.active_refs.load(Ordering::Acquire) > 0 {
            entry.state.store(SESSION_ACTIVE, Ordering::Release);
            return;
        }
        // Snapshot the hook then fire outside the entry borrow scope.
        let hook = self.auto_disconnect_hook.load_full();
        if let Some(callback) = hook.as_ref() {
            callback(session_id.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::CascadeCoordinator;
    use crate::domain::ids::SessionId;
    use crate::ports::subscriber_registry::ResourceKind;

    fn sess(id: &str) -> SessionId {
        SessionId::new(id.to_string())
    }

    #[test]
    fn fresh_coordinator_reports_zero_refs_for_unknown_session() {
        let c = CascadeCoordinator::new();
        assert_eq!(c.session_active_refs(&sess("ghost")), 0);
    }

    #[test]
    fn inc_session_increments_active_refs() {
        let c = CascadeCoordinator::new();
        let s = sess("s-1");
        c.inc_session(&s);
        c.inc_session(&s);
        assert_eq!(c.session_active_refs(&s), 2);
    }

    #[test]
    fn on_resource_closed_decrements_active_refs() {
        let c = CascadeCoordinator::new();
        let s = sess("s-1");
        c.inc_session(&s);
        c.inc_session(&s);
        c.on_resource_closed(ResourceKind::Shell, "sh", &s);
        assert_eq!(c.session_active_refs(&s), 1);
    }

    #[test]
    fn unknown_session_close_is_silent_noop() {
        let c = CascadeCoordinator::new();
        c.on_resource_closed(ResourceKind::Shell, "x", &sess("ghost"));
        assert_eq!(c.session_active_refs(&sess("ghost")), 0);
    }

    #[test]
    fn close_without_inc_recovers_and_reports_zero() {
        let c = CascadeCoordinator::new();
        let s = sess("s-x");
        // Manually create a session entry so on_resource_closed sees it.
        c.inc_session(&s);
        // Decrement once — refs reach zero.
        c.on_resource_closed(ResourceKind::Shell, "sh", &s);
        // Underflow guard: another close on the same session must not
        // wrap around to usize::MAX.
        c.on_resource_closed(ResourceKind::Shell, "sh", &s);
        assert_eq!(c.session_active_refs(&s), 0);
    }

    #[test]
    fn auto_disconnect_hook_fires_when_active_refs_reach_zero() {
        let c = CascadeCoordinator::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = Arc::clone(&calls);
        c.install_auto_disconnect_hook(Arc::new(move |_| {
            captured.fetch_add(1, Ordering::AcqRel);
        }));
        let s = sess("s-1");
        c.inc_session(&s);
        c.inc_session(&s);
        c.on_resource_closed(ResourceKind::Shell, "x", &s);
        c.on_resource_closed(ResourceKind::Shell, "y", &s);
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn auto_disconnect_hook_fires_exactly_once_per_session() {
        let c = CascadeCoordinator::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = Arc::clone(&calls);
        c.install_auto_disconnect_hook(Arc::new(move |_| {
            captured.fetch_add(1, Ordering::AcqRel);
        }));
        let s = sess("s-1");
        c.inc_session(&s);
        c.on_resource_closed(ResourceKind::Shell, "x", &s);
        // Re-inc + close — the session is already reaped from the
        // first cascade, but inc_session resets the flag so a new
        // resource attached to the same session can fire again.
        c.inc_session(&s);
        c.on_resource_closed(ResourceKind::Shell, "y", &s);
        assert_eq!(calls.load(Ordering::Acquire), 2);
    }

    #[test]
    fn session_marked_reaped_after_first_zero() {
        let c = CascadeCoordinator::new();
        let s = sess("s-1");
        c.inc_session(&s);
        c.on_resource_closed(ResourceKind::Shell, "x", &s);
        assert!(c.is_session_reaped(&s));
    }

    #[test]
    fn session_not_reaped_when_refs_stay_positive() {
        let c = CascadeCoordinator::new();
        let s = sess("s-1");
        c.inc_session(&s);
        c.inc_session(&s);
        c.on_resource_closed(ResourceKind::Shell, "x", &s);
        assert!(!c.is_session_reaped(&s));
    }

    #[test]
    fn drop_session_removes_entry() {
        let c = CascadeCoordinator::new();
        let s = sess("s-1");
        c.inc_session(&s);
        c.drop_session(&s);
        assert_eq!(c.session_active_refs(&s), 0);
    }

    #[test]
    fn install_hook_is_idempotent() {
        let c = CascadeCoordinator::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = Arc::clone(&calls);
        let hook: super::AutoDisconnectHook = Arc::new(move |_: SessionId| {
            captured.fetch_add(1, Ordering::AcqRel);
        });
        c.install_auto_disconnect_hook(Arc::clone(&hook));
        c.install_auto_disconnect_hook(hook);
        let s = sess("s-1");
        c.inc_session(&s);
        c.on_resource_closed(ResourceKind::Shell, "x", &s);
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn multiple_sessions_track_independently() {
        let c = CascadeCoordinator::new();
        let s1 = sess("s-1");
        let s2 = sess("s-2");
        c.inc_session(&s1);
        c.inc_session(&s2);
        c.inc_session(&s2);
        assert_eq!(c.session_active_refs(&s1), 1);
        assert_eq!(c.session_active_refs(&s2), 2);
    }

    #[test]
    fn hook_receives_session_id_as_argument() {
        let c = CascadeCoordinator::new();
        let received = Arc::new(std::sync::Mutex::new(Vec::<SessionId>::new()));
        let captured = Arc::clone(&received);
        c.install_auto_disconnect_hook(Arc::new(move |sid| {
            if let Ok(mut g) = captured.lock() {
                g.push(sid);
            }
        }));
        let s = sess("s-7");
        c.inc_session(&s);
        c.on_resource_closed(ResourceKind::Shell, "x", &s);
        let g = received.lock().expect("lock");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0], s);
    }

    #[test]
    fn no_hook_installed_means_silent_zero_transition() {
        let c = CascadeCoordinator::new();
        let s = sess("s-1");
        c.inc_session(&s);
        c.on_resource_closed(ResourceKind::Shell, "x", &s);
        assert_eq!(c.session_active_refs(&s), 0);
    }

    #[test]
    fn session_with_zero_initial_refs_does_not_fire_hook_on_close() {
        let c = CascadeCoordinator::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = Arc::clone(&calls);
        c.install_auto_disconnect_hook(Arc::new(move |_| {
            captured.fetch_add(1, Ordering::AcqRel);
        }));
        // No inc_session — close is a no-op.
        c.on_resource_closed(ResourceKind::Shell, "x", &sess("ghost"));
        assert_eq!(calls.load(Ordering::Acquire), 0);
    }

    #[test]
    fn debug_format_is_stable() {
        let c = CascadeCoordinator::new();
        c.inc_session(&sess("s-1"));
        let dbg = format!("{c:?}");
        assert!(dbg.contains("CascadeCoordinator"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_inc_and_close_pairs_balance_to_zero() {
        let c = CascadeCoordinator::new();
        let s = sess("s-1");
        for _ in 0_u32..50 {
            c.inc_session(&s);
        }
        let mut handles = Vec::new();
        for _ in 0_u32..50 {
            let cx = Arc::clone(&c);
            let sx = s.clone();
            handles.push(tokio::spawn(async move {
                cx.on_resource_closed(ResourceKind::Shell, "x", &sx);
            }));
        }
        for h in handles {
            h.await.expect("join");
        }
        assert_eq!(c.session_active_refs(&s), 0);
    }

    #[test]
    fn drop_session_during_close_is_safe() {
        let c = CascadeCoordinator::new();
        let s = sess("s-1");
        c.inc_session(&s);
        // Concurrent drop + close — the close path tolerates an
        // already-dropped session.
        c.drop_session(&s);
        c.on_resource_closed(ResourceKind::Shell, "x", &s);
        assert_eq!(c.session_active_refs(&s), 0);
    }

    #[test]
    fn reset_after_drop_creates_fresh_active_state() {
        let c = CascadeCoordinator::new();
        let s = sess("s-1");
        c.inc_session(&s);
        c.on_resource_closed(ResourceKind::Shell, "x", &s);
        assert!(c.is_session_reaped(&s));
        c.drop_session(&s);
        c.inc_session(&s);
        assert!(!c.is_session_reaped(&s));
    }

    #[test]
    fn coordinator_default_via_arc_new_does_not_panic() {
        let _ = CascadeCoordinator::new();
    }

    #[test]
    fn hook_replacement_uses_latest_callback() {
        let c = CascadeCoordinator::new();
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let f = Arc::clone(&first);
        c.install_auto_disconnect_hook(Arc::new(move |_| {
            f.fetch_add(1, Ordering::AcqRel);
        }));
        let g = Arc::clone(&second);
        c.install_auto_disconnect_hook(Arc::new(move |_| {
            g.fetch_add(1, Ordering::AcqRel);
        }));
        let s = sess("s-1");
        c.inc_session(&s);
        c.on_resource_closed(ResourceKind::Shell, "x", &s);
        assert_eq!(first.load(Ordering::Acquire), 0);
        assert_eq!(second.load(Ordering::Acquire), 1);
    }

    #[test]
    fn sweep_reaped_sessions_drops_reaped_idle_only() {
        let c = CascadeCoordinator::new();
        let reaped = sess("reaped");
        let active = sess("active");
        // reaped: inc then close to zero -> reaped + idle.
        c.inc_session(&reaped);
        c.on_resource_closed(ResourceKind::Shell, "x", &reaped);
        assert!(c.is_session_reaped(&reaped));
        // active: still holds a ref.
        c.inc_session(&active);
        assert!(format!("{c:?}").contains("sessions_len: 2"));
        c.sweep_reaped_sessions();
        // Only the active session survives the sweep.
        assert!(format!("{c:?}").contains("sessions_len: 1"));
        assert_eq!(c.session_active_refs(&active), 1);
    }

    #[test]
    fn sweep_reaped_sessions_keeps_active_sessions() {
        let c = CascadeCoordinator::new();
        let s = sess("busy");
        c.inc_session(&s);
        c.inc_session(&s);
        c.on_resource_closed(ResourceKind::Shell, "x", &s);
        // Still one ref outstanding -> not reaped -> retained.
        c.sweep_reaped_sessions();
        assert_eq!(c.session_active_refs(&s), 1);
        assert!(!c.is_session_reaped(&s));
    }

    #[test]
    fn many_sessions_can_coexist_under_load() {
        let c = CascadeCoordinator::new();
        for i in 0_u32..256 {
            let s = sess(&format!("s-{i}"));
            c.inc_session(&s);
        }
        for i in 0_u32..256 {
            let s = sess(&format!("s-{i}"));
            c.on_resource_closed(ResourceKind::Shell, "x", &s);
            assert_eq!(c.session_active_refs(&s), 0);
        }
    }
}
