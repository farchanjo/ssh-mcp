# Development Reference

Contributor-facing reference for ssh-mcp. Build / test / lint gates, lock-free invariants enforced by Clippy, hot-path sequence diagrams for the data-path internals, the test layering strategy, and the Cargo feature flags.

Cross references:

- [ARCHITECTURE.md](./ARCHITECTURE.md) — module map and layer rules.
- [OPERATIONS.md](./OPERATIONS.md) — operator-facing runbook.
- [adr/0002-adopt-hexagonal-architecture.md](./adr/0002-adopt-hexagonal-architecture.md) — design rationale.

## Build gates

Four gates must stay green on every commit:

```bash
cargo build --release                              # All binaries (default + port_forward)
cargo build --release --bin ssh-mcp                # HTTP server (axum 0.8 + rmcp 1.6)
cargo build --release --bin ssh-mcp-stdio          # Stdio MCP transport
cargo build --release --bin ssh-mcp-tail           # NDJSON daemon
cargo build --release --no-default-features        # No port forwarding

cargo test --lib --quiet                                   # ~1.9k+ lib tests (1979 on v7.0 master)
cargo test --tests --features test-fixtures --quiet        # 134 integration tests across 9 binaries (v4_smoke 2, v5_smoke 8, v5_daemon_smoke 5, v6_resume_smoke 12, v7_rsync_smoke 9, chaos 41, chaos_rsync 16, property 32, property_rsync 9)
cargo test --features test-fixtures                # Use cases vs deterministic adapters

cargo fmt --all -- --check
cargo clippy --release --all-features -- -D warnings   # Strict lint gate (production-only)
```

### Clippy gate is production-only

The canonical command `cargo clippy --release --all-features -- -D warnings` must always exit 0. Test targets are intentionally excluded — `forbid(clippy::unwrap_used)` / `forbid(clippy::expect_used)` is structurally incompatible with the `#[tokio::test]` macro expansion (the macro injects its own `#[allow(...)]` group, which `forbid` rejects via E0453).

Production code stays under the full strict baseline; test code is gated by `cargo test --lib` (must keep green) plus `cargo build --release --all-targets` (must stay warning-free). New `unwrap()` / `expect()` outside test modules still fails the production clippy gate.

### MSRV

Rust **1.95** (Rust 2024 edition baseline + AFIT + APIs stabilised through 1.95).

### Clippy configuration

Strict enforcement via `Cargo.toml` `[lints.clippy]`:

- **Lint groups**: `clippy::all`, `clippy::pedantic`, `clippy::nursery`, `clippy::cargo` at `deny`.
- **Layer A (forbid)**: `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro`, `exit`, `mem_forget`, `infinite_loop`, `print_stdout`, `print_stderr`.
- **Lock-free invariants** (deny): `await_holding_lock`, `await_holding_refcell_ref`, `significant_drop_in_scrutinee`, `significant_drop_tightening`, `mutex_atomic`, `mutex_integer`. Every hot-path state type carries **zero** `Mutex` fields.
- **Quality denies**: `wildcard_enum_match_arm`, `as_conversions`, `clone_on_ref_ptr`, `implicit_clone`, `ref_patterns`, `absolute_paths`, `pub_use`, `allow_attributes_without_reason`, `format_push_string`, `if_then_some_else_none`, `rc_mutex`, `redundant_type_annotations`, `same_name_method`, `tests_outside_test_module`, etc.
- **Thresholds** (`clippy.toml`): `cognitive-complexity-threshold = 25`, `too-many-lines-threshold = 30`, `too-many-arguments-threshold = 7`, `type-complexity-threshold = 250`.
- **Allowed**: `multiple_crate_versions` (transitive deps from russh / axum).

All `#[allow(...)]` attributes **must** include a `reason = "..."`. Never disable a lint to silence a warning — fix the code instead.

### General coding conventions

- Methods < 30 lines, SOLID principles.
- Lock-free everywhere on the hot path: `DashMap`, `ArcSwap`, `OnceCell`, `Atomic*`, `tokio::sync::broadcast`, `tokio::sync::Notify`, `mpsc` for owned-resource serialization.
- Use cases stay generic over their ports — **no `Box<dyn Trait>` in hot paths**. Async ports use `trait-variant` AFIT; the dyn-safe slices (`LaneAdmin`) live alongside the async slice for cold-path operations.
- Match exhaustively (no `_ =>` for closed enums; `wildcard_enum_match_arm = "deny"`).
- `Arc::clone(&x)` — never `x.clone()` on an `Arc` (`clone_on_ref_ptr = "deny"`).

## Test layers

| Layer | Path | What it covers | Cost |
|---|---|---|---|
| Lib unit tests | `src/**/{mod,*}.rs::tests` | ~1.9k+ tests (1979 on v7.0 master) across domain, application, adapters, infra. Use cases run against in-memory fakes when feasible. | seconds |
| `test-fixtures` | gated by `cargo test --features test-fixtures` | Use cases against deterministic adapters (`FakeClock`, `DeterministicIdGen`) for reproducible bug bisection. | seconds |
| Integration tests | `tests/*.rs` | 134 active tests across 9 binaries: `v4_smoke` (2, wire-compat snapshot), `v5_smoke` (8), `v5_daemon_smoke` (5, against `ssh-mcp-tail daemon`), `v6_resume_smoke` (12, ADR 0010 SFTP resume), `v7_rsync_smoke` (9, ADR 0011 rsync hybrid), `chaos` (41, ADR-driven failure-mode coverage), `chaos_rsync` (16, ADR 0011 chaos), `property` (32, lifecycle / mux / lane invariants), `property_rsync` (9, ADR 0011 property). Most need `--features test-fixtures`. e2e VM tests (`v7_rsync_e2e_vm` 2 + `v7_rsync_wire_e2e_vm` 6) are gated `--features e2e-vm`. | tens of seconds |
| Loom invariants | `tests/lockfree_invariants.rs` + `tests/lockfree_invariants_rsync.rs` (both `#[cfg(loom)]`) | 27 interleavings total: 20 in `lockfree_invariants` (v4 baseline + Phase 1 lifecycle + Phase 2 mux) + 7 in `lockfree_invariants_rsync` (RsyncSession state, rsync lane pause/resume, file-list ordering, sparse-file handling). Compiles to empty when loom not enabled. | minutes (full mode currently blocked by upstream) |
| Python integration | `scripts/test_*.py` | `requires_sshd` end-to-end suites against a real OpenSSH server. | minutes |
| Stress | `scripts/stress_*.py` | 5 stress scripts (concurrent writes, lagged sub, locks, multi-host, subscribe). | varies |

### Loom invariants (20 total)

