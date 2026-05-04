# ADR 0006: Backpressure Policies for Subscriber Lanes

## Status

Proposed (v5.0.0). Implementation tracked under Phase 2 of the v5 roadmap. Depends on ADR 0004 (Channel Mux).

## Context

ssh-mcp v4 surfaces backpressure at six points in the data path:

1. **russh recv** — TCP window flow control on the underlying socket. Out of our control; bounded by kernel.
2. **per-resource ring buffer** — `RunningShell.history` (`ArcSwap<RingBuffer>`), `RunningCommand.output_history` (`ArcSwap<OutputBuffer>`). Bounded by `max_buffer_size` per shell or `SSH_COMMAND_MAX_BUFFER_SIZE`. Head-drop on overflow with `compensate_truncation` cursor adjustment.
3. **debouncer task** — coalesces `next_seq + poke` events into one `notifications/resources/updated` per debounce window (`SSH_NOTIFY_DEBOUNCE_MS`, default 50 ms; force flush at `SSH_NOTIFY_FORCE_FLUSH_MS`, default 1 s).
4. **broadcast channel** (`output_tx`) — bounded by `SSH_*_BROADCAST_CAP`. Subscribers that fall behind surface `RecvError::Lagged`; recovery is to load a fresh snapshot from `output_history` and continue.
5. **rmcp transport buffer** — implicit; rmcp manages this.
6. **stdout writer** (in HTTP / stdio transports) — implicit OS buffer.

This works for a single peer per URI. With Phase 2's per-sub_id channel mux (ADR 0004), a new fronteira appears: each sub_id has its own bounded `mpsc::channel(N)` between the per-resource debouncer fan-out and the consumer task. The mpsc is per-lane, single-producer (debouncer) / single-consumer (lane drain). When the consumer is slow, the lane fills.

The v4 broadcast channel uses `RecvError::Lagged` — slow subscribers lose events and must rebuild from `output_history`. This is a single fixed policy. Phase 2 needs:

- An explicit per-sub_id choice between strict zero-loss (latency cost) and lossy fast-drain (gap markers).
- A snapshot-rebuild path that converts a transient overflow into a delivered snapshot, masking the gap from the LLM.
- Stats per sub_id so an LLM can detect lag and self-correct.
- A predictable mpsc full behaviour that does not deadlock the daemon.

Two approaches were evaluated:

1. **Single global policy.** Pick one default (BlockSlow) and ship it. Rejected because audit-log workloads need zero loss while real-time monitoring tolerates gaps; one policy does not fit both.
2. **Per-sub_id policy + per-fronteira strategy.** Each lane picks a policy; each fronteira has explicit, documented behaviour. Selected.

## Decision

Define **four lag policies** (chosen per-sub_id at subscribe time) and an explicit **per-fronteira strategy** for the six backpressure points listed above. Document the failure mode at each fronteira so the daemon never deadlocks and every drop is observable.

### LagPolicy enum

```rust
pub enum LagPolicy {
    /// Block the producer (debouncer task) until the consumer drains.
    /// Zero loss guarantee.  Latency growth = lag duration.
    /// Producer .await on `mpsc::Sender::send`.
    BlockSlow,

    /// Pop oldest event from the lane's mpsc; push the new event.
    /// Emit `{"ev":"lagged","sub_id":"...","dropped":N}` marker so the
    /// consumer knows it lost N events. Use when monitoring tolerates
    /// gaps.
    DropOldest,

    /// Ignore the new event when the mpsc is full. Emit lagged marker.
    /// Rare; use when you prefer to retain historical context over
    /// keeping up with current events.
    DropNewest,

    /// (Default.) Drop the lane's mpsc backlog when it fills. Next
    /// drain triggers a `read_resource(uri, cursor=current_seq)` that
    /// returns the live snapshot from the per-resource ring buffer.
    /// Emit `{"ev":"snapshot","sub_id":"...","cursor":N,"delta":<bytes>}`.
    /// Zero loss as long as the ring buffer covers the gap; otherwise
    /// emits a `LAG_DETECTED` warning.
    Snapshot,
}
```

`Snapshot` is the default because:

- The ring buffer already holds the live tail (`max_buffer_size` per shell / cmd).
- Rebuild is O(buf_size); typically <1 MB, so the worst-case latency is sub-ms on local memory.
- The consumer sees a strictly-monotonic sequence number; it can detect that a gap was bridged and update its UI accordingly.

### Per-fronteira behaviour matrix

