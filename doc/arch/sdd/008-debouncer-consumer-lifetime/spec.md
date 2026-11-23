---
id: 01a2035b-0e74-7c21-9713-4f4fbbae9b66
number: 008
slug: debouncer-consumer-lifetime
status: implemented
created_at: 2026-11-22T13:03:48.852788769Z
---
# Feature Specification: Debouncer Consumer Lifetime

Feature: 008-debouncer-consumer-lifetime
Created: 2026-11-22

A debouncer of `(kind, resource_id)` serves two kinds of consumer: the peer
subscriptions of `MemoryRegistry` (`resources/subscribe`) and the lanes that
`sub_open` opened on the same URI. Today the debouncer follows the peer list
alone. `unsubscribe_sync` stops it as soon as the URI's peer vector becomes
empty (`src/adapters/subscription/memory_registry.rs:440-476`; the stop is at
`:459-467`) and never asks whether a lane remains, so a lane that is still
bound stops receiving its byte-threshold, debounced, force-flush and keepalive
notifications. In the other direction, a lane that is closed stops nothing:
`UnsubscribeUseCase::execute` closes the lane and touches no debouncer
(`src/application/subscription_admin.rs:273-293`, close at `:286`), and
`stop_debouncer` has that one caller (`memory_registry.rs:412-420`; LSP
`findReferences` returns the declaration and `:466`). This feature makes the
debouncer follow both consumer kinds and die with the last one.

Every fact in this specification was measured on 2026-11-22 on the tree of
feature 007, in the throwaway copy `/var/tmp/ssh-mcp/end-probe` made from
`/root/dev/ssh-mcp`. Every `file:line` is the repository line, printed in the
same step that wrote it here. The reproduction tests are **not in the tree**;
they are kept in `/var/tmp/ssh-mcp/end-repro.patch` on the session host with
its run log `/var/tmp/ssh-mcp/end-repro-run.log`. No production code changes in
this phase.

## Why

- A lane and a peer subscription share one debouncer per URI. When the peer
  leaves, the lane is silently orphaned: `sub_stats` keeps answering, the
  resource keeps producing, and nothing wakes the lane. The reproduction
  `crate::adapters::subscription::memory_registry::tests::peer_unsubscribe_stops_the_debouncer_the_lane_still_needs`
  (not in the tree; `/var/tmp/ssh-mcp/end-repro.patch`) pins the current
  behavior: after `SubscriberRegistryAsync::unsubscribe` the lane is still
  listed, `has_debouncer` is false, and a byte-threshold write plus a `poke`
  produce no notification after 1.3 s.
- The inverse leak is measured in the same patch: a lane close leaves the task
  and four map entries behind
  (`...::debouncer_task_survives_lane_close_and_the_end_of_its_resource`) and
  eight open/close cycles leave eight tasks
  (`...::every_open_and_close_cycle_on_a_distinct_id_leaks_one_task`). An HTTP
  session builds one composition per session
  (`src/composition/prod.rs:762-774`), and the task holds a strong
  `Arc<MemoryRegistry>` taken from `self_ref.upgrade()`
  (`memory_registry.rs:376-380`, moved into `tokio::spawn` at `:401-408`), so
  the registry outlives the composition
  (`...::debouncer_task_keeps_the_registry_alive_after_all_handles_drop`).
- `DebouncerActivator` exposes only `ensure_for_uri`
  (`src/ports/notifier.rs:86-90`): nothing releases. `sub_open` calls it after
  the lane is inserted (`src/application/subscription_admin.rs:157-163`), so
  the open side is already ordered behind the insert; the close side has no
  link at all.
- The lane adapter already has a close notification that composition installs:
  the `CloseSink` type (`src/adapters/subscription/subscriber_lane.rs:366`),
  `install_close_sink` (`:404-406`), `fire_close_sink` (`:409-414`) and its
  call in `close_lane` (`:825-829`). Composition wires it to the mux
  (`src/composition/prod.rs:337-344`) and the registry is reachable there as
  the lane bridge (`:362-366`). The missing link is one adapter-side signal and
  one registry-side decision.

## Decision provenance

The split of the measured problem into features and the boundary of this
feature were decided by the orchestrator under the owner's instruction to
proceed, and are registered for the owner's review. The mechanism (how the
registry learns that a lane was closed) is **not** decided here: it is a plan
decision, and the plan chooses the composition-installed closures (Option A,
`plan.md` D-1). [ADR 0008](../../adr/0008-debouncer-consumer-lifetime.md)
records the context, the drivers, the options with what is measured about each,
and the constraints. The repair of the resource-end leaks
(shell close, command terminal state, transfer, rsync, serial, forward, remote
EOF), of the peer/HTTP end, of the health-lane escape, of the cascade cost, of
the guard/open window and of the lane lifetime is out of scope and named below.

## Behavior after the feature

1. A debouncer is not stopped while any consumer of either kind remains. A
   `resources/unsubscribe`, a `drop_peer` or a peer GC that empties the peer
   list of a URI leaves the task running when a lane on that URI still exists,
   and the lane keeps receiving every flush kind.
2. The debouncer is stopped when its last consumer goes, whichever kind it was:
   the last lane on the URI is closed (by `sub_close`, by the session-disconnect
   and agent-disconnect cascades, or by any other caller of the lane adapter's
   close) while no peer subscription remains, or the last peer subscription is
   removed while no lane remains. Stopping removes the task and its map entries
   exactly as `stop_debouncer` does today (`memory_registry.rs:412-420`).
3. No interleaving of an open and a close on the same URI leaves a live
   consumer without a debouncer, or a debouncer without a consumer. The three
   races are stated in FR-004 with the rule that makes each safe. The rule
   holds no `Mutex` on a path a producer takes; `DashMap`, atomics, `Notify`
   and channels are allowed (P-02,
   `doc/arch/memory/constitution.md:93-96`).
4. Once every consumer of a registry is gone and every external handle is
   dropped, no debouncer task keeps the registry alive
   (`...::debouncer_task_keeps_the_registry_alive_after_all_handles_drop`
   shows the task holds the last `Arc` today). This is about the state after
   consumers left through point 2; a lane that is never closed is out of scope.
5. No wire change: no tool, argument, answer line, notification or environment
   variable changes; `make tools-export-check` stays `unchanged`.

## The two consumer kinds and the link

Facts the plan needs, each read from the code:

- The peer kind lives in `MemoryRegistry::subscribers`
  (`memory_registry.rs:123-124`) and is removed by `unsubscribe_sync`
  (`:440-476`), reached from `SubscriberRegistryAsync::unsubscribe` (`:846-848`)
  and `drop_peer`/`drop_peer_sync` (`:425-430`, `:850-852`).