`tests/lockfree_invariants.rs` is gated behind `#[cfg(loom)]`. To run:

```bash
RUSTFLAGS="--cfg loom" cargo test --test lockfree_invariants --release
```

The tests permute concurrent interleavings on:

- **v4 baseline (8 invariants)**: `ringhistory_reader_observes_consistent_snapshot`, `ringhistory_two_writers_reader_atomicity`, `oncemodel_only_first_set_wins`, `oncemodel_reader_sees_stable_value_after_first_observation`, `cursor_fetch_max_with_compensate_truncation`, `cursor_double_compensation_saturates_at_zero`, `slow_subscriber_recovers_after_lag`, `sequence_allocation_no_duplicates`.
- **Phase 1 lifecycle (4 new)**: `loom_lifecycle_concurrent_subscribe_unsubscribe`, `loom_grace_fire_vs_resubscribe`, `loom_cascade_double_disconnect`, `loom_cursor_atomic_advance`.
- **Phase 2 mux (4 new)**: `loom_mux_round_robin_no_starvation`, `loom_lane_mpsc_drop_oldest_monotonic`, `loom_lane_pause_resume_no_loss`, `loom_subid_cursor_atomic_advance`.
- **Phase 2/3/4 extensions (4 new)**: `loom_phase2_replay_during_concurrent_subscribe`, `loom_phase3_leak_watcher_no_double_alert`, `loom_phase3_release_when_no_subs_grace`, `loom_phase4_embed_transport_shutdown_race`.
- **Phase 5 rsync (7 new, `tests/lockfree_invariants_rsync.rs`)**: `RsyncSession` state CAS transitions, rsync lane pause/resume under concurrent producer pressure, file-list ordering, sparse-hole detection race, terminal-event observation order, cancel vs natural-completion race, capability-probe cache write/read race.

Total currently shipped: **27 `#[test]` annotations** across two files — `tests/lockfree_invariants.rs` (20 = 8 + 4 + 4 + 4) + `tests/lockfree_invariants_rsync.rs` (7, v7.0).

Full loom mode is currently blocked by upstream tokio/loom incompatibility in russh + axum (documented in the test file and `Cargo.toml`).

## Cargo features

| Feature | Default | Effect |
|---|---|---|
| `port_forward` | on | Includes the `forward://` resource scheme and the `ssh_forward` tool surface (catalog grows from 35 to 36 tools). |
| `test-fixtures` | off | Wires deterministic adapters (`FakeClock`, `DeterministicIdGen`). For tests; never enable in production builds. |

---

## Lock-free invariants

ssh-mcp v5 preserves the v3 / v4 lock-free baseline: every shared producer / consumer path uses `Arc<ArcSwap<T>>`, atomics, broadcast / mpsc channels, and `OnceCell` instead of `Mutex` / `RwLock`. v5 stacks three new categories of lock-free state on top: lifecycle adapter atomics ([ADR 0003](./adr/0003-lifecycle-binding.md)), subscription mux atomics ([ADR 0004](./adr/0004-channel-mux-fairness.md)), and cascade refcount atomics ([ADR 0003](./adr/0003-lifecycle-binding.md)) — all preserve the strict baseline lints (`await_holding_lock`, `mutex_atomic`, `mutex_integer`, `significant_drop_in_scrutinee`).

> **v5.0 / v7.0 — four categories of lock-free state, all merged.** Phase 1 landed `ResourceLifecycle` + `SessionLifecycle` atomics; Phase 2 landed per-`SubId` `MultiplexLane` mpsc + `ChannelMux` round-robin cursor; cascade refcount sits on `SessionLifecycle.active_refs`; Phase 5 (v7.0 — ADR 0011) added `RsyncSession` aggregate atomics (status `AtomicU8` + counters `AtomicU64`) + per-rsync `mpsc::Sender` for the wire driver task. All Mutex-free; all loom-verifiable.

### Adapter-internal carriers

H17.5a deleted the v3 monolith. v4.1 H17.6 finished the foundational decouple: lock-free state carriers (`RunningCommand`, `RunningShell`, `RunningTransfer`) and the global `SUBSCRIPTION_REGISTRY` were relocated under the owning adapters. v5 adds three new carriers (`ResourceLifecycle`, `SessionLifecycle`, `MultiplexLane` + `ChannelMux`) under their own adapter subtrees.

| Carrier | Owner module | Consumer / surface |
|---------|---------------------|--------------------|
| `RunningCommand` | `src/adapters/ssh/internal/async_command.rs` | consumed by `src/adapters/ssh/russh_adapter.rs`, snapshot-read via `src/adapters/output_stream/russh_output.rs` |
| `RunningShell` | `src/adapters/ssh/internal/shell.rs` | consumed by `src/adapters/ssh/russh_adapter.rs` |
| `RunningTransfer` | `src/adapters/sftp/internal/transfer.rs` | consumed by `src/adapters/sftp/russh_sftp_adapter.rs` |
| `SUBSCRIPTION_REGISTRY` (global + `spawn_peer_gc` task) | `src/adapters/subscription/legacy.rs` (transitional) | v5 use cases consume `MemoryRegistry<N>` at `src/adapters/subscription/memory_registry.rs`. The legacy adapter coexists until SSH/SFTP runtime adapters are wired through the port surface end-to-end. |
| `SessionRef` (russh handle + health channel) | `src/adapters/ssh/internal/session.rs` + `src/adapters/ssh/internal/types.rs` | consumed by `src/adapters/ssh/russh_adapter.rs` |
| `ForwardHandle` (feature-gated) | `src/adapters/ssh/internal/types.rs` | consumed by the forward use case via `src/adapters/repo/dashmap/forward.rs` |
| **`ResourceLifecycle`** (v5 — Phase 1) | `src/adapters/lifecycle/refcount.rs` | consumed by every long-running use case via `LifecyclePolicyPort` |
| **`SessionLifecycle`** (v5 — Phase 1) | `src/adapters/lifecycle/cascade.rs` | aggregator for cascade — consulted by the session reaper before TTL eviction |
| **`MultiplexLane`** (v5 — Phase 2) | `src/adapters/subscription/subscriber_lane.rs` | one per `SubId`; consumed by the per-lane drain task and `ChannelMux` |
| **`ChannelMux`** (v5 — Phase 2) | `src/adapters/subscription/channel_mux.rs` | round-robin fan-in across all lanes; feeds the outbound writer (rmcp Peer or NDJSON formatter) |

The repository adapters (`src/adapters/repo/dashmap/{session,command,shell,transfer,forward}.rs`) and the subscription registry (`src/adapters/subscription/memory_registry.rs`) remain the port surface for use cases.

