# Observability Strategy

ssh-mcp is a subscribe-first SSH MCP server shipped as a single package
(version 7.1.3, `Cargo.toml:14`) with three binaries: `ssh-mcp` (HTTP
transport), `ssh-mcp-stdio` (stdio transport) and `ssh-mcp-tail` (NDJSON
daemon) (`Cargo.toml:25-39`).

This document states the observability surface the binary really emits and
the conventions that surface follows. The surface is the MCP wire plus
`tracing` log lines on a local stream. There is no telemetry backend: no
OTLP exporter, no metrics endpoint, no distributed trace. Every claim below
names the file and line that carries it; where a convention is absent, the
absence is stated with the evidence that shows it.

## Signals

Five signal families exist. Each one is readable by the MCP host, by an
operator on the process stream, or by both.

| Signal | Where emitted | How a reader gets it |
| --- | --- | --- |
| Per-subscription counters (`events_sent`, `bytes_sent`, `lagged_drops`, `lagged_recoveries`, `queue_depth`, `queue_high_watermark`, `block_total_ms`) | `src/infra/mcp/render/subscription.rs:415-436` | `sub_stats` tool call with a `sub_id`; counters are sampled from lane atomics (`src/adapters/subscription/subscriber_lane.rs:103-118`). Four of the seven never move in production — see Metrics |
| Inline-push counters (`inline_events_sent`, `inline_bytes_sent`) | `src/infra/mcp/render/subscription.rs:441-452` | `sub_stats`, only for a lane that opted into inline push |
| Global counters (`lanes_total`, `events_sent_total`, `bytes_sent_total`, `lagged_drops_total`, `lagged_recoveries_total`, `queue_high_watermark_max`) | `src/infra/mcp/render/subscription.rs:491-507`, computed at `src/application/subscription_admin.rs:603-624` | `sub_stats_all` tool call |
| Rsync session counters (`files_total`, `files_done`, `files_deleted`, `files_failed`, `bytes_total`, `bytes_transferred`, `bytes_skipped`) | `src/infra/mcp/render/rsync.rs:139-160` from `src/domain/rsync.rs:41-56` | `ssh_rsync_stats` tool call with an `RSYNC_ID`, and the `SyncCompleted` aggregate on the `rsync://<id>/progress` stream |
| Transfer progress (`PROGRESS` percent, `bytes_transferred/total`) | `src/infra/mcp/render/sftp.rs:178-237` | `ssh_transfer_progress` tool call, or `sub_open transfer://<id>/progress` |
| Session list incl. `healthy`, `EXPIRES_AT`, agent id | `src/infra/mcp/render/connection.rs:302-348` | `ssh_sessions` tool call |
| `SUB_LEAK_RISK` warnings | `src/infra/mcp/render/connection.rs:353-362` (markdown line) and `:367-386` (structured `warnings` array) | appended to the response of each `ssh_list_*` call that touches a leaked resource; source alerts from `src/adapters/lifecycle/leak_watcher.rs:145-181` |
| Session health resource | `src/application/read_resource.rs:636-647` (payload), template at `src/infra/mcp/resource_templates.rs:105-114` | `resources/read` on `session://<id>/health`, or push through `resources/subscribe` |
| Resource-change notification (`notifications/resources/updated`) | `src/adapters/notifier/rmcp_adapter.rs:54-76` | MCP notification on a subscribed peer |
| Progress notification for a leak alert (`notifications/progress`) | `src/infra/mcp/leak_warn_bridge.rs:154-167`, emitter at `src/infra/mcp/progress.rs:93-101` | MCP `notifications/progress` when the tool call supplied `_meta.progressToken`; the pump is spawned only when a watcher is present (`src/infra/mcp/leak_warn_bridge.rs:112-123`) |
| Inline output notification (`notifications/ssh/output`) | `src/adapters/notifier/rmcp_adapter.rs:31` and `:78-110` | MCP notification for a host that opted into the ADR 0012 capability at `initialize` |
| NDJSON `heartbeat` | `src/embed/event_mux.rs:134-155`, wire shape `src/embed/formatter.rs:205-216` | stdout of `ssh-mcp-tail` every `SSH_HEARTBEAT_INTERVAL_S` (default 30 s — `src/adapters/config/internal/mod.rs:965`) |
| NDJSON `daemon_stats` | `src/embed/event_mux.rs:174-194` | stdout of `ssh-mcp-tail` every `SSH_DAEMON_STATS_INTERVAL_S` (default 60 s — `src/adapters/config/internal/mod.rs:974`) |
| NDJSON stream events (`ack`, `started`, `push`, `completed`, `transfer_progress`, `shell_output`, `snapshot`, `lagged`, `warn`, `closed`, `resource_closed`, `inline_push`) | `src/embed/formatter.rs`, documented per event at `docs/DAEMON.md:286-436` | stdout of `ssh-mcp-tail`, one JSON object per line |
| Log lines | `tracing` sites listed under Logs | process stream: stdout for `ssh-mcp` HTTP, stderr for `ssh-mcp-stdio` and `ssh-mcp-tail` |