- The lane kind lives in `SubscriberLaneAdapter::lanes`/`by_uri`
  (`src/adapters/subscription/subscriber_lane.rs:315-319`) and is removed by
  `remove_lane` (`:560-570`), which knows `lane.kind` and `lane.uri` before the
  removal and drops the global slot after it.
- `close_lane` (`:824-833`) fires the close sink after a successful removal;
  the sink receives only `&SubId` today (`:366`, `:409-414`).
- Composition installs the mux close sink at `src/composition/prod.rs:341-344`
  and keeps the registry as `subscribers` (`:293`) and as the lane bridge
  (`:362-366`, `install_lane_bridge` at `memory_registry.rs:258-264`; the field
  is `:181-186`).
- P-01 (`doc/arch/memory/constitution.md:89-91`): an adapter never calls
  another adapter directly, so the link goes through a port or through a sink
  composition installs. No adapter imports `crate::application`.
- A strong cycle is measured: feature 005's design A, an adapter-owned
  `Arc<dyn LaneResumeSink>`, failed
  `adapter_and_bridge_are_released_after_drop`
  (`doc/arch/sdd/005-subscription-pause-delivery/research.md:46-70`, `:163`;
  the test is `src/adapters/subscription/lane_bridge.rs:1244-1259`). A sink
  that captures `Arc<MemoryRegistry>` while the registry holds the bridge and
  the bridge holds the adapter would reintroduce it; the plan must state which
  side is weak.
- `stop_debouncer` removes `debounce_tasks`, `wakers`, `flush_now` and
  `bytes_since_flush` (`memory_registry.rs:412-420`); `ensure_debouncer`
  reuses an existing counter when present (`:387-392`). `sequence_counters`
  and `event_tails` are not removed by `stop_debouncer`.

## Symbols whose behavior changes

`findReferences` was run on each free function before writing this list. Trait
methods declared in `src/ports/` did not answer `findReferences` (the LSP server
indexed free functions; `hover` on `LaneAdmin` was also empty), so the three
trait-call sites were found with a text search and are marked as such.

### `stop_debouncer`

`memory_registry.rs:412`. `findReferences`: `:466` (the only call) and `:412`
(declaration). After the feature the stop happens only when both consumer kinds
are empty; the function becomes `release_if_idle_uri` (public, called from the
peer path and the close sink) and the four removals move to the private
`remove_debouncer`.

### `ensure_debouncer`

`memory_registry.rs:371`. `findReferences`: `:499`
(`subscribe_with_sub_id`), `:655` (`ensure_for_uri`), `:841`
(`SubscriberRegistryAsync::subscribe`) and `:371` (declaration). It keeps the
spawn; FR-004 fixes the return-early race.

### `unsubscribe_sync`

`memory_registry.rs:440`. `findReferences`: `:428` (`drop_peer_sync`), `:847`
(`unsubscribe`) and `:440` (declaration). Its stop branch becomes conditional
on the other consumer kind, through the presence query.

### `ensure_for_uri`

`src/ports/notifier.rs:89` (trait), `memory_registry.rs:651` (impl).
`findReferences` on the impl returns only `:651`; text search finds the caller
`src/application/subscription_admin.rs:162` and the test stub `:1721`.

### Lane close path

`LaneAdmin::close` is declared at `src/ports/subscriber_lane.rs:126` and
implemented at `subscriber_lane.rs:736-738`; the `SubscriberLaneAsync`
implementation is `:824-833`. Text search for `.close(` finds the three
production callers: `src/application/subscription_admin.rs:286`,
`src/application/disconnect_session.rs:186`, `src/application/disconnect_agent.rs:187`
(the last two inside the duplicated `close_lanes_for_resource`). The close sink
type and its install/fire sites are listed above.

### `CloseSink`

`subscriber_lane.rs:366`; `findReferences`: `:336` (field), `:389`
(initialization), `:404` (install). The signature is `Fn(&SubId)` today; the
plan grows it with the URI (`plan.md` D-1).

## Test fates

### Existing tests whose subject changes

Built from the references above and from the tests that drive
`unsubscribe_sync`, `ensure_for_uri`, `stop_debouncer` and the lane close.

| Existing test | File:line | Asserts today | Fate |
| --- | --- | --- | --- |
| `unsubscribe_clears_snapshot_and_progress` | `memory_registry.rs:1286` | the peer entry and cursors are removed | kept — the removal is unchanged; the debouncer stop becomes conditional |
| `unsubscribe_idempotent_returns_false_on_second_call` | `memory_registry.rs:1310` | second unsubscribe is `false` | kept — return value unchanged |
| `unsubscribe_clears_sub_progress` | `memory_registry.rs:1525` | `(sub_id, uri)` cursors are removed | kept |
| `unsubscribe_frees_slot_for_next_subscribe` | `memory_registry.rs:1709` | unsubscribe frees a cap slot | kept |
| `re_subscribe_by_same_peer_does_not_consume_extra_slot` | `memory_registry.rs:1751` | a refresh reuses the entry | kept |
| `drop_peer_clears_all_uris` | `memory_registry.rs:1355` | `drop_peer` clears every URI of the peer | kept |
| `gc_closed_peers_drops_only_closed_handles` | `memory_registry.rs:1385` | GC drops only closed peers | kept |
| `record_bytes_fires_at_threshold_boundary` | `memory_registry.rs:1184` | a threshold write wakes the task | kept — the task is started by the same test |
| `record_bytes_does_not_fire_below_threshold` | `memory_registry.rs:1162` | below threshold no flush | kept |
| `close_lane_then_again_returns_sub_not_found` | `subscriber_lane.rs:964` | second close is `SubNotFound` | kept — the sink fires once on the successful close |
| `rx_sink_receives_receiver_on_open` | `subscriber_lane.rs:1291` | the rx sink fires on open | kept |
| `open_lane_enforces_max_per_uri`, `open_lane_enforces_max_total` | `subscriber_lane.rs:1137`, `:1170` | cap refusals | kept |
| `unsubscribe_closes_lane_and_returns_uri` | `subscription_admin.rs:829` | `sub_close` removes the lane | kept — no peer is registered, so the task now stops; the test does not observe it |
| `unsubscribe_unknown_returns_sub_not_found` | `subscription_admin.rs:862` | unknown id is `SubNotFound` | kept |
| `unsubscribe_outcome_carries_uri_when_lane_known` | `subscription_admin.rs:1421` | the outcome echoes the URI | kept |
| `adapter_and_bridge_are_released_after_drop` | `lane_bridge.rs:1245` | adapter and bridge are released | kept — the release argument of feature 005 still holds |
| `refused_sub_open_leaves_no_lane_and_no_debouncer` | `subscription_admin.rs:1829` | a refused open starts no task | kept — the open order is unchanged |
| `tests/chaos_inline_push.rs` lane lifecycle calls | `:221`, `:406`, `:447` | inline-push behavior | kept |
| `tests/v5_smoke.rs` sub_open calls | `:88`, `:577`, `:688`, `:786`, `:910` | sub_open behavior | kept |

