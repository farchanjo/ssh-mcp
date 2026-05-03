# Locks Reference (v4.0.0)

ssh-mcp v4 preserves the v3 **lock-free** baseline: every shared producer / consumer path uses `Arc<ArcSwap<T>>`, atomics, broadcast / mpsc channels, and `OnceCell` instead of `Mutex` / `RwLock`. The v4 hexagonal layout (see [ARCHITECTURE.md](./ARCHITECTURE.md)) moves the carriers into `src/adapters/` while preserving every lint, every channel size, and every acquisition rule.

This document is the source of truth for the patterns, the lints that enforce them, and the acquisition order for the residual `DashMap` shard locks that are still in play (briefly, never across `.await`).

Cross references:

- [ARCHITECTURE.md](./ARCHITECTURE.md) — module map and threading overview.
- [RESOURCES.md](./RESOURCES.md) — backpressure features that depend on these primitives.
- [MIGRATION_v3_to_v4.md](./MIGRATION_v3_to_v4.md) — file-path map between v3 and v4 modules.

## Foundational `mcp::*` modules still live

H17.5a deleted ~14k LOC of orphaned v3 code (see [adr/0002](./adr/0002-adopt-hexagonal-architecture.md)). The lock-free state carriers (`RunningCommand`, `RunningShell`, `RunningTransfer`) and the global `SUBSCRIPTION_REGISTRY` survived because the russh / SFTP adapters delegate into them. The patterns below describe both the runtime location (`mcp::*`) and the v4 surface (`adapters::*`) that consumers see.

| Carrier | v3 owner module (still runtime-active) | v4 surface |
|---------|----------------------------------------|------------|
| `RunningCommand` | `src/mcp/async_command.rs` | consumed by `src/adapters/ssh/russh_adapter.rs`, snapshot-read via `src/adapters/output_stream/russh_output.rs` |
| `RunningShell` | `src/mcp/shell.rs` | consumed by `src/adapters/ssh/russh_adapter.rs` |
| `RunningTransfer` | `src/mcp/transfer.rs` | consumed by `src/adapters/sftp/russh_sftp_adapter.rs` |
| `SUBSCRIPTION_REGISTRY` (global + `spawn_peer_gc` task) | `src/mcp/subscription.rs` | v4 use cases consume `MemoryRegistry<N>` at `src/adapters/subscription/memory_registry.rs`. Both coexist during the v4.0.0 transition window — H17.6 will collapse them. |
| `SessionRef` (russh handle + health channel) | `src/mcp/session.rs` + `src/mcp/types.rs` | consumed by `src/adapters/ssh/russh_adapter.rs` |
| `ForwardHandle` (feature-gated) | `src/mcp/types.rs` | consumed by the forward use case via `src/adapters/repo/dashmap/forward.rs` |

The new repository adapters (`src/adapters/repo/dashmap/{session,command,shell,transfer,forward}.rs`) and the new subscription registry (`src/adapters/subscription/memory_registry.rs`) are the v4 surface for use cases. Use cases never touch `SESSION_STORAGE` / `COMMAND_STORAGE` / `SHELL_STORAGE` / `TRANSFER_STORAGE` / `SUBSCRIPTION_REGISTRY` directly — they take `Arc<R: Repository>` / `Arc<R: SubscriberRegistryAsync>` generics and the composition root pins one concrete adapter per port.

## Lock-free invariants enforced

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

Combined with the standard `unwrap_used` / `panic` forbids, this guarantees that no async path in production can deadlock on a contended sync primitive.

## Patterns by structure

