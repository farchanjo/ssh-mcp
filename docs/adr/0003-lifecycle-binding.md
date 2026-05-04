# ADR 0003: Lifecycle Binding for Subscribe-First Resources

## Status

Proposed (v5.0.0). Implementation tracked under Phase 1 of the v5 roadmap.

## Context

ssh-mcp v4 keeps two distinct conceptual surfaces strictly decoupled:

- **Subscription** (observability): a peer registers via `resources/subscribe`; the `MemoryRegistry` debouncer fans push notifications out over the rmcp transport.
- **Resource ownership** (remote state): a shell, an async command, or an in-flight SFTP transfer is a real artefact on the remote host. It is created by `ssh_shell_open`, `ssh_execute`, or `ssh_upload`/`ssh_download`, and torn down by an explicit `ssh_shell_close`, `ssh_cancel_command`, or `ssh_disconnect` (which cascades through every owned resource).

These two surfaces never interact at the lifecycle level. A subscriber can attach to a `command://<id>/output` URI, fail to consume the events, drop its rmcp transport, and the underlying remote command will keep running until the parent session is disconnected or the command finishes on its own. In practice this is the dominant leak vector reported by 27B-class LLM hosts that drive the server: the host launches a long shell, opens a subscription, then either dies, forgets the subscription, or polls the resource via a non-push code path. The remote shell zombies until the inactivity sweeper terminates the session — which itself does not fire while a subscriber is still nominally connected.

The v5 roadmap identified four concrete failure modes:

1. **Caller crash with active subscription.** Peer GC sweeps the closed transport (interval 30 s default) and calls `MemoryRegistry::drop_peer`, but the resource (`shell://<id>/output`) stays alive because nothing else reads ownership state.
2. **Caller forgets to subscribe.** A 27B-class LLM opens a shell, immediately tries `ssh_shell_read` instead of `resources/subscribe`, and the ring buffer fills until the inactivity TTL fires. With no subscriber in the registry, no observer ever notices.
3. **Caller subscribes then unsubscribes politely.** The resource ownership remains intact, but the caller assumed unsubscribe meant "I am done with this resource". The mismatch produces silent leaks at scale.
4. **Cascade misses.** A session with two shells, one of which is closed manually while the other still has subscribers, currently keeps the session alive even though the second shell has no observers and was opened by the same caller.

Three alternatives were on the table:

1. **Make `resources/unsubscribe` an idempotent close**: any unsubscribe immediately tears down the resource. Rejected because it conflates two semantics — pausing observability vs releasing the remote artefact — and breaks legitimate workflows that want to detach a subscriber while another consumer keeps running (multi-tenant fan-out, debugging tools).
2. **Lengthen the inactivity TTL and trust GC**: keep the v4 model and rely on the existing reaper. Rejected because TTL-based reaping is opaque to the caller, makes test orchestration brittle (FakeClock has to advance explicitly), and ignores the cascade case.
3. **Lifecycle binding with explicit policy.** The caller declares at resource creation time whether the resource should auto-clean when no subscribers remain. Selected.

## Decision

Introduce a **lifecycle binding** layer that tracks each long-running resource through an explicit state machine and enforces a per-resource policy. Subscriptions remain observability primitives; ownership is an independent layer with refcount semantics; the two are wired through CAS transitions on a `LifecycleState` enum.

### State machine

```
       subscribe (sub_count++)
     ┌────────────────────────┐
     │                        │
     ▼                        │
┌──────────────┐      ┌───────┴────────┐
│    Owned     │ ───▶ │   Observed     │
│ (sub_count=0)│ first│ (sub_count>=1) │
└──────┬───────┘ sub  └────────┬───────┘
       │                       │
       │ explicit close        │ last unsubscribe
       ▼                       ▼
┌──────────────┐      ┌────────────────┐
│    Closed    │ ◀─── │   Releasing    │
│ (released)   │grace │ (sub_count=0,  │
└──────────────┘timer │  timer armed)  │
       ▲      fires   └────────┬───────┘
       │                       │
       │                       │ new subscribe
       └───────────────────────┘  within grace
                                  ─▶ Observed
                                  (cancel timer)
```

Invariants:

- `Owned`: the caller created the resource without subscribing yet. Subscriber count is zero. Lives forever (until manual close) when `release_when_no_subs = false`. Lives for at most `LIFECYCLE_OWN_GRACE_MS` when `release_when_no_subs = true` (default policy choice — see §Defaults below).
- `Observed`: at least one subscriber. Resource is healthy.
- `Releasing`: subscriber count just hit zero AND the policy requested auto-release. A `tokio::time::sleep_until` task is armed for `grace_ms` milliseconds. New subscribes during this window cancel the timer (CAS `Releasing -> Observed`).
- `Closed`: the resource has been released. Cascade decrements the parent session refcount; further subscribe attempts return `RESOURCE_GONE`.

### Lock-free implementation contract

Per-resource state lives behind a single `Arc<ResourceLifecycle>`. Internally:

| Field | Type | Why this primitive |
|---|---|---|
| `state` | `AtomicU8` (encoded `LifecycleState`) | CAS transitions; `compare_exchange` rejects invalid edges |
| `sub_count` | `AtomicUsize` | `fetch_add` / `fetch_sub` with `AcqRel` |
| `grace_until_ms` | `AtomicU64` | epoch-ms deadline; 0 when not armed |
| `policy` | `ArcSwap<LifecyclePolicy>` | hot-reload of grace duration / cascade flag without locking readers |
| `waker` | `Arc<tokio::sync::Notify>` | wakes the grace timer task when policy or deadline changes |
| `session_id` | `SessionId` | parent for cascade refcount |