### Primitives map

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart LR
    subgraph LIFE["lifecycle adapter"]
        RL["ResourceLifecycle"]
        SL["SessionLifecycle"]
    end
    subgraph MUX["subscription mux"]
        ML["MultiplexLane"]
        CM["ChannelMux"]
    end
    subgraph CASCADE["cascade refcount"]
        AR["active_refs"]
    end
    subgraph RING["ring buffer"]
        RB["RingBuffer"]
    end

    A1["AtomicU8 state"]
    A2["AtomicUsize sub_count"]
    A3["AtomicU64 grace_until_ms"]
    A4["ArcSwap LifecyclePolicy"]
    A5["Notify waker"]

    A6["AtomicU64 byte_cursor"]
    A7["mpsc Sender / Receiver"]
    A8["ArcSwap FilterRule"]
    A9["AtomicBool pause_flag"]
    A10["8 stats atomics"]

    A11["DashMap lanes"]
    A12["AtomicUsize cursor_lane"]
    A13["Notify mux_waker"]
    A14["mpsc mux_tx"]

    A15["AtomicUsize active_refs"]
    A16["AtomicU64 idle_until_ms"]
    A17["ArcSwap SessionPolicy"]

    A18["ArcSwap RingBuffer"]
    A19["broadcast Sender"]

    RL --> A1
    RL --> A2
    RL --> A3
    RL --> A4
    RL --> A5
    SL --> A15
    SL --> A16
    SL --> A17

    ML --> A6
    ML --> A7
    ML --> A8
    ML --> A9
    ML --> A10
    CM --> A11
    CM --> A12
    CM --> A13
    CM --> A14

    AR --> A15
    RB --> A18
    RB --> A19

    style LIFE fill:#161b22,color:#f0f6fc,stroke:#30363d
    style MUX fill:#161b22,color:#f0f6fc,stroke:#30363d
    style CASCADE fill:#161b22,color:#f0f6fc,stroke:#30363d
    style RING fill:#161b22,color:#f0f6fc,stroke:#30363d
    style RL fill:#238636,color:#f0f6fc,stroke:#2ea043
    style SL fill:#238636,color:#f0f6fc,stroke:#2ea043
    style ML fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style CM fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style AR fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style RB fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
```

### Lints enforced

The following clippy lints are denied at the workspace level (`Cargo.toml` `[lints.clippy]`):

```toml
await_holding_lock              = "deny"
await_holding_refcell_ref       = "deny"
significant_drop_in_scrutinee   = "deny"
significant_drop_tightening     = "deny"
mutex_atomic                    = "deny"
mutex_integer                   = "deny"
```

Together they reject:

- Holding a `MutexGuard` / `RwLockReadGuard` / `RefMut` across a `.await` point.
- Constructing `Mutex<AtomicXxx>` or `Mutex<integer>` (use the atomic directly).
- Letting a `DashMap` shard guard live longer than necessary inside a scrutinee scope.

Combined with the standard `unwrap_used` / `panic` forbids, this guarantees no async path in production can deadlock on a contended sync primitive. **The v5 adapters preserve every invariant**: `ResourceLifecycle`, `SessionLifecycle`, `MultiplexLane`, and `ChannelMux` carry **zero** `Mutex` fields.

### Lifecycle adapter invariants (Phase 1)

Per-resource state lives behind `Arc<ResourceLifecycle>` at `src/adapters/lifecycle/refcount.rs`. The grace timer task lives in `src/adapters/lifecycle/grace_timer.rs`.

#### Per-resource fields

| Field | Type | Memory ordering |
|---|---|---|
| `state` | `AtomicU8` (encoded `LifecycleState { Owned=0, Observed=1, Releasing=2, Closed=3 }`) | CAS via `compare_exchange(Acquire, Acquire)`; writers use `AcqRel` for `swap`; readers use `Acquire`. |
| `sub_count` | `AtomicUsize` | `fetch_add(AcqRel)` on subscribe; `fetch_sub(AcqRel)` on unsubscribe; reads use `Acquire`. |
| `grace_until_ms` | `AtomicU64` (epoch ms; 0 when not armed) | `Relaxed` reads (deadline check); `Release` writes (when arming the timer). |
| `policy` | `ArcSwap<LifecyclePolicy>` | hot-reload of `grace_ms` / `cascade_session` flag without locking readers. `load_full()` returns an `Arc` snapshot. |
| `waker` | `Arc<tokio::sync::Notify>` | `notify_waiters()` on policy change or deadline update. |
| `session_id` | `SessionId` (immutable) | parent ref for cascade (write-once at construction). |

#### CAS state machine

```text
   on_subscribe (sub_count == 0 -> 1)
   ┌─────────────────────────────────┐
   │                                  │
   ▼                                  │
 Owned ───────── on_subscribe ────► Observed
   │                                  │
   │ explicit close                   │ on_unsubscribe (sub_count -> 0)
   │                                  │ AND policy.release_when_no_subs
   ▼                                  ▼
 Closed  ◄─ grace timer fires ──── Releasing  ◄─┐
                                      │          │
                                      └─ on_subscribe (cancel timer)
```

CAS edges are explicit: `compare_exchange(current_state, new_state, Acquire, Acquire)`. Invalid transitions return `LifecycleStateConflict` (a defensive `INTERNAL` class error per [ADR 0007](./adr/0007-error-taxonomy.md) — should never occur in correct code).

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
stateDiagram-v2
    [*] --> Owned: track_resource<br/>(state init = Owned)
    Owned --> Observed: compare_exchange<br/>Acquire / Acquire<br/>(first on_subscribe)
    Observed --> Observed: fetch_add(AcqRel)<br/>fetch_sub(AcqRel)<br/>(sub_count flux)
    Observed --> Releasing: compare_exchange<br/>Acquire / Acquire<br/>(last unsubscribe<br/>+ release_when_no_subs)
    Releasing --> Observed: compare_exchange<br/>Acquire / Acquire<br/>(re-subscribe within grace)
    Releasing --> Closed: compare_exchange<br/>AcqRel / Acquire<br/>(timer fires)
    Owned --> Closed: compare_exchange<br/>AcqRel / Acquire<br/>(explicit close)
    Observed --> Closed: compare_exchange<br/>AcqRel / Acquire<br/>(explicit close)
    Closed --> [*]: cascade<br/>fetch_sub(AcqRel)

    classDef owned fill:#21262d,color:#8b949e,stroke:#30363d
    classDef observed fill:#238636,color:#f0f6fc,stroke:#2ea043
    classDef releasing fill:#9e6a03,color:#f0f6fc,stroke:#bf8700
    classDef closed fill:#cf222e,color:#f0f6fc,stroke:#f85149

    class Owned owned
    class Observed observed
    class Releasing releasing
    class Closed closed
```