No existing test is removed or renamed. The four tests of the patch below are
adapted in place when they land.

### Tests of `/var/tmp/ssh-mcp/end-repro.patch`

The measurement patch is **not in the tree**; it lives on the session host at
`/var/tmp/ssh-mcp/end-repro.patch` with its run log
`/var/tmp/ssh-mcp/end-repro-run.log`. Every test it holds has one fate:

| Patch test | File in the patch | Fate |
| --- | --- | --- |
| `peer_unsubscribe_stops_the_debouncer_the_lane_still_needs` | `src/adapters/subscription/memory_registry.rs` | becomes the proving test of FR-001 and FR-002; the two debouncer assertions flip |
| `debouncer_task_survives_lane_close_and_the_end_of_its_resource` | `src/adapters/subscription/memory_registry.rs` | becomes the proving test of FR-002; renamed `last_lane_close_stops_the_debouncer` |
| `every_open_and_close_cycle_on_a_distinct_id_leaks_one_task` | `src/adapters/subscription/memory_registry.rs` | becomes the proving test of FR-002; renamed `every_open_and_close_cycle_leaves_no_task` |
| `debouncer_task_keeps_the_registry_alive_after_all_handles_drop` | `src/adapters/subscription/memory_registry.rs` | becomes the proving test of FR-005; closes the lane before dropping the handles |
| `lane_bridge_delivers_to_a_lane_whose_peer_is_closed` | `src/adapters/subscription/memory_registry.rs` | not added — peer end is out of scope (F7) |
| `close_lanes_for_resource_scans_the_whole_lane_list` | `src/application/disconnect_session.rs` | not added — cascade cost is out of scope (F7) |
| `agent_disconnect_uses_the_same_cascade_and_spares_the_health_lane` | `src/application/disconnect_agent.rs` | not added — health lane and duplicate cascade are out of scope (F7) |
| `closing_the_shell_releases_its_lane_and_stops_its_debouncer` | `src/application/subscription_admin.rs` | not added — resource end is out of scope (F7) |
| `completing_a_command_releases_its_lane_and_stops_its_debouncer` | `src/application/subscription_admin.rs` | not added — resource end is out of scope (F7) |
| `command_completion_does_not_flush_pending_bytes_before_the_terminal_write` | `src/application/subscription_admin.rs` | not added — last notification is out of scope (F7) |
| `shell_close_does_not_flush_pending_bytes_before_the_terminal_write` | `src/application/subscription_admin.rs` | not added — last notification is out of scope (F7) |
| `a_resource_removed_between_guard_and_open_still_gets_a_lane` | `src/application/subscription_admin.rs` | not added — the guard/open window is out of scope (F7) |
| `unknown_resource_lane_receives_delivery_once_the_resource_appears` | `src/application/subscription_admin.rs` | not added — guard behavior is out of scope, but it stays the delivery proof for a lane started by the activator |
| `closing_the_shell_leaves_its_lane_open`, `completed_command_leaves_its_lane_open`, `closed_peer_leaves_its_lane_open`, `session_disconnect_closes_command_shell_and_transfer_lanes_but_not_health`, `open_and_end_cycles_leak_one_lane_each_until_the_cap_refuses`, `open_and_end_cycles_exhaust_the_global_cap`, `subscribe_unknown_resource_returns_resource_not_found` | `src/application/subscription_admin.rs` | not added — F7 defects |
| `auto_close_lane_is_reaped_after_grace_window`, `lease_lane_outlives_its_ttl` | `src/application/subscription_admin.rs` | not added — F2 |
| `oversized_filter_is_accepted_by_sub_open`, `oversized_regex_compiles_even_past_the_configured_cap`, `nested_group_bomb_compiles_under_the_timeout` | subscription admin / filter | not added — F4 |
| `replay_from_cursor_enqueues_a_snapshot_for_the_consumer`, `sub_replay_answers_ok_into_a_lane_queue_nothing_drains` | subscriber lane / prod | not added — F3 |
| `lifetime_accepts_only_the_snake_case_spelling`, `lag_policy_rejects_non_snake_case_and_unknown_values` | args | not added — F5 |
| `subscribe_op_parses_every_documented_field`, `subscribe_lifetime_is_free_form_but_lag_policy_is_typed`, `subscribe_op_maps_to_resources_subscribe_not_sub_open` | parser | not added — F6 |

## User Stories

- As a host implementer, I want a lane to keep delivering after a peer
  subscription on the same URI goes away, so that one surface leaving does not
  silence the other.
- As an operator, I want a closed lane to release its debouncer, so that
  open/close churn does not accumulate tasks.
- As a maintainer, I want the stop decision written once and safe against the
  three open/close interleavings, so that neither consumer kind can be orphaned.
- As an operator of the HTTP transport, I want a dropped session to free its
  registry once its consumers are gone, so that sessions do not accumulate.

## Out of Scope

Each item is recorded; none is repaired here.

- **Lanes that nobody closes when their resource ends** (shell close, command
  terminal state, transfer, rsync, serial, forward, remote EOF) and the last
  notification before such a close. Recorded in `session.md`; the patch tests
  are listed in the fate table.
- **Lanes of a peer or an HTTP session that went away.** Recorded in
  `session.md`; `lane_bridge_delivers_to_a_lane_whose_peer_is_closed` pins the
  lane side.
- **The session health lane the disconnect cascades skip.** Recorded in
  `session.md`.
- **The cost of `close_lanes_for_resource`.** Recorded in `session.md`; the
  patch test pins one full-list scan for one lane.
- **The window between the guard and the lane open.** Recorded in `session.md`
  and in feature 007's Out of Scope.
- **Lane lifetime (`auto_close`, `lease`).** Recorded in `session.md`.
- **`sub_replay` and the mux drain.** Recorded in `session.md`.
- **Repository rows a remote EOF leaves `Running`.** Recorded in `session.md`.
- **The `unsubscribe_sync` window between the empty check and the remove.** A
  subscriber inserted there is removed with the entry. Reproduced twice in six
  runs, at rounds 75352 and 15209; recorded in `session.md`; the line is
  unchanged.
- **The per-session producer forwarder and the frozen descriptions.** Recorded
  in `session.md`.

## Functional Requirements

### FR-001 A peer-list emptiness alone does not stop the debouncer

`unsubscribe_sync` stops the task only when both consumer kinds are empty. After
`SubscriberRegistryAsync::unsubscribe` (`memory_registry.rs:846-848`) or
`drop_peer`/`drop_peer_sync` (`:425-430`, `:850-852`) removes the last peer of a
URI, a lane still bound to that URI keeps its task and receives the
byte-threshold, debounced, force-flush and keepalive notifications. Peer-side
bookkeeping (subscriber vector, `peer_progress`, `sub_progress`) is unchanged.