The hot path takes zero `Mutex`. The `await_holding_lock`, `significant_drop_in_scrutinee`, and `mutex_atomic` clippy denials in `Cargo.toml [lints.clippy]` continue to enforce this. The grace timer task owns a `JoinHandle` that the adapter aborts on `Closed`.

### Cascade through `SessionLifecycle`

Each session carries an analogous refcount aggregator:

| Field | Type |
|---|---|
| `active_refs` | `AtomicUsize` (shells + commands + transfers + manual `pin`) |
| `idle_until_ms` | `AtomicU64` |
| `state` | `AtomicU8` (`Active | Idle | Releasing | Closed`) |
| `policy` | `ArcSwap<SessionPolicy { persistent: bool, idle_grace_ms: u32 }>` |

`release_resource` is the single chokepoint:

1. CAS `state: Releasing -> Closed`.
2. Drop the owner handle (delegates to existing `Disconnect*UseCase` for shell/command/transfer).
3. `session.active_refs.fetch_sub(1, AcqRel)`.
4. If `active_refs == 0` AND `!session.policy.persistent` AND not already `Releasing`, arm the session-level grace timer.

This wires cleanly into the existing `SessionReaper` task: the reaper now consults `SessionLifecycle.active_refs` before deciding to disconnect, which means a session with active resources will never be reaped even if its inactivity TTL fires (correct behaviour — refcount supersedes TTL).

### Defaults

Policy defaults preserve v4 semantics so the upgrade path is opt-in:

```
LifecyclePolicy {
    release_when_no_subs: false,       // v4-compat: callers must close explicitly
    grace_ms: 2_000,                    // 2 s grace window when opted in
    cascade_session: true,              // when policy is opted in, cascade is the right thing
}

SessionPolicy {
    persistent: false,                  // honours the existing ssh_connect persistent flag
    idle_grace_ms: 5_000,               // 5 s before the session-level reaper fires
}
```

Every `ssh_shell_open`, `ssh_execute`, `ssh_upload`, `ssh_download` call accepts a new optional parameter `release_when_no_subs: Option<bool>` (Phase 3). Phase 1 lands the layer with the v4-compat default; Phase 3 surfaces the parameter into the MCP tool schema.

### Error model

New `DomainError` variants:

- `ResourceGone(String)` — subscribe attempt against `Closed`.
- `LifecycleStateConflict { current: LifecycleState, attempted: &'static str }` — a CAS transition the state machine refuses (defensive; should never occur in correct code but surfaces bugs early).
- `SessionRefcountUnderflow(SessionId)` — cascade decrement that would drive refcount past zero. Bug-detect.

These map onto MCP wire codes `RESOURCE_GONE`, `INTERNAL_ERROR`, `INTERNAL_ERROR` respectively (see ADR 0007 — error taxonomy).

## Consequences

### Positive

- **Self-cleaning resources.** Callers that opt into `release_when_no_subs = true` no longer leak shells/commands/transfers when their MCP transport drops. Peer GC + grace timer + cascade close the loop without operator action.
- **Cascade without tied lifecycle.** A session with 5 shells, where 3 are observed and 2 are not, no longer prevents the unobserved 2 from being released — but it also does not force-disconnect the parent session as long as any observed resource is alive.
- **Lock-free hot path preserved.** `state` / `sub_count` / `grace_until_ms` are atomics; `policy` is `ArcSwap`; the waker is `Notify`. `cargo clippy --release --all-features -- -D warnings` exit-0 invariant holds.
- **Loom-verifiable.** Phase 1 ships four new loom tests covering subscribe/unsubscribe race, grace fire vs re-subscribe, cascade double-disconnect, and cursor monotonicity.

### Negative

- **One additional indirection per subscribe / unsubscribe.** `MemoryRegistry::subscribe` now calls `lifecycle_policy.on_subscribe` after registering the peer. The benchmark target is < 1 µs added on the hot path; verified by Phase 5 micro-benchmarks.
- **State-machine surface area to test.** The 4-state machine plus cascade adds ~16 race scenarios that need property + loom coverage. Phase 1 commits ≥4 loom invariants and ≥120 unit tests; Phase 5 raises the bar with property tests.
- **Two grace timers per resource potentially.** The resource grace timer and the session grace timer are independent. A resource that releases triggers a session-level decrement which may itself arm a session timer. Both have explicit cancel paths and bounded TTLs (default 2 s + 5 s); the worst case is two short tasks running concurrently per session.

### Neutral

- **No public MCP wire change in Phase 1.** The new policy parameter on `ssh_shell_open` / `ssh_execute` / `ssh_upload` / `ssh_download` lands in Phase 3 alongside the LLM UX overhaul. Phase 1 wires the layer underneath with the v4-compat default, so existing MCP hosts see no behaviour change.
- **Existing `SessionReaper` keeps working.** Refcount supersedes TTL: a session with `active_refs > 0` is never reaped by the inactivity sweeper. A session that drops to zero refs falls through to the existing TTL path if `idle_grace_ms` expires before any new resource is opened.

## References

- [Phase 1 Lifecycle Binding implementation plan](../arch/v5-phase1.md) (forthcoming)
- [ADR 0004 — Channel Mux Fairness](./0004-channel-mux-fairness.md) — depends on lifecycle binding
- [ADR 0005 — LLM UX Priorities](./0005-llm-ux-priorities.md) — surfaces the `release_when_no_subs` flag
- [docs/LOCKS.md](../LOCKS.md) — lock-free invariants this ADR preserves
- [docs/RESOURCES.md](../RESOURCES.md) — resource scheme contract
