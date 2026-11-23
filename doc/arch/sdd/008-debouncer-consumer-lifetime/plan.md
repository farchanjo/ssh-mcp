# Implementation Plan: Debouncer Consumer Lifetime

## Overview

The per-URI debouncer follows both consumer kinds. A peer-list emptiness no
longer stops it while a lane is bound, and the last lane close with no peer
stops it under the same `DashMap` entry guard that `ensure_debouncer` takes,
so the stop decision and the stop are one step with respect to an ensure on
the same key. The link is two composition-installed closures: a presence query
in the registry (weak lane adapter) and a close sink extended with the URI
(weak registry). `sub_open`, `sub_close`, `resources/subscribe` and
`resources/unsubscribe` keep their signatures.

The spike at `/var/tmp/ssh-mcp/deb-spike` carries code, tests, documents and
`session.md` to a green gate. `research.md` carries the base results, the gate
table, the stability counts, the nine mutation checks, the validate result, the
s20/s07 runs and the patch.

## Technical Approach

### 1. Where the link lives (FR-001 to FR-003, FR-006; Q-6, Q-7, Q-9; D-1)

Chosen: **(a) composition-installed closures.** A `LanePresence` closure
(`Arc<dyn Fn(&str) -> bool + Send + Sync>`) is installed in the registry by
`build_use_cases` and captures `Weak<SubscriberLaneAdapter>`; the existing
`CloseSink` gains the URI, and the closure installed next to the mux sink
captures `Weak<MemoryRegistry>` and calls `release_if_idle_uri`. The registry
keeps one stop predicate, `release_if_idle_uri`, and `unsubscribe_sync` calls
it.

Alternatives, measured against P-01 (`constitution.md:89-91`) and the
existing sinks:

- **(b) Methods on the two ports that already connect the adapters.**
  `LaneNotifierBridge` (`src/ports/notifier.rs:138-155`) gains a presence
  query implemented by `LaneFanoutBridge` (it already holds
  `Arc<SubscriberLaneAdapter>`); `DebouncerActivator` (`:86-90`) gains
  `release_if_idle`. The lane adapter would hold `Weak<dyn DebouncerActivator>`
  and call it from `close_lane`. Cost: two port traits, the bridge impl, a new
  adapter field and its install, plus the same composition lines; the adapter
  ends up calling a port (allowed) but also needs a second weak handle.
- **(c) A new port.** A `LaneLifetime` trait with `lane_present` and
  `release_if_idle`, implemented by the registry and held weak by the adapter.
  Clearest vocabulary, one new file and the same wiring as (b); rejected as
  more surface than two closures need.
- **(d) A per-URI lane count in the registry.** A `DashMap<String,
  AtomicUsize>` incremented after the lane insert (`subscriber_lane.rs:543-544`)
  and decremented only when `remove_lane` removed (`:560-570`), with no
  increment on the global reservation (`:811`) or a per-URI cap refusal
  (`:537-542`). Rejected: it duplicates the consumer test the registry can ask
  for and adds a reset rule on every early return.

(a) changes `install_close_sink` (one caller, inside the builder,
`prod.rs:700`), the `CloseSink` type and `fire_close_sink` (one call site,
`subscriber_lane.rs:830`), adds `LanePresence`, the field,
`install_lane_presence`, the single `consumers_empty` predicate (taking the
query the caller loaded), `release_if_idle_uri` and `remove_debouncer`, plus
the one link builder in `lane_bridge.rs`. No use case changes. No adapter imports
`crate::application`; the adapter calls no other adapter. Both captured handles
are weak, so no cycle forms and feature 005's
`adapter_and_bridge_are_released_after_drop` (`lane_bridge.rs:1244-1259`) stays
green; the release test of FR-005 proves the registry side. The producer path
is untouched: the presence query runs only inside a stop, and a stop runs on a
lane close or on a peer-list emptiness, never on `record_bytes` or `poke`.

### 2. The atomic stop (FR-004, Q-10; D-2)

`debounce_tasks` is the single source of truth for "a task exists". One
private `remove_debouncer` performs the four removals; `release_if_idle_uri`
calls it. No test-only copy exists, so tests stop tasks through the paths
production uses.