| Structure | Pattern | Notes |
|-----------|---------|-------|
| `RunningShell.history` | `Arc<ArcSwap<RingBuffer>>` + `ArcSwap::rcu` | Reader RCU loop ensures truncation composes with concurrent appends. The reader task and any `read_shell` clear can race safely. |
| `RunningShell.output_tx` | `broadcast::Sender<Bytes>` | Lagged auto-recovery via snapshot from `history`. |
| `RunningShell.input_tx` | `mpsc::Sender<WriteRequest>` | Single dedicated writer task owns `ChannelWriter` — no `Mutex`. Capacity = 64 frames. |
| `RunningShell.last_activity_ms` | `Arc<AtomicU64>` (epoch ms) | Replaces the v2 `Mutex<Instant>`. Read by the inactivity monitor without locking. |
| `RunningShell.data_notify` | `Arc<Notify>` | Wakes intra-server long-poll readers (`read_shell.wait`, `wait_for_pattern`). |
| `RunningShell.max_buffer_size` | `Arc<AtomicU64>` | Tunable at runtime without touching the reader task. |
| `RunningCommand.output_history` | `Arc<ArcSwap<OutputBuffer>>` | Same RCU pattern as shell. |
| `RunningCommand.output_tx` | `broadcast::Sender<OutputChunk>` | `OutputChunk = Stdout { seq, data } \| Stderr { seq, data } \| Closed { seq, exit_code }`. |
| `RunningCommand.exit_code` | `Arc<OnceCell<i32>>` | Write-once. |
| `RunningCommand.error` | `Arc<OnceCell<String>>` | Write-once. |
| `RunningCommand.timed_out` | `Arc<AtomicBool>` | |
| `RunningCommand.output_read` | `Arc<AtomicBool>` | Set by `get_command_output` to signal the cleanup task that the buffer can be reaped. |
| `RunningTransfer.bytes_transferred` | `Arc<AtomicU64>` | |
| `RunningTransfer.total_bytes` | `Arc<AtomicU64>` | |
| `RunningTransfer.error` | `Arc<OnceCell<String>>` | Write-once. |
| `RunningTransfer.progress_tx` | `broadcast::Sender<ProgressEvent>` | `Tick { seq, bytes_transferred, total_bytes } \| Completed \| Failed \| Cancelled`, all carrying a per-resource `seq`. |
| `RunningTransfer.data_notify` | `Arc<Notify>` | Wakes intra-server long-poll progress readers. |
| `SessionRef.handle` | `Arc<russh::client::Handle<SshClientHandler>>` | Cheap clone; russh handles are internally `Arc`-backed. |
| `SessionRef.channel_permits` | `Arc<Semaphore>` (`CHANNEL_CONCURRENCY_PER_SESSION = 1`) | Serialises russh channel openings per session — prevents `MaxSessions` exhaustion under rapid `execute + cancel` bursts. |
| `SessionRef.health_tx` | `broadcast::Sender<HealthEvent>` | `Healthy \| Unhealthy \| Disconnected`, all carrying `seq`. |
| `ForwardHandle.events_tx` (feature-gated) | `broadcast::Sender<ForwardEvent>` | `Accept \| Close \| Stopped`, all carrying `seq`. |
| `DashMap*Repo` (session / command / shell / transfer / forward) | `Arc<DashMap<Id, Entity>>` primary + secondary `DashMap<AgentId, HashSet<SessionId>>` index | Lock-free externally; shard locks are held briefly inside operations. v4 surface for the v3 `SESSION_STORAGE` / `COMMAND_STORAGE` / `SHELL_STORAGE` / `TRANSFER_STORAGE` globals. |
| `MemoryRegistry.subscribers` | `DashMap<uri, Vec<SubscriberHandle>>` | Snapshot-clone-then-drop pattern before any `.await`. Generic over the notifier `N: NotifierPort`. |
| `MemoryRegistry.peer_progress` | `DashMap<(peer_id, uri), Arc<PeerProgress>>` | Per-peer cursor; `byte_cursor: AtomicU64` + `last_seq_seen: AtomicU64`. |
| `MemoryRegistry.sequence_counters` | `DashMap<(kind, id), Arc<AtomicU64>>` | Per-resource monotonic seq. |
| `MemoryRegistry.wakers` | `DashMap<(kind, id), Arc<Notify>>` | Wakes the per-resource debouncer task. |
| `MemoryRegistry.debounce_tasks` | `DashMap<(kind, id), JoinHandle<()>>` | One task per active resource; aborted when the last subscriber leaves. |
| `PeerTable` (`src/adapters/notifier/rmcp_peer.rs`) | `Arc<DashMap<PeerId, Arc<rmcp::Peer<RoleServer>>>>` | Re-exposed as a type alias under `src/infra/mcp/peer_handle.rs`. The `RmcpPeerHandle` wrapper registers on construction and removes itself on `Drop`. |

## Acquisition order (residual DashMap shard locks)

DashMap acquires per-shard locks under the hood. They are not awaited across, but they are held briefly during `entry`, `get_mut`, `insert`, `remove`. To stay deadlock-free, any code that touches multiple maps in a single critical section must acquire them in the order below.

```
1. adapters::repo::dashmap::session   shard
2. adapters::repo::dashmap::command   shard
3. adapters::repo::dashmap::shell     shard
4. adapters::repo::dashmap::transfer  shard
5. adapters::repo::dashmap::forward   shard (feature-gated)
6. adapters::subscription::memory_registry::subscribers       shard
7. adapters::subscription::memory_registry::peer_progress     shard
8. adapters::subscription::memory_registry::sequence_counters shard
9. adapters::subscription::memory_registry::wakers            shard
10. adapters::subscription::memory_registry::debounce_tasks   shard
11. adapters::notifier::rmcp_peer::PeerTable                  shard
```

