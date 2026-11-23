---
id: 01a20295-ecdf-7a90-a102-1f4595a1152f
number: 006
slug: resources-subscribe-lane-removal
status: implemented
created_at: 2026-11-22T09:28:29.663995681Z
---
# Feature Specification: Resources Subscribe Lane Removal

Feature: 006-resources-subscribe-lane-removal
Created: 2026-11-22

`resources/subscribe` mints a per-call `SubId` lane that nothing can use.
The id is returned by the use case
(`src/application/subscribe_resource.rs:168-171`) but read in production only
by the log at `src/infra/mcp/resource_handlers.rs:825`; rmcp's `subscribe`
returns `()` (`:817`), so the client never receives it. The lane is opened with
`LanePolicy::default()` (`src/application/subscribe_resource.rs:182`), whose
`peer` is `None` (`src/ports/subscriber_lane.rs:33-46`), and
`LaneFanoutBridge` skips a lane without a peer
(`src/adapters/subscription/lane_bridge.rs:282-284`, `:264-266`, `:483-485`),
so nothing is ever delivered through it. `resources/unsubscribe` carries no
`sub_id` on the wire (`src/infra/mcp/resource_handlers.rs:866`), so the
lane-close branch of `UnsubscribeResourceUseCase::execute`
(`src/application/unsubscribe_resource.rs:106-113`) never runs. What is left
is a leak: a `sub_list` / `sub_stats` row and one of the 16 per-URI lane slots
(`src/adapters/subscription/subscriber_lane.rs:536-545`), consumed per
subscribe/unsubscribe cycle; the 17th cycle fails with
`max subs per URI exceeded` (measured; see the reproduction tests below).

Every fact in this specification was measured on 2026-11-22 on the tree of
feature 005, in the throwaway copy `/var/tmp/ssh-mcp/resub-probe` made from
`/root/dev/ssh-mcp`. Every `file:line` is the repository line, printed in the
same step that wrote it here. The reproduction tests are **not in the tree**;
they are kept in `/var/tmp/ssh-mcp/resub-repro.patch` on the session host with
its run log `/var/tmp/ssh-mcp/resub-repro-run.log`. No production code changes
in this phase.

## Why the removal

- The lane is unobservable and inert: no `sub_id` on the wire, no peer on the
  lane, no delivery through it, and every operation a client could run on an
  id read from `sub_list` (`sub_pause`, `sub_filter`, `sub_replay`,
  `sub_close`) has no effect on the notifications that peer receives.
- It is the last open residual of feature 005. Feature 005 gated both lane
  paths (`src/adapters/subscription/lane_bridge.rs:269`, `:289`, `:483`) but
  could not gate the per-peer fan-out
  (`src/adapters/subscription/memory_registry.rs:985-990`); after this feature
  no lane exists for a `resources/subscribe` subscription, so the residual
  ends instead of being repaired.
- The removal deletes three defects at once: the lane leak on unsubscribe
  (`session.md:124-125`), the 17th-cycle cap failure (FR-011), and the false
  promise that `sub_pause` gates such a subscription (`session.md:112-117`).
- A repeated subscribe of one `(peer, URI)` must count once. Today it counts
  twice in the lifecycle refcount (`src/application/subscribe_resource.rs:164`
  runs on every call) while the registry keeps one entry
  (`src/adapters/subscription/memory_registry.rs:592-596`), so one
  `resources/unsubscribe` strands a refcount.

## Decision provenance

Design 2 of the measurement report was chosen by the orchestrator under the
owner's instruction to proceed, and is recorded in
[ADR 0006](../../adr/0006-resources-subscribe-lane-removal.md) as a decision
registered for the owner's review, not as the owner's own choice. Design 1
(a `(peer, URI)` to `SubId` association) is the rejected alternative and the
ADR states why with the measured facts.

## Removed symbols, with every reference

`findReferences` was run on each symbol before writing this list. Production
references are named; test references are listed so the plan can update them.

### `SubscribeResourceOutcome.sub_id` and the lane handle

- `src/application/subscribe_resource.rs:60` — field declaration.
  `findReferences`: `:170` (constructor), `:625` and `:659` (tests),
  `src/infra/mcp/resource_handlers.rs:825` (the log). The field, the
  `SubId` import and the `log_subscribe_outcome` function go.
- `src/application/subscribe_resource.rs:90` — `lane` field.
  `findReferences`: `:118`, `:129`, `:180`. The field goes.
- `src/application/subscribe_resource.rs:128` — `with_subscriber_lane`.
  `findReferences`: `src/composition/prod.rs:524`, test at `:652`.
- `src/application/subscribe_resource.rs:174` — `open_lane_if_wired`.
  `findReferences`: `:183` (its only call). The function goes.
- `src/composition/prod.rs:517-526` — the composition call that wires the
  lane into `SubscribeResourceUseCase`; `Arc::clone(&lane_admin)` at `:526`
  goes with it.
- `src/infra/mcp/resource_handlers.rs:816` and `:820-832` — the call to
  `log_subscribe_outcome` and the function itself; `SubscribeResourceOutcome`
  stays as the return type of `execute` but keeps only `uri`.

### `UnsubscribeResourceRequest.sub_id` and the lane handle

- `src/application/unsubscribe_resource.rs:41` — field declaration.
  `findReferences`: `:107` (the guard), `:228`, `:352`, `:389`, `:439`
  (test constructors), `src/infra/mcp/resource_handlers.rs:866` (the only
  production constructor, which already passes `None`). The field and the
  `SubId` import go.
- `src/application/unsubscribe_resource.rs:64` — `lane` field.
  `findReferences`: `:77`, `:84`, `:106`.
- `src/application/unsubscribe_resource.rs:83` — `with_subscriber_lane`.
  `findReferences`: `src/composition/prod.rs:529`, tests `:346`, `:381`.
- `src/application/unsubscribe_resource.rs:106-113` — the lane-close branch.
- `src/composition/prod.rs:529-531` — the composition call that wires the
  lane into `UnsubscribeResourceUseCase`.

