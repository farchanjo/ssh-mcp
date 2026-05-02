# Locks Reference (v3.0.0)

ssh-mcp v3 is a **lock-free** SSH multiplexer. Every shared producer / consumer path uses `Arc<ArcSwap<T>>`, atomics, broadcast / mpsc channels, and `OnceCell` instead of `Mutex` / `RwLock`. This document is the source of truth for the patterns, the lints that enforce them, and the acquisition order for the residual `DashMap` shard locks that are still in play (briefly, never across `.await`).

Cross references:

- [ARCHITECTURE.md](./ARCHITECTURE.md) — module map and threading overview.
- [RESOURCES.md](./RESOURCES.md) — backpressure features that depend on these primitives.

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

| Structure                                    | Pattern                                                              | Notes                                                                                                                                  |
| -------------------------------------------- | -------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| `RunningShell.history`                       | `Arc<ArcSwap<RingBuffer>>` + `ArcSwap::rcu`                          | Reader RCU loop ensures truncation composes with concurrent appends. The reader task and any `ssh_shell_read` clear can race safely. |
| `RunningShell.output_tx`                     | `broadcast::Sender<Bytes>`                                            | Lagged auto-recovery via snapshot from `history`.                                                                                      |
| `RunningShell.input_tx`                      | `mpsc::Sender<WriteRequest>`                                          | Single dedicated writer task owns `ChannelWriter` — no `Mutex`. Capacity = 64 frames.                                                  |
| `RunningShell.last_activity_ms`              | `Arc<AtomicU64>` (epoch ms)                                          | Replaces the v2 `Mutex<Instant>`. Read by the inactivity monitor without locking.                                                       |
| `RunningShell.data_notify`                   | `Arc<Notify>`                                                        | Wakes intra-server long-poll readers (`ssh_shell_read.wait`, `ssh_shell_wait_for`).                                                    |
| `RunningShell.max_buffer_size`               | `Arc<AtomicU64>`                                                     | Tunable at runtime without touching the reader task.                                                                                   |
| `RunningCommand.output_history`              | `Arc<ArcSwap<OutputBuffer>>`                                          | Same RCU pattern as shell.                                                                                                              |
| `RunningCommand.output_tx`                   | `broadcast::Sender<OutputChunk>`                                      | `OutputChunk` = `Stdout { seq, data } \| Stderr { seq, data } \| Closed { seq, exit_code }`.                                              |
| `RunningCommand.exit_code`                   | `Arc<OnceCell<i32>>`                                                 | Write-once.                                                                                                                             |
| `RunningCommand.error`                       | `Arc<OnceCell<String>>`                                              | Write-once.                                                                                                                             |
| `RunningCommand.timed_out`                   | `Arc<AtomicBool>`                                                    |                                                                                                                                         |
| `RunningCommand.output_read`                 | `Arc<AtomicBool>`                                                    | Set by `ssh_get_command_output` to signal the cleanup task that the buffer can be reaped.                                              |
| `RunningTransfer.bytes_transferred`          | `Arc<AtomicU64>`                                                     |                                                                                                                                         |
| `RunningTransfer.total_bytes`                | `Arc<AtomicU64>`                                                     |                                                                                                                                         |
| `RunningTransfer.error`                      | `Arc<OnceCell<String>>`                                              | Write-once.                                                                                                                             |
| `RunningTransfer.progress_tx`                | `broadcast::Sender<ProgressEvent>`                                   | `Tick { seq, bytes_transferred, total_bytes } \| Completed \| Failed \| Cancelled`, all carrying a per-resource `seq`.                   |
| `RunningTransfer.data_notify`                | `Arc<Notify>`                                                        | Wakes intra-server long-poll progress readers.                                                                                          |
| `SessionRef.handle`                          | `Arc<russh::client::Handle<SshClientHandler>>`                        | Cheap clone; russh handles are internally `Arc`-backed.                                                                                 |
| `SessionRef.channel_permits`                 | `Arc<Semaphore>` (`CHANNEL_CONCURRENCY_PER_SESSION = 1`)              | Serialises russh channel openings per session — prevents `MaxSessions` exhaustion under rapid `execute + cancel` bursts.               |
| `SessionRef.health_tx`                       | `broadcast::Sender<HealthEvent>`                                     | `Healthy \| Unhealthy \| Disconnected`, all carrying `seq`.                                                                              |
| `ForwardHandle.events_tx` (feature-gated)    | `broadcast::Sender<ForwardEvent>`                                     | `Accept \| Close \| Stopped`, all carrying `seq`.                                                                                       |
| `Storage` (Session / Command / Shell / Transfer) | `DashMap<String, T>`                                              | Lock-free externally; shard locks are held briefly inside operations.                                                                  |
| `SubscriptionRegistry.subscribers`           | `DashMap<uri, Vec<SubscriberHandle>>`                                | Snapshot-clone-then-drop pattern before any `.await`.                                                                                  |
| `SubscriptionRegistry.peer_progress`         | `DashMap<(peer_id, uri), Arc<PeerProgress>>`                         | Per-peer cursor; `byte_cursor: AtomicU64` + `last_seq_seen: AtomicU64`.                                                                 |
| `SubscriptionRegistry.sequence_counters`     | `DashMap<(kind, id), Arc<AtomicU64>>`                                | Per-resource monotonic seq.                                                                                                             |
| `SubscriptionRegistry.wakers`                | `DashMap<(kind, id), Arc<Notify>>`                                   | Wakes the per-resource debouncer task.                                                                                                  |
| `SubscriptionRegistry.debounce_tasks`        | `DashMap<(kind, id), JoinHandle<()>>`                                | One task per active resource; aborted when the last subscriber leaves.                                                                  |