The foundational `src/mcp/{storage,subscription}` shards (still runtime-active) follow the same relative order behind the v4 adapter surface — adapters wrap calls into the foundational module without holding both shards simultaneously.

Rules:

- **NEVER hold a higher-numbered shard while acquiring a lower one** in the same task.
- **NEVER `.await` while a `DashMap` ref or guard is alive.** The `await_holding_lock` lint enforces this for `Mutex`/`RwLock` but not `DashMap`; the `significant_drop_in_scrutinee` lint catches the common pattern of `for entry in &map { ... .await ... }`. The codebase preventively snapshots-and-drops (see `MemoryRegistry::snapshot_subscribers`, `MemoryRegistry::gc_closed_peers`).
- **Producers `poke` then drop the waker guard** before doing anything else. The debouncer task then runs entirely outside the registry's locks.

The current code base touches at most two shards in one critical section (subscribe writes to `subscribers` then to `peer_progress`); the order above leaves headroom for future cross-cutting features.

## Channel sizing and recovery

| Channel | Capacity env | Default | Recovery on `Lagged` |
|---------|--------------|---------|----------------------|
| `RunningShell.output_tx` | `SSH_SHELL_BROADCAST_CAP` | 1024 | Subscriber re-reads `history` snapshot via `ArcSwap::load_full`, resumes broadcast. |
| `RunningCommand.output_tx` | `SSH_COMMAND_BROADCAST_CAP` | 1024 | Same — read from `output_history`. |
| `RunningTransfer.progress_tx` | `SSH_TRANSFER_BROADCAST_CAP` | 256 | Re-read atomic counters (`bytes_transferred`, `total_bytes`) and `status_rx`. |
| `SessionRef.health_tx` | `SSH_SESSION_BROADCAST_CAP` | 256 | Re-read `SessionInfo` via the session repository. |
| `ForwardHandle.events_tx` | `SSH_FORWARD_BROADCAST_CAP` | 256 | Forward storage is not persisted yet — broadcast is best-effort until the H17.6 surface lands. |
| `RunningShell.input_tx` (mpsc) | hard-coded | 64 | Backpressure on the producer (`application::write_shell::WriteShellUseCase`); no Lagged path. |

## Loom invariants

`tests/lockfree_invariants.rs` is gated behind `#[cfg(loom)]`. To run:

```bash
RUSTFLAGS="--cfg loom" cargo test --test lockfree_invariants --release
```

The tests permute concurrent interleavings on the shell `ArcSwap<RingBuffer>` rcu loop, the command `ArcSwap<OutputBuffer>` snapshot path, and the registry's `peer_progress` cursor advance. When loom is not enabled, the binary compiles to an empty test set. Full loom mode is currently blocked by upstream tokio/loom incompatibility in russh + axum (documented in the test file and `Cargo.toml`).

## Backpressure references

- Sequence numbers, keepalive, cumulative chunks: see [RESOURCES.md](./RESOURCES.md#backpressure-features-a--b--d).
- Per-peer cursor independence: see [RESOURCES.md](./RESOURCES.md#per-peer-cursor-behaviour).
- Truncation compensation across all peer cursors on a URI: `MemoryRegistry::compensate_truncation`.

## When you must add a new lock-free state

Decision tree:

1. **Single writer, many readers, immutable snapshots?** -> `Arc<ArcSwap<T>>` + `rcu` for compose-with-others writers.
2. **Write-once terminal value?** -> `Arc<OnceCell<T>>`.
3. **Counter or boolean flag?** -> `AtomicU64` / `AtomicBool` (never `Mutex<u64>` / `Mutex<bool>` — `mutex_integer` / `mutex_atomic` will reject it).
4. **Live event fan-out?** -> `tokio::sync::broadcast::Sender<Event>` with a per-resource sequence number.
5. **Wake intra-server pollers?** -> `tokio::sync::Notify`.
6. **Single-consumer queue?** -> `tokio::sync::mpsc` with a dedicated owner task; never wrap a writer in a `Mutex`.
7. **Map-keyed registry?** -> `dashmap::DashMap`, with a `snapshot then drop guard then iterate / await` pattern.

If none of the above fits, write a design note in your PR description before reaching for `Mutex` / `RwLock`. Adding a sync lock to an async path is a regression in this code base.