### What stays

`SubscriberLaneAdapter`, `LaneAdmin`, `LanePolicy`, the `sub_*` tools and
`UnsubscribeUseCase` (`src/application/subscription_admin.rs:239-273`) are
untouched: `sub_open` lanes, `sub_close`, `sub_pause` and the rest keep
working exactly as today. The registry entry, its cursors and the debouncer
stay; only the lane minted by `resources/subscribe` goes.

## Test fates

Every existing test that reads a removed symbol, calls `with_subscriber_lane`
on either use case, or pins the residual has exactly one fate below. "Removed"
always names the subject that disappeared; "replaced" names the new test and
what it asserts, so the coverage is visibly not weaker.

| Existing test | File:line | Asserts today | Fate |
| --- | --- | --- | --- |
| `subscribe_outcome_sub_id_is_none_when_lane_not_wired` | `src/application/subscribe_resource.rs:615` | `outcome.sub_id` is `None` when no lane is wired (`:625`) | removed — the `Option<SubId>` outcome field is the only subject; replaced by `resources_subscribe_records_no_lane_and_delivers_once` (no lane, one delivery) |
| `subscribe_with_lane_returns_sub_id_on_outcome` | `src/application/subscribe_resource.rs:628` | a wired lane makes the outcome carry a non-empty id (`:652`, `:659`) | removed — the lane wiring and the outcome id are the only subject; replaced by `resources_subscribe_records_no_lane_and_delivers_once` |
| `unsubscribe_routes_to_canonical_uri` | `src/application/unsubscribe_resource.rs:232` | the canonical URI reaches the registry | adapted — only `req()` loses the field |
| `unsubscribe_is_idempotent_for_unknown_peer` | `src/application/unsubscribe_resource.rs:246` | two calls for an unknown peer both succeed | adapted — only `req()` loses the field |
| `malformed_uri_returns_invalid_argument` | `src/application/unsubscribe_resource.rs:263` | a bad scheme is `InvalidArgument` and the registry is untouched | adapted — only `req()` loses the field |
| `unsubscribe_routes_each_scheme_to_canonical_form` | `src/application/unsubscribe_resource.rs:278` | each of five schemes routes canonically | adapted — only `req()` loses the field |
| `missing_id_returns_invalid_argument` | `src/application/unsubscribe_resource.rs:295` | a URI without an id is `InvalidArgument` | adapted — only `req()` loses the field |
| `unsubscribe_carries_peer_id_through_to_registry` | `src/application/unsubscribe_resource.rs:308` | the peer id reaches the registry | adapted — only `req()` loses the field |
| `unsubscribe_with_lane_closes_matching_sub_id` | `src/application/unsubscribe_resource.rs:321` | a request carrying a `sub_id` closes that lane (`:346`, `:352`, `:358-364`) | removed — close-by-`sub_id` is the only subject; replaced by `resources_unsubscribe_removes_the_registry_entry` (entry gone, second call succeeds) and `resources_unsubscribe_leaves_a_sub_open_lane_open` (a lane is no longer touched) |
| `unsubscribe_tolerates_unknown_sub_id_when_lane_wired` | `src/application/unsubscribe_resource.rs:367` | an unknown `sub_id` is tolerated (`:381`, `:389`) | removed — tolerating an unknown id is the only subject; replaced by `resources_unsubscribe_removes_the_registry_entry` (second call is a silent success) |
| `release_resource_reaches_closed_via_production_unsubscribe_path` | `src/application/unsubscribe_resource.rs:398` | a `release_when_no_subs` resource reaches `Closed` and the session refcount is debited (`:439`) | adapted — only the request constructor loses the field |
| `resources_subscribe_lane_is_listed_but_pause_does_not_gate_the_peer_fanout` | `src/adapters/subscription/memory_registry.rs:1787` | the lane is listed, pause returns `OK`, the peer event still arrives, lane counters stay 0 | replaced by `resources_subscribe_records_no_lane_and_delivers_once` in the application module (no lane row; one peer event; zero counters) |

The tests not listed read neither removed symbol and are kept unchanged.

### Tests of `/var/tmp/ssh-mcp/resub-repro.patch`

The measurement patch is **not in the tree**; it lives on the session host at
`/var/tmp/ssh-mcp/resub-repro.patch` with its run log
`/var/tmp/ssh-mcp/resub-repro-run.log`. Every test it holds has one fate:

| Patch test | File in the patch | Fate |
| --- | --- | --- |
| `seventeenth_resources_subscribe_with_interleaved_unsubscribe_succeeds` | `src/application/subscribe_resource.rs` | becomes the proving test of FR-011, name kept; it fails on the base tree at round 16 |
| `repeated_resources_subscribe_inflates_refcount_and_lanes_only` | `src/application/subscribe_resource.rs` | replaced by `repeated_resources_subscribe_counts_once_in_the_lifecycle` (one entry, refcount 1 then 0, no lane) |
| `lane_limit_failure_leaves_a_live_subscription_that_still_delivers` | `src/application/subscribe_resource.rs` | replaced by `lifecycle_refusal_leaves_no_registry_entry_and_no_delivery` (`ResourceGone`, empty snapshot, no event) |
| `resources_unsubscribe_closes_the_lane_it_opened` | `src/application/subscribe_resource.rs` | replaced by `resources_subscribe_records_no_lane_and_delivers_once` (no lane, one delivery) |
| `resources_unsubscribe_keeps_the_lane_it_does_not_know_about` | `src/application/unsubscribe_resource.rs` | replaced by `resources_unsubscribe_removes_the_registry_entry` (entry gone, second call succeeds) |
| `resources_subscribe_lane_delivers_once_and_never_through_the_bridge` | `src/adapters/subscription/memory_registry.rs` | replaced by `resources_subscribe_records_no_lane_and_delivers_once`; the direct peerless-vs-peer lane comparison is the `sub_open` behaviour that stays |
| `peer_gc_and_drop_peer_leave_the_resources_subscribe_lane_open` | `src/adapters/subscription/memory_registry.rs` | replaced by `resources_subscribe_records_no_lane_and_delivers_once`; the GC subject is out of scope |
| `gc_debits_the_lifecycle_refcount_of_the_reaped_peer` | `src/adapters/subscription/memory_registry.rs` | not added — reproduces an out-of-scope defect (peer GC lifecycle debit) and still fails on the repaired tree |
| `concurrent_unsubscribe_does_not_swallow_a_new_subscriber` | `src/adapters/subscription/memory_registry.rs` | not added — reproduces an out-of-scope race and still fails on the repaired tree |
| `installing_a_second_forwarder_keeps_the_first_alive` | `src/adapters/subscription/legacy.rs` | not added — reproduces an out-of-scope defect (per-session forwarder) |
| `subscribe_unknown_resource_returns_resource_not_found` | `src/application/subscription_admin.rs` | not added — out of scope (`sub_open` existence check) |
| `auto_close_lane_is_reaped_after_grace_window` | `src/application/subscription_admin.rs` | not added — out of scope (lane lifetime) |