The log stream is the only signal that leaves the process without the MCP
host asking for it. Every other signal is pull-based or an opt-in push.

## Metrics

**The project has no metrics backend.** There is no OpenTelemetry, no
Prometheus endpoint, no `metrics` crate and no scrape target. The evidence
is the dependency list at `Cargo.toml:59-137`: it names `tracing`
(`Cargo.toml:125`) and `tracing-subscriber` with only the `env-filter`
feature (`Cargo.toml:126`), and nothing else in the telemetry space;
`Cargo.lock` contains no `opentelemetry`, `prometheus`, `metrics` or
`tokio-console` package. No OTLP endpoint, collector, dashboard or alert
rule is configured anywhere in the repository.

What exists in place of a metrics backend is a fixed set of in-process
counters, sampled on request and rendered onto the MCP wire. Every counter
is a monotonic `u64` or a sampled `usize`; nothing is a histogram, a gauge
with a time dimension, or a timer. The third column states what moves each
counter in production, which is not the same thing as who reads it: several
of these counters are moved only by code the tests drive.

| Counter | Defined at | Moved in production by | Scope | Surfaced by |
| --- | --- | --- | --- | --- |
| `events_sent` | `src/domain/subscription.rs:193` | One increment per delivered `notifications/resources/updated`: `LaneState::record_notify` (`src/adapters/subscription/subscriber_lane.rs:271-273`) called from `ship_legacy_notify` (`src/adapters/subscription/lane_bridge.rs:339`). The filtered-event path in `lane_dispatch` also counts (`subscriber_lane.rs:570`) and that path is test-only. | one subscription (lane) | `sub_stats`, and summed by `sub_stats_all` |
| `bytes_sent` | `src/domain/subscription.rs:195` | The same call, and it adds `0` every time: the production entry ignores its byte count and passes an empty slice — `LaneNotifierBridge::notify_lanes` takes `_bytes_added: usize` and calls `notify_lanes_with_bytes(uri, &[])` (`src/adapters/subscription/lane_bridge.rs:500-510`), so `record_notify(0)` reaches `record_send(0)`. The field is read by `sub_stats` and reads `0` for the life of a production process. | one lane | `sub_stats`, `sub_stats_all` |
| `lagged_drops` | `src/domain/subscription.rs:198` | Nothing in production. `LaneAtomics::record_drop` (`subscriber_lane.rs:126-128`) is reached only from `lane_dispatch_drop_newest` (`:596`), `lane_dispatch_snapshot` (`:633`) and `lane_dispatch_drop_oldest` (`:639`), all under `lane_dispatch` (`:566`), whose only caller is `SubscriberLaneAdapter::produce` (`:424`, call at `:429`) — a test-only entry point. Reads `0` for the life of a production process. | one lane | `sub_stats`, `sub_stats_all` |
| `lagged_recoveries` | `src/domain/subscription.rs:200` | `sub_replay`: `LaneAtomics::record_recovery` (`subscriber_lane.rs:130-132`) runs in `replay_from_cursor` (`:878`) under `LaneAdmin::replay` (`:751`), called by the `sub_replay` use case (`src/application/subscription_admin.rs:444`). The other call site, `lane_dispatch_block_slow` (`:666`), is test-only. | one lane | `sub_stats`, `sub_stats_all` |
| `queue_depth` | `src/domain/subscription.rs:202` | Nothing in production. `LaneAtomics::observe_depth` (`subscriber_lane.rs:134-138`) has exactly one call site, the tail of `lane_dispatch` (`:581`), reached only through `produce`. Reads `0` for the life of a production process. | one lane | `sub_stats` |
| `queue_high_watermark` | `src/domain/subscription.rs:204` | Nothing in production: same single call site as `queue_depth` (`subscriber_lane.rs:581`, `fetch_max` inside `observe_depth`). Reads `0` for the life of a production process. | one lane | `sub_stats`, max across lanes by `sub_stats_all` |
| `block_total_ms` | `src/domain/subscription.rs:206` | Nothing, in production or in tests: the atomic is declared at `subscriber_lane.rs:99` and only ever loaded, in `snapshot` (`:111`). `lane_dispatch_block_slow` degrades to `lane_dispatch_snapshot` instead of accounting a wait (`subscriber_lane.rs:676-686`). Reads `0` always. | one lane | `sub_stats` |
| `byte_triggered_flushes` | `src/domain/subscription.rs:214` | The registry-level counter, not the lane field: the debouncer's byte-triggered flush branch, `flush_now.notified()` → `fetch_add(1)` (`src/adapters/subscription/memory_registry.rs:931-934`). The lane copy of the field is hardcoded `0` (`subscriber_lane.rs:112-116`). | resource fan-out | nothing on the wire: the registry counter is read only by `byte_triggered_flushes_total()` (`memory_registry.rs:522-523`), and `sub_stats` does not render the field |
| `inline_events_sent`, `inline_bytes_sent` | `src/adapters/subscription/subscriber_lane.rs:188` and `:194` | The inline-push leg, per delivered fragment, for a lane that opted in: `LaneNotifierBridge::notify_lanes_inline` (`src/adapters/subscription/lane_bridge.rs:512`) called from the producer site (`src/adapters/subscription/memory_registry.rs:718`). The legacy entry never reaches it, because it passes an empty slice (`lane_bridge.rs:510`). | one opted-in lane | `sub_stats`, gated on `inline_push` (`src/infra/mcp/render/subscription.rs:441-447`) |
| `files_total`, `files_done`, `files_deleted`, `files_failed`, `bytes_total`, `bytes_transferred`, `bytes_skipped` | `src/domain/rsync.rs:41-56` | rsync executor and wire transport in production; each field is monotonic non-decreasing (`src/domain/rsync.rs:32-34`) | one rsync session | `ssh_rsync_stats`, `SyncCompleted`, `rsync://<id>/progress` |
| `bytes_transferred` / `total_bytes` (transfer) | `src/infra/mcp/render/sftp.rs:183` and `:226-232` | SFTP adapter through the transfer repository, in production | one transfer | `ssh_transfer_progress`, `transfer://<id>/progress` |
| `active_lanes`, `events_sent`, `bytes_sent`, `lagged_drops`, `lagged_recoveries`, `max_queue_high_watermark` | `src/ports/channel_mux.rs:12-25` | `ChannelMux::aggregate_stats` (`src/adapters/subscription/channel_mux.rs:209-210`) sums the mux counters, and the mux drain task that would move them is never spawned in production (`spawn_drain`, `channel_mux.rs:120`, has test-only callers; `src/composition/prod.rs:344` drops the receiver). | whole mux | no tool and no event reads this port today; `sub_stats_all` sums lane snapshots instead (`src/application/subscription_admin.rs:603-624`) |