`release_if_idle_uri` takes `debounce_tasks.entry(key)`. A vacant entry
returns. An occupied entry evaluates both consumer kinds while holding the
shard write guard: peer list empty (`subscribers.get(uri)`) and no lane (the
presence query answers `false`). If either has a consumer, the entry stays and
the guard drops. Otherwise it removes `wakers`, `flush_now` and `bytes_since_flush`
first, then removes the entry and aborts the handle.

`ensure_debouncer` takes the same entry. Occupied returns; vacant re-checks
both consumer kinds under the guard and, when they are empty, returns without
spawning, so an opener that lost its consumer creates nothing. Otherwise it
builds and inserts the waker, flush, counter and task, then inserts the handle.
Spawning holds no `.await`. The presence query is loaded once per decision. A
registry with no presence query skips the pre-spawn check, so an explicit
ensure keeps its task and only a peer unsubscribe stops it.

Interleavings of FR-004:

1. **`sub_open` inserts the lane, then ensures.** If the stop holds the entry
   and reads the lane table after the insert, it sees the lane and keeps the
   task; if it reads before, it removes, and the opener's ensure finds the
   entry vacant, re-checks the now-empty consumers under the guard and returns
   without spawning. There is no early return before the consumer is visible.
2. **`resources/subscribe` inserts the peer entry, then ensures.** Same
   argument against `subscribers`; the peer insert completes before the
   ensure.
3. **A peer unsubscribe races a `sub_open`.** The stopper holds the entry
   while it reads both tables; the opener either is visible or runs after the
   removal, re-checks under the guard and spawns nothing.

Because removal of the other three maps happens under the entry guard and
before the entry, a task is never left without its waker or flush entry. Q-10b
fails on the base tree (`left: true, right: false`, a task without its waker);
the removal order repairs it. No `Mutex` is taken; the shard write lock of the
entry is the only exclusion and the producer (`record_bytes`, `poke`) reads
the maps lock-free.

### 3. What a stop removes (Q-8; D-3)

The four maps `stop_debouncer` owns today: `debounce_tasks`, `wakers`,
`flush_now`, `bytes_since_flush`. `sequence_counters` and `event_tails` stay:
`next_seq`/`current_seq` number and report events for a later
`resources/read` (`read_resource.rs:595`, `:632`, `:673`), and
`tail_snapshot` serves forward history (`:716`); resetting either is a wire
change. Their unbounded growth is recorded in `session.md` and repaired where
the resource ends.

### 4. `close_lane` and the sink (D-4)

`remove_lane` returns the removed `Arc<LaneState>` (`subscriber_lane.rs:560-570`),
so `close_lane` uses `lane.uri` from that value: no second lookup can disagree
with the removal. The mux unregister and the release sink fire only after a
successful removal, once; the early return answers `SubNotFound`.

### 4a. Lock order (D-8)

One nesting order holds everywhere: the `debounce_tasks` entry (write shard),
then a `subscribers` read, then `by_uri` and `lanes` reads through the presence
query, then the three wakeup maps (`bytes_since_flush`, `wakers`, `flush_now` —
independent shards, so their order among themselves is free). No path takes the
entry while holding a `subscribers`, `by_uri` or `lanes` guard:
`insert_subscriber` and `unsubscribe_sync` release the `subscribers` shard
before they call the ensure or the release. `allocate_lane_checked` and
`snapshot_lanes_for_uri` nest `by_uri` then `lanes`; `remove_lane` is
sequential — `lanes.remove` returns before `by_uri.get_mut` — so the two orders
never interleave inside one guard. `consumers_empty` is called with the entry
held and takes the `subscribers` read, then the query takes `by_uri` and
`lanes`. `entry.insert` consumes the entry and drops the guard
(`memory_registry.rs:501`). The wakeup maps are removed while the entry guard
is still held, so a concurrent ensure waits on the shard and rebuilds all four
entries together (`remove_debouncer`, `:334-348`).

