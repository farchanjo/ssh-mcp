---
status: proposed
date: 2026-11-22
deciders: [Fabricio Archanjo]
consulted: []
informed: []
---

# Subscription Pause Delivery

## Context and Problem Statement

`sub_pause` is documented as a delivery gate: "Suspend lane drain. Subsequent
events accumulate in the lane mpsc until sub_resume"
(`src/infra/mcp/tool_router.rs:2441`), and `docs/OPERATIONS.md:103` gives
`sub_resume` as the cure when a lane delivers nothing. The use case only
stores `LaneState::paused` (`src/application/subscription_admin.rs:314-320`),
and no delivery path loads it. `findReferences` on the field returns the
declaration (`src/adapters/subscription/subscriber_lane.rs:160`), the `Debug`
read (`:218`), the `sub_list` read (`:299`) and the two stores (`:832`,
`:842`). A paused lane keeps receiving.

Two paths deliver to a lane peer, both reachable from every transport:

- the debouncer-driven legacy notification, `MemoryRegistry::broadcast`
  (`src/adapters/subscription/memory_registry.rs:962`) →
  `LaneNotifierBridge::notify_lanes` (`:973`) →
  `LaneFanoutBridge::notify_one_lane`
  (`src/adapters/subscription/lane_bridge.rs:280`) →
  `notify_resource_updated`;
- the producer-driven inline push,
  `SubscriptionRegistry::record_bytes_with_tail`
  (`src/adapters/subscription/legacy.rs:458`) → the forwarder (`:461`) →
  `MemoryRegistry::record_bytes_with_tail` (`memory_registry.rs:700`) →
  `notify_lanes_inline` (`:718`) → the per-URI FIFO consumer
  (`lane_bridge.rs:440`) → `notify_ssh_output`.

A third path, the per-peer fan-out (`memory_registry.rs:985-987`), serves the
peerless lane that `resources/subscribe` opens
(`src/application/subscribe_resource.rs:182-183`) and is not a lane path.

The reproduction test added from `/var/tmp/ssh-mcp/defects-repro.patch` fails
on the base tree:

```
thread '...::paused_lane_receives_no_delivery_until_resume' panicked at
src/adapters/subscription/lane_bridge.rs:998:
legacy notify must not reach a paused lane
```

The published surface is frozen (P-04 and P-05,
`doc/arch/memory/constitution.md:101-108`). Both tool descriptions, both
argument descriptors and both response shapes stay byte-identical.

## Decision Drivers

- A pause must be observable: after `sub_pause` returns, the lane's peer
  receives nothing until `sub_resume`.
- A resume must not lose the bytes produced during the pause, and must not
  duplicate a byte the consumer already read.
- One slow consumer must not degrade another lane on the same URI or another
  peer.
- No new storage and no unbounded growth: the resource ring already holds the
  bytes and already has a cap (10 MiB default,
  `src/adapters/config/internal/mod.rs:93`, `:97`).
- No new wire: no tool, argument, response line, error code, notification
  method or environment variable.
- The hot path stays lock-free (P-02,
  `doc/arch/memory/constitution.md:93-96`).

## Considered Options

- **Option A — gate both paths; one legacy catch-up notification at resume;
  compose inline payloads while paused and drop them.** The consumer reads
  the missed bytes from its own `(peer, uri)` cursor with
  `resources/read?cursor=auto`. Touches the bridge, the adapter (a cold-path
  resume hook, in the shape of the existing `rx_sink` / `close_sink` sinks)
  and composition. Uses the notifier, the cursor and the ring that already
  exist. Costs one notifier call per resume and one transient payload per
  producer write while paused; stores nothing.
- **Option B — gate both paths; re-send the paused bytes as inline fragments
  at resume.** No new per-lane store is needed: the bytes are in the resource
  ring. The lane cursor is relative to the lane, though, so the missing piece
  is the mapping from lane cursor to ring offset; with it the resume could
  read the ring and emit fragments, at the cost of de-duplication against a
  consumer that also read the same range.
- **Option C — gate only the legacy path.** Half a repair: an inline lane
  keeps receiving `notifications/ssh/output` while paused, so the flag stays
  partly cosmetic.
- **Option D — leave the code and correct the description.** Impossible
  without moving the frozen catalogue: the descriptions are part of it and
  P-05 forbids editing them in place.

## Decision Outcome

Chosen option: "Option A". It satisfies every driver with the least new
state. Option B is the recorded fallback if the legacy catch-up signal is
later rejected for an inline-only host; Option C leaves the defect half
repaired; Option D would move the frozen catalogue for a behaviour that can
be implemented instead.

The gate is an `Acquire` load of the existing `paused` flag on both producer
entries and once more at the dispatch consumer, so a fragment already queued
when the pause lands is dropped. While paused the inline payload is still
composed so `seq` and the absolute `cursor` advance; skipping the compose
would make `cursor_after` under-count every paused byte for the rest of the
lane life. The resume use case runs only on a true-to-false swap and sends
exactly one `notifications/resources/updated`; it carries no bytes and
counts as one delivered event in `events_sent`.

The published wording is only partly true and stays. The descriptions say
events "accumulate in the lane mpsc until sub_resume", "flush in order" and
that "the next push emits `LAG_DETECTED`". A lane without inline push
observes the equivalent of the first: nothing arrives while paused, one
notification arrives at resume, and the cursor read returns everything
missed. A lane **with** inline push does not: the paused bytes are never sent
as `notifications/ssh/output`, the next fragment's `cursor_after` jumps over
them, and the host must read the gap itself. Nothing drains the mpsc
(`ChannelMuxAdapter::spawn_drain` has no production caller) and nothing calls
`produce`, so the "in the lane mpsc", "flush in order" and `LAG_DETECTED`
clauses are not literal either. The descriptions are frozen by P-04 and
P-05; `session.md` records the three sentences and the unimplemented `NOOP`
status under the owner's open items, and `docs/API.md` tells the inline host
what it observes and what to do.

A lane opened by `resources/subscribe` is a residual of the same defect, not
a neutral fact. `sub_list` lists it with its `sub_id`
(`src/adapters/subscription/subscriber_lane.rs:708-713`), so a host can pause
it and get `OK`, but its delivery is the per-peer fan-out
(`memory_registry.rs:985-987`), which the gate does not reach. A test pins
the behaviour and `session.md` records it; the feature that associates a
`(peer, uri)` pair with its lane closes it.

The decision was taken in this session on the owner's instruction to carry
the whole update plan forward, and it is recorded for the owner's review.

### Consequences

- Good: a paused lane stops receiving both notification methods, and the
  flag stops being cosmetic.
- Good: the resume catch-up is lossless and duplicate-free for a consumer
  that reads from its cursor; the ring already bounds what can accumulate.
- Good: no new storage, no new wire, no new dependency, no new lock.
- Bad: one catch-up notification per resume, even when nothing was produced
  (AR-2 in the specification).
- Bad: an inline-only host that ignores legacy notifications gets no explicit
  resume signal; it resumes on the next producer write or the 5 s force
  flush (AR-1). An inline host that does see the legacy notification must
  read the gap itself (Q-1).
- Bad: a lane opened by `resources/subscribe` still delivers while paused;
  the gate does not reach the per-peer fan-out, and the wire does not return
  that `sub_id` (FR-011).
- Bad: the reproduction test's final assertion changes from 1 to 2 resource
  events (AR-5).
- Neutral: lane lifetime, `resources/unsubscribe`, `sub_replay` and the
  undrained mux stay as they are (Out of Scope in the specification).
