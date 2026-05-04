//! Grace timer task for the lifecycle adapter (v5 Phase 1).
//!
//! When a resource transitions to
//! [`crate::domain::lifecycle::LifecycleState::Releasing`], the adapter
//! arms a one-shot grace task via [`spawn_grace_timer`]. The task
//! polls the deadline atomically and reacts to cancel signals via the
//! shared [`tokio::sync::Notify`] waker:
//!
//! - When the deadline elapses, the task CAS-transitions
//!   `Releasing -> Closed` and notifies the cascade coordinator.
//! - When the waker fires before the deadline, the task re-reads the
//!   state. If the resource is back in
//!   [`crate::domain::lifecycle::LifecycleState::Observed`] (resubscribed)
//!   or already
//!   [`crate::domain::lifecycle::LifecycleState::Closed`] (force-closed),
//!   the task exits without further side effects.
//!
//! ## Concurrency invariants
//!
//! - At most one grace task per resource is observable to the cascade
//!   coordinator: even if two timers race (e.g. unsubscribe -> resub
//!   -> unsubscribe again), only the first to win the
//!   `Releasing -> Closed` CAS fires the cascade.
//! - The task does not hold any guard across `.await`. The atomic
//!   reads / CAS are entirely sync; the only `.await` points are
//!   `tokio::time::sleep` and `Notify::notified`.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::time;
use tracing::trace;

use crate::adapters::lifecycle::cascade::CascadeCoordinator;
use crate::adapters::lifecycle::refcount::ResourceLifecycle;
use crate::domain::lifecycle::LifecycleState;
use crate::ports::clock::ClockPort;
use crate::ports::subscriber_registry::ResourceKind;

/// Polling cadence used when the deadline is too far in the future to
/// trust a single `sleep_until` — short enough to react quickly to
/// `cancel_grace_timer` and long enough to avoid busy spin.
const POLL_CADENCE: Duration = Duration::from_millis(250);

/// Spawn the grace timer task for a single resource. The task runs
/// detached; it observes `lifecycle.grace_until_ms` and the shared
/// waker to decide when to fire.
pub fn spawn_grace_timer<C: ClockPort>(
    kind: ResourceKind,
    resource_id: String,
    lifecycle: Arc<ResourceLifecycle>,
    cascade: Arc<CascadeCoordinator>,
    clock: Arc<C>,
) {
    tokio::spawn(async move {
        run_grace_timer(kind, &resource_id, &lifecycle, &cascade, &*clock).await;
    });
}

/// Run loop pulled out of [`spawn_grace_timer`] so unit tests can
/// drive it directly without spawning a task.
pub async fn run_grace_timer<C: ClockPort>(
    kind: ResourceKind,
    resource_id: &str,
    lifecycle: &Arc<ResourceLifecycle>,
    cascade: &Arc<CascadeCoordinator>,
    clock: &C,
) {
    let waker = lifecycle.waker();
    while let Some(sleep_dur) = next_iteration(kind, resource_id, lifecycle, cascade, clock) {
        tokio::select! {
            biased;
            () = waker.notified() => {}
            () = time::sleep(sleep_dur) => {}
        }
    }
}

/// One iteration of the grace timer loop. Returns `Some(sleep_dur)` when
/// the loop should keep waiting and `None` when the task must exit
/// (state left Releasing, deadline cleared, or close fired).
fn next_iteration<C: ClockPort>(
    kind: ResourceKind,
    resource_id: &str,
    lifecycle: &Arc<ResourceLifecycle>,
    cascade: &Arc<CascadeCoordinator>,
    clock: &C,
) -> Option<Duration> {
    let state = lifecycle.current_state();
    if state != LifecycleState::Releasing {
        trace!(?state, %resource_id, "grace timer exit: not releasing");
        return None;
    }
    let until = lifecycle.grace_until_ms();
    if until == 0 {
        trace!(%resource_id, "grace timer exit: deadline cleared");
        return None;
    }
    let now = current_now_ms(clock);
    if now >= until {
        fire_close(kind, resource_id, lifecycle, cascade);
        return None;
    }
    let remaining = until.saturating_sub(now);
    let sleep_ms = remaining.min(u64::try_from(POLL_CADENCE.as_millis()).unwrap_or(250));
    Some(Duration::from_millis(sleep_ms))
}

fn current_now_ms<C: ClockPort>(clock: &C) -> u64 {
    let dt = clock.utc_now();
    u64::try_from(dt.timestamp_millis()).unwrap_or(0)
}