**Acceptance.**
`crate::adapters::subscription::memory_registry::tests::peer_unsubscribe_stops_the_debouncer_the_lane_still_needs`
(not in the tree; `/var/tmp/ssh-mcp/end-repro.patch`) is adapted: after the
unsubscribe it asserts the lane is still listed, `has_debouncer` is true, and a
threshold write plus a `poke` deliver one notification within a bounded wait.
It is a `#[tokio::test]`; today it passes pinning the opposite. The debouncer
count is read through the `#[cfg(test)]` accessor `has_debouncer` added by the
measurement; the module reads the four maps through its parent's private
fields.

### FR-002 The last lane close with no peer stops the debouncer

Closing the last lane of a URI through `SubscriberLaneAsync::close_lane`
(`subscriber_lane.rs:824-833`) while no peer subscription remains stops the
task and removes the four map entries `stop_debouncer` owns today
(`memory_registry.rs:412-420`). `sequence_counters` and `event_tails` are not
the debouncer's and survive (`Q-8`). This covers `sub_close`
(`subscription_admin.rs:286`), both cascades
(`disconnect_session.rs:186`, `disconnect_agent.rs:187`) and direct callers.
The close still answers as today; the mux close sink still fires.

**Acceptance.**
`...::last_lane_close_stops_the_debouncer` (adapted from
`debouncer_task_survives_lane_close_and_the_end_of_its_resource`; not in the
tree; `/var/tmp/ssh-mcp/end-repro.patch`) opens one lane, ensures the task,
closes the lane with no peer, and asserts `has_debouncer` false and the task
map empty. `...::every_open_and_close_cycle_leaves_no_task`
(adapted from `every_open_and_close_cycle_on_a_distinct_id_leaks_one_task`)
runs eight cycles and asserts the count 0. Both are `#[tokio::test]` and bound
every wait.

### FR-003 The last peer unsubscribe with no lane still stops the debouncer

The behavior that exists today is kept for the case it was written for: when
the last peer subscription of a URI goes and no lane is bound to that URI, the
task is stopped. A stopped task stays stopped until a consumer returns.

**Acceptance.**
`...::last_peer_unsubscribe_without_a_lane_stops_the_debouncer` is new, a
`#[tokio::test]` in `crate::adapters::subscription::memory_registry::tests`:
subscribe a peer, ensure the task, unsubscribe, assert `has_debouncer` false
and the count 0. It passes on the base tree and stays green.

### FR-004 The three open/close races keep one debouncer per live consumer

The invariant is: while at least one consumer of either kind exists for a URI,
exactly one task exists for it; when the last consumer goes, the task goes. The
invariant must hold under three interleavings:

1. **`sub_open` inserts its lane, then asks for the debouncer**
   (`subscription_admin.rs:157-163`), while the last lane of the URI is being
   closed.
2. **`resources/subscribe` inserts its peer entry, then asks for the
   debouncer** (`memory_registry.rs:822-844`), while the last lane is closed.
3. **A peer unsubscribe races a `sub_open`.**

At no time after an `ensure` returns, while a consumer of either kind exists,
may the task be absent. The decision is taken under the `debounce_tasks` entry
that `ensure` also takes: an opener that finds the entry vacant re-checks both
consumer kinds under the same guard and, when they are empty, returns without
spawning, so every caller registers its consumer before it asks for the
debouncer. A stop decides and removes under that entry too. Concurrent ensures
on one key spawn exactly one task (Q-10a measured ten on the base tree), and a
stop removes the wakeup maps before the task so no live task loses its
`wakers`/`flush_now` entries (Q-10b). The rule holds no `Mutex` on a producer
path (P-02).

**Acceptance.** Six `#[tokio::test]`s in
`crate::adapters::subscription::memory_registry::tests`, each bounded and with
a fixed round count: `open_racing_last_lane_close_keeps_one_debouncer`,
`resources_subscribe_racing_last_lane_close_keeps_one_debouncer` and
`peer_unsubscribe_racing_sub_open_keeps_one_debouncer` (200 rounds each),
`concurrent_ensure_spawns_one_task` (32 tasks on a barrier),
`ensure_racing_stop_never_splits_the_task_from_its_waker` (5,000 rounds) and
`a_stop_that_holds_the_entry_makes_the_opener_rebuild_every_map` (a blocked
query proves the entry is held). Each round asserts the live task and its two
wakeup entries after the race, and one write is delivered to the peer of the
lane that remains; `ensure_racing_…` ends every round with no task, no waker
and no flush entry, because the lane is closed and every task joined. The last
test is the deterministic one: it fails 10 of 10 runs under either mutation
that breaks the rule.

### FR-005 The registry is released after the last consumer leaves

With no consumer left and every external handle dropped, no task holds the
registry. The link introduced by FR-002 must not create a strong cycle: feature
005 measured that a sink capturing a strong adapter or registry handle fails
`adapter_and_bridge_are_released_after_drop` (`lane_bridge.rs:1244-1259`;
`doc/arch/sdd/005-subscription-pause-delivery/research.md:46-70`). The plan
states which side is weak.

**Acceptance.** `...::registry_is_released_after_the_last_consumer_leaves`
(adapted from `debouncer_task_keeps_the_registry_alive_after_all_handles_drop`;
not in the tree; `/var/tmp/ssh-mcp/end-repro.patch`) runs one sequence: open
one lane on one URI, ensure its debouncer, drop every external handle except the
adapter that owns the lane, close that lane with no peer subscription present,
drop the adapter, then await a bounded loop (50 ms per step, at most 2 s) on a
`Weak<MemoryRegistry>` until `upgrade()` is `None`; assert it became `None`.
`adapter_and_bridge_are_released_after_drop` stays green.

### FR-006 The production composition installs the link

The close signal that reaches the registry is installed by `build_use_cases`,
next to the mux sinks and the lane bridge (`src/composition/prod.rs:333-366`).
A hand-built adapter that installs nothing keeps the old behavior; production
must not.

**Acceptance.** The proof observes the debouncer itself in the composition
`build_use_cases` returns, not a notification: with the lane closed, no lane
remains for the bridge to deliver to, so "no notification" holds whether the
task was stopped or not. The proof is
`crate::composition::prod::tests::production_wiring_stops_the_debouncer_when_the_last_lane_closes`
(Q-11): a `#[tokio::test]` in `src/composition/prod.rs` that builds the
composition through a private inner function returning the registry, opens a
lane, closes it through `use_cases.sub_unsubscribe`, and asserts
`has_debouncer` is false. It fails without the link and passes with
it; it is one composition test because `build_use_cases` installs the
process-wide forwarder (`prod.rs:541`).