## User Stories

- As a host implementer, I want a `resources/subscribe` subscription to cost
  one registry entry and no lane, so that a long subscribe/unsubscribe loop
  never exhausts the per-URI lane cap.
- As a host implementer, I want a repeated subscribe of the same `(peer, URI)`
  to count once, so that one `resources/unsubscribe` balances the lifecycle.
- As a host implementer, I want a subscribe that fails to leave nothing
  behind, so that a failed call never delivers a notification.
- As an operator, I want `sub_list` and `sub_stats_all` to show only lanes
  that can actually deliver, so that the lane inventory stays meaningful.
- As a maintainer, I want the documents that describe a peerless lane
  corrected, so that the next reader does not look for a `sub_id` that is not
  on the wire.

## Out of Scope

Each item is a recorded defect or a measured adjacent behaviour, with the
reason it is not repaired here. The first two are added to the defect list of
`session.md` in this phase (FR-010).

- **Peer GC of the hexagonal registry and its lifecycle debit.**
  `MemoryRegistry::drop_peer_sync` removes the registry entry without calling
  `LifecyclePolicyPort::on_unsubscribe`
  (`src/adapters/subscription/memory_registry.rs:424-429`), and the use case
  that would run it, `PeerGcUseCase::execute`
  (`src/application/peer_gc.rs:68-71`), has no production caller: it is
  constructed at `src/composition/prod.rs:533` and no `.peer_gc` read exists;
  the pump at `src/composition/prod.rs:805-817` calls only the legacy global
  and the `PeerTable`. Reproduced by
  `crate::adapters::subscription::memory_registry::tests::gc_debits_the_lifecycle_refcount_of_the_reaped_peer`,
  which is not in the tree; it is in `/var/tmp/ssh-mcp/resub-repro.patch` on
  the session host and fails on the base tree with
  `assertion left == right failed: the reaped peer's subscription must be
  debited from the lifecycle` (left 1, right 0).
- **The `unsubscribe_sync` race.** The shard guard is dropped after
  `became_empty` is computed and the URI is removed afterwards, so a
  concurrent subscribe that lands in the window is removed with the empty
  entry (`src/adapters/subscription/memory_registry.rs:458-467`). Reproduced
  by `...::concurrent_unsubscribe_does_not_swallow_a_new_subscriber`, which is
  not in the tree; it is in `/var/tmp/ssh-mcp/resub-repro.patch` on the
  session host and fails on the base tree with
  `round 1: concurrent unsubscribe swallowed the new subscriber`.
- **The per-session producer forwarder.** Every server instance installs
  itself as the single process-wide forwarder
  (`src/composition/prod.rs:544-546`, `session.md:105-107`).
- **`sub_open` not checking that the resource exists**
  (`src/application/subscription_admin.rs:135-162`, `session.md:122-123`).
- **Lane lifetime (`auto_close`, `lease`)** stored and never enforced
  (`src/adapters/subscription/subscriber_lane.rs:156`, `session.md:118-119`).
- **The daemon `subscribe` op dropping `lifetime`, `grace_ms`, `lag_policy`,
  `filter` and `start_cursor`** (`src/embed/parser.rs:95-111`,
  `src/embed/dispatcher.rs:578-585`, `session.md:160-162`).
- **The entry/lifecycle window.** A concurrent `resources/unsubscribe` that
  lands between the registry entry and `on_subscribe`
  (`src/application/subscribe_resource.rs:159-164`) removes the entry first,
  makes `on_unsubscribe` underflow and return `DomainError::Internal`
  (`src/adapters/lifecycle/refcount.rs:546-555`), and leaves the lifecycle
  count at 1 with no entry. Pre-existing and unchanged by this feature
  (Q-7); recorded in `session.md`.
- **The registry `SubId` and its per-`SubId` cursors.** The registry still
  mints a `SubId` per subscriber
  (`src/adapters/subscription/memory_registry.rs:825`) and maintains
  `(sub_id, uri)` cursors (`:599-608`), but after this feature
  `sub_id_for` (`:300-313`), `sub_byte_cursor` (`:281-288`),
  `advance_sub_byte_cursor` (`:290-298`) and `sub_progress` (`:265-279`) have
  no production caller outside the module. Left in place (Q-10).
- **The frozen tool descriptions and argument descriptors.** They are
  byte-identical after this feature; `make tools-export-check` must stay
  `unchanged` (FR-007). The catalogue was checked and says nothing that
  becomes false: it mentions `resources/subscribe` only as a preferred push
  path over polling (`docs/TOOL_CATALOG.md:1125`, `:1166`,
  `docs/tool-catalog.json` for the same two strings) and as a host capability
  (`docs/TOOL_CATALOG.md:673`, `:1225`).
- **Every other recorded defect**, and the conformity work on the baseline
  errors.

## Functional Requirements

### FR-001 `resources/subscribe` opens no lane

