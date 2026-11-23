---
status: proposed
date: 2026-11-22
deciders: [Fabricio Archanjo]
consulted: []
informed: []
---

# Resources Subscribe Lane Removal

## Context and Problem Statement

Related decisions: the hexagonal layout is ADR 0002, lifecycle binding is
ADR 0003, the lane and `SubId` are ADR 0004's, and the pause gate is ADR
0005's; this ADR removes the lane 0004 introduced from the legacy path and
leaves 0003's lifecycle and 0005's gate on the `sub_open` path intact.

`resources/subscribe` mints a per-call `SubId` and opens a lane
(`src/application/subscribe_resource.rs:165-167`, `:174-188` with
`LanePolicy::default()` at `:182`). The id never reaches the client:
`SubscribeResourceOutcome.sub_id` is read in production only by the log at
`src/infra/mcp/resource_handlers.rs:825`, and rmcp's `subscribe` handler
returns `()` (`:817`). The lane is opened without a peer
(`src/ports/subscriber_lane.rs:33-46`), and `LaneFanoutBridge` skips a lane
without a peer (`src/adapters/subscription/lane_bridge.rs:282-284`,
`:264-266`, `:483-485`), so no delivery ever goes through it. The per-peer
fan-out (`src/adapters/subscription/memory_registry.rs:985-990`) is what
reaches the peer.

The lane is not neutral. `resources/unsubscribe` carries no `sub_id` on the
wire (`src/infra/mcp/resource_handlers.rs:866`), so the lane-close branch of
`UnsubscribeResourceUseCase::execute`
(`src/application/unsubscribe_resource.rs:106-113`) never runs. Every
subscribe/unsubscribe cycle leaks one lane and one of the 16 per-URI lane
slots (`src/adapters/subscription/subscriber_lane.rs:536-545`); the 17th
cycle fails with `max subs per URI exceeded`. The lane also appears in
`sub_list`, `sub_stats` and `sub_stats_all` and accepts `sub_pause`, which
flips a flag no delivery path for that lane reads.

Feature 005 gated both real lane paths
(`src/adapters/subscription/lane_bridge.rs:269`, `:289`, `:483`) and left this
lane as a pinned residual
(`crate::adapters::subscription::memory_registry::tests::resources_subscribe_lane_is_listed_but_pause_does_not_gate_the_peer_fanout`).

The reproduction tests, run on 2026-11-22 in `/var/tmp/ssh-mcp/resub-probe`,
fail on the base tree:

```
...::resources_unsubscribe_closes_the_lane_it_opened
  resources/unsubscribe must close the lane resources/subscribe opened
...::seventeenth_resources_subscribe_with_interleaved_unsubscribe_succeeds
  subscribe round 16 failed: max subs per URI exceeded for shell://sh-1/output (limit 16)
```

The first message is the desired behaviour, not a contradiction of feature 005:
005 recorded that the lane-close branch never runs
(`doc/arch/sdd/005-subscription-pause-delivery/spec.md:91-92`), and its test
asserts the repair.

They are **not in the tree**; they are kept in
`/var/tmp/ssh-mcp/resub-repro.patch` on the session host with the run log
`/var/tmp/ssh-mcp/resub-repro-run.log`.

A second defect compounds the first: `execute` increments the lifecycle
refcount on every call (`src/application/subscribe_resource.rs:164`) while the
registry keeps one entry per `(peer, URI)`
(`src/adapters/subscription/memory_registry.rs:592-596`), so a repeated
subscribe is counted twice and one unsubscribe strands a refcount.

## Decision Drivers

- A subscription must cost what it uses: one registry entry, one refcount, no
  lane that cannot deliver.
- The published wire must not move. No tool, argument, descriptor, response
  line, error code, notification method or environment variable changes.
- The per-URI lane cap must serve `sub_open`, which does use it.
- The lifecycle refcount must balance across a repeated subscribe and one
  unsubscribe.
- A failed subscribe must leave no live subscription behind.
- The hot path stays lock-free (P-02,
  `doc/arch/memory/constitution.md:93-96`); the change touches no hot-path
  primitive.

## Considered Options