### FR-007 No wire change

No tool, argument, descriptor, answer line, status value, notification method
or environment variable changes. The only new observable is the lifetime of a
background task.

**Acceptance.** `make tools-export-check` exits 0 with `unchanged` twice, and
`git diff --stat docs/TOOL_CATALOG.md docs/tool-catalog.json` is empty. The
scenario stays `@planned`.

### FR-008 The gate is green with the new tests

The verified-change gate of P-06 (`doc/arch/memory/constitution.md:109-112`)
passes, run from `/root/dev/ssh-mcp`, with the feature-007 floors and scripts:

- `cargo fmt --all -- --check`
- `cargo check --all-features --all-targets`
- `cargo clippy --all-features -- -D warnings`
- `cargo build --bins`
- `cargo nextest run --test-threads num-cpus --lib` (at least 2174 passing)
- `cargo nextest run --test-threads num-cpus --tests --features test-fixtures`
  (at least 2349 passing)
- `cargo build --no-default-features --target-dir target/no-default-features`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_http.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_transfer_progress.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v7_rsync_http.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v47_subscriptions.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v47_idempotency.py`
- `PYTHONPATH=scripts SSH_MCP_STDIO_BIN=$PWD/target/debug/ssh-mcp-stdio SSH_MCP_BIN=$PWD/target/debug/ssh-mcp SSH_MCP_TAIL_BIN=$PWD/target/debug/ssh-mcp-tail /root/dev/ssh-mcp/.venv/bin/python -m pytest -q scripts/test_v5_adversarial.py` (63 passed)
- `PYTHONPATH=scripts SSH_MCP_STDIO_BIN=$PWD/target/debug/ssh-mcp-stdio SSH_MCP_BIN=$PWD/target/debug/ssh-mcp SSH_MCP_TAIL_BIN=$PWD/target/debug/ssh-mcp-tail /root/dev/ssh-mcp/.venv/bin/python -m pytest -q scripts/test_v51_flush_bytes.py` (1 passed)
- `cargo audit`

No lint is disabled and no `#[allow]` or `#[expect]` is added. The floors are
the feature-007 totals; this feature adds tests and removes none.

**Acceptance.** Each command exits 0; the nextest runs report at least the
floors; `git diff` shows no `#[ignore]`, no removed test and no new `#[allow]`
or `#[expect]`. The scenario stays `@planned`.

### FR-009 `speckit validate` has no feature finding

The feature-007 baseline was exit 6 with 68 errors and no warning. Landing
hygiene then pinned `Cargo.lock` and completed the README architecture link,
image, table of contents and Mermaid diagram, removing five baseline findings.
The final baseline is 63 errors: `fqcn.relative-super` 46, `branch.else` 6,
`pin.ci.unpinned-action` 5, `pin.docker.unpinned-image` 2, and one each of
`llms-txt.file.missing`, `fqcn.glob-import`, `crew.roster.dangling` and
`ci.gitlab.missing`. No finding names a file of this feature or `session.md`;
no warning fires.

**Acceptance.** `speckit validate` reports 63 errors with that rule set and no
warning, and no error or warning names
`doc/arch/sdd/008-debouncer-consumer-lifetime/`,
`doc/arch/adr/0008-debouncer-consumer-lifetime.md`,
`doc/arch/schemas/debouncer-consumer-lifetime.cue`,
`doc/arch/specs/features/debouncer-consumer-lifetime.feature` or `session.md`.
Three `info` lines name the schema (first-class collection) and name it at the
baseline too. The scenario stays `@planned`.

### FR-010 `session.md` records the measured defects

`session.md` gains, in "Defects verified in the code during the session", one
entry per defect of the measurement the list does not have yet, each with its
`file:line` read from the file, and, where a reproduction exists, the test name,
the statement that it is **not in the tree**, its path
`/var/tmp/ssh-mcp/end-repro.patch`, and what it pins. Existing entries are
extended, not duplicated. At least:

- the peer unsubscribe that stops a debouncer a lane still needs
  (`src/adapters/subscription/memory_registry.rs:440-476`, `:466`);
- the debouncer task that keeps the registry alive after its composition is
  dropped (`memory_registry.rs:376-380`, `:401-408`;
  `src/composition/prod.rs:762-774`);
- a remote shell EOF that leaves the repository row `Running` and publishes only
  the local tail (`src/adapters/ssh/russh_adapter.rs:1498-1527`, `:1566`);
- rsync terminal states that publish no notification
  (`src/application/rsync_sync.rs:754-778`, `:795-813`);
- `serial_close` and its frozen description that promises a cascade and a final
  event (`src/infra/mcp/tool_router.rs:2601`,
  `src/adapters/serial/state.rs:399-406`, `:573-629`);
- the absence of any caller for `close_forward`
  (`src/adapters/ssh/russh_adapter.rs:890-897`, `:437-450`, `:695`);
- the duplicated `close_lanes_for_resource` and its full-list scan
  (`src/application/disconnect_session.rs:175-188`,
  `src/application/disconnect_agent.rs:176-189`,
  `src/adapters/subscription/subscriber_lane.rs:713-720`);
- `sub_close` documented as answering `SUB_CLOSE: NOT_FOUND` while the code
  answers an error result (`docs/API.md:1122`,
  `src/infra/mcp/tool_router.rs:1380`, `:878-882`);
- the last notification that is not flushed before the terminal write
  (`src/adapters/ssh/internal/client.rs:1144-1150`,
  `src/adapters/ssh/russh_adapter.rs:1511-1526`);
- the guard/open window (`src/application/subscription_admin.rs:150-160`).

**Acceptance.** Each entry exists with its `file:line`; every patch test says
it is not in the tree and names `/var/tmp/ssh-mcp/end-repro.patch`; no existing
entry is duplicated. The scenario stays `@planned`.

### FR-011 The stop decision exists once and the code rules hold

One predicate decides the stop: both consumer kinds empty. It is called from
the peer-removal path and from the lane-close path; the stop has one
implementation, `remove_debouncer`, with its four map removals. Production
code stays panic-free (P-03, `doc/arch/memory/constitution.md:97-101`), imports
no `super::`, has no `else` after an early return, and no comment names a
feature, a requirement id or a work item.

**Acceptance.** Four decidable checks, each run on the final tree:

- `grep -rn 'remove_debouncer' src` shows one declaration and its one call from
  `release_if_idle_uri`; no test-only copy of the four removals exists.
- The `speckit validate` counts of `fqcn.relative-super` and `branch.else` stay
  at the FR-009 baseline, 46 and 6, and no finding of either rule appears on a
  line the feature adds.
- No added line of the production diff contains `unwrap`, `expect(`, `panic!`,
  `#[allow` or `#[expect]`: `git diff -U0 -- 'src/*.rs' | grep '^+'` shows
  none.