`SubscribeResourceUseCase::execute` parses, confirms the resource, registers
the peer and bumps the lifecycle; it no longer calls `LaneAdmin::open`. The
outcome keeps `uri` only. The handler logs nothing about a `sub_id`. After a
successful call `SubscriberLanePort::list_subs`
(`src/adapters/subscription/subscriber_lane.rs:715-720`) returns no lane for
the URI, and delivery is the per-peer fan-out only: one
`notifications/resources/updated` per flush.

**Acceptance.**
`crate::application::subscribe_resource::tests::resources_subscribe_records_no_lane_and_delivers_once`
subscribes a seeded shell against a real `MemoryRegistry` and a real
`SubscriberLaneAdapter`, asserts `list_subs()` is empty, triggers one
byte-threshold flush and asserts exactly one recorded notification for the
peer and zero lane counters. It is a `#[tokio::test]`; the scenario stays
`@planned`. The two tests that read the removed outcome field are removed per
the fate table above, and their coverage lives in this test.

### FR-002 `resources/unsubscribe` carries no `sub_id` and removes the entry

`UnsubscribeResourceRequest` keeps `uri` and `peer_id` only; the use case
parses, removes the `(peer, URI)` entry and debits the lifecycle only when the
registry reports that it removed one (`:114-132`, unchanged). It does not
touch a `sub_open` lane on the same URI.

**Acceptance.**
`crate::application::unsubscribe_resource::tests::resources_unsubscribe_removes_the_registry_entry`
subscribes, unsubscribes, asserts the snapshot is empty and asserts a second
unsubscribe is a silent success;
`...::resources_unsubscribe_leaves_a_sub_open_lane_open` opens a `sub_open`
lane on the same URI, runs the use case and asserts the lane still exists.
Both are `#[tokio::test]`; the scenarios stay `@planned`.
`...::unsubscribe_with_lane_closes_matching_sub_id` and
`...::unsubscribe_tolerates_unknown_sub_id_when_lane_wired` are removed with
the field; their coverage is in these two tests, per the fate table.

### FR-003 A repeated subscribe of one `(peer, URI)` counts once in the lifecycle

`SubscriberRegistryAsync::subscribe` reports whether it created a `(peer, URI)`
entry or refreshed an existing one. `SubscribeResourceUseCase::execute` calls
`LifecyclePolicyPort::on_subscribe` only on creation, so two calls for the same
pair leave one registry entry and refcount 1, and one `resources/unsubscribe`
returns the count to 0. The debouncer activation stays keyed on the first
subscriber of the resource, which is a different fact: the current
`insert_subscriber` return (`src/adapters/subscription/memory_registry.rs:596`,
`entry.len() == 1`) is false for a new second peer and true for a refresh of
the sole peer, so the implementation derives the pair's prior existence before
the `retain` (`:594`; `check_capacity` already computes it at `:541-543`).

The port method is `SubscriberRegistryAsync::subscribe`
(`src/ports/subscriber_registry.rs:157-163`). `goToImplementation` on the
trait method returns nothing because the `#[trait_variant::make]` alias
(`src/ports/subscriber_registry.rs:149-150`) is macro-generated; a text search
`SubscriberRegistryAsync for` finds every implementor:

- `src/adapters/subscription/memory_registry.rs:809` — the production
  `MemoryRegistry<N>`.
- `src/application/subscribe_resource.rs:358` — the test `RecordingRegistry`.
- `src/application/unsubscribe_resource.rs:186` — the test `RecordingRegistry`.

Production callers of the method: `src/application/subscribe_resource.rs:160`
only. Test callers in `memory_registry.rs` and
`tests/chaos/chaos37_v4_legacy_subscribe_with_v5_features.rs:93` ignore the
return value and need no logic change, only the stub signatures.

**Acceptance.**
`crate::application::subscribe_resource::tests::repeated_resources_subscribe_counts_once_in_the_lifecycle`
subscribes twice, asserts one registry entry, refcount 1 and no lane, then
unsubscribes once and asserts refcount 0 and an empty snapshot; a second
unsubscribe stays at 0. It fails on the base tree with refcount 2. It is a
`#[tokio::test]`; the scenario stays `@planned`. The measurement test
`...::repeated_resources_subscribe_inflates_refcount_and_lanes_only` is
replaced by this one.

### FR-004 A failed subscribe leaves nothing behind

After the registry entry exists, the only remaining fallible step is
`LifecyclePolicyPort::on_subscribe` (`src/application/subscribe_resource.rs:164`).
The port has two implementors. `NoopLifecycle::on_subscribe` always returns
`Ok(())` (`src/ports/lifecycle_policy.rs:175-177`).
`RefcountedLifecycleAdapter::on_subscribe` (`src/adapters/lifecycle/refcount.rs:524-540`)
returns `DomainError::ResourceGone` from two sites and nothing else:
`promote_to_observed` when the entry is already `Closed` before the increment
(`:419-424`), and `compensate_after_subscribe` when a concurrent
`force_close` / grace-timer fire closes the entry between
`promote_to_observed` and the `fetch_add` (`:482-488`). In the second case the
adapter has already undone its own increment (`:483`); in the first no
increment happened. On **any** error from `on_subscribe` (today only
`ResourceGone`) `execute` removes the created entry again with
`SubscriberRegistryAsync::unsubscribe` — only when the call created it, so a
refresh does not delete a pre-existing subscription — and returns the error.
The lifecycle count is therefore already at its pre-call value when the
error is returned. A client told the call failed receives no notification for
it.

The rollback removes the record unconditionally for a created call, even when
a concurrent refresh of the same `(peer, URI)` has replaced it in the
meantime; the refresh client's success is not re-validated. The justification
is consistency, not quiescence: once the lifecycle refuses the resource, every
later `resources/subscribe` of it is refused the same way while the retained
`Closed` entry lives (`src/adapters/lifecycle/refcount.rs:371-380`,
`:419-424`), so removing the record keeps the registry in step with what every
later caller is told. The lifecycle `Closed` state does not tear the resource
down (`src/adapters/lifecycle/grace_timer.rs:148`;
`src/adapters/lifecycle/refcount.rs:564`), so a still-running producer can
emit and the refresh caller that was told success can miss those
notifications (AR-4).