Three honest gaps belong in this section.

- Four of the seven per-lane atomics never move in production.
  `lagged_drops`, `queue_depth`, `queue_high_watermark` and `block_total_ms`
  read `0` for the life of a production process, because the only code that
  moves them runs under `SubscriberLaneAdapter::produce`
  (`src/adapters/subscription/subscriber_lane.rs:424`), whose callers are
  all tests. `bytes_sent` moves but adds `0` on every call, because the
  production entry passes an empty byte slice
  (`src/adapters/subscription/lane_bridge.rs:500-510`). `events_sent` and
  `lagged_recoveries` are the two that do move: the first counts delivered
  resource notifications, the second counts `sub_replay` calls. A host that
  reads `sub_stats` against a running server therefore learns the
  notification count and the replay count, and nothing about queue depth or
  lag.
- `REVIEW_CHECKLIST.md:36` states "Per-lane atomics (`events_sent`,
  `bytes_sent`) increment on every successful drain." No drain runs in
  production: `ChannelMuxAdapter::spawn_drain`
  (`src/adapters/subscription/channel_mux.rs:120`) has test-only callers and
  `src/composition/prod.rs:344` drops the mux outbound receiver when
  `build_use_cases` returns. The sentence describes the tested component,
  not the running server.
- The `daemon_stats` NDJSON event carries four counters that are hardcoded
  to `0`: `active_sessions`, `active_subs`, `ring_buffer_overflows_total`
  and `mpsc_full_events_total` (`src/embed/event_mux.rs:182-188`). The
  module comment states that the embed layer does not observe them and that
  the zeros exist so consumers can wire a forward-compatible schema
  (`src/embed/event_mux.rs:158-164`). The live values are the ones
  `sub_stats_all` returns, because that use case reads the lane registry
  directly.