| Path | Guard held | Released before | Anchor |
| --- | --- | --- | --- |
| `consumers_empty` | entry (caller), then `subscribers` read, then `by_uri`+`lanes` read | returns | `:288-301` |
| `release_if_idle_uri` | entry, then the consumer read | entry dropped at `occupied.remove` | `:310-332` |
| `remove_debouncer` | entry passed in, held across the three map writes | caller drops at `occupied.remove` | `:334-348` |
| `ensure_debouncer` | entry, then the consumer read, then `bytes_since_flush`, `wakers`, `flush_now` | `entry.insert` at `:501` | `:455-502` |
| `debouncer_task` | none on registry maps; `bytes_since_flush` swap in `broadcast_and_reset` | returns | `:1004-1045` |
| `insert_subscriber` | `subscribers` shard | returns; callers ensure after | `:662-705` |
| `unsubscribe_sync` | `subscribers.get_mut` in `is_some_and` | closure ends, `remove`, then release | `:528-561` |
| `broadcast` | `subscribers` shard | `Vec` collected, before notify | `:1058-1087` |
| `gc_closed_peers` | `subscribers` iter | collects ids, calls `drop_peer_sync` after | `:886-901` |
| `drop_peer_sync` | none | calls `unsubscribe_sync` per URI | `:513-518` |
| `poke` | `wakers` read | returns | `:761-765` |
| `record_bytes` | `bytes_since_flush` entry | drops before the `flush_now` read | `:767-793` |
| `record_bytes_with_tail` | none on registry maps | bridge load, `event_tails` rcu | `:795-826` |
| `snapshot_lanes_for_uri` | `by_uri` then `lanes` reads | returns | `subscriber_lane.rs:453-461` |
| `allocate_lane_checked` | `by_uri` write, then `lanes` write | block ends before open returns | `:520-547` |
| `remove_lane` | `lanes` write, then, after it returns, `by_uri` write | function end; close sink after | `:560-570` |
| `close_lane` | none | fires the sink after `remove_lane` | `:824-834` |
| close sink | none | calls release | `lane_bridge.rs:552-572` |

The anchors were printed with:

```
cd /var/tmp/ssh-mcp/deb-spike
grep -n 'fn consumers_empty\|fn release_if_idle_uri\|fn remove_debouncer\|fn ensure_debouncer\|async fn debouncer_task\|fn insert_subscriber\|fn unsubscribe_sync\|async fn broadcast\|fn gc_closed_peers\|fn drop_peer_sync\|fn poke\|fn record_bytes\|fn record_bytes_with_tail\|fn snapshot_lanes_for_uri\|fn allocate_lane_checked\|fn remove_lane\|async fn close_lane\|fn install_lane_lifetime' \
  src/adapters/subscription/memory_registry.rs src/adapters/subscription/subscriber_lane.rs src/adapters/subscription/lane_bridge.rs
```

and each cited range re-read with `sed -n '<start>,<end>p'` to confirm the
function boundaries.

### 5. The production proof (FR-006, Q-11; D-5)

`build_use_cases` becomes a thin public wrapper over a private
`build_use_cases_inner` that also returns the one handle the test reads,
`Arc<MemoryRegistry<RmcpNotifier>>`. Nothing new is public. A
`#[cfg(test)] mod tests` in `src/composition/prod.rs` destructures the tuple,
opens a lane through `use_cases.sub_subscribe`, closes it through
`use_cases.sub_unsubscribe`, and asserts `has_debouncer` false. The module
carries the crate's standard attribute:

```rust
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests use unwrap for brevity per AGENTS.md test policy"
)]
mod tests {
```

A unit test may call the composition because nextest runs every test in its own
process (`.config/nextest.toml`), so the process-wide forwarder installed by
`build_use_cases` (`prod.rs:541`) is replaced only inside that process; `cargo
test` is not used in this project.

### 6. Tests (D-6)

Every wait is bounded: `tokio::time::timeout` or a 4 s deadline loop at 20 ms
steps. Fixtures use fixed timestamps. No test under `src/adapters/` imports
`crate::application`; the two cascade tests live in the application modules.