- No added comment (`//`, `///`, `//!`) contains `FR-`, a feature number or a
  `Q-N` question id.

The scenario stays `@planned`.

### FR-012 A registry without a presence query keeps the peer-list semantics

A `MemoryRegistry` built without [`install_lane_presence`] cannot see lanes and
must behave as it does today: an explicit `ensure_for_uri` keeps its task (the
pre-spawn consumer check is skipped), and a peer-list emptiness still stops it. This
covers every hand-built harness and an embedder that installs no query. Every
production construction is `MemoryRegistry::from_env` inside
`build_use_cases` (`src/composition/prod.rs:290`), which installs the query at
`:339-341`; the embedded daemon reaches it through `build_use_cases`
(`src/composition/embed.rs:64`).

**Acceptance.**
`crate::adapters::subscription::memory_registry::tests::ensure_without_a_presence_query_keeps_the_task`
asserts the task survives the ensure and is released by a later
`release_if_idle_uri`; `...::peer_unsubscribe_without_a_presence_query_stops_the_task`
asserts a peer unsubscribe on the same kind of registry stops it. Both are
`#[tokio::test]`.

### FR-013 The two non-debouncer per-resource maps survive a stop

A stop removes the four debouncer maps and leaves `sequence_counters` and
`event_tails`, because `current_seq` feeds `resources/read`
(`src/application/read_resource.rs:595`, `:632`, `:673`) and `tail_snapshot`
feeds the forward read (`:716`). Their unbounded growth is a recorded defect,
not repaired here.

**Acceptance.**
`...::per_resource_maps_keep_the_sequence_and_tail_after_a_stop` runs eight
cycles and releases each task; it asserts `debounce_tasks`, `wakers` and
`flush_now` empty and `sequence_counters` and `event_tails` at 8.
`bytes_since_flush` keeps 8 entries too, but under the `forward://` kind the
test also writes, not under the stopped shell key. It is a `#[tokio::test]`.

### FR-014 Bytes written while no task exists are delivered on the next wakeup

With no task, `record_bytes` still accumulates; the next consumer's ensure
reuses the counter, and the next `poke` delivers one bare-URI notification
after one debounce window. The bytes themselves are read from the resource
ring. Nothing is lost from the ring.

**Acceptance.**
`...::bytes_written_before_a_debouncer_exists_are_delivered_by_the_next_wakeup`
writes below and above the threshold with no task, opens a consumer, and
awaits exactly one notification within 4 s. `...::forward_tail_survives_without_a_task`
writes and reads a forward tail with no task. Both are `#[tokio::test]`.

## Security Requirements

- **Data sensitivity/classification.** No payload changes. The feature reads
  the existing per-URI consumer tables and writes no new data; the bytes a lane
  already receives are the resource bytes.
- **Authentication/authorization.** No authenticated surface is added. The
  stop/ensure decision is in-process and grants no access a subscribe or a
  close would not grant.
- **Input validation.** The URI is already parsed before a lane exists
  (`src/application/read_resource.rs:164-192`); no new untrusted input is
  parsed. The registry parses the URI again in `ensure_for_uri`
  (`memory_registry.rs:651-657`) and treats a malformed one as a no-op.
- **Cryptography in transit/at rest.** None is added; no secret is read.
- **Logging/audit.** No new log line is required. A stopped task is visible as
  the absence of the force-flush/keepalive notifications and through
  `sub_stats_all`; a missed stop is visible as a task that outlives its
  consumers.
- **Error-handling information exposure.** The close path keeps answering
  `SubNotFound` for an unknown id (`subscriber_lane.rs:824-833`); the stop
  decision is infallible and swallows nothing but the already-returned
  `Option`.

## Acceptance Scenarios

Given a peer subscription and a lane on one URI
When  the peer unsubscribes
Then  the lane keeps receiving its flushes (FR-001).

Given a lane on a URI with no peer subscription
When  the lane is closed
Then  the debouncer task is stopped (FR-002).

Given a peer subscription on a URI with no lane
When  the peer unsubscribes
Then  the debouncer task is stopped (FR-003).

Given a `sub_open`, a `resources/subscribe` and a peer unsubscribe racing a
close of the last lane
When  the race resolves
Then  exactly one debouncer remains for the live consumer (FR-004).

Given the last consumer of a registry is gone and every external handle dropped
When  the close path has run
Then  no task keeps the registry alive (FR-005).

Given the composition built by `build_use_cases`
When  the last lane of a URI closes with no peer
Then  the debouncer of that URI observed through the composition is stopped
      (FR-006).

Given the repaired tree
When  `make tools-export-check` runs
Then  both catalogue files are `unchanged` (FR-007).

Given the repaired tree
When  the gate commands of FR-008 run
Then  each exits 0 with at least 2174 lib tests and 2349 tests with the
integration suites (FR-008).

Given the feature's artifacts on disk
When  `speckit validate` runs over the corpus
Then  the error count stays at 68 with the same rule set and no warning fires
(FR-009).

Given the change landed
When  a reader opens `session.md`
Then  every defect of the measurement carries its `file:line` and its
reproduction note (FR-010).

Given the stop decision in the source
When  it is searched
Then  it exists once and the code rules hold (FR-011).

## Accepted Risks

- **AR-1 No residual open/close window.** The decision and the removal are one
  step under the `debounce_tasks` entry, and an ensure re-checks once after
  inserting; after an ensure returns with a consumer present the task exists.
  The spike's race tests pass 200 to 500 times each.
- **AR-2 A lane that is never closed keeps its task and its registry.** Out of
  scope; recorded in `session.md`.
- **AR-3 A stopped debouncer removes `bytes_since_flush`.** Accepted and
  measured (Q-2): the next consumer starts from zero, receives one URI
  notification on its next wakeup, and reads the bytes from the resource ring;
  no byte is lost from the ring.
- **AR-4 A test that needs both adapter types runs in an adapter test module.**
  It proves the mechanism without `crate::application`; the production wiring is
  proven separately by FR-006.

## Observability

No metric, log event or trace span is added or removed. The effect is visible
as one fewer parked task per closed URI and as the continuation of the
force-flush/keepalive cadence on a lane whose peer left. `sub_stats_all` keeps
its current totals. Conventions stay those of
`doc/arch/observability/observability.md`.

## Clarifications

Answered on 2026-11-22 in `/var/tmp/ssh-mcp/deb-probe`, a copy of the
feature-007 tree; logs under `/var/tmp/ssh-mcp/logs/deb-probe-*.log`. Q-6, Q-7
and Q-9 are plan decisions, settled by `plan.md` D-1; every other question has
an answer. Q-10 and Q-11 were added by the coordinator after the first
delivery.