- `docs/OPERATIONS.md:864-867` describes `sub_stats_all` as returning
  `active_sessions`, `mux_queue_depth`, `peer_gc_pace_per_min` and
  `rejected_ops_total`. The tool does not return those names: the rendered
  keys are `LANES_TOTAL`, `EVENTS_SENT_TOTAL`, `BYTES_SENT_TOTAL`,
  `LAGGED_DROPS_TOTAL`, `LAGGED_RECOVERIES_TOTAL` and
  `QUEUE_HIGH_WATERMARK_MAX` (`src/infra/mcp/render/subscription.rs:493-504`).
  The code is the source of truth; the operations document carries drift.

## Logs

Logging is `tracing`, installed once per process by a private helper:

```rust
fn install_subscriber<W>(make_writer: W) {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).with_writer(make_writer).init();
}
```
(`src/composition/prod.rs:729-738`)

The filter is `RUST_LOG`, read through the `env-filter` feature
(`Cargo.toml:126`), and its default level is `info`
(`src/composition/prod.rs:733`). The formatter is the default `fmt` text
format: no JSON layer, no span-event layer, no sampling, no log rotation
and no file sink is configured anywhere.

Destination per binary, which is the load-bearing part of the convention:

| Binary | Stream | Where set |
| --- | --- | --- |
| `ssh-mcp` (HTTP) | stdout | `install_subscriber(io::stdout)` in `run_http` (`src/composition/prod.rs:774`) |
| `ssh-mcp-stdio` | stderr | `install_subscriber(io::stderr)` in `run_stdio` (`src/composition/prod.rs:824`) |
| `ssh-mcp-tail` (daemon) | stderr | `src/composition/embed.rs:396-403`; stdout stays strictly NDJSON (`docs/DAEMON.md:440`) |

Level conventions, from the sites on the hot paths:

- `info` marks a process or subsystem lifecycle edge: transport start
  (`src/composition/prod.rs:777`, `:789`, `:825`), peer-GC and watcher spawn
  (`src/composition/prod.rs:805`, `:840`), signal receipt
  (`src/embed/signals.rs:86-88`), Ctrl-C (`src/composition/prod.rs:760`).
- `warn` marks a degraded but survivable path: host-key rejection
  (`src/adapters/ssh/internal/session.rs:72-87`, with `host` and `port` as
  structured fields and never the key material), a failed `ctrl_c` handler
  or task join (`src/composition/prod.rs:761`, `:812`, `:857-859`), signal
  handler init failure (`src/embed/signals.rs:78`).
- `error` is rare and reserved for a hard failure on a driver loop.
- `debug` and `trace` carry per-event detail behind an explicit filter:
  lifecycle CAS edges and subscription internals are the documented
  `trace` targets (`docs/OPERATIONS.md:846-852`).
- Drops on a full channel are logged at `trace`, not `warn`, because they
  are expected under pressure: `heartbeat tx try_send` and
  `daemon_stats tx try_send` (`src/embed/event_mux.rs:150`, `:190`), and a
  lagged alert receiver (`src/infra/mcp/leak_warn_bridge.rs:143-145`).

Message conventions: a lowercase sentence fragment describing the event,
with the variable parts carried as structured fields (`addr = %bind_addr`,
`host = %self.host`, `port = self.port`, `lagged = n`) rather than
interpolated into the text. A module that needs a stable filter target sets
one explicitly, as `target: "leak_warn_bridge"` does
(`src/infra/mcp/leak_warn_bridge.rs:144`).

`print!`, `print_stdout` and `print_stderr` are forbidden by the workspace
lint table (`Cargo.toml:156` and the forbid block at `Cargo.toml:158-168`),
so no byte reaches a process stream outside this subscriber and the NDJSON
formatter. `docs/CONFIGURATION.md:210` records the same contract for
operators.

## Tracing

**The project emits no distributed trace.** There is no
`tracing-opentelemetry`, no OTLP exporter, no W3C `traceparent`
propagation, no span exporter and no trace backend. The evidence is
`Cargo.toml:59-137` and `Cargo.lock`, neither of which carries any of those
crates.

The only span-producing layer in the process is the HTTP request logger:
`TraceLayer::new_for_http()` is installed on the axum router
(`src/composition/prod.rs:786`), from the `tower-http` dependency with its
`trace` feature (`Cargo.toml:89`). It creates a per-request span on the
tracing subscriber; the subscriber prints no span events
(`src/composition/prod.rs:734-737`), so the span is visible only as the
current span of events logged inside the request, and it is exported
nowhere. The stdio and daemon transports install no span layer at all.