| Test | Module | Builds | Asserts | Bound / determinism | FR | Base |
| --- | --- | --- | --- | --- | --- | --- |
| `peer_unsubscribe_keeps_the_debouncer_a_lane_still_needs` | `crate::adapters::subscription::memory_registry::tests` | registry, lane adapter, bridge, recording notifier, peer entry and lane | lane still listed, task present, one notification to the lane peer | 4 s deadline | FR-001, FR-002 | FAIL |
| `last_lane_close_stops_the_debouncer` | same | one lane, no peer | task and count 0 | none (sync) | FR-002 | PASS |
| `every_open_and_close_cycle_leaves_no_task` | same | 8 cycles | count 0 | none | FR-002 | PASS |
| `last_peer_unsubscribe_without_a_lane_stops_the_debouncer` | same | peer entry only | task and count 0 | none | FR-003 | PASS |
| `open_racing_last_lane_close_keeps_one_debouncer` | same | 200 rounds: L1 open + task, close L1 racing open L2 + ensure | one task, waker, flush, one delivery to L2; then 0 | 200 rounds | FR-004 | FAIL |
| `resources_subscribe_racing_last_lane_close_keeps_one_debouncer` | same | 200 rounds: lane + task, subscribe racing close | task while subscribed, one delivery; 0 after unsubscribe | 200 rounds | FR-004 | FAIL |
| `peer_unsubscribe_racing_sub_open_keeps_one_debouncer` | same | 200 rounds: peer + task, unsubscribe racing open lane + ensure | task, waker, flush, one delivery; 0 after close | 200 rounds | FR-004 | PASS |
| `concurrent_ensure_spawns_one_task` | same | 32 tasks on a `Barrier` | `strong_count` +1, one task, waker, flush | barrier | FR-004 | FAIL |
| `ensure_racing_stop_never_splits_the_task_from_its_waker` | same | 5,000 rounds: lane + task, ensure racing the close of that lane, no peer | no task, no waker and no flush entry every round, since the lane is closed and every task joined; a final live consumer ends with all three and one delivery | fixed loop; 0.6 s | FR-004 | FAIL |
| `a_stop_that_holds_the_entry_makes_the_opener_rebuild_every_map` | same | query blocks inside the entry; stop on a thread, ensure on another, parked on the shard first | one task, waker, flush, one delivery, query called twice | channels, 2 s; 10 ms park wait | FR-004 | FAIL |
| `ensure_after_the_last_consumer_left_spawns_nothing` | same | one lane closed while the entry is vacant, then ensure | no task, no strong handle taken, and the three maps empty | none | FR-002 | FAIL |
| `registry_is_released_after_the_last_consumer_leaves` | same | one lane, weak registry, close then drop | `Weak::upgrade` becomes `None` | 2 s deadline, 50 ms | FR-005 | PASS |
| `session_cascade_stops_child_debouncers_and_spares_the_health_one` | `crate::application::disconnect_session::tests` | session, shell, lane, sink | child task 0; health task 1 | none | FR-002, Q-12 | PASS |
| `agent_cascade_stops_child_debouncers_and_spares_the_health_one` | `crate::application::disconnect_agent::tests` | same for the agent | same | none | FR-002, Q-12 | PASS |
| `production_wiring_stops_the_debouncer_when_the_last_lane_closes` | `crate::composition::prod::tests` | `build_use_cases_inner` | task 0 after `sub_close` | none | FR-006 | PASS |
| `ensure_without_a_presence_query_keeps_the_task` | `crate::adapters::subscription::memory_registry::tests` | bare registry | task survives the ensure | none | FR-012 | PASS |
| `peer_unsubscribe_without_a_presence_query_stops_the_task` | same | bare registry, peer entry | task 0 after unsubscribe | none | FR-012 | PASS |
| `per_resource_maps_keep_the_sequence_and_tail_after_a_stop` | same | 8 cycles and releases | `debounce_tasks`, `wakers`, `flush_now` empty; `sequence_counters` and `event_tails` 8; `bytes_since_flush` 8 under the forward key | none | FR-013 | PASS |
| `bytes_written_before_a_debouncer_exists_are_delivered_by_the_next_wakeup` | same | no task, below and above threshold | one URI notification on the next wakeup | 4 s deadline | FR-014 | PASS |
| `forward_tail_survives_without_a_task` | same | bare registry, forward tail | the forward tail is readable with no task; it proves the history is not the debouncer's | none | FR-014 | PASS |