#### Why `await_holding_lock = "deny"` still holds

The hot path takes zero `Mutex`. The grace timer task uses `tokio::time::sleep_until(deadline)` and `.await`s on the `Arc<Notify>` waker — neither is a sync lock. The `compare_exchange` calls are non-blocking. `ArcSwap::load_full()` returns a snapshot `Arc` and drops the swap-side guard before any `.await`. The lint continues to enforce that no future code introduces a sync `Mutex<LifecycleState>` shortcut.

#### Loom invariants (Phase 1 — 4 new interleavings)

`tests/lockfree_invariants.rs`:

1. **Subscribe / unsubscribe race** — concurrent `on_subscribe` + `on_unsubscribe` never observe `sub_count` going negative; the state machine settles deterministically.
2. **Grace fire vs re-subscribe** — a subscribe arriving *during* `Releasing -> Closed` either cancels the timer (success path) or returns `RESOURCE_GONE` (terminal path); never an invalid intermediate state.
3. **Cascade double-disconnect** — concurrent close + cascade decrement never drives `active_refs` past zero; protected by `SessionRefcountUnderflow` defensive error.
4. **Cursor monotonicity** — under contention, no observed cursor decreases. Tested across the full lifecycle CAS chain.

### Subscription mux invariants (Phase 2)

Per-lane state lives behind `Arc<MultiplexLane>` at `src/adapters/subscription/subscriber_lane.rs`. The drain orchestrator lives at `src/adapters/subscription/channel_mux.rs`.

#### Per-lane fields

| Field | Type | Why this primitive |
|---|---|---|
| `byte_cursor` | `Arc<AtomicU64>` | per-`SubId` cursor; independent from peer cursor; `fetch_max` on advance preserves monotonicity. |
| `tx` | `tokio::sync::mpsc::Sender<SubscriptionMessage>` | bounded, single-producer (debouncer), single-consumer (lane task). Capacity = `SSH_LANE_BUFFER` (default 1024). |
| `policy` | `LagPolicy` (enum, copy-Cell) | per-lane backpressure choice. |
| `filter` | `ArcSwap<FilterRule>` | hot-reloadable regex / level. `load_full()` returns a snapshot before any `.await`. |
| `lifecycle` | `Arc<ResourceLifecycle>` | back-link to the owning resource (Phase 1). |
| `stats` | `SubscriberStats` (8 atomics) | `events_sent: AtomicU64`, `bytes_sent: AtomicU64`, `lagged_drops: AtomicU64`, `lagged_recoveries: AtomicU64`, `queue_depth: AtomicUsize`, `queue_high_watermark: AtomicUsize`, `block_total_ms: AtomicU64`. All `.load(Relaxed)` reads; `.fetch_add(Relaxed)` writes. `fetch_max(Relaxed)` for high watermark. |
| `pause_flag` | `AtomicBool` | `Acquire` reads on the drain side; `Release` writes from `sub_pause` / `sub_resume`. |

#### ChannelMux fairness

The `ChannelMux` adapter owns a `DashMap<SubId, MultiplexLane>` plus the round-robin cursor:

| Field | Type | Memory ordering |
|---|---|---|
| `lanes` | `DashMap<SubId, Arc<MultiplexLane>>` | shard-locked briefly during `entry` / `remove`; never held across `.await`. |
| `cursor_lane` | `AtomicUsize` | `Acquire` load to start the round-robin; `Release` store after a successful drain. Wraps via `(idx + 1) % lanes.len()`. |
| `mux_waker` | `Arc<tokio::sync::Notify>` | `notify_one()` on producer enqueue; drain task `.await`s when no lane has work. |
| `mux_tx` | `mpsc::Sender<OutboundEvent>` | global outbound channel feeding the rmcp peer or NDJSON formatter. Capacity = `SSH_MUX_BUFFER` (default 8192). |

Drain loop (no spinning, no Mutex):

1. Snapshot active lanes via `dashmap::iter()` (per-shard guards held briefly).
2. If empty, `mux_waker.notified().await`.
3. From `cursor_lane.load(Acquire)`, iterate lanes in order; on each `try_recv`, first non-empty wins.
4. Forward via `mux_tx.send(...).await` (yields back to dispatcher when full — see [ADR 0006](./adr/0006-backpressure-policies.md) §Mux mpsc handling).
5. `cursor_lane.store((idx + 1) % len, Release)`.

Fairness invariant: between two adjacent backlogged lanes A and B, `cursor_lane` advances after every successful drain, so the mux drains them in alternation. A lane producing 10x faster will not starve a slower one.

#### Loom invariants (Phase 2 — 4 new interleavings)

1. **Mux fairness** — under simultaneous backpressure on two lanes, the round-robin cursor visits each lane within bounded steps.
2. **Lane mpsc full + drop_oldest** — concurrent `try_send` (drop_oldest) + drain never violates monotonic seq numbers; the dropped event is reflected in `lagged_drops`.
3. **Concurrent lane add/remove during drain** — a `lane_close` racing with `try_recv` never produces a stale read or a use-after-free; DashMap shard guards are scoped tightly.
4. **Cursor advance under contention** — `byte_cursor.fetch_max` under concurrent advances always settles to the supremum.

#### Why `await_holding_lock = "deny"` still holds

The drain loop never holds a DashMap shard guard across `.await`. The pause/resume path does a `pause_flag.swap(true, AcqRel)` and yields without entering any sync section. The filter pipeline reads the filter via `ArcSwap::load_full()` and drops the swap-side guard before applying the regex (which can cost CPU but has no `.await`). All `mpsc::Sender::send().await` and `mpsc::Receiver::recv().await` calls are async-native — they yield to the runtime, never block a thread.

### Cascade refcount (Phase 1)

The session-level aggregator lives at `src/adapters/lifecycle/cascade.rs`.

#### `SessionLifecycle` fields

| Field | Type | Memory ordering |
|---|---|---|
| `active_refs` | `AtomicUsize` | `fetch_add(AcqRel)` on resource open (shells, commands, transfers, manual `pin`); `fetch_sub(AcqRel)` on resource close. Underflow defensively trapped via `compare_exchange` (returns `SESSION_REFCOUNT_UNDERFLOW`). |
| `idle_until_ms` | `AtomicU64` | `Release` write when `active_refs` drops to 0; `Relaxed` read by the reaper. |
| `state` | `AtomicU8` (`Active=0, Idle=1, Releasing=2, Closed=3`) | CAS edges: `Active -> Idle` on `active_refs == 0`, `Idle -> Releasing` on `idle_grace_ms` elapsed, `Releasing -> Closed` after disconnect. |
| `policy` | `ArcSwap<SessionPolicy { persistent: bool, idle_grace_ms: u32 }>` | persistence flag honours the `ssh_connect persistent` argument. |

