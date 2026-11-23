---
status: proposed
date: 2026-11-22
deciders: [Fabricio Archanjo]
consulted: []
informed: []
---

# Sub Open Resource Check

## Context and Problem Statement

Related decisions: the hexagonal layout is ADR 0002, lifecycle binding is
ADR 0003, the lane and `SubId` are ADR 0004's, the pause gate is ADR 0005's,
and the legacy lane removal is ADR 0006's. This ADR adds one guard to the
`sub_open` path; it changes no lane primitive and no other tool.

Refinement notes, so the overlap with the neighbours is explicit: ADR 0004
governs the lane this guard opens against and 0007 refines only the open step;
ADR 0006 removed the legacy lane and 0007 guards the surviving path; ADR 0003
governs the lifecycle, which `sub_open` does not call; ADR 0001 is the
dependency baseline and 0007 adds no dependency.

`sub_open` opens a lane on any URI that parses. `SubscribeUseCase` holds only
the lane, the activator and the capability registry
(`src/application/subscription_admin.rs:81-85`); `execute` parses, opens the
lane and asks for a debouncer (`:135-162`). No repository is queried, so a
typo in an id is accepted and yields a live lane that never produces. The lane
still costs one of the 16 per-URI and 1024 global slots
(`src/adapters/config/internal/mod.rs:200`, `:205`), a `sub_list` /
`sub_stats` row and one debouncer task
(`src/adapters/subscription/memory_registry.rs:371-410`), and nothing stops
that task (`:459-468` is reached only from the peer-registry unsubscribe).

`resources/subscribe` already refuses a missing Shell, Command, Transfer or
Session through `ensure_exists` (`src/application/subscribe_resource.rs:156-190`)
and deliberately lets Forward, Serial and Rsync through, because a forward
subscriber may attach before the producer publishes (`:175-177`) and serial
and rsync state live outside the application repositories (`:178-187`). The
two subscribe surfaces therefore disagree about a missing id.

The `sub_open` description lists `RESOURCE_NOT_FOUND`
(`src/infra/mcp/tool_router.rs:2389`; catalogue copy at
`docs/TOOL_CATALOG.md:1475` and `docs/tool-catalog.json:1501`). No production
path emits that code. The catalogue is frozen: `make tools-export-check` must
stay `unchanged`.

The reproduction tests, run on 2026-11-22 in `/var/tmp/ssh-mcp/sub-probe`,
fail on the base tree:

```
...::subscribe_unknown_resource_returns_resource_not_found
  sub_open of a missing resource must fail, got Ok(SubscribeOutcome { sub_id: …, uri: "shell://does-not-exist/output", … })
...::auto_close_lane_is_reaped_after_grace_window
  an auto_close lane must be closed once its grace window elapses
```

The second test is out of scope here; it is listed to show the patch also
carries the adjacent defects. The patch is **not in the tree**; it is kept in
`/var/tmp/ssh-mcp/sub-repro.patch` on the session host with the run log
`/var/tmp/ssh-mcp/sub-repro-run.log`.

## Decision Drivers

- A typo must fail at once, and a refused call must cost nothing.
- The two subscribe surfaces must answer a missing id the same way.
- The kind policy must exist once, including the three permissive kinds and
  their reasons.
- No adapter may import `crate::application`; the hexagon keeps its direction.
- The published wire must not move beyond the new refusal.
- The catalogue is frozen; the description's false `RESOURCE_NOT_FOUND` is
  recorded, not edited.

## Considered Options

- **Option A — reuse the existing per-kind variants and their codes.** The
  shared policy returns `ShellNotFound`, `CommandNotFound`, `TransferNotFound`
  or `SessionNotFound`; `sub_open` renders them through
  `render_tool_error` (`src/infra/mcp/tool_router.rs:167-173`) into
  `SHELL_NOT_FOUND`, `COMMAND_NOT_FOUND`, `TRANSFER_NOT_FOUND` and
  `SESSION_NOT_FOUND` (`:788-807`) with the cures at
  `src/infra/mcp/error_detail.rs:83-92`. No new variant, no new code, no new
  mapping. Touches the two use cases, the shared policy, composition and
  documents.
- **Option B — add `RESOURCE_NOT_FOUND` and map the four variants onto it.**
  Matches the frozen description. Costs a new wire code, a new `DomainError`
  variant or a renderer special case, a new cure line, and a second meaning
  for the same four variants beside every other tool that returns the per-kind
  code. A host that handles `SHELL_NOT_FOUND` from `ssh_shell_read` would need
  a second branch for `sub_open`.
- **Option C — keep `sub_open` permissive and correct only the description.**
  Cheapest, but leaves the dead lane, the cap cost and the debouncer task, and
  keeps the two surfaces disagreeing.

## Decision Outcome

Chosen option: **Option A**. The per-kind code is what the server already
answers for the same four variants on every other tool; reusing it keeps one
error vocabulary and needs no new wire value. Option B would satisfy the
description but split the vocabulary, and the description stays false either
way until the catalogue is regenerated (feature F8). Option C repairs nothing.

The answer for each kind is fixed in the specification's behavior table: the
variant, the code, the reason and the cure. Because `sub_open` never calls
`LifecyclePolicyPort::on_subscribe` (`src/application/subscription_admin.rs:135-162`),
it does not return `RESOURCE_GONE`: a resource whose lifecycle is `Closed`
while its repository row still exists is accepted, and only the row decides.
The production composition must install the guard on the object it puts in
`UseCases::sub_subscribe` (`src/composition/prod.rs:544-547`); the hand-built
tests alone would stay green if it did not (FR-011). A refusal is a `CallToolResult`
with `is_error: Some(true)` (`src/infra/mcp/helpers/structured.rs:105-111`);
`run_claimed` does not publish such a result to the idempotency cache
(`src/infra/mcp/tool_router.rs:519-533`), so a retry with the same key
re-runs the check.

The decision was taken by the orchestrator under the owner's instruction to
proceed and is recorded for the owner's review, not as the owner's own choice.
The owner may still choose Option B; the specification names every place the
choice reaches.

### Consequences

- Good: a missing id is refused before any lane, cap slot, stats row or
  debouncer exists.
- Good: `sub_open` and `resources/subscribe` answer a missing id with the same
  variant and the same code.
- Good: one kind policy, with the three permissive arms and their reasons, in
  one place.
- Good: no new dependency, no new lock, no new storage, no new wire code.
- Bad: a host that used a placeholder id, or that subscribed before the
  resource existed for the four repository kinds, now fails. The create tools
  return synchronously before the documented subscribe calls, so the documented
  workflows are unaffected; the tests and scripts that used placeholders are
  adapted (Q-1 in the specification).
- Bad: the frozen description's `RESOURCE_NOT_FOUND` stays false. Recorded in
  `session.md` and in the specification (FR-005); the alternative remains
  Option B when the catalogue is regenerated.
- Neutral: the check narrows but does not close the window between the check
  and the open; a resource removed in that window still yields a lane. F7
  closes it later.
- Neutral: lane lifetime, `sub_replay`, the filter cap, the daemon ops and the
  resource-end leaks stay as they are (Out of Scope in the specification).
- Owner review: the answer code is registered above, the synthetic-id risk is
  AR-1 and the description mismatch AR-4 in the specification (Q-1, Q-6,
  Q-7).