The Base column is the base5 run of `research.md` §2: `FAIL` marks a test that
pins the change, `PASS` a behaviour the base copy already has through the API
shim. The shim includes the close sink, so `last_lane_close_…`,
`every_open_and_close_cycle_…` and `peer_unsubscribe_racing_…` pass there and
are held by mutation (b) instead. All 20 pass on the spike.

The three race tests loop a fixed 200 times and assert the invariant every
round; the concurrent-ensure test synchronises 32 tasks on a `Barrier`; the
stability run repeats each test 200 times, and 500 times for
`peer_unsubscribe_racing_sub_open_keeps_one_debouncer`.
`scripts/test_v47_subscriptions.py` was run 20 times per copy and is known
flaky on its own (4/20 base, 2/20 prototype, all at `:648`); a bounded re-read
would cure it and is recorded in `session.md`.

### 7. Documents (D-7)

| File:line | Today | Required |
| --- | --- | --- |
| `docs/OPERATIONS.md:789` | "Last unsubscribe per URI aborts the debouncer task." | "Last consumer per URI (peer or lane) aborts the debouncer task." |
| `CHANGELOG.md:17` | — | one `### Changed` bullet: the task follows both consumer kinds; one spawn per key |
| `session.md` | defects recorded | repair marks on the four debouncer entries and the ensure entry; every citation into a changed file re-anchored on the final spike tree |
| `docs/CONFIGURATION.md:124-133` | debouncer timing table | no lifetime claim; stays |
| `docs/ARCHITECTURE.md:388,689` | diagrams name the debouncer | no stop condition; stays |
| `docs/adr/0003-lifecycle-binding.md:11`, `docs/adr/0004-channel-mux-fairness.md:143-155` | accepted history | stays |
| `docs/TOOL_CATALOG.md`, `docs/tool-catalog.json` | frozen | unchanged |

### 8. Gate (FR-008, FR-009; D-9)

The FR-008 command list plus `cargo check --all-features --all-targets`,
`cargo build --bins` and `make tools-export-check`, one cargo invocation at a
time, then `speckit validate` in the spike. Expected totals: 2154 + 20 = 2174
lib and 2329 + 20 = 2349 with integration; the two scripts at 63 and 1 passed.
Any difference is explained before the next step.

## Decisions

### D-1 Link shape and home

Chosen (a). Rejected (b) two port methods, (c) a new port, (d) a counter, for
the reasons and costs in §1. Both handles are weak; no cycle.

The one builder, `install_lane_lifetime` (`lane_bridge.rs:554`), lives in
`src/adapters/subscription/`: that module already holds the lane adapter and is
installed into the registry, so naming both needs no import of
`crate::composition` from `src/adapters` or `src/application` (P-01; no file
of those layers imports composition today except through this builder). The
composition root calls it and composes the mux unregister through `on_close`.
The no-op notifier they share lives in `src/adapters/subscription/fake.rs`,
gated `#[cfg(any(test, feature = "test-fixtures"))]` like
`src/adapters/ssh/fake.rs`; it is reachable from every layer without
`crate::composition`.

### D-2 Atomic rule

`debounce_tasks` entry is the single source of truth; the conditional removal
and the ensure both take it, and both decide under it; the other three maps
are removed under it, before the entry. §2 replays the three interleavings.
The post-ensure release of the delivery-2 plan is gone: the decision happens
before the spawn, so a task is never spawned and aborted in the same call.

### D-3 Stop scope

Four maps. `sequence_counters` and `event_tails` stay; the growth is recorded
in `session.md`.

### D-4 Close path

The removed `Arc<LaneState>` carries the URI; sinks fire once, after success;
mux first.

### D-5 Production proof

Private `build_use_cases_inner` plus a `prod.rs` test module; the builder is
`pub` in the adapter module, and nothing else new is public.

### D-6 Tests

The table of §6 is the complete set; bounds and determinism are stated per row.

### D-7 Documents

Only `session.md`, `docs/OPERATIONS.md:789` and `CHANGELOG.md:17` change.