### Q-1 Every production caller of the lane close and of `stop_debouncer`

Call hierarchy (LSP) on `stop_debouncer` (`memory_registry.rs:412`): one
incoming, `unsubscribe_sync` at `:466`. On `unsubscribe_sync` (`:440`): two
incoming, `drop_peer_sync` (`:428`) and `unsubscribe` (`:847`). On
`drop_peer_sync` (`:425`): `gc_closed_peers` (`:812`) and `drop_peer` (`:851`).

Text fallback for the trait callers (the server does not answer for the
`SubscriberRegistryAsync` method): `SubscriberRegistryAsync::unsubscribe` is
called in production only from `src/application/unsubscribe_resource.rs:81`
(`resources/unsubscribe`) and `src/application/subscribe_resource.rs:149` (the
lifecycle rollback). `MemoryRegistry::drop_peer` and `gc_closed_peers` have
**no production caller**: `PeerGcUseCase::execute` (`src/application/peer_gc.rs:68-71`)
is never read from `UseCases`, and the pump calls the *legacy*
`SUBSCRIPTION_REGISTRY.gc_closed_peers` (`src/adapters/subscription/legacy.rs:664`),
not the hexagonal registry. The daemon reaches the peer path by name:
`Op::Unsubscribe` maps to `_resources_unsubscribe` (`src/embed/dispatcher.rs:39`),
which sends `resources/unsubscribe` (`:615-644`), which reaches
`unsubscribe_resource.rs:81`. No reaper exists.

Lane close (`LaneAdmin::close`, `src/ports/subscriber_lane.rs:126`; impl
`subscriber_lane.rs:736-738`, async impl `:824-833`) has three production
callers, all text: `subscription_admin.rs:286` (`sub_close`),
`disconnect_session.rs:186`, `disconnect_agent.rs:187`.

### Q-2 Does a stopped debouncer lose bytes a later consumer needs

Measured:
`crate::adapters::subscription::memory_registry::tests::bytes_written_before_a_debouncer_exists_are_delivered_by_the_next_wakeup`
passes on the base tree. With no task, `record_bytes` still creates the counter
lazily and accumulates (`memory_registry.rs:681-707`); `flush_now` and `wakers`
have no entry, so no notify fires. The next consumer's `ensure_debouncer`
reuses the existing counter (`:387-392`), and the next `poke` flushes it after
one debounce window (~1 s), delivering **one bare-URI notification** with
`bytes_added` = the accumulated value (`:961-970`). The bytes themselves are
not in the notification; the resource ring is the read path, and a forward tail
is readable through `tail_snapshot` (`:742-746`) because
`record_bytes_with_tail` appends it (`:729-738`). A stopped counter is therefore
not lost for the next wakeup; what is lost is any cadence while no task exists.

### Q-3 What the embedded daemon and each HTTP session observe

The daemon wires one process-wide composition (`src/composition/embed.rs:62-83`)
and runs until process exit; its tasks end with the process. HTTP builds one
composition per session through the factory (`prod.rs:762-774`); a session end
drops that composition and runs **no close path** — there is no session-end
hook, and the peer-GC pump owns only the legacy registry and the `PeerTable`
(`legacy.rs:663-675`). The task holds `Arc<MemoryRegistry>`, so it survives the
drop (the Q-4 test). Transport-drop lanes are therefore out of scope.

### Q-4 Which existing tests and script cases depend on the old behavior

The prototype is in the probe only: `CloseSink` gains the URI
(`subscriber_lane.rs:366,409-414,824-833`), the registry gains a lane-presence
probe and `release_if_idle_uri` (`memory_registry.rs:192,261,273-299`), and
`ensure_debouncer` takes a `DashMap` entry guard (`:406-446`). Results against
the unmodified copy:

| Suite | Baseline | Prototype | Changed existing case |
| --- | --- | --- | --- |
| lib | 2154 passed | 2161 passed | none; the five probe tests that fail on baseline pass with the prototype |
| `--tests --features test-fixtures` | 2329 passed | 2336 passed | none |
| `test_http.py` | 15 passed | 15 passed | none |
| `test_transfer_progress.py` | 1 passed, 1 skipped | 1 passed, 1 skipped | none |
| `test_v7_rsync_http.py` | 10 passed | 10 passed | none |
| `test_v47_subscriptions.py` | 20 passed | 1 failed, 19 passed, then 20 passed on rerun | none; `test_s07_one_peer_multiple_uri_subscribes_independent_streams` flaked on the first run (`S7_B_MARK` missed) and passes on rerun on both copies |
| `test_v47_idempotency.py` | 9 passed | 9 passed | none |
| `test_v5_adversarial.py` | 63 passed | 63 passed | none |
| `test_v51_flush_bytes.py` | 1 passed | 1 passed | none |

The five baseline failures are the probe tests themselves:
`production_wiring_…` (FR-006), `session_cascade_…` and `agent_cascade_…`
(Q-12), `per_resource_maps_…` (Q-8, assertion on the two leaks) and
`concurrent_ensure_…` (Q-10a, assertion of one spawn). No existing test or
script case changes result, so no fate-table row is adapted for dependency
reasons.

### Q-5 The exact expected test totals

New lib tests, by name: FR-001/FR-002 `peer_unsubscribe_keeps_the_debouncer_a_lane_still_needs`;
FR-002 `last_lane_close_stops_the_debouncer`,
`ensure_after_the_last_consumer_left_spawns_nothing`,
`every_open_and_close_cycle_leaves_no_task`; FR-003
`last_peer_unsubscribe_without_a_lane_stops_the_debouncer`; FR-004
`open_racing_last_lane_close_keeps_one_debouncer`,
`resources_subscribe_racing_last_lane_close_keeps_one_debouncer`,
`peer_unsubscribe_racing_sub_open_keeps_one_debouncer`,
`concurrent_ensure_spawns_one_task`,
`ensure_racing_stop_never_splits_the_task_from_its_waker`,
`a_stop_that_holds_the_entry_makes_the_opener_rebuild_every_map`; FR-005
`registry_is_released_after_the_last_consumer_leaves`; FR-006
`production_wiring_stops_the_debouncer_when_the_last_lane_closes`; FR-012
`ensure_without_a_presence_query_keeps_the_task`,
`peer_unsubscribe_without_a_presence_query_stops_the_task`; FR-013
`per_resource_maps_keep_the_sequence_and_tail_after_a_stop`; FR-014
`bytes_written_before_a_debouncer_exists_are_delivered_by_the_next_wakeup`,
`forward_tail_survives_without_a_task`; plus the two cascade tests. That is
20 lib tests. No integration test: FR-006 lives in
`src/composition/prod.rs` (Q-11). Floors: lib 2154 + 20 = 2174; with
integration 2329 + 20 = 2349. No test is removed; `stop_runs_…` is replaced by
the deterministic `a_stop_that_holds_the_entry_…`.