`DomainError::ResourceGone` displays as `resource gone: {0}`
(`src/domain/error.rs:130`) and `map_resource_error` sends it to
`McpError::resource_not_found(message, None)`
(`src/infra/mcp/resource_handlers.rs:110`, `:72-79`); the server states no
other code name for it.

**Acceptance.**
`crate::application::subscribe_resource::tests::lifecycle_refusal_leaves_no_registry_entry_and_no_delivery`
tracks a shell, `force_close`s it, calls the use case, asserts the
`DomainError::ResourceGone` variant, asserts the snapshot is empty and the
refcount 0, triggers a flush and asserts no notification. It is a
`#[tokio::test]`; the scenario stays `@planned`. The measurement test
`...::lane_limit_failure_leaves_a_live_subscription_that_still_delivers` is
replaced by this one (the lane cap can no longer fail the call).

### FR-005 The feature-005 residual ends and its pin changes

No lane exists for a `resources/subscribe` subscription, so there is nothing
for `sub_pause` to fail to gate. The pinning test
`crate::adapters::subscription::memory_registry::tests::resources_subscribe_lane_is_listed_but_pause_does_not_gate_the_peer_fanout`
(`src/adapters/subscription/memory_registry.rs:1787-1851`) is replaced by
FR-001's proving test in
`crate::application::subscribe_resource::tests::resources_subscribe_records_no_lane_and_delivers_once`,
which asserts `list_subs()` is empty, triggers a flush, asserts exactly one
peer event and asserts the lane counters stay zero. No test under
`src/adapters/` imports `crate::application`; the adapter module keeps only its
own tests. The passages are corrected:

| Document | Line | Today | Required |
| --- | --- | --- | --- |
| `doc/arch/functional/functional.md` | 229-236 | "Open limitation: a lane opened by `resources/subscribe` is delivered by the per-peer fan-out … which the flag does not gate" | no lane is opened; the per-peer fan-out delivers; the `sub_resume` cure works for `sub_open` lanes |
| `doc/arch/functional/functional.md` | 341 | F8 row "a lane opened by `resources/subscribe` is still delivered while paused … unproven" | "`resources/subscribe` opens no lane; the per-peer fan-out delivers" bound to the new test |
| `doc/arch/runbooks/incident-push-delivery-lost.md` | 106-111 | "Open residual: a lane opened by `resources/subscribe` … close the lane with `sub_close` and open a new one with `sub_open`" | no residual; `sub_list` shows no row for a `resources/subscribe` subscription |
| `docs/API.md` | 1128 | "A lane opened by `resources/subscribe` is delivered by the per-peer fan-out and keeps delivering while paused." | `resources/subscribe` opens no lane; pause has no target there |
| `session.md` | 112-117 | defect entry "closed by the feature that associates a `(peer, uri)` pair with its lane" | marked repaired by this feature (the removal, not the association) |

**Acceptance.** The five passages read as required and the replacement test
exists and passes. The scenario stays `@planned`.

### FR-006 The documents that are false today state the truth

| Document | Line | Today | Required |
| --- | --- | --- | --- |
| `docs/LLM_GUIDE.md` | 69 | "Every `sub_open` (or legacy `resources/subscribe`) returns a `sub_id` …" | only `sub_open` returns a `sub_id`; a legacy subscription is peer-keyed and has no lane |
| `docs/LLM_GUIDE.md` | 73 | "only explicit `sub_close` or peer-transport disconnect closes it" | `sub_close` closes a `sub_open` lane; `resources/unsubscribe` removes the legacy subscription |
| `docs/RESOURCES.md` | 291-292 | diagram: `sub_close … (or resources/unsubscribe)` → `lane_admin.close(sub_id)` | `resources/unsubscribe` removes the registry entry; only `sub_close` reaches a lane |
| `docs/ARCHITECTURE.md` | 780 | "Per `(SubId, Uri)` cursor in v5; `(PeerId, Uri)` synthesised for legacy hosts" | the legacy method keeps only the `(PeerId, Uri)` cursor and opens no lane |
| `doc/arch/functional/functional.md` | 203-205 | "the lane path … The legacy `resources/subscribe` path and the lane path share the registry" | `resources/subscribe` uses the registry only |
| `docs/OPERATIONS.md` | 71 | "A host called `sub_open` (or the legacy `resources/subscribe`), the call returned a `sub_id`" | only `sub_open` returns a `sub_id` |
| `docs/LLM_GUIDE.md` | 236-238 | "Track every sub_id returned by sub_open (or the legacy resources/subscribe)" | `sub_open` returns the id `sub_close` takes; a legacy subscription is ended with `resources/unsubscribe` |
| `docs/RESOURCES.md` | 266-272 | one message "(or resources/subscribe)", "subscribe / register lane", "ack (SUB_ID)" | two paths: `sub_open` registers a lane and acks `SUB_ID`; `resources/subscribe` subscribes and returns the empty result |
| `README.md` | 32 | "Each `resources/subscribe` mints a `SubId` … mpsc::channel …" | `sub_open` mints the id and lane; a legacy subscription is peer-keyed with no lane |
| `src/application/subscribe_resource.rs` | 56-60 | outcome doc: "Surfaced via `_meta.sub_id` …" | the outcome carries the canonical URI only |
| `CHANGELOG.md` | `[Unreleased]` → `### Changed` | — | one new bullet: the lane is no longer opened; `sub_list` / `sub_stats` / `sub_stats_all` show no row and no lane slot is consumed |
| `docs/MIGRATION.md` | `## v7.1.3 → unreleased …` | — | one bullet under that section: what a host observes and that the wire is unchanged |