## Acquisition order (residual DashMap shard locks)

DashMap acquires per-shard locks under the hood. They are not awaited across, but they are held briefly during `entry`, `get_mut`, `insert`, `remove`. To stay deadlock-free, any code that touches multiple maps in a single critical section must acquire them in the order below.

```
1. SHELL_STORAGE / COMMAND_STORAGE / TRANSFER_STORAGE / SESSION_STORAGE shard
2. SUBSCRIPTION_REGISTRY.subscribers shard
3. SUBSCRIPTION_REGISTRY.peer_progress shard
4. SUBSCRIPTION_REGISTRY.sequence_counters shard
5. SUBSCRIPTION_REGISTRY.wakers shard
6. SUBSCRIPTION_REGISTRY.debounce_tasks shard
```

Rules:

- **NEVER hold a higher-numbered shard while acquiring a lower one** in the same task.
- **NEVER `.await` while a `DashMap` ref or guard is alive.** The `await_holding_lock` lint enforces this for `Mutex`/`RwLock` but not `DashMap`; the `significant_drop_in_scrutinee` lint catches the common pattern of `for entry in &map { ... .await ... }`. The codebase preventively snapshots-and-drops (see `SubscriptionRegistry::snapshot_subscribers`, `SubscriptionRegistry::gc_closed_peers`).
- **Producers `poke` then drop the waker guard** before doing anything else. The debouncer task then runs entirely outside the registry's locks.

The current code base touches at most two shards in one critical section (subscribe writes to `subscribers` then to `peer_progress`); the order above leaves headroom for future cross-cutting features.

## Channel sizing and recovery

| Channel                          | Capacity env                  | Default | Recovery on `Lagged`                                                                  |
| -------------------------------- | ----------------------------- | ------- | ------------------------------------------------------------------------------------- |
| `RunningShell.output_tx`         | `SSH_SHELL_BROADCAST_CAP`     | 1024    | Subscriber re-reads `history` snapshot via `ArcSwap::load_full`, resumes broadcast.    |
| `RunningCommand.output_tx`       | `SSH_COMMAND_BROADCAST_CAP`   | 1024    | Same — read from `output_history`.                                                    |
| `RunningTransfer.progress_tx`    | `SSH_TRANSFER_BROADCAST_CAP`  | 256     | Re-read atomic counters (`bytes_transferred`, `total_bytes`) and `status_rx`.         |
| `SessionRef.health_tx`           | `SSH_SESSION_BROADCAST_CAP`   | 256     | Re-read `SessionInfo` via `SESSION_STORAGE.get`.                                       |
| `ForwardHandle.events_tx`        | `SSH_FORWARD_BROADCAST_CAP`   | 256     | Forward storage is not persisted yet — broadcast is best-effort until E13 ships.       |
| `RunningShell.input_tx` (mpsc)   | hard-coded                     | 64      | Backpressure on the producer (`tools/shell.rs::ssh_shell_write`); no Lagged path.      |

## Loom invariants

`tests/lockfree_invariants.rs` is gated behind `#[cfg(loom)]`. To run:

```bash
RUSTFLAGS="--cfg loom" cargo test --test lockfree_invariants --release
```

The tests permute concurrent interleavings on the shell `ArcSwap<RingBuffer>` rcu loop, the command `ArcSwap<OutputBuffer>` snapshot path, and the registry's `peer_progress` cursor advance. When loom is not enabled, the binary compiles to an empty test set.

## Backpressure references

- Sequence numbers, keepalive, cumulative chunks: see [RESOURCES.md](./RESOURCES.md#backpressure-features-a--b--d).
- Per-peer cursor independence: see [RESOURCES.md](./RESOURCES.md#per-peer-cursor-behaviour).
- Truncation compensation across all peer cursors on a URI: `SubscriptionRegistry::compensate_truncation`.

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