### D-8 Lock order

Tasks entry, then `subscribers`, then `by_uri`; guards released before the next
map; `entry.insert` drops the guard before the re-check. §4a lists each path.

### D-9 Gate

The FR-008 list plus the three build/check commands and the catalogue check.
FR-012 to FR-014 are proven by the no-query, per-resource-map and
byte-delivery tests of §6.

## File Map

| File | Requirement |
| --- | --- |
| `src/adapters/subscription/memory_registry.rs` | FR-001 to FR-005, FR-012 to FR-014 (presence, release, pre-spawn decision, no-query rule, maps, bytes; tests) |
| `src/adapters/subscription/subscriber_lane.rs` | FR-002, FR-004 (CloseSink URI, fire site) |
| `src/adapters/subscription/lane_bridge.rs` | FR-006 (the one `install_lane_lifetime` builder, weak handles) |
| `src/adapters/subscription/fake.rs` | FR-006 (shared `StubNotifier`, gated like `ssh/fake.rs`) |
| `src/adapters/subscription/mod.rs` | FR-006 (the gated `fake` module) |
| `src/composition/prod.rs` | FR-006 (calls the builder, composes the mux unregister, inner split, test) |
| `src/application/disconnect_session.rs`, `src/application/disconnect_agent.rs` | FR-002, Q-12 (tests) |
| `docs/OPERATIONS.md`, `CHANGELOG.md` | D-7 |
| `session.md` | FR-010 (repair marks, re-anchoring) |

No other file changes.

## Verification Plan

| # | Command | Expected |
| --- | --- | --- |
| 1 | `cargo fmt --all -- --check` | exit 0 |
| 2 | `cargo check --all-features --all-targets` | exit 0, warnings 0 |
| 3 | `cargo clippy --all-features -- -D warnings` | exit 0 |
| 4 | `cargo build --no-default-features --target-dir target/no-default-features` | exit 0 |
| 5 | `cargo build --bins` | exit 0 |
| 6 | `cargo nextest run --test-threads num-cpus --lib` | 2174 passed |
| 7 | `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` | 2349 passed |
| 8 | `cargo audit` | exit 0, the one allowed warning |
| 9 | `make tools-export-check` | both `unchanged` |
| 10–14 | the five Python suites | 15 / 2 / 10 / 20 / 9 passed |
| 15 | the two scripts | 63 passed; 1 passed |

Mutation checks: §5 of `research.md` (peer-only stop, no lane stop, link
removed, non-atomic ensure, strong registry handle, no-query rule removed, and
the three race mutations g, h, i).

## Constitution

- **D-01 spec first** — the change follows the clarified specification.
- **D-03/P-01 layering** — closures installed by composition; no adapter
  imports `crate::application`; no adapter calls another adapter.
- **D-05 calisthenics** — one registry predicate, one removal helper, one link builder.
- **D-06 FQCN** — new imports use `crate::…`.
- **D-07 lints** — no lint disabled; only the standard test-module attribute.
- **D-08/P-03 fail loud** — production stays panic-free.
- **D-11 stable contracts** — no wire change.
- **D-12 small changes** — one concern, revertible as one patch.
- **P-02 lock-free** — no `Mutex`; the entry shard is the only exclusion.
- **P-04/P-05 frozen wire** — catalogue untouched.
- **P-06 gate** — the verification plan is the gate.

## Rollback

Reverse-apply `/var/tmp/ssh-mcp/deb-spike.patch` as one unit. It is generated
from `/var/tmp/ssh-mcp/final-007/tree` in `a/` and `b/` form and carries the
files of the File Map, the tests and the three documents: `11 files changed,
1150 insertions(+), 54 deletions(-)`, SHA-256
`8737f2c57e4538ffed147550d6ff0d0f21e9353adc250ead77def92d2e1ecc4f`.
`patch -p1 --dry-run` in a scratch copy of that tree exits 0.

## Open Decisions

Q-6, Q-7 and Q-9 are settled by D-1. FR-012 to FR-014 are covered by the
table. The owner-review items stay: the split and the boundary, and the
out-of-scope defects recorded in `session.md`.