Three substitutes carry the correlation job that a trace would carry:

1. Per-lane monotonic sequence numbers. `SubscriberStats` is sampled from
   the lane atomics and every push carries the `seq` it was written at
   (`docs/DAEMON.md:416-418`), and a health snapshot reports `last_seq`
   (`src/application/read_resource.rs:646`). A consumer that sees a gap
   knows it missed events and can re-read with `?cursor=auto`.
2. The `lagged` NDJSON event names the number of events a slow consumer
   lost (`docs/DAEMON.md:367-374`), and `sub_stats` reports the same fact
   as `lagged_drops` and `lagged_recoveries`.
3. `SUB_LEAK_RISK` alerts carry the canonical resource URI, the resource
   age in milliseconds and the severity tier
   (`src/adapters/lifecycle/leak_watcher.rs:58-70`,
   `src/adapters/lifecycle/leak_watcher.rs:214`), which is the closest
   thing to a causal chain the lifecycle surface has.

`docs/OPERATIONS.md:299` mentions a `tokio-console` feature for CPU
profiling. That feature does not exist: the manifest declares `default`,
`port_forward`, `test-fixtures` and `e2e-vm` and no console feature
(`Cargo.toml:145-149`), and no console subscriber is installed in any
binary. A profiling session therefore has the OS tooling and `RUST_LOG`
detail only.

## Cardinality

No custom metric labels.

The project emits no metric label of any kind, because it emits no metric
(see Metrics). The word "label" has no referent in this codebase: no
counter carries a key-value dimension and no exporter exists to attach one.
What the code does bound is the registry the counters live in and the maps
the watcher keeps, and those bounds are these:

- Subscriptions per URI: default 16, floor 1
  (`src/adapters/config/internal/mod.rs:200`, `:202`).
- Subscriptions in total: default 1024, floor 1
  (`src/adapters/config/internal/mod.rs:205`, `:207`).
- Per-lane mpsc capacity: default 1024, floor 16, cap 65 536
  (`src/adapters/config/internal/mod.rs:186`, `:188`, `:190`).
- Mux outbound capacity: default 8192, floor 64, cap 1 048 576
  (`src/adapters/config/internal/mod.rs:193`, `:195`, `:197`).
- Replay window per lane: default 1 048 576 bytes, cap 64 MiB
  (`src/adapters/config/internal/mod.rs:225`, `:229`).
- Leak-alert broadcast capacity: 256 alerts
  (`src/adapters/lifecycle/leak_watcher.rs:48`).
- Leak-alert map key: `(ResourceKind, resource_id)`, one entry per leaked
  resource (`src/adapters/lifecycle/leak_watcher.rs:103`, `:139`).
- Closed-entry retention: 60 000 ms, after which the eviction sweep drops
  the entry (`src/adapters/lifecycle/refcount.rs:64`).
- Filter regex length: cap 1024 characters
  (`src/adapters/config/internal/mod.rs:217`).
- NDJSON line size: cap 16 MiB
  (`src/adapters/config/internal/mod.rs:960`).
- Distinct log targets: the five documented targets
  (`docs/OPERATIONS.md:846-852`).

Consequences that follow from those numbers:

- A counter series is identified by `sub_id` and by nothing else. There is
  no host, user, port or command label on any counter, so a burst of new
  SSH sessions grows the session repository but not the counter space.
- The global subscription cap (1024) is the ceiling on how many
  subscription-scoped series `sub_stats_all` can walk in one call
  (`src/application/subscription_admin.rs:604-624`).
- The leak watcher is the only component that retains an unbounded-looking
  map. It is bounded by live resources plus the 60 s closed-entry retention
  (`src/adapters/lifecycle/refcount.rs:64`), and its alert emitters drop
  rather than block when the 256-slot broadcast is full
  (`src/adapters/lifecycle/leak_watcher.rs:42-48`).
- The scan cadence of the watcher is fixed at one second
  (`src/adapters/lifecycle/leak_watcher.rs:50-56`), with a warn threshold
  of 2 s and the kill phase off by default
  (`src/adapters/lifecycle/leak_watcher.rs:93-100`, values resolved from
  `SSH_SUB_LEAK_RISK_WARN_S` and `SSH_SUB_LEAK_RISK_KILL_S` at
  `src/adapters/config/internal/mod.rs:285-306`).