#### `release_resource` chokepoint

A single function in `src/adapters/lifecycle/cascade.rs` enforces the cascade order:

1. CAS resource `state: Releasing -> Closed` (rejects if already Closed).
2. Drop the owner handle (delegates to `DisconnectShellUseCase` / `CancelCommandUseCase` / SFTP cancel / forward stop).
3. `session.active_refs.fetch_sub(1, AcqRel)`.
4. If `active_refs == 0` AND `!session.policy.persistent` AND state was not already `Releasing`, arm the session-level grace timer (`tokio::time::sleep_until` + `Notify` waker — same shape as the resource grace timer).

#### Reaper supersedes TTL

The existing `SessionReaper` task consults `active_refs > 0` before honouring the inactivity TTL:

- `active_refs > 0` -> never reap (correct: refcount supersedes TTL).
- `active_refs == 0` AND `now > idle_until_ms + idle_grace_ms` -> proceed with disconnect.
- `active_refs == 0` AND `now <= idle_until_ms + idle_grace_ms` -> defer; recheck on next reaper tick.

#### Why no `Mutex` is needed

`active_refs` is `AtomicUsize` — never `Mutex<usize>` (`mutex_integer = "deny"`). `idle_until_ms` is `AtomicU64` — same. The `state` CAS chain uses `compare_exchange` for explicit edges. The grace timer is a `tokio::task::JoinHandle` plus a `Notify`, never a `Mutex<bool>` flag. The reaper takes a snapshot of `active_refs.load(Acquire)` and `idle_until_ms.load(Relaxed)` without any guard.

### Patterns by structure