| Fronteira | Default | Override env var | Failure mode |
|---|---|---|---|
| russh recv | TCP window | n/a | Backpressure propagates to remote sshd. |
| ring buffer | `max_buffer_size` head-drop + `compensate_truncation` | `SSH_SHELL_MAX_BUFFER`, `SSH_COMMAND_MAX_BUFFER_SIZE` | Cursor monotonicity preserved; head bytes lost. |
| debouncer | 50 ms coalesce, 1 s force flush, 30 s keepalive | `SSH_NOTIFY_DEBOUNCE_MS`, `SSH_NOTIFY_FORCE_FLUSH_MS`, `SSH_NOTIFY_KEEPALIVE_S` | One outbound notification per window regardless of input rate. |
| **lane mpsc (per sub_id)** | `Snapshot` | `SSH_LAG_POLICY_DEFAULT`, per-call `lag_policy` | Per LagPolicy table above. |
| **mux mpsc (global)** | bounded; round-robin yields when full | `SSH_MUX_BUFFER` (default 8192) | Mux yields back to dispatcher via `try_send` failure handling; lane producer detects this and follows its lag policy. |
| stdout writer | OS pipe buffer; `SIGPIPE` triggers daemon shutdown grace | n/a | Daemon detects pipe broken, drains pending events, exits cleanly. |

### BlockSlow timeout safety

`BlockSlow` is unbounded by default — the producer waits as long as the consumer needs. Operators that want a hard ceiling set `SSH_BP_BLOCK_TIMEOUT_MS` (default 5000). On timeout, the producer falls back to `Snapshot` semantics (drop backlog + force snapshot rebuild) and emits a `LAG_BACKPRESSURE` warning. This converts a misbehaving consumer from a deadlock vector into a degraded-but-alive consumer.

### Stats wired through SubscriberStats

Every drop / block / recovery increments an atomic counter on the lane's `SubscriberStats`:

| Counter | Increments when |
|---|---|
| `events_sent` | Outbound mpsc `send` succeeds. |
| `bytes_sent` | Outbound `send` succeeds; payload size accumulated. |
| `lagged_drops` | DropOldest / DropNewest discards an event. |
| `lagged_recoveries` | Snapshot rebuild completes successfully. |
| `queue_depth` | Sampled via `mpsc::Sender::capacity` minus `len`. |
| `queue_high_watermark` | `fetch_max` on every `send`. |
| `block_total_ms` | Cumulative time spent in `BlockSlow` waits. |

The `ssh_sub_stats` MCP tool exposes these per sub_id; the global `ssh_daemon_stats` aggregates across all subs.

### Mux mpsc handling

The global mux mpsc capacity is `SSH_MUX_BUFFER` (default 8192). When full:

1. The lane consumer task that tried to `send` falls back to its lane's `LagPolicy`. For `Snapshot`, this means flushing the lane's local backlog.
2. The mux drain loop is wakeup-driven; no spinning. When the outbound writer drains, it `notify_one` on the mux waker, and lanes resume.
3. If the outbound writer is genuinely stuck (e.g. NDJSON consumer paused), every lane's `Snapshot` rebuild will produce identical "drop and re-snapshot on resume" behaviour — no deadlock, no unbounded growth.

### Filter pipeline interaction

When a lane has a filter (regex / level), filtering happens **before** the mpsc enqueue. A lane with a strict filter that excludes 99 % of events will rarely fill its mpsc; backpressure stats will reflect the filtered rate, not the raw production rate.

## Consequences

### Positive

- **Per-workload tuning.** Audit log consumers select `BlockSlow`; monitoring consumers select `Snapshot`. Each gets the right tradeoff.
- **No deadlocks.** Every fronteira has a documented overflow strategy. The daemon never spins on `send` indefinitely; `BlockSlow` has a timeout escape hatch.
- **Self-attributing failures.** A laggy consumer shows up in its own stats without affecting peers. Operators see exactly which sub_id is the bottleneck.
- **Snapshot is the safe default.** A consumer that does nothing special inherits zero-loss-with-gap-bridging behaviour. The ring buffer absorbs all reasonable transient slowness.

### Negative

- **More state per lane.** Eight atomic counters + the `LagPolicy` + the filter regex add ~200 bytes of state per sub_id. At 65 K subs that's 13 MB of stats, dominated by the filter compiled regex. Acceptable.
- **Snapshot rebuild is O(buf_size).** A consumer recovering from drop pays a constant cost per gap. With 1 MB buffers the cost is sub-ms; with 16 MB buffers it can be tens of ms. Operators that need lower-latency recovery tune `SSH_SHELL_MAX_BUFFER` down.
- **Operator-visible warnings.** `LAG_BACKPRESSURE` and `LAG_DETECTED` markers are wire-level events; misconfigured consumers will produce a steady stream until tuned.

### Neutral

- **Backwards compatibility.** v4 hosts that subscribe via the legacy `(PeerId, Uri)` path inherit `Snapshot` policy by default. Behaviour matches v4 semantics (slow subscribers recover via snapshot rebuild).
- **Test surface scales.** Phase 5 ships ≥8 backpressure scenarios per (lane × policy) combination. With 4 policies × 4 representative lane sizes, that's ~32 scenarios — covered by parameterised proptest cases.

## References

- [ADR 0004 — Channel Mux](./0004-channel-mux-fairness.md) — defines the per-sub_id lane structure.
- [ADR 0007 — Error Taxonomy](./0007-error-taxonomy.md) — `LAG_*` codes.
- [docs/CONFIGURATION.md](../CONFIGURATION.md) — env var defaults.
- [docs/RESOURCES.md](../RESOURCES.md) — resource scheme contract.