`docs/MIGRATION.md:625` and `:641` (inside `## v4 → v5`) and
`docs/adr/0004-channel-mux-fairness.md:136-137` are history and stay as
written; the new MIGRATION bullet says the v5 note predates the removal.
The generated catalogue files (`docs/TOOL_CATALOG.md`,
`docs/tool-catalog.json`) stay unchanged (FR-007).

**Acceptance.** The twelve edits read as required; `grep -rn
'resources/subscribe' docs README.md` prints no sentence that gives the
legacy method a lane or a `sub_id` except the historical ADR/release-note
passages named above, which stay. The two historical passages are unchanged.
The scenario stays `@planned`.

### FR-007 No wire change

No tool, argument, descriptor, response line, status value, error code,
notification method or environment variable changes. `resources/subscribe`
already answers the empty result (`src/infra/mcp/resource_handlers.rs:817`);
`resources/unsubscribe` already answers the empty result (`:869-870`). The
`SSH_MAX_SUBS_PER_URI` / `SSH_MAX_SUBS_TOTAL` lane limits stay, but a
`resources/subscribe` subscription no longer consumes them (the registry's own
caps at `src/adapters/subscription/memory_registry.rs:167-175` still apply).

**Acceptance.** `make tools-export-check` reports `unchanged` for both
catalogue files; the synchronous
`tests/v4_smoke.rs::prod_server_exposes_full_tool_surface` stays green. The
scenario stays `@planned`.

### FR-008 The gate is green with the reference counts plus the new tests

The verified-change gate of P-06 (`doc/arch/memory/constitution.md:109-112`)
passes, run from `/root/dev/ssh-mcp`:

- `cargo fmt --all -- --check`
- `cargo clippy --all-features -- -D warnings`
- `cargo nextest run --test-threads num-cpus --lib` (at least 2142 passing;
  2141 on the feature-005 tree, −5 tests that leave, +6 that arrive)
- `cargo nextest run --test-threads num-cpus --tests --features test-fixtures`
  (at least 2316 passing; the same arithmetic over 2315)
- `cargo build --no-default-features --target-dir target/no-default-features`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_http.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_transfer_progress.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v7_rsync_http.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v47_subscriptions.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v47_idempotency.py`
- `cargo audit`

No lint is disabled and no `#[allow]` or `#[expect]` is added. Coverage is
governed by the two fate tables: no test is ignored or weakened, and no test
is removed except those the tables mark removed or replaced; every
replacement asserts at least the coverage it replaces. Patch tests marked
"not added" stay in `/var/tmp/ssh-mcp/resub-repro.patch` and are not part of
the gate.

**Acceptance.** Each command exits 0; the two nextest runs report at least the
counts above; `git diff` shows no `#[ignore]`, no test removed outside the
tables, and no new `#[allow]` or `#[expect]`. The scenario stays `@planned`.

### FR-009 `speckit validate` adds no finding to the baseline

The baseline is the error set `speckit validate` reports on the tree before
this feature with the binary in use, `speckit v0.49.0`
(`cdb45907 2026-11-22T08:38:26Z`): exit 6 with 68 errors and no warning.
Rules: `fqcn.relative-super` 46, `branch.else` 6,
`pin.ci.unpinned-action` 5, `pin.docker.unpinned-image` 2, and one each of
`readme.mermaid.missing`, `readme.index.missing`, `readme.image.missing`,
`readme.arch-link.missing`, `pin.cargo.lockfile-missing`,
`llms-txt.file.missing`, `fqcn.glob-import`, `crew.roster.dangling` and
`ci.gitlab.missing`. No rule is waived and no feature-006 file is named.

**Acceptance.** `speckit validate` reports 68 errors with that rule set and no
warning, and no finding names a file of this feature. The scenario stays
`@planned`.

### FR-010 The two out-of-scope defects are recorded

`session.md` gains, in "Defects verified in the code during the session", one
entry for the never-invoked `PeerGcUseCase` (citing
`src/application/peer_gc.rs:68-71` and `src/composition/prod.rs:533`, `:805-817`,
with `crate::adapters::subscription::memory_registry::tests::gc_debits_the_lifecycle_refcount_of_the_reaped_peer`)
and one for the `unsubscribe_sync` race (citing
`src/adapters/subscription/memory_registry.rs:458-467`, with
`...::concurrent_unsubscribe_does_not_swallow_a_new_subscriber`). Both tests
are **not in the tree**; they are in `/var/tmp/ssh-mcp/resub-repro.patch` on
the session host and fail on the base tree with the messages quoted in Out of
Scope. The entries say so.

**Acceptance.** Both entries exist with their `file:line` and test name. The
scenario stays `@planned`.

### FR-011 More than 16 subscribe/unsubscribe cycles on one URI succeed

The per-URI lane cap defaults to 16
(`DEFAULT_MAX_SUBS_PER_URI`, `src/adapters/config/internal/mod.rs:200`, floored
at 1 by `:202`). A client that repeats `resources/subscribe` then
`resources/unsubscribe` on one URI performs 17 cycles; every call succeeds,
because no lane is opened and `unsubscribe_sync` releases the registry slot
(`src/adapters/subscription/memory_registry.rs:439-474`). The 17th subscribe
no longer fails with `max subs per URI exceeded`.

**Acceptance.**
`crate::application::subscribe_resource::tests::seventeenth_resources_subscribe_with_interleaved_unsubscribe_succeeds`
runs 17 subscribe/unsubscribe rounds against a lane adapter still constructed
with the default cap of 16 and panics on the first failed round. It is in
`/var/tmp/ssh-mcp/resub-repro.patch`, not in the tree; the feature adds it
under the same name. It fails on the base tree at round 16 with
`max subs per URI exceeded for shell://sh-1/output (limit 16)`. It is a
`#[tokio::test]`; the scenario stays `@planned`.

## Security Requirements

- **Data sensitivity/classification.** The removal moves no bytes and stores
  nothing new. The subscription still carries the URI and the live peer handle;
  the shell and command bytes stay in the resource ring that already holds
  them (default cap 10 MiB,
  `src/adapters/config/internal/mod.rs:93`, `:97`).