- **Option A — keep the lane and associate `(peer, URI)` with its `SubId`.**
  The lane carries the peer (`LanePolicy.peer` is already a field), a
  `DashMap<(PeerId, String), SubId>` in the lane adapter lets
  `resources/unsubscribe`, peer GC and `drop_peer` close it, and the per-peer
  fan-out reads the lane's pause flag before notifying. It also needs the
  `sub_id` on the wire (or a `sub_list` lookup) for a client to drive it.
  Touches `ports/subscriber_lane.rs`, the lane adapter, both use cases,
  `memory_registry.rs` and composition; one or two new port methods; a new
  initialization order because the registry is built before the lane adapter
  (`src/composition/prod.rs:292` vs `:325`).
- **Option B — stop opening a lane from `resources/subscribe`.** The use
  case, the outcome, the handler log and the composition wiring lose the lane;
  the lane-close branch and the `sub_id` field of the unsubscribe request
  leave with it. Delivery stays the per-peer fan-out; the registry entry, its
  cursors and the debouncer stay. Touches two application files, one infra
  file and composition, plus documents; no new port, no order change.
- **Option C — leave the code and correct the documents only.** Keeps the
  leak and the cap failure; the documents would have to say a legacy
  subscription owns a lane that does nothing, which is the current accidental
  behaviour rather than a design.

## Decision Outcome

Chosen option: **Option B**. The measured facts decide it: the id never
reaches the client and the SDK's handler signature returns no body, the lane
has no peer so nothing is delivered through it, it consumes one of the 16
per-URI lane slots, and every operation a client could run on its `sub_id`
after reading it from `sub_list` (`sub_pause`, `sub_filter`, `sub_replay`,
`sub_close`) has no effect on delivery. Option A repairs the lane into a
first-class surface; Option B deletes a surface nothing can use. Option C
leaves two defects in place for no saving over B.

The same change balances the lifecycle: `SubscriberRegistryAsync::subscribe`
reports whether it created the `(peer, URI)` entry or refreshed it, and
`on_subscribe` runs only on creation. A failed `on_subscribe`
(`DomainError::ResourceGone`, `src/adapters/lifecycle/refcount.rs:482-488`)
removes a created entry again, so a client told the call failed is not
notified.

The decision was taken by the orchestrator under the owner's instruction to
proceed and is recorded for the owner's review, not as the owner's own choice.

### Consequences

- Good: a subscribe/unsubscribe loop on one URI no longer exhausts the 16
  per-URI lane slots; the 17th cycle succeeds (proving test
  `seventeenth_resources_subscribe_with_interleaved_unsubscribe_succeeds`,
  added from the patch under the same name).
- Good: `sub_list`, `sub_stats` and `sub_stats_all` show only lanes that can
  deliver, and `sub_pause` is no longer offered on a subscription it cannot
  gate.
- Good: the lifecycle refcount balances across a repeated subscribe and one
  unsubscribe; a failed subscribe leaves nothing behind.
- Good: no wire change, no new dependency, no new lock, no new storage.
- Bad: a host that read a `sub_id` from `sub_list` for a legacy subscription
  loses that row and the `sub_pause` call that accepted it (AR-1 in the
  specification).
- Bad: option A remains the path if the legacy method is later made a
  first-class lane surface; it would then also need the id on the wire.
- Neutral: peer GC of the hexagonal registry, the `unsubscribe_sync` race,
  lane lifetime, `sub_open` existence checking and the daemon op parameters
  stay as they are (Out of Scope in the specification); the first two are
  added to the `session.md` defect list.
- Neutral: the entry/lifecycle window (an unsubscribe between the registry
  entry and `on_subscribe`) is pre-existing and unchanged; the unsubscribe
  underflows first and both calls end with a count of 1 and no entry
  (`src/application/subscribe_resource.rs:159-164`,
  `src/adapters/lifecycle/refcount.rs:546-555`). Recorded in `session.md`,
  not repaired.
- Bad: the rollback of a failed created subscribe removes the record even
  after a concurrent refresh, and `Closed` does not stop the producer
  (`src/adapters/lifecycle/grace_timer.rs:148`,
  `src/adapters/lifecycle/refcount.rs:564`), so the refresh caller told
  success can miss notifications; the registry stays consistent with what
  every later caller is told (`refcount.rs:371-380`, `:419-424`). AR-4 in the
  specification.
- Neutral: the registry keeps its per-subscriber `SubId` and `(sub_id, uri)`
  cursors; `sub_id_for` and the cursor readers have no production caller after
  the removal and stay in place.
- Owner review: the loss of the `sub_list` row for a legacy subscription is
  AR-1, registered for the owner (Q-1 in the specification).