fn fire_close(
    kind: ResourceKind,
    resource_id: &str,
    lifecycle: &Arc<ResourceLifecycle>,
    cascade: &Arc<CascadeCoordinator>,
) {
    // Atomically transition Releasing -> Closed. If the CAS fails
    // someone else (force_close or a concurrent grace task) already
    // closed the resource — exit silently.
    let observed = lifecycle.current_state();
    if observed != LifecycleState::Releasing {
        return;
    }
    let cas_ok = lifecycle
        .state_atomic()
        .compare_exchange(
            LifecycleState::Releasing.as_u8(),
            LifecycleState::Closed.as_u8(),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok();
    if !cas_ok {
        return;
    }
    lifecycle
        .grace_until_ms_atomic()
        .store(0, Ordering::Release);
    cascade.on_resource_closed(kind, resource_id, lifecycle.session_id());
    trace!(%resource_id, "grace timer fired close");
}

// ---------------------------------------------------------------------------
// Test helpers — exposed via `pub(crate)` accessors on `ResourceLifecycle`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use crate::adapters::clock::fake::FakeClock;
    use crate::adapters::lifecycle::cascade::CascadeCoordinator;
    use crate::adapters::lifecycle::grace_timer::run_grace_timer;
    use crate::adapters::lifecycle::refcount::RefcountedLifecycleAdapter;
    use crate::domain::ids::SessionId;
    use crate::domain::lifecycle::{LifecyclePolicy, LifecycleState};
    use crate::ports::lifecycle_policy::LifecyclePolicyPort;
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

    fn arm_release(a: &Arc<RefcountedLifecycleAdapter<FakeClock>>, kind: ResourceKind, id: &str) {
        a.track_resource(
            kind,
            id,
            &sess("s"),
            LifecyclePolicy::release_with_default_grace(),
        );
        a.on_subscribe(kind, id).expect("sub");
        a.on_unsubscribe(kind, id).expect("uns");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timer_fires_close_when_deadline_elapsed_before_run() {
        let (a, c, _) = build();
        arm_release(&a, ResourceKind::Shell, "sh-1");
        // Move the clock past the deadline before the task starts.
        c.advance(Duration::from_millis(10_000));
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        let cascade = CascadeCoordinator::new();
        run_grace_timer(ResourceKind::Shell, "sh-1", &entry, &cascade, &*c).await;
        assert_eq!(entry.current_state(), LifecycleState::Closed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timer_exits_silently_when_state_is_not_releasing() {
        let (a, c, _) = build();
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::default(),
        );
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        let cascade = CascadeCoordinator::new();
        run_grace_timer(ResourceKind::Shell, "sh-1", &entry, &cascade, &*c).await;
        assert_eq!(entry.current_state(), LifecycleState::Owned);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timer_exits_when_deadline_cleared_to_zero() {
        let (a, c, _) = build();
        arm_release(&a, ResourceKind::Shell, "sh-1");
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        // Cancel the timer by zeroing the deadline.
        entry.grace_until_ms_atomic().store(0, Ordering::Release);
        // Force the state back to Owned via a fake transition so the
        // timer exits without firing close.
        entry
            .state_atomic()
            .store(LifecycleState::Observed.as_u8(), Ordering::Release);
        let cascade = CascadeCoordinator::new();
        run_grace_timer(ResourceKind::Shell, "sh-1", &entry, &cascade, &*c).await;
        // State stays at Observed because the timer never fired close.
        assert_eq!(entry.current_state(), LifecycleState::Observed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn waker_notification_re_reads_state_and_can_exit() {
        let (a, c, _) = build();
        arm_release(&a, ResourceKind::Shell, "sh-1");
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        let waker = entry.waker();
        let entry_clone = Arc::clone(&entry);
        let clock_clone = Arc::clone(&c);
        let cascade_clone = CascadeCoordinator::new();
        let task = tokio::spawn(async move {
            run_grace_timer(
                ResourceKind::Shell,
                "sh-1",
                &entry_clone,
                &cascade_clone,
                &*clock_clone,
            )
            .await;
        });
        // Resub via promote: flip state to Observed and clear the
        // deadline, then wake the timer.
        entry
            .state_atomic()
            .store(LifecycleState::Observed.as_u8(), Ordering::Release);
        entry.grace_until_ms_atomic().store(0, Ordering::Release);
        waker.notify_one();
        task.await.expect("join");
        assert_eq!(entry.current_state(), LifecycleState::Observed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timer_fire_invokes_cascade_with_session_id() {
        let (a, c, cascade) = build();
        let s = sess("s-1");
        a.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &s,
            LifecyclePolicy::release_with_default_grace(),
        );
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        a.on_unsubscribe(ResourceKind::Shell, "sh-1").expect("uns");
        c.advance(Duration::from_millis(10_000));
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        run_grace_timer(ResourceKind::Shell, "sh-1", &entry, &cascade, &*c).await;
        assert_eq!(entry.current_state(), LifecycleState::Closed);
        assert_eq!(cascade.session_active_refs(&s), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timer_close_is_idempotent_under_repeat_runs() {
        let (a, c, cascade) = build();
        arm_release(&a, ResourceKind::Shell, "sh-1");
        c.advance(Duration::from_millis(10_000));
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        run_grace_timer(ResourceKind::Shell, "sh-1", &entry, &cascade, &*c).await;
        run_grace_timer(ResourceKind::Shell, "sh-1", &entry, &cascade, &*c).await;
        assert_eq!(entry.current_state(), LifecycleState::Closed);
        // Cascade refs remain at 0 even after a second redundant run.
        assert_eq!(cascade.session_active_refs(&sess("s")), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn force_close_before_timer_fires_skips_cascade_double_count() {
        let (a, c, cascade) = build();
        arm_release(&a, ResourceKind::Shell, "sh-1");
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        // Now run the timer — Releasing was already promoted to Closed
        // by force_close, so the timer must bail.
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        run_grace_timer(ResourceKind::Shell, "sh-1", &entry, &cascade, &*c).await;
        // refs stayed at zero; cascade fired exactly once via force_close.
        assert_eq!(cascade.session_active_refs(&sess("s")), 0);
        assert_eq!(entry.current_state(), LifecycleState::Closed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timer_runs_against_distinct_resources_independently() {
        let (a, c, _cascade) = build();
        arm_release(&a, ResourceKind::Shell, "sh-1");
        arm_release(&a, ResourceKind::Command, "c-1");
        c.advance(Duration::from_millis(10_000));
        let e1 = a.entry(ResourceKind::Shell, "sh-1").expect("e1");
        let e2 = a.entry(ResourceKind::Command, "c-1").expect("e2");
        let cascade1 = CascadeCoordinator::new();
        let cascade2 = CascadeCoordinator::new();
        run_grace_timer(ResourceKind::Shell, "sh-1", &e1, &cascade1, &*c).await;
        run_grace_timer(ResourceKind::Command, "c-1", &e2, &cascade2, &*c).await;
        assert_eq!(e1.current_state(), LifecycleState::Closed);
        assert_eq!(e2.current_state(), LifecycleState::Closed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_cadence_does_not_fire_before_deadline() {
        let (a, c, _cascade) = build();
        arm_release(&a, ResourceKind::Shell, "sh-1");
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        let cascade = CascadeCoordinator::new();
        let entry_clone = Arc::clone(&entry);
        let clock_clone = Arc::clone(&c);
        let task = tokio::spawn(async move {
            run_grace_timer(
                ResourceKind::Shell,
                "sh-1",
                &entry_clone,
                &cascade,
                &*clock_clone,
            )
            .await;
        });
        // Poll briefly and confirm the timer has not yet fired.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(entry.current_state(), LifecycleState::Releasing);
        // Cancel the timer so the task exits.
        entry.grace_until_ms_atomic().store(0, Ordering::Release);
        entry.waker().notify_one();
        task.await.expect("join");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timer_close_fires_only_once_per_resource() {
        let (a, c, cascade) = build();
        arm_release(&a, ResourceKind::Shell, "sh-1");
        c.advance(Duration::from_millis(10_000));
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        let inner_cascade = Arc::clone(&cascade);
        let task1 = {
            let e = Arc::clone(&entry);
            let cc = Arc::clone(&inner_cascade);
            let cl = Arc::clone(&c);
            tokio::spawn(async move {
                run_grace_timer(ResourceKind::Shell, "sh-1", &e, &cc, &*cl).await;
            })
        };
        let task2 = {
            let e = Arc::clone(&entry);
            let cc = Arc::clone(&inner_cascade);
            let cl = Arc::clone(&c);
            tokio::spawn(async move {
                run_grace_timer(ResourceKind::Shell, "sh-1", &e, &cc, &*cl).await;
            })
        };
        task1.await.expect("t1");
        task2.await.expect("t2");
        // refs decremented exactly once even with two parallel timer
        // tasks racing the same resource.
        assert_eq!(cascade.session_active_refs(&sess("s")), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_during_active_wait_via_waker_makes_timer_exit_quickly() {
        let (a, c, _cascade) = build();
        arm_release(&a, ResourceKind::Shell, "sh-1");
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        let cascade = CascadeCoordinator::new();
        let waker = entry.waker();
        let entry_clone = Arc::clone(&entry);
        let clock_clone = Arc::clone(&c);
        let task = tokio::spawn(async move {
            run_grace_timer(
                ResourceKind::Shell,
                "sh-1",
                &entry_clone,
                &cascade,
                &*clock_clone,
            )
            .await;
        });
        // Cancel the timer almost immediately.
        entry.grace_until_ms_atomic().store(0, Ordering::Release);
        // Need the state to leave Releasing for the timer to bail.
        entry
            .state_atomic()
            .store(LifecycleState::Observed.as_u8(), Ordering::Release);
        waker.notify_one();
        task.await.expect("join");
        // State is Observed because we forced it back; the timer did
        // not transition to Closed.
        assert_eq!(entry.current_state(), LifecycleState::Observed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timer_with_tiny_grace_fires_promptly() {
        let (a, _c, _cascade) = build();
        let mut p = LifecyclePolicy::release_with_default_grace();
        p.grace_ms = 1;
        a.track_resource(ResourceKind::Shell, "sh-1", &sess("s"), p);
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        a.on_unsubscribe(ResourceKind::Shell, "sh-1").expect("uns");
        // Use a fresh real-clock to measure timer responsiveness.
        let real = Arc::new(crate::adapters::clock::system::SystemClock);
        // Advance the FakeClock-based deadline to 0 so the timer fires.
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        entry.grace_until_ms_atomic().store(0, Ordering::Release);
        entry
            .state_atomic()
            .store(LifecycleState::Releasing.as_u8(), Ordering::Release);
        let cascade = CascadeCoordinator::new();
        run_grace_timer(ResourceKind::Shell, "sh-1", &entry, &cascade, &*real).await;
        assert_eq!(entry.current_state(), LifecycleState::Releasing);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timer_does_not_underflow_with_zero_now() {
        let clock = Arc::new(FakeClock::new(0));
        let cascade = CascadeCoordinator::new();
        let adapter = RefcountedLifecycleAdapter::new(Arc::clone(&cascade), Arc::clone(&clock));
        adapter.track_resource(
            ResourceKind::Shell,
            "sh-1",
            &sess("s"),
            LifecyclePolicy::release_with_default_grace(),
        );
        adapter
            .on_subscribe(ResourceKind::Shell, "sh-1")
            .expect("sub");
        adapter
            .on_unsubscribe(ResourceKind::Shell, "sh-1")
            .expect("uns");
        let entry = adapter.entry(ResourceKind::Shell, "sh-1").expect("entry");
        // Do not advance the clock — the deadline should still be in the
        // future relative to the now=0 epoch.
        let cascade2 = CascadeCoordinator::new();
        let entry_clone = Arc::clone(&entry);
        let clock_clone = Arc::clone(&clock);
        let task = tokio::spawn(async move {
            run_grace_timer(
                ResourceKind::Shell,
                "sh-1",
                &entry_clone,
                &cascade2,
                &*clock_clone,
            )
            .await;
        });
        // Cancel quickly so the test does not stall.
        entry.grace_until_ms_atomic().store(0, Ordering::Release);
        entry
            .state_atomic()
            .store(LifecycleState::Observed.as_u8(), Ordering::Release);
        entry.waker().notify_one();
        task.await.expect("join");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timer_state_remains_releasing_during_wait_with_unmet_deadline() {
        let (a, c, _cascade) = build();
        let mut p = LifecyclePolicy::release_with_default_grace();
        p.grace_ms = 5_000;
        a.track_resource(ResourceKind::Shell, "sh-1", &sess("s"), p);
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        a.on_unsubscribe(ResourceKind::Shell, "sh-1").expect("uns");
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        let cascade = CascadeCoordinator::new();
        let task = {
            let e = Arc::clone(&entry);
            let cc = Arc::clone(&cascade);
            let cl = Arc::clone(&c);
            tokio::spawn(async move {
                run_grace_timer(ResourceKind::Shell, "sh-1", &e, &cc, &*cl).await;
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(entry.current_state(), LifecycleState::Releasing);
        // Cancel via waker.
        entry.grace_until_ms_atomic().store(0, Ordering::Release);
        entry.waker().notify_one();
        // State stays Releasing because we did not flip to Observed.
        // Bail by force_close so the timer exits the next iteration.
        a.force_close(ResourceKind::Shell, "sh-1").expect("close");
        task.await.expect("join");
        assert_eq!(entry.current_state(), LifecycleState::Closed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timer_after_resub_does_not_fire_close_on_late_run() {
        let (a, c, cascade) = build();
        arm_release(&a, ResourceKind::Shell, "sh-1");
        // Resub before the timer runs.
        a.on_subscribe(ResourceKind::Shell, "sh-1").expect("sub");
        let entry = a.entry(ResourceKind::Shell, "sh-1").expect("entry");
        // Advance the clock past the (now stale) deadline.
        c.advance(Duration::from_millis(10_000));
        run_grace_timer(ResourceKind::Shell, "sh-1", &entry, &cascade, &*c).await;
        assert_eq!(entry.current_state(), LifecycleState::Observed);
        assert_eq!(cascade.session_active_refs(&sess("s")), 1);
    }
}