### Q-6 Where the stop decision lives — plan decision

Constraints: the stop decision needs both consumer kinds visible; the peer kind
is `MemoryRegistry::subscribers` (`memory_registry.rs:123-124`), the lane kind
is `SubscriberLaneAdapter::lanes`/`by_uri` (`subscriber_lane.rs:315-319`); no
adapter imports `crate::application` and an adapter does not call another
adapter (P-01, `constitution.md:89-91`); the link goes through a port or a
composition-installed sink (`prod.rs:333-366`); no `Mutex` on a producer path
(P-02, `constitution.md:93-96`). The plan may put the decision in the registry
and the signal in a sink, or invert both behind a port. It puts the decision in
the registry and the signal in the extended sink (`plan.md` D-1).

### Q-7 Which existing sink and port could carry it — plan decision

`CloseSink` receives `&SubId` (`subscriber_lane.rs:366`) and is fired at
`:825-829`; `remove_lane` holds `lane.kind` and `lane.uri` (`:560-570`).
`LaneNotifierBridge` receives `(uri, bytes_added)` (`src/ports/notifier.rs:142-148`);
`DebouncerActivator` receives `uri` (`:86-90`). A new sink or an extended
`CloseSink` both fit the fire site; the activator is one-way and cannot carry
the close. Whether the type grows or a second sink is added is the plan's; it
grows `CloseSink` with the URI (`plan.md` D-1).

### Q-8 Do the per-resource maps grow without bound

Measured with
`...::per_resource_maps_grow_over_open_and_close_cycles`: after eight
open/close cycles and eight stops, `sequence_counters` holds 8 entries and
`event_tails` 8; `stop_debouncer` removes only `debounce_tasks`, `wakers`,
`flush_now` and `bytes_since_flush` (`memory_registry.rs:412-420`).

**They are not the debouncer's and stay out of the stop.** Call hierarchy and
readers:

- `sequence_counters` (`memory_registry.rs:136`) is created lazily by
  `sequence_counter` (`:354-365`) from `next_seq` (`:664-667`); producers
  number events unconditionally through it (`client.rs:1071`, `:1108`,
  `:1145`; `serial/state.rs:652`; `russh_adapter.rs:939`), and
  `current_seq` (`:669-673`) is read by `resources/read` for shell, command
  and transfer (`read_resource.rs:595`, `:632`, `:673`) and rendered as
  `last_seq`.
- `event_tails` (`:156`) is appended by `append_event_tail` (`:334-352`) from
  `record_bytes_with_tail` for forward (`:729-738`) and read by
  `tail_snapshot` (`:742-746`) from `read_resource.rs:716`.

Removing either at the last consumer would restart a live resource's `last_seq`
at 1 for the next reader and drop forward history a later `resources/read`
returns — a wire-visible change FR-007 forbids. Both maps are therefore
recorded as a defect outside this feature in `session.md` (8 entries after 8
cycles, removed nowhere), to be repaired where the resource ends. FR-002 names
the four maps `stop_debouncer` owns.

### Q-9 Is a lane count needed, and what resets it — plan decision

If the plan carries a count instead of asking the adapter, it must increment
after the lane is inserted (`subscriber_lane.rs:543-544`), not on the global
reservation (`:811`), and must not increment when the per-URI cap refuses
(`:537-542`); it must decrement only when `remove_lane` actually removed a lane
(`:560-570`), so the `None` early return does not underflow. Concurrent closes
of one id are idempotent through the same `lanes.remove` (`:561`). The plan
asks the adapter instead of carrying a count (`plan.md` D-1).

### Q-10 Are `ensure_debouncer` and `stop_debouncer` atomic

(a) **Defect, in scope.** 32 concurrent `ensure_for_uri` calls on one URI
spawned 10 tasks on the base tree while the task map kept one handle
(`concurrent_ensure_spawns_more_than_one_task`), so nine tasks are
unreachable and `stop_debouncer` aborts only one. The cause is the
`contains_key` test followed by a later insert (`memory_registry.rs:371-410`).
With a `DashMap` entry guard the same probe spawns 1 and maps 1. (b) **Defect,
in scope.** `stop_debouncer` removed the task entry first and the wakeup maps
afterwards, so a concurrent ensure could insert the waker between the two
removals and leave a live task without it; the per-round spike test
`ensure_racing_stop_never_splits_the_task_from_its_waker` catches the split
(`left: true, right: false`) within 5,000 rounds on the base tree. Removing the
three maps before the entry under the same guard, and re-checking the consumers
once after an ensure inserts, keeps the task and its waker together. FR-004
needs one spawn per key under concurrency and no split; the entry guard and the
removal order provide both.

### Q-11 How the production wiring of FR-006 can be proven

The composition returns only the private `ProdWiring` tuple (`prod.rs:210-217`,
`build_use_cases` at `:677`); the registry is a local binding. The process-wide
`SUBSCRIPTION_REGISTRY` exposes poke/record forwarding but no task count.
Options: (a) a `#[cfg(test)] mod tests` inside `src/composition/prod.rs` plus a
private inner function that also returns the registry it builds; (b) make
`build_use_cases` return it always (changes every caller); (c) an integration
file, which cannot see the private tuple without a new public surface. Chosen
for the plan: (a). It needs one `cfg(test)` split of `build_use_cases` (the
inner function returns the tuple plus the registry); the crate already keeps
test modules in `composition/status_sinks.rs`, so a module in `prod.rs` follows
the existing shape. Proven in the probe: the test fails on the base tree
(`the production composition must stop the task when the last lane closes`) and
passes with the prototype. FR-006 names it:
`crate::composition::prod::tests::production_wiring_stops_the_debouncer_when_the_last_lane_closes`.

### Q-12 The two cascades

Both `DisconnectSessionUseCase` and `DisconnectAgentUseCase` close lanes
through `LaneAdmin::close` (`disconnect_session.rs:186`,
`disconnect_agent.rs:187`). Probe tests
`session_cascade_stops_child_debouncers_and_spares_the_health_one` and
`agent_cascade_stops_child_debouncers_and_spares_the_health_one` pass on the
base5 shim (its close sink already reaches the base rule) and are held by
mutation (b). The `session://<id>/health` lane is not closed by either cascade
(`disconnect_session.rs:243,259,280`), so its debouncer stays; that is out of
scope and recorded in `session.md`.

## Owner review

- The orchestrator decided the split of the measured problem and this feature's
  boundary under the owner's instruction to proceed; both are registered here
  for the owner's review.
- The mechanism is a plan decision; ADR 0008 records the options, the
  constraints and the plan's choice (Option A).
- The out-of-scope list names every defect the measurement recorded elsewhere,
  so the next features can pick them up without re-measuring.
