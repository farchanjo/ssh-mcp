---
status: proposed
date: 2026-11-22
deciders: [Fabricio Archanjo]
consulted: []
informed: []
---

# Debouncer Consumer Lifetime

## Context and Problem Statement

Related decisions: the hexagonal layout is ADR 0002, lifecycle binding is
ADR 0003, the lane and `SubId` are ADR 0004's, the pause gate is ADR 0005's,
the legacy lane removal is ADR 0006's, and the `sub_open` guard is ADR 0007's.
This ADR changes one lifetime rule of the per-resource debouncer; it adds no
tool, no argument and no wire value.

A debouncer of `(kind, resource_id)` has two consumer kinds: the peer
subscriptions of `MemoryRegistry` and the lanes bound to the URI. Today the
task follows the peer list alone. `unsubscribe_sync` stops it as soon as the
peer vector becomes empty (`src/adapters/subscription/memory_registry.rs:440-476`,
stop at `:459-467`) and never asks whether a lane remains. A lane that is
still bound is orphaned: `sub_stats` keeps answering and no flush reaches it.
The other direction leaks: `UnsubscribeUseCase::execute` closes the lane and
touches no debouncer (`src/application/subscription_admin.rs:273-293`), and
`stop_debouncer` has that one caller (`memory_registry.rs:412-420`). The task
holds a strong `Arc<MemoryRegistry>` (`:376-380`, `:401-408`), so a dropped
HTTP composition is kept alive by it.

The reproduction tests fail or pin the wrong behavior on the base tree; they are
**not in the tree**; they are kept in `/var/tmp/ssh-mcp/end-repro.patch` on the
session host with the run log `/var/tmp/ssh-mcp/end-repro-run.log`:

```
...::peer_unsubscribe_stops_the_debouncer_the_lane_still_needs
  after the peer leaves, has_debouncer is false and no flush reaches the lane
...::debouncer_task_survives_lane_close_and_the_end_of_its_resource
  one task survives its lane
...::every_open_and_close_cycle_on_a_distinct_id_leaks_one_task
  eight cycles leave eight tasks
...::debouncer_task_keeps_the_registry_alive_after_all_handles_drop
  Weak::upgrade is still Some with every external handle dropped
```

The whole measured problem is larger than one feature; this ADR is the first
part only. Resource-end leaks, the peer/HTTP end, the health-lane escape, the
cascade cost, the guard/open window and lane lifetime stay out of scope and are
recorded in `session.md`.

## Decision Drivers

- A lane must keep delivering while a peer subscription on the same URI comes
  and goes; one surface leaving must not silence the other.
- A closed lane must release its task, so open/close churn does not accumulate
  tasks and map entries.
- No interleaving of an open and a close may leave a live consumer without a
  task or a task without a consumer.
- No `Mutex` on a path a producer takes (P-02,
  `doc/arch/memory/constitution.md:93-96`).
- No adapter imports `crate::application`; an adapter does not call another
  adapter directly (P-01, `doc/arch/memory/constitution.md:89-91`).
- No strong cycle: feature 005 measured that an adapter-owned strong sink
  retains the adapter (`doc/arch/sdd/005-subscription-pause-delivery/research.md:46-70`;
  `src/adapters/subscription/lane_bridge.rs:1244-1259`).
- No wire change: `make tools-export-check` stays `unchanged`.

## Considered Options

- **Option A — the registry learns about lane closes through a
  composition-installed sink that holds `Weak<MemoryRegistry>`, and one
  registry predicate stops only when both consumer kinds are empty.** The sink
  is fired by `close_lane` (`subscriber_lane.rs:824-833`) next to the existing
  mux sink; one `install_lane_lifetime` builder in
  `src/adapters/subscription/lane_bridge.rs` installs both closures, and the
  composition root (`src/composition/prod.rs:340-342`) composes the mux
  unregister through it. The predicate is called from
  `unsubscribe_sync` and from the sink, under the same `debounce_tasks` entry
  the ensure takes; an ensure that finds the entry vacant re-checks both kinds
  under the same guard and returns without spawning when none is left. No new
  lock, no new trait: the existing `CloseSink` (`subscriber_lane.rs:366`)
  grows the URI.
- **Option B — a new `ports` trait implemented by the registry, installed by
  composition, with the same weak handle and predicate.** Cleaner layering
  vocabulary than an adapter-local closure; costs a trait, an implementor and
  a wiring step, and the trait is called from an adapter method, so it is still
  an adapter-to-port call.
- **Option C — the registry keeps a per-URI lane counter updated by open/close
  sinks, and `unsubscribe_sync` consults the counter.** No call from the
  adapter into registry logic beyond the counter; costs a second shared map and
  a reset rule on failed opens and early returns, and duplicates the consumer
  test the registry can already perform.
- **Option D — an adapter-owned strong sink (feature 005 design A).**
  Rejected by measurement: the release test fails because the strong cycle
  retains the adapter.

## Decision Outcome

Chosen option: **Option A**, taken in the plan phase (`plan.md`, D-1) against
the evidence below. A composition-installed presence query and a close sink
extended with the URI, both capturing weak handles, and one registry predicate
that stops only when both consumer kinds are empty. The plan keeps the stop
decision and the removal under the same `debounce_tasks` entry the ensure
takes, and decides before the spawn, so the three FR-004 interleavings hold.

What the evidence settles:

- Option D is ruled out by feature 005's measurement: a strong adapter-owned
  sink fails the release test.
- The link must cross a port or a composition-installed sink; no adapter may
  import `crate::application` or call another adapter directly (P-01).
- Whichever side the link captures strongly, the other must be weak, or the
  release property of FR-005 fails.
- The stop decision belongs where both consumer kinds are visible. The registry
  owns the peer table and the task map; the lane adapter owns the close signal.
  Option A uses both as they are; Option B renames the same call through a
  port; Option C adds a counter and its reset rules.
- The three races of FR-004 must hold under whichever mechanism is chosen.

The split of the measured problem and the boundary of this feature were decided
by the orchestrator under the owner's instruction to proceed; both are
registered for the owner's review, not as the owner's own choice. The mechanism
above is the plan's decision, not the orchestrator's.

### Consequences

- Good: a lane stops being orphaned by a peer unsubscribe, a `drop_peer` or a
  peer GC.
- Good: a closed lane releases its task and the four per-resource map entries
  `stop_debouncer` owns.
- Good: no new dependency, no new lock, no new storage, no wire change.
- Good: the stop decision exists once and is called from both consumer paths.
- Good: making `ensure_debouncer` atomic against itself also removes the
  duplicate-spawn defect measured as Q-10a (ten tasks for 32 concurrent
  ensures, one reachable).
- Bad: `close_lane` gains a second synchronous sink call; both sinks run per
  close on a cold path.
- Bad: the registry learns about lanes without owning them, so the plan must
  keep the weak side and the re-check honest.
- Neutral: a stopped counter is not restored; a later consumer starts from zero
  (Q-2). The resource ring remains the recovery path.
- Neutral: `sequence_counters` and `event_tails` are not the debouncer's and
  survive a stop; their unbounded growth is recorded in `session.md` (Q-8).
- Neutral: lanes that are never closed keep their task; resource-end leaks are
  the next features' work (Out of Scope in the specification).
- Owner review: the split and the boundary are registered above; the mechanism
  is a plan decision, and the three races are FR-004 and AR-1 in the
  specification.