- **Authentication/authorization.** No authenticated surface and no credential
  moves; the lane handle and the refcount are in-process state.
- **Input validation.** The URI is parsed exactly as today
  (`src/application/subscribe_resource.rs:145-155`). No new parser, no new
  field: `sub_id` is removed, so one less opaque value is accepted.
- **Cryptography in transit/at rest.** None is added; notifications keep their
  transport.
- **Logging/audit.** One `tracing::info!` line disappears
  (`src/infra/mcp/resource_handlers.rs:826-830`, "resources/subscribe: SubId
  synthesised"). No line is added; no level, target or format changes
  elsewhere.
- **Error-handling information exposure.** No error code or message changes.
  `ResourceGone` keeps its existing text (`src/domain/error.rs`), and the new
  compensation path returns it unchanged.

## Acceptance Scenarios

Given a seeded shell and a peer
When  `resources/subscribe` runs
Then  no lane exists for the URI and one flush records exactly one peer event (FR-001).

Given a subscribed peer
When  `resources/unsubscribe` runs
Then  the registry entry is gone and a second call succeeds (FR-002).

Given a `sub_open` lane on a URI
When  `resources/unsubscribe` runs for the same URI
Then  the lane still exists (FR-002).

Given one `(peer, URI)` subscribed twice
When  `resources/unsubscribe` runs once
Then  the lifecycle refcount is 0 and the snapshot is empty (FR-003).

Given a tracked shell already `Closed`
When  `resources/subscribe` runs
Then  it returns `DomainError::ResourceGone` (mapped to `McpError::resource_not_found`, message `resource gone: …`), the snapshot stays empty and no event arrives (FR-004).

Given a `resources/subscribe` subscription
When  `sub_list` runs
Then  no lane row is returned (FR-005).

Given the repaired tree
When  `make tools-export-check` runs
Then  both catalogue files are `unchanged` (FR-007).

Given the repaired tree
When  the gate commands of FR-008 run
Then  each exits 0 with at least 2142 lib tests and 2316 tests with the
integration suites (FR-008).

Given the feature's artifacts on disk
When  `speckit validate` runs over the corpus
Then  the error count stays at 68 with the same rule set and no warning fires (FR-009).

Given the change landed
When  a reader opens `session.md`
Then  both out-of-scope defects carry their `file:line` and reproduction test (FR-010).

Given one URI and the default lane cap of 16
When  17 subscribe/unsubscribe cycles run
Then  every round succeeds (FR-011).

## Accepted Risks

- **AR-1 A host that read a `sub_id` from `sub_list` for a legacy
  subscription loses the row.** Accepted and registered for the owner's review
  (Q-1): the id could not be obtained from the subscribe response, and pausing
  it had no effect on delivery. The exact row that disappears is recorded in
  Q-1. Blocks FR-001 and FR-005.
- **AR-2 The lane adapter stays wired into composition for `sub_open`.** No
  code is removed there; only the two use-case wirings go. Accepted: the
  adapter is still the `sub_*` surface. Blocks nothing.
- **AR-3 `cargo audit` and the Python suites are slower than the Rust gate.**
  Accepted: they are the feature-005 gate list and the change is internal.
  Blocks FR-008.
- **AR-4 A refresh told success can miss notifications for a resource the
  lifecycle has just refused.** The rollback of the failed created call removes
  the shared record, and `Closed` does not stop the producer
  (`src/adapters/lifecycle/grace_timer.rs:148`,
  `src/adapters/lifecycle/refcount.rs:564`); every later subscribe is refused
  `ResourceGone` until the entry is swept (`refcount.rs:371-380`). Accepted:
  the registry matches what later callers are told, and the alternative
  (leave a record for a refused resource) would deliver to a subscription the
  lifecycle says is gone. Blocks FR-004.

## Observability

The feature removes one `info` log line ("resources/subscribe: SubId
synthesised") and no metric, NDJSON event or trace span. `sub_list` and
`sub_stats_all` stop showing a row for a `resources/subscribe` subscription.
`sub_stats` for an unknown id keeps returning `SUB_NOT_FOUND` (unchanged).
`sub_open` lanes render and count exactly as before. Conventions stay those of
`doc/arch/observability/observability.md`.

## Clarifications

### Session 2026-11-22

- **Decision provenance.** Design 2 was chosen by the orchestrator under the
  owner's instruction to proceed and is recorded in ADR 0006 for the owner's
  review. Q-1 below is the second owner-review item; the rest are answered by
  measurement or by code.
- **Q-1 Host dependence on a `sub_list` row — owner review.** No host binary
  exists here, so this is a recorded decision: AR-1 stays accepted and is
  registered for the owner. What a host sees today, measured on the unmodified
  tree in `/var/tmp/ssh-mcp/lane-probe` with a real
  `SubscribeResourceUseCase`, a real `MemoryRegistry` and a real
  `SubscriberLaneAdapter`, then the three admin use cases. The subscription
  appears in `sub_list` as one `SubSummary` (`sub_id` UUIDv7, `kind: Shell`,
  `resource_id: "sh-probe"`, `uri: "shell://sh-probe/output"`,
  `lag_policy: Snapshot`, `lifetime: Manual`, `paused: false`, all eight
  `SubscriberStats` counters `0`); `sub_stats` returns the same eight counters
  plus `inline: Some(InlineLaneCounters { inline_push: false, inline_events_sent: 0, inline_bytes_sent: 0 })`;
  `sub_stats_all` returns `lanes_total: 1` and every total `0`. All of that
  disappears. Log: `/var/tmp/ssh-mcp/logs/lane-probe-q1.log`.
- **Q-2 HTTP reconnect — unchanged, no migration sentence.** The HTTP service
  builds a fresh composition per session through the factory
  (`src/composition/prod.rs:757-770`; the comment at `:811-815` states one
  `McpSshServer` and one `PeerTable` per session), so a reconnect already got
  a new peer id and a new subscription before this feature. Only the
  `sub_list` row disappears, which Q-1 records.
- **Q-3 Catalogue — measured unchanged.** `make tools-export-check` in
  `/var/tmp/ssh-mcp/lane-probe` prints `unchanged: docs/TOOL_CATALOG.md` and
  `unchanged: docs/tool-catalog.json`, exit 0,
  `/var/tmp/ssh-mcp/logs/lane-probe-tools-export.log`. The `resources/subscribe`
  hits in the descriptions/descriptors (`src/infra/mcp/tool_router.rs:1770`,
  `:2253`; `src/infra/mcp/args/shell.rs:254`, `:280`) name no lane.
- **Q-4 Disposition type — plan decision.** The plan must respect: the type
  lives in `src/ports/` next to the port
  (`src/ports/subscriber_registry.rs:157-163`); it is not a `bool`; it
  distinguishes exactly "created" and "refreshed"; the first-subscriber signal
  for the debouncer (`src/adapters/subscription/memory_registry.rs:596`,
  `:827-832`) is a different fact and keeps working.
- **Q-5 Gate totals — exact.** Five existing tests stop existing (four
  removed, one replaced) and six tests are added, so the expected totals are
  2142 lib tests (2141 − 5 + 6) and 2316 with the integration suites
  (2315 − 5 + 6). Removed: `subscribe_outcome_sub_id_is_none_when_lane_not_wired`,
  `subscribe_with_lane_returns_sub_id_on_outcome`,
  `unsubscribe_with_lane_closes_matching_sub_id`,
  `unsubscribe_tolerates_unknown_sub_id_when_lane_wired`; replaced:
  `resources_subscribe_lane_is_listed_but_pause_does_not_gate_the_peer_fanout`.
  Added: `resources_subscribe_records_no_lane_and_delivers_once` (which is
  also the replacement of the pinned test),
  `resources_unsubscribe_removes_the_registry_entry`,
  `resources_unsubscribe_leaves_a_sub_open_lane_open`,
  `repeated_resources_subscribe_counts_once_in_the_lifecycle`,
  `lifecycle_refusal_leaves_no_registry_entry_and_no_delivery`,
  `seventeenth_resources_subscribe_with_interleaved_unsubscribe_succeeds`.
  FR-008 and the scenario say "at least" those totals.
- **Q-6 Tests outside the two modules — measured none depend on the lane.**
  With only the composition wiring removed in `/var/tmp/ssh-mcp/lane-probe`:
  `cargo nextest run --lib` 2141 passed, `--tests --features test-fixtures`
  2315 passed, `cargo build --bins` exit 0, `test_http.py` 15 passed,
  `test_transfer_progress.py` 1 passed / 1 skipped (the suite's own
  fast-machine skip), `test_v7_rsync_http.py` 10 passed,
  `test_v47_subscriptions.py` 20 passed, `test_v47_idempotency.py` 9 passed.
  No assertion changed; the fate tables need no new row. Logs:
  `/var/tmp/ssh-mcp/logs/lane-probe-{lib,tests,build,py-http,py-transfer,py-rsync,py-subs,py-idem}.log`.
- **Q-7 Entry/lifecycle window — pre-existing, unchanged, not repaired.**
  `execute` registers the entry (`src/application/subscribe_resource.rs:159-161`)
  and only then calls `on_subscribe` (`:164`). A concurrent unsubscribe of the
  same pair removes the entry (`src/application/unsubscribe_resource.rs:114`),
  sees `was_subscribed == true`, and calls `on_unsubscribe` (`:122-123`),
  whose `fetch_sub` returns the pre-increment `0`; the adapter re-adds 1 and
  returns `DomainError::Internal` (`src/adapters/lifecycle/refcount.rs:546-555`),
  which the unsubscribe surfaces as an internal error. Both calls finish with
  the registry entry gone and the lifecycle count at 1 — a phantom count. The
  order is identical on the base tree (the lane open at `:165-167` comes
  after), so the feature neither widens nor narrows it. Recorded in Out of
  Scope and in `session.md`.
- **Q-8 Rollback against a concurrent refresh — specified in FR-004.** Call A
  creates the entry; call B of the same peer and URI refreshes it (the
  `retain`/`push` at `src/adapters/subscription/memory_registry.rs:592-596`
  keeps one record and returns `first == true` for the sole peer) and is told
  success; A's `on_subscribe` then fails with `ResourceGone` and A's rollback
  removes the record B was told it has. B's success is not re-validated. The
  rule is justified by consistency: every later subscribe of the refused
  resource fails the same way while the retained `Closed` entry lives
  (`src/adapters/lifecycle/refcount.rs:371-380`, `:419-424`). `Closed` does
  not stop the producer (`src/adapters/lifecycle/grace_timer.rs:148`,
  `refcount.rs:564`), so B can miss notifications; that is AR-4. FR-004
  states the rule.
- **Q-9 Embedded daemon — no change.** `findReferences` on
  `SubscribeResourceOutcome.sub_id` returns only
  `src/application/subscribe_resource.rs` and
  `src/infra/mcp/resource_handlers.rs:825`; the dispatcher never names the
  type. Its ack hard-codes `sub_id: None` (`src/embed/dispatcher.rs:592`), and
  the `unsubscribe` op sends its `sub_id` string as the URI
  (`:625-627`), which is documented at `docs/DAEMON.md:129-160`. No NDJSON
  field or op changes.
- **Q-10 Registry `SubId` and cursors — untouched.** The registry keeps
  minting a `SubId` (`src/adapters/subscription/memory_registry.rs:825`) and
  populating `(sub_id, uri)` cursors (`:599-608`). `sub_id_for` (`:300-313`),
  `sub_byte_cursor` (`:281-288`), `advance_sub_byte_cursor` (`:290-298`) and
  `sub_progress` (`:265-279`) have no production caller outside the module and
  are dead in production after this feature; the cursors are still maintained
  and cleared. Recorded in Out of Scope with its place, not changed.