| Structure | Pattern | Notes |
|-----------|---------|-------|
| `RunningShell.history` | `Arc<ArcSwap<RingBuffer>>` + `ArcSwap::rcu` | Reader RCU loop ensures truncation composes with concurrent appends. |
| `RunningShell.output_tx` | `broadcast::Sender<Bytes>` | Lagged auto-recovery via snapshot from `history`. |
| `RunningShell.input_tx` | `mpsc::Sender<WriteRequest>` | Single dedicated writer task owns `ChannelWriter` — no `Mutex`. Capacity = 64 frames. |
| `RunningShell.last_activity_ms` | `Arc<AtomicU64>` (epoch ms) | Replaces the v2 `Mutex<Instant>`. |
| `RunningShell.data_notify` | `Arc<Notify>` | Wakes intra-server long-poll readers. |
| `RunningShell.max_buffer_size` | `Arc<AtomicU64>` | Tunable at runtime without touching the reader task. |
| `RunningCommand.output_history` | `Arc<ArcSwap<OutputBuffer>>` | Same RCU pattern as shell. |
| `RunningCommand.output_tx` | `broadcast::Sender<OutputChunk>` | `OutputChunk = Stdout { seq, data } | Stderr { seq, data } | Closed { seq, exit_code }`. |
| `RunningCommand.exit_code` / `error` | `Arc<OnceCell<…>>` | Write-once. |
| `RunningCommand.timed_out` / `output_read` | `Arc<AtomicBool>` | |
| `RunningTransfer.bytes_transferred` / `total_bytes` | `Arc<AtomicU64>` | Live counters; v4.8.1 wired the progress watcher to mirror them into the repo. |
| `RunningTransfer.error` | `Arc<OnceCell<String>>` | Write-once. |
| `RunningTransfer.progress_tx` | `broadcast::Sender<ProgressEvent>` | `Tick { seq, bytes_transferred, total_bytes } | Completed | Failed | Cancelled`. |
| `RunningTransfer.data_notify` | `Arc<Notify>` | Wakes intra-server long-poll progress readers. |
| `SessionRef.handle` | `Arc<russh::client::Handle<SshClientHandler>>` | Cheap clone. |
| `SessionRef.channel_permits` | `Arc<Semaphore>` (`CHANNEL_CONCURRENCY_PER_SESSION = 1`) | Serialises russh channel openings per session. |
| `SessionRef.health_tx` | `broadcast::Sender<HealthEvent>` | |
| `ForwardHandle.events_tx` (feature-gated) | `broadcast::Sender<ForwardEvent>` | |
| `DashMap*Repo` | `Arc<DashMap<Id, Entity>>` + secondary indexes | Lock-free externally; shard locks held briefly. |
| `MemoryRegistry.subscribers` | `DashMap<uri, Vec<SubscriberHandle>>` | Snapshot-clone-then-drop pattern before any `.await`. |
| `MemoryRegistry.peer_progress` (legacy) / `(SubId, Uri) cursor` (v5) | `DashMap<…, Arc<PeerProgress>>` | v5 adds `(SubId, Uri)` keys alongside the legacy `(PeerId, Uri)` for backwards compat. |
| `MemoryRegistry.sequence_counters` | `DashMap<(kind, id), Arc<AtomicU64>>` | Per-resource monotonic seq. |
| `MemoryRegistry.wakers` | `DashMap<(kind, id), Arc<Notify>>` | Wakes the per-resource debouncer task. |
| `MemoryRegistry.debounce_tasks` | `DashMap<(kind, id), JoinHandle<()>>` | One task per active resource; aborted when last subscriber leaves. |
| **`ResourceLifecycle`** (v5) | `AtomicU8 + AtomicUsize + AtomicU64 + ArcSwap + Notify` | See [Lifecycle adapter invariants](#lifecycle-adapter-invariants-phase-1). |
| **`SessionLifecycle`** (v5) | `AtomicUsize + AtomicU64 + AtomicU8 + ArcSwap` | See [Cascade refcount](#cascade-refcount-phase-1). |
| **`MultiplexLane`** (v5) | `AtomicU64 cursor + mpsc + ArcSwap filter + 8 stats atomics + AtomicBool pause` | See [Subscription mux invariants](#subscription-mux-invariants-phase-2). |
| **`ChannelMux`** (v5) | `DashMap + AtomicUsize cursor + Notify waker + mpsc` | See [Subscription mux invariants](#subscription-mux-invariants-phase-2). |
| `PeerTable` | `Arc<DashMap<PeerId, Arc<rmcp::Peer<RoleServer>>>>` | `RmcpPeerHandle` registers on construction, removes on `Drop`. |

### Acquisition order (residual DashMap shard locks)

DashMap acquires per-shard locks under the hood. They are not awaited across, but they are held briefly during `entry`, `get_mut`, `insert`, `remove`. To stay deadlock-free, any code that touches multiple maps in a single critical section must acquire them in the order below.

```
 1. adapters::repo::dashmap::session   shard
 2. adapters::repo::dashmap::command   shard
 3. adapters::repo::dashmap::shell     shard
 4. adapters::repo::dashmap::transfer  shard
 5. adapters::repo::dashmap::forward   shard (feature-gated)
 6. adapters::subscription::memory_registry::subscribers       shard
 7. adapters::subscription::memory_registry::peer_progress     shard
 8. adapters::subscription::memory_registry::sub_progress      shard (v5 — (SubId, Uri) cursor)
 9. adapters::subscription::memory_registry::sequence_counters shard
10. adapters::subscription::memory_registry::wakers            shard
11. adapters::subscription::memory_registry::debounce_tasks    shard
12. adapters::subscription::channel_mux::lanes                 shard (v5)
13. adapters::lifecycle::refcount::resources                   shard (v5)
14. adapters::lifecycle::cascade::sessions                     shard (v5)
15. adapters::notifier::rmcp_peer::PeerTable                   shard
```

The legacy global `adapters::subscription::legacy::SUBSCRIPTION_REGISTRY` follows the same relative order — its shard layout is identical to `MemoryRegistry`'s, and the SSH/SFTP runtime adapters never hold both registries' shards simultaneously.

#### Rules

- **NEVER hold a higher-numbered shard while acquiring a lower one** in the same task.
- **NEVER `.await` while a `DashMap` ref or guard is alive.** The `await_holding_lock` lint enforces this for `Mutex`/`RwLock` but not `DashMap`; the `significant_drop_in_scrutinee` lint catches the common pattern of `for entry in &map { ... .await ... }`. The codebase preventively snapshots-and-drops.
- **Producers `poke` then drop the waker guard** before doing anything else. The debouncer task then runs entirely outside the registry's locks.
- **The mux drain task** snapshots `lanes` (DashMap iter over a shard) and drops the iter before any `.await` on `mpsc::Sender::send`.
- **Lifecycle CAS** never races a DashMap shard: `ResourceLifecycle` and `SessionLifecycle` are owned by the lifecycle adapter and consumed via the port surface; the DashMap of `(ResourceKey -> Arc<ResourceLifecycle>)` is iterated only by the leak-risk watcher background task ([ADR 0005](./adr/0005-llm-ux-priorities.md)), which uses the snapshot-then-drop pattern.

The current code base touches at most three shards in one critical section (Phase 2 subscribe writes to `subscribers`, `sub_progress`, and `lanes`); the order above leaves headroom.

### Channel sizing and recovery

| Channel | Capacity env | Default | Recovery on `Lagged` / full |
|---------|--------------|---------|----------------------|
| `RunningShell.output_tx` | `SSH_SHELL_BROADCAST_CAP` | 1024 | Subscriber re-reads `history` snapshot via `ArcSwap::load_full`. |
| `RunningCommand.output_tx` | `SSH_COMMAND_BROADCAST_CAP` | 1024 | Same — read from `output_history`. |
| `RunningTransfer.progress_tx` | `SSH_TRANSFER_BROADCAST_CAP` | 256 | Re-read atomic counters (`bytes_transferred`, `total_bytes`) and `status_rx`. |
| `SessionRef.health_tx` | `SSH_SESSION_BROADCAST_CAP` | 256 | Re-read `SessionInfo` via the session repository. |
| `ForwardHandle.events_tx` | `SSH_FORWARD_BROADCAST_CAP` | 256 | Forward storage is not persisted yet — broadcast is best-effort. |
| `RunningShell.input_tx` (mpsc) | hard-coded | 64 | Backpressure on the producer; no Lagged path. |
| **`MultiplexLane.tx`** (v5 mpsc, per `SubId`) | `SSH_LANE_BUFFER` | 1024 | Per-lane `LagPolicy`: `BlockSlow` / `DropOldest` / `DropNewest` / `Snapshot` (default — drop backlog + ring-buffer rebuild). [ADR 0006](./adr/0006-backpressure-policies.md). |
| **`ChannelMux.mux_tx`** (v5 mpsc, global outbound) | `SSH_MUX_BUFFER` | 8192 | `try_send` failure -> lane producer follows its lag policy. |

### Backpressure fronteira flow

Six fronteiras between the russh receiver and the host stdout / socket. Each carries a distinct lag-recovery story.

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart LR
    F0["russh recv<br/>tcp / pty"]
    F1["F1<br/>broadcast Sender<br/>SSH_SHELL_BROADCAST_CAP=1024<br/>lag = snapshot via ArcSwap"]
    F2["F2<br/>per-resource debouncer<br/>200 ms / 1 s flush"]
    F3["F3<br/>per-(SubId, Uri) lane<br/>SSH_LANE_BUFFER=1024<br/>LagPolicy: BlockSlow / DropOldest /<br/>DropNewest / Snapshot"]
    F4["F4<br/>ChannelMux mux_tx<br/>SSH_MUX_BUFFER=8192<br/>round-robin try_recv"]
    F5["F5<br/>rmcp Peer / NDJSON writer<br/>tokio io"]
    F6["F6<br/>stdout / socket"]

    F0 --> F1
    F1 --> F2
    F2 --> F3
    F3 --> F4
    F4 --> F5
    F5 --> F6

    style F0 fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style F1 fill:#238636,color:#f0f6fc,stroke:#2ea043
    style F2 fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style F3 fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    style F4 fill:#238636,color:#f0f6fc,stroke:#2ea043
    style F5 fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    style F6 fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
```

### When you must add a new lock-free state

Decision tree:

1. **Single writer, many readers, immutable snapshots?** -> `Arc<ArcSwap<T>>` + `rcu` for compose-with-others writers.
2. **Write-once terminal value?** -> `Arc<OnceCell<T>>`.
3. **Counter or boolean flag?** -> `AtomicU64` / `AtomicBool` (never `Mutex<u64>` / `Mutex<bool>` — `mutex_integer` / `mutex_atomic` will reject it).
4. **Live event fan-out, single producer?** -> `tokio::sync::broadcast::Sender<Event>` with a per-resource sequence number.
5. **Live event fan-out, per-subscriber isolation?** -> `tokio::sync::mpsc` per `SubId` plus a `ChannelMux` round-robin drainer (Phase 2 pattern).
6. **Wake intra-server pollers?** -> `tokio::sync::Notify`.
7. **Single-consumer queue?** -> `tokio::sync::mpsc` with a dedicated owner task; never wrap a writer in a `Mutex`.
8. **Map-keyed registry?** -> `dashmap::DashMap`, with a `snapshot then drop guard then iterate / await` pattern.
9. **State machine across atomic edges?** -> `AtomicU8` encoding the enum + `compare_exchange(Acquire, Acquire)` for each transition; reject invalid edges defensively (Phase 1 pattern).

If none of the above fits, write a design note in your PR description before reaching for `Mutex` / `RwLock`. Adding a sync lock to an async path is a regression in this code base.

---

## Hot-path sequence diagrams

Sequence diagrams covering the data-path internals — the non-trivial flows that contributors most often need to debug. Operator-facing recovery flows live in [OPERATIONS.md](./OPERATIONS.md#recovery-flows).

### 1. Connect, execute, disconnect (golden path)

The minimal end-to-end flow: open a connection, run one command, tear down.

```mermaid
sequenceDiagram
    participant Client as MCP client
    participant Server as McpSshServer
    participant SSH as russh
    participant Remote as Remote host

    Client->>Server: ssh_connect(address=example.com, username=alice, password=...)
    Server->>SSH: connect_to_ssh_with_retry()
    SSH->>Remote: TCP + SSH handshake
    Remote-->>SSH: authenticated
    SSH-->>Server: handle
    Server->>Server: SessionRepository.insert(SessionEntity)
    Server-->>Client: SSH_CONNECT: OK\nSESSION_ID: a3f2b1d7-...

    Client->>Server: ssh_exec(session_id, command="uname -a")
    Server->>Server: register RunningCommand + spawn task
    Server-->>Client: SSH_EXEC: STARTED\nCOMMAND_ID: 7d4c8e2a-...

    par command runs in background
        Server->>SSH: open_channel + exec
        SSH->>Remote: exec "uname -a"
        Remote-->>SSH: stdout + exit 0
        SSH-->>Server: OutputChunk + exit
        Server->>Server: ArcSwap publish + OnceCell::set(exit_code=0)
    end

    Client->>Server: ssh_exec_output(command_id, wait=true)
    Server->>Server: status_rx watch (Completed)
    Server-->>Client: SSH_EXEC_OUTPUT: COMPLETED\nEXIT: 0\n--- stdout ... ---

    Client->>Server: ssh_disconnect(session_id)
    Server->>Server: cancel commands, close shells, abort transfers
    Server->>SSH: handle.disconnect(ByApplication)
    Server-->>Client: SSH_DISCONNECT: OK
```

### 2. Subscribe-first PTY interactive

The recommended UX for interactive shells: subscribe to push notifications instead of polling.

```mermaid
sequenceDiagram
    participant Client as MCP client
    participant Server as McpSshServer
    participant Reg as SubscriptionRegistry
    participant Reader as PTY reader task
    participant Writer as PTY writer task

    Client->>Server: ssh_connect(...)
    Server-->>Client: SESSION_ID

    Client->>Server: ssh_shell_open(session_id, term=xterm)
    Server->>Reader: spawn (ArcSwap of RingBuffer + broadcast + Notify)
    Server->>Writer: spawn (mpsc::Receiver of WriteRequest)
    Server-->>Client: SSH_SHELL_OPEN: OK\nSHELL_ID: 4b9c8e2a-...\nTERM: xterm 80x24

    Client->>Server: resources/subscribe shell://4b9c8e2a-.../output
    Server->>Server: peer_id = PeerTable.get_or_mint(Mcp-Session-Id or Stdio)
    Server->>Reg: subscribe(Shell, "4b9c8e2a-...", uri, peer_id, peer)
    Reg->>Reg: spawn debouncer (first subscriber)
    Server-->>Client: ()

    Client->>Server: ssh_shell_write(shell_id, "ls -la\n")
    Server->>Writer: WriteRequest::Data(b"ls -la\n")
    Writer->>Reader: bytes flow back
    Reader->>Reader: rcu append + head-trim
    Reader->>Reg: poke(Shell, "4b9c8e2a-...")

    Reg->>Reg: tokio::sleep(50 ms)
    Reg-->>Client: notifications/resources/updated shell://4b9c8e2a-.../output

    Client->>Server: resources/read shell://4b9c8e2a-.../output?cursor=auto
    Server-->>Client: text="$ ls -la\n..." + _meta{kind=shell, cursor=128, buffer_size=128, last_seq=3, status=open}

    Note over Reader,Reg: Subsequent pokes within 50 ms<br/>collapse into one notification.

    Client->>Server: resources/unsubscribe shell://4b9c8e2a-.../output
    Server->>Reg: unsubscribe(peer_id, uri) -> debouncer aborted

    Client->>Server: ssh_shell_close(shell_id)
    Server-->>Client: SSH_SHELL_CLOSE: OK
    Client->>Server: ssh_disconnect(session_id)
```

### 3. wait_for fallback (gated single-shot)

Branch a workflow on the first matching pattern (e.g. `password:`, `Permission denied`, `$ `).

```mermaid
sequenceDiagram
    participant Client as MCP client
    participant Server as McpSshServer
    participant Shell as RunningShell

    Client->>Server: ssh_shell_open(...) -> SHELL_ID

    Client->>Server: ssh_shell_write(shell_id, "ssh root@bastion\n")

    Client->>Server: ssh_shell_wait_for(shell_id, patterns=["password:", "Permission denied", "$ "], timeout_secs=15)

    loop until match / timeout / closed
        Server->>Shell: load_full() snapshot
        Server->>Server: scan_for_first_match(buffer, patterns)
        alt pattern hit
            Server-->>Client: SSH_SHELL_WAIT_FOR: MATCHED\nMATCHED_PATTERN: password:
            Note over Client: Branch on MATCHED_PATTERN
            Client->>Server: ssh_shell_write(shell_id, "secret\n")
        else timeout
            Server-->>Client: SSH_SHELL_WAIT_FOR: TIMEOUT\n--- data ... ---
        else shell closed
            Server-->>Client: SSH_SHELL_WAIT_FOR: CLOSED\n--- data ... ---
        end
    end
```

### 4. send_key Ctrl+C interrupt

Send a semantic keystroke to interrupt a running command without closing the shell.

```mermaid
sequenceDiagram
    participant Client as MCP client
    participant Server as McpSshServer
    participant Shell as RunningShell
    participant Remote as Remote PTY

    Client->>Server: ssh_shell_open(...) returns SHELL_ID
    Client->>Server: ssh_shell_write(shell_id, INFINITE_LOOP_CMD)
    Note over Client,Remote: INFINITE_LOOP_CMD is a busy loop such as `while true do date && sleep 1 done`.

    Note over Remote: shell prints date every second

    Client->>Server: ssh_shell_press(shell_id, key=ctrl_c)
    Server->>Server: ShellKey::CtrlC.encode(empty_mods) returns b"\x03"
    Server->>Shell: input_tx.send(WriteRequest::Data(b"\x03"))
    Shell->>Remote: \x03
    Remote-->>Shell: ^C $
    Server-->>Client: SSH_SHELL_PRESS OK<br/>SHELL_ID: ...<br/>KEY: ctrl_c<br/>BYTES_SENT: 1

    Note over Client: Shell remains open and ready for next command.
```

### 5. Async command with realtime monitoring (subscribe)

Subscribe to `command://<id>/output` to observe stdout/stderr live.

```mermaid
sequenceDiagram
    participant Client as MCP client
    participant Server as McpSshServer
    participant Reg as SubscriptionRegistry
    participant Cmd as RunningCommand

    Client->>Server: ssh_exec(session_id, command="cargo build --release")
    Server->>Cmd: spawn (ArcSwap of OutputBuffer + broadcast + OnceCell)
    Server-->>Client: SSH_EXEC: STARTED\nCOMMAND_ID: 7d4c8e2a-...

    Client->>Server: resources/subscribe command://7d4c8e2a-.../output
    Server->>Reg: subscribe(Command, "7d4c8e2a-...", uri, peer_id, peer)
    Reg->>Reg: spawn debouncer
    Server-->>Client: ()

    loop output chunks
        Cmd->>Cmd: ArcSwap publish + broadcast
        Cmd->>Reg: poke(Command, "7d4c8e2a-...")
        Reg-->>Client: notifications/resources/updated (debounced)
        Client->>Server: resources/read command://7d4c8e2a-.../output?cursor=auto
        Server-->>Client: text + _meta{kind=command, cursor, buffer_size, last_seq, status=running}
    end

    Cmd->>Cmd: OnceCell::set(exit_code=0) and status_rx becomes Completed
    Cmd->>Reg: poke(Command, "7d4c8e2a-...")
    Reg-->>Client: notifications/resources/updated (final)
    Client->>Server: resources/read ...?cursor=auto
    Server-->>Client: text + _meta{kind=command, status=completed, last_seq=N}

    Client->>Server: resources/unsubscribe command://7d4c8e2a-.../output
```

### 6. SFTP upload with progress subscribe

Subscribe to `transfer://<id>/progress` for tick-driven progress updates.

```mermaid
sequenceDiagram
    participant Client as MCP client
    participant Server as McpSshServer
    participant Reg as SubscriptionRegistry
    participant Tr as RunningTransfer
    participant SFTP as russh-sftp

    Client->>Server: ssh_upload(session_id, local_path, remote_path)
    Server->>Tr: spawn (AtomicU64 + broadcast::Sender of ProgressEvent + OnceCell)
    Server-->>Client: SSH_UPLOAD: STARTED\nTRANSFER_ID: 8f7e6d5c-...

    Client->>Server: resources/subscribe transfer://8f7e6d5c-.../progress
    Server->>Reg: subscribe(Transfer, "8f7e6d5c-...", uri, peer_id, peer)
    Reg->>Reg: spawn debouncer (force-flush every 1 s)
    Server-->>Client: ()

    loop 32 KiB chunks
        SFTP->>Tr: write chunk
        Tr->>Tr: bytes_transferred.fetch_add(32 KiB)
        Tr->>Tr: progress_tx.send(ProgressEvent::Tick{seq, bytes, total})
        Tr->>Reg: poke(Transfer, "8f7e6d5c-...")
        Reg-->>Client: notifications/resources/updated (debounced)
        Client->>Server: resources/read transfer://8f7e6d5c-.../progress
        Server-->>Client: JSON body + _meta{kind=transfer, last_seq, status=running}
    end

    Tr->>Tr: progress_tx.send(ProgressEvent::Completed{seq, bytes})
    Tr->>Reg: poke(Transfer, ...)
    Reg-->>Client: notifications/resources/updated
    Client->>Server: resources/read ...
    Server-->>Client: JSON body + _meta{kind=transfer, last_seq=N, status=completed}

    Client->>Server: resources/unsubscribe transfer://8f7e6d5c-.../progress
```

### 7. Lifecycle CAS chain (Phase 1)

The state machine and cascade refcount as observed end-to-end.

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
sequenceDiagram
    participant UC as Use case
    participant RL as ResourceLifecycle
    participant GT as Grace timer
    participant SL as SessionLifecycle
    participant Reaper as SessionReaper

    UC->>RL: track_resource(session_id)
    RL->>SL: active_refs.fetch_add(1, AcqRel)
    UC->>RL: on_subscribe()
    RL->>RL: state CAS Owned -> Observed
    Note over UC,RL: ... time passes, work happens ...
    UC->>RL: on_unsubscribe() (last)
    RL->>RL: state CAS Observed -> Releasing
    RL->>GT: arm grace_until_ms (= now + grace_ms)
    GT-->>RL: sleep_until(deadline)
    Note over GT: timer fires
    GT->>RL: state CAS Releasing -> Closed
    RL->>SL: active_refs.fetch_sub(1, AcqRel)
    alt active_refs == 0 AND not persistent
        SL->>SL: arm session-level grace
        Reaper->>SL: tick (consults active_refs)
        SL-->>Reaper: 0 - proceed disconnect
    else active_refs nonzero
        SL-->>Reaper: tick - defer, refcount positive
    end
```

---

## Backpressure references

- Sequence numbers, keepalive, cumulative chunks: [RESOURCES.md](./RESOURCES.md).
- Per-lane LagPolicy semantics: [adr/0006-backpressure-policies.md](./adr/0006-backpressure-policies.md).
- BlockSlow timeout safety (`SSH_BP_BLOCK_TIMEOUT_MS` default 5000): [adr/0006-backpressure-policies.md](./adr/0006-backpressure-policies.md) §BlockSlow timeout safety.
- Truncation compensation across all peer cursors on a URI: `MemoryRegistry::compensate_truncation`.

## See also

- [ARCHITECTURE.md](./ARCHITECTURE.md) — module map and threading overview.
- [OPERATIONS.md](./OPERATIONS.md) — operator-facing runbook.
- [RESOURCES.md](./RESOURCES.md) — backpressure features that depend on these primitives.
- [CONFIGURATION.md](./CONFIGURATION.md) — env-var table.
- [adr/0003-lifecycle-binding.md](./adr/0003-lifecycle-binding.md) — lifecycle adapter design.
- [adr/0004-channel-mux-fairness.md](./adr/0004-channel-mux-fairness.md) — channel mux + sub_id design.
- [adr/0006-backpressure-policies.md](./adr/0006-backpressure-policies.md) — LagPolicy semantics.
- [MIGRATION.md](./MIGRATION.md) — historical migration narratives.
