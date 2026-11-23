---
id: 01a202fb-f2b6-7f03-a9c7-3c6d3087db1b
number: 007
slug: sub-open-resource-check
status: implemented
created_at: 2026-11-22T11:19:55.830210311Z
---
# Feature Specification: Sub Open Resource Check

Feature: 007-sub-open-resource-check
Created: 2026-11-22

`sub_open` opens a lane on any parseable URI. It never asks a repository
whether the resource exists: `SubscribeUseCase` holds only the lane, the
activator and the capability registry (`src/application/subscription_admin.rs:81-85`),
its `execute` parses, opens the lane and starts the debouncer
(`:135-162`), and no other step probes a repository. `resources/subscribe`
does probe, through `ensure_exists`
(`src/application/subscribe_resource.rs:156-190`), which refuses a missing
Shell, Command, Transfer or Session and deliberately lets Forward, Serial and
Rsync through. After this feature `sub_open` shares that one policy. A refused
call is told the same domain error `resources/subscribe` returns for the kind,
and it opens no lane, takes no per-URI or global cap slot, leaves no stats row
and starts no debouncer.

Every fact in this specification was measured on 2026-11-22 on the tree of
feature 006, in the throwaway copy `/var/tmp/ssh-mcp/sub-probe` made from
`/root/dev/ssh-mcp`. Every `file:line` is the repository line, printed in the
same step that wrote it here. The reproduction tests are **not in the tree**;
they are kept in `/var/tmp/ssh-mcp/sub-repro.patch` on the session host with
its run log `/var/tmp/ssh-mcp/sub-repro-run.log`. No production code changes
in this phase.

## Why

- A typo in an id is accepted today and yields a live lane that never produces,
  while the caller believes it observes a resource. The lane consumes a
  per-URI and a global slot, a `sub_list` / `sub_stats` row and one debouncer
  task, and the ninth such lane on one URI is refused with a cap error that
  names the wrong cause
  (`crate::application::subscription_admin::tests::unknown_resource_lane_still_consumes_a_cap_slot_and_a_stats_row`,
  not in the tree; it pins eight accepted lanes and the ninth
  `MaxSubsPerUriExceeded` on `shell://ghost/output`).
- `resources/subscribe` already refuses the four repository-backed kinds
  (`src/application/subscribe_resource.rs:170-174`). Two subscribe surfaces
  that disagree about a missing id are a trap for a host that switches from
  one to the other.
- The `sub_open` description lists `RESOURCE_NOT_FOUND`
  (`src/infra/mcp/tool_router.rs:2389`; catalogue copy at
  `docs/TOOL_CATALOG.md:1475` and `docs/tool-catalog.json:1501`), and no path
  emits it. This feature makes the answer true to a code the rest of the
  server already uses and records the remaining description mismatch for the
  owner.

## Decision provenance

The answer code is a decision taken by the orchestrator under the owner's
instruction to proceed, and is recorded in
[ADR 0007](../../adr/0007-sub-open-resource-check.md) as a decision registered
for the owner's review, not as the owner's own choice. The chosen answer is the
per-kind code the server already produces for the same domain error
(`SHELL_NOT_FOUND`, `COMMAND_NOT_FOUND`, `TRANSFER_NOT_FOUND`,
`SESSION_NOT_FOUND`), not a new `RESOURCE_NOT_FOUND`. The rejected alternative
is to add `RESOURCE_NOT_FOUND` and map the four variants onto it, which would
match the frozen description but collide with the four codes every other tool
already returns for the same variants.

## Behavior after the feature

The check runs after the URI parses and before `LaneAdmin::open`. For each
kind:

| Kind | Check | Refusal variant | Answer code | Reason line | Detail line |
| --- | --- | --- | --- | --- | --- |
| `shell://<id>/output` | `ShellRepository::get` | `DomainError::ShellNotFound` | `SHELL_NOT_FOUND` | `no active shell with the given ID` | `Recreate via ssh_shell_open; subscribe to shell://<id>/output to track lifecycle.; <id>` |
| `command://<id>/output` | `CommandRepository::get` | `DomainError::CommandNotFound` | `COMMAND_NOT_FOUND` | `no async command with the given ID` | `Use ssh_commands; subscribe to command://<id>/output for completion events.; <id>` |
| `transfer://<id>/progress` | `TransferRepository::get` | `DomainError::TransferNotFound` | `TRANSFER_NOT_FOUND` | `no transfer with the given ID` | `Re-issue ssh_upload or ssh_download to obtain a fresh transfer_id.; <id>` |
| `session://<id>/health` | `SessionRepository::get` | `DomainError::SessionNotFound` | `SESSION_NOT_FOUND` | `no active SSH session with the given ID` | `Use ssh_sessions; recreate via ssh_connect if the id is stale.; <id>` |
| `forward://<id>/events` | none | — | — | — | — |
| `serial://<id>/output` | none | — | — | — | — |
| `rsync://<id>/progress` | none | — | — | — | — |

Sources: variants at `src/domain/error.rs:17-31`; codes and reasons at
`src/infra/mcp/tool_router.rs:788-807`; the static cures at
`src/infra/mcp/error_detail.rs:83-92`, merged with the id by `with_detail`
(`:291-299`); the body by `format_error` (`src/infra/mcp/helpers/error.rs:31-53`)
and the structured twin by `format_error_structured` (`:74-99`). `sub_open`
renders through `render_tool_error` (`src/infra/mcp/tool_router.rs:167-173`)
from its error arm (`:1320`); it does not use the closest-match renderer
(`:192-202`), so no suggestion list is added.

The three permissive kinds stay permissive for the reasons the shared policy
already carries: a forward subscriber may attach before the producer publishes
(`src/application/subscribe_resource.rs:175-177`), and serial and rsync state
live outside the application repositories, so the infra layer validates the id
before any subscribe call reaches the use case (`:178-187`).

## Single policy, one table

The kind table above is implemented once and called by both use cases. The
constraints the plan must respect:

- `resources/subscribe` keeps its behavior for the four checked kinds and the
  three permissive ones; its five `ensure_*`/`ensure_exists` tests stay green.
- No adapter imports `crate::application`: the shared policy may live in
  `src/application/` (called by both use cases) or behind a `src/ports/` trait
  implemented by a composition-built adapter; it may not be a call from an
  adapter into an application module.
- `SubscribeUseCase` is not generic today (`src/application/subscription_admin.rs:81-85`);
  `SubscribeResourceUseCase` is generic over four repositories
  (`src/application/subscribe_resource.rs:66-80`). A design that makes the
  policy a trait object keeps `UseCases` and every `SubscribeUseCase::new`
  call site stable; a design that adds four generics to `SubscribeUseCase`
  must update the 26 `new` references below.
- The table of "which kinds are checked, which pass" appears in exactly one
  place in `src/`. The prose table above may be duplicated in documents.

## Symbols whose signature may change

`findReferences` was run on each symbol before writing this list. The policy
moves out of `subscribe_resource.rs`; the public constructors are the risk
surface.

### `ensure_exists`

`src/application/subscribe_resource.rs:156`. `findReferences`: `:124` (call)
and `:156` (declaration). Its four helpers are `:192`, `:203`, `:214`, `:225`
and move with it.

### `SubscribeUseCase::new`

`src/application/subscription_admin.rs:93`. `findReferences` (26):

- `src/application/subscription_admin.rs`: `:702`, `:723`, `:740`, `:757`,
  `:1092`, `:1109`, `:1126`, `:1143`, `:1160`, `:1177`, `:1194`, `:1211`,
  `:1231`, `:1272`, `:1533`, `:1552`, `:1571` (unit tests, all with a bare
  lane adapter and no repository).
- `tests/chaos_inline_push.rs`: `:221`, `:406`, `:447`.
- `tests/v5_smoke.rs`: `:88`, `:577`, `:688`, `:786`, `:910`.

### `SubscribeUseCase::execute`

`src/application/subscription_admin.rs:135`. `findReferences` (31): the
production call `src/infra/mcp/tool_router.rs:1313`; unit-test calls `:704`,
`:725`, `:742`, `:759`, `:1094`, `:1111`, `:1128`, `:1145`, `:1162`, `:1179`,
`:1196`, `:1213`, `:1233`, `:1244`, `:1255`, `:1274`, `:1285`, `:1536`,
`:1555`, `:1573`; integration calls `tests/chaos_inline_push.rs:250`, `:411`,
`:463`, `:465` and `tests/v5_smoke.rs:90`, `:583`, `:690`, `:788`, `:912`.

### `SubscribeUseCase::with_activator`

`src/application/subscription_admin.rs:106`. `findReferences`: `:107` and
`src/composition/prod.rs:545`. Production wiring; the guard is installed on the
same object.

### `SubscribeResourceUseCase` and `execute`

The type is referenced at `src/composition/prod.rs:87`, `:517`,
`src/composition/mod.rs:53`, `:169`, `src/infra/mcp/resource_handlers.rs:39`,
`:794` and in its own tests. Keeping `ensure_exists` behavior identical means
no signature change is required; only its module may move.

## Test fates

### Existing tests whose subject changes

| Existing test | File:line | Asserts today | Fate |
| --- | --- | --- | --- |
| `subscribe_to_unknown_shell_returns_shell_not_found` | `src/application/subscribe_resource.rs:478` | a missing shell is `ShellNotFound` | kept — proves the shared policy still returns the per-kind variant |
| `subscribe_to_unknown_command_returns_command_not_found` | `src/application/subscribe_resource.rs:506` | a missing command is `CommandNotFound` | kept |
| `subscribe_to_forward_uri_always_succeeds` | `src/application/subscribe_resource.rs:545` | a forward URI is permissive | kept — proves the permissive arm |
| `subscribe_to_known_shell_succeeds_and_records_call`, `subscribe_to_known_command_succeeds`, `subscribe_to_known_transfer_succeeds`, `subscribe_to_known_session_succeeds` | `:461`, `:493`, `:519`, `:532` | seeded kinds pass | kept |
| `malformed_uri_returns_invalid_argument`, `cursor_query_in_uri_is_stripped_from_canonical` | `:558`, `:571` | parse behavior unchanged | kept |
| `resources_subscribe_records_no_lane_and_delivers_once`, `repeated_resources_subscribe_counts_once_in_the_lifecycle`, `lifecycle_refusal_leaves_no_registry_entry_and_no_delivery`, `seventeenth_resources_subscribe_with_interleaved_unsubscribe_succeeds` | `:696`, `:732`, `:775`, `:809` | feature-006 behavior | kept — the guard runs before the registry and does not change a seeded call |
| `subscription_admin` unit tests of `sub_open` | `src/application/subscription_admin.rs:702-1573` | lane behavior on synthetic ids | kept — the guard is installed only by the production composition, so `new` stays permissive (Q-1) |
| `tests/v5_smoke.rs` sub_open tests | `:90`, `:583`, `:690`, `:788`, `:912` | inline-push and split behavior on synthetic ids | kept — it builds `SubscribeUseCase::new` directly (Q-1) |
| `tests/chaos_inline_push.rs` | `:250`, `:411`, `:463`, `:465` | inline-push concurrency on synthetic ids | kept — it builds `SubscribeUseCase::new` directly (Q-1) |
| `scripts/test_v5_adversarial.py::test_subscribe_invalid_uri_scheme` | `:640` | `badscheme://x/y` is `INVALID_ARGUMENT` | kept — parse fails before the guard |
| `...::test_subscribe_invalid_lifetime_string`, `...::test_subscribe_invalid_lag_policy` | `:646`, `:654` | schema rejection | kept — deserialization fails before the use case |
| `...::test_subscribe_invalid_filter_regex` | `:660` | `filter="["` is `INVALID_ARGUMENT` on `shell://x/output` | adapted — probe fails with `SHELL_NOT_FOUND`; use `forward://x/events` (Q-1) |
| `...::test_max_subs_per_uri_exceeded_with_low_cap` | `:719` | the third `sub_open` on `shell://overflow-test/output` is a cap error | adapted — probe fails at the first call with `SHELL_NOT_FOUND`; use `forward://overflow-test/events` (Q-1) |
| `...::test_subscribe_unsubscribe_concurrent_burst` | `:745` | 50 pairs on `shell://burst-<i>/output` | adapted — probe fails with `SHELL_NOT_FOUND`; use `forward://burst-<i>/events` (Q-1) |
| `...::test_subscribe_returns_required_hint_line` | `:781` | the HINT line on `shell://hint-test/output` | adapted — probe fails with `SHELL_NOT_FOUND`; use `forward://hint-test/events` (Q-1) |
| `...::test_sub_list_during_burst_churn` | `:963` | ten rows on `shell://churn-<i>/output` | adapted — probe fails with `SHELL_NOT_FOUND`; use `forward://churn-<i>/events` (Q-1) |
| `scripts/test_v51_flush_bytes.py::test_subscribe_hint_mentions_flush_bytes_knob` | `:64` | HINT knobs on `command://placeholder/output` | adapted — probe fails with `COMMAND_NOT_FOUND`; use `forward://placeholder/events` (Q-1) |
| `scripts/chaos_v5_subscribe.py` shell-URI tests | `:139`, `:187`, `:424`, `:467` | burst, pause, grace, signal storm on `shell://...` | already failing on the base tree at `:118` (`SSH_SUB_LIST` prefix) and `:285` (`SSH_SUB_STATS`); the guard adds no new failure (Q-1) |
| `scripts/chaos_v5_subscribe.py` command-URI tests | `:233`, `:298`, `:365` | exec then `command://<cid>/output` | kept — the command exists |
| `scripts/test_resources.py` | whole file | `resources/subscribe` | kept |

### Tests of `/var/tmp/ssh-mcp/sub-repro.patch`

The measurement patch is **not in the tree**; it lives on the session host at
`/var/tmp/ssh-mcp/sub-repro.patch` with its run log
`/var/tmp/ssh-mcp/sub-repro-run.log`. Every test it holds has one fate:

| Patch test | File in the patch | Fate |
| --- | --- | --- |
| `subscribe_unknown_resource_returns_resource_not_found` | `src/application/subscription_admin.rs` | becomes the proving test of FR-001; renamed `subscribe_unknown_shell_is_refused` because the code is `SHELL_NOT_FOUND` |
| `unknown_resource_lane_still_consumes_a_cap_slot_and_a_stats_row` | `src/application/subscription_admin.rs` | replaced by `refused_sub_open_leaves_no_lane_and_no_debouncer` (one refusal, empty `list_subs`, no stats row, no activator call) |
| `unknown_resource_lane_receives_delivery_once_the_resource_appears` | `src/application/subscription_admin.rs` | replaced by `refused_then_created_resource_can_be_subscribed` (refusal, create, open, one delivery) |
| `closing_the_shell_leaves_its_lane_open` | `src/application/subscription_admin.rs` | not added — out-of-scope F7 |
| `completed_command_leaves_its_lane_open` | `src/application/subscription_admin.rs` | not added — out-of-scope F7 |
| `session_disconnect_closes_command_shell_and_transfer_lanes_but_not_health` | `src/application/subscription_admin.rs` | not added — out-of-scope F7 |
| `closed_peer_leaves_its_lane_open` | `src/application/subscription_admin.rs` | not added — out-of-scope F7 |
| `open_and_end_cycles_leak_one_lane_each_until_the_cap_refuses` | `src/application/subscription_admin.rs` | not added — out-of-scope F7 |
| `open_and_end_cycles_exhaust_the_global_cap` | `src/application/subscription_admin.rs` | not added — out-of-scope F7 |
| `auto_close_lane_is_reaped_after_grace_window` | `src/application/subscription_admin.rs` | not added — out-of-scope F2 |
| `lease_lane_outlives_its_ttl` | `src/application/subscription_admin.rs` | not added — out-of-scope F2 |
| `oversized_filter_is_accepted_by_sub_open` | `src/application/subscription_admin.rs` | not added — out-of-scope F4; its URI changes to `forward://big-filter/events` so it still opens |
| `replay_from_cursor_enqueues_a_snapshot_for_the_consumer` | `src/adapters/subscription/subscriber_lane.rs` | not added — out-of-scope F3; adapter-level, no service |
| `sub_replay_answers_ok_into_a_lane_queue_nothing_drains` | `src/composition/prod.rs` | not added — out-of-scope F3; its URI changes to `forward://replay-probe/events` |
| `lifetime_accepts_only_the_snake_case_spelling`, `lag_policy_rejects_non_snake_case_and_unknown_values` | `src/infra/mcp/args/subscription.rs` | not added — out-of-scope F5 |
| `subscribe_op_parses_every_documented_field`, `subscribe_lifetime_is_free_form_but_lag_policy_is_typed`, `subscribe_op_maps_to_resources_subscribe_not_sub_open` | `src/embed/parser.rs` | not added — out-of-scope F6 |
| `oversized_regex_compiles_even_past_the_configured_cap`, `nested_group_bomb_compiles_under_the_timeout` | `src/adapters/subscription/filter.rs` | not added — out-of-scope F4 |

## User Stories

- As a host implementer, I want a `sub_open` typo to fail at once, so that a
  dead lane never enters my inventory.
- As a host implementer, I want the same answer from `sub_open` and
  `resources/subscribe` for a missing id, so that switching surfaces does not
  change the error I handle.
- As a host implementer, I want a refused call to cost nothing, so that a
  retry loop cannot exhaust the lane caps.
- As an operator, I want `sub_list` to show only lanes bound to a resource
  that exists, so that the inventory stays actionable.
- As a maintainer, I want the kind policy written once, so that a later kind
  cannot drift between the two subscribe surfaces.

## Out of Scope

Each item is a recorded defect, with the place it is recorded. None is
repaired here.

- **Lane lifetime (F2).** `auto_close` and `lease` are stored and never
  enforced (`src/adapters/subscription/subscriber_lane.rs:156`; the only read
  is `summary()` at `:298`). Recorded in `session.md` and in
  `doc/arch/functional/functional.md:237-240`.
- **`sub_replay` and the never-started mux drain (F3).**
  `ChannelMuxAdapter::spawn_drain` (`src/adapters/subscription/channel_mux.rs:120-145`)
  has no production caller, `src/composition/prod.rs:344` binds the outbound
  receiver to `_mux_outbound_rx`, and the `Snapshot` stays in the lane queue
  (`src/adapters/subscription/subscriber_lane.rs:860-887`). Recorded in
  `session.md` and `doc/arch/functional/functional.md:241-246`.
- **Filter length cap (F4).** `resolve_filter_regex_max`
  (`src/adapters/config/internal/mod.rs:904-912`) has no production caller;
  `FilterPipeline::compile_regex` (`src/adapters/subscription/filter.rs:56-59`)
  checks only syntax. Recorded in `session.md` and
  `doc/arch/functional/functional.md` (F4 rows).
- **Dead error codes and the `auto-close` spelling (F5).**
  `INVALID_LIFETIME` and `INVALID_LAG_POLICY` (`src/domain/error.rs:205-214`)
  are constructed by no production path; `docs/LLM_GUIDE.md:1667-1685`
  documents them. Recorded in `session.md`.
- **Daemon `subscribe` and `unsubscribe` ops (F6).** The dispatcher drops
  every parameter but the URI (`src/embed/dispatcher.rs:552`, `:570-603`) and
  sends the `sub_id` string as a URI on unsubscribe (`:625-627`). Recorded in
  `session.md`.
- **Lanes and debouncers left behind when a resource or a peer ends (F7).**
  No terminal path closes a lane; the session disconnect cascade covers only
  command, shell and transfer (`src/application/disconnect_session.rs:243`,
  `:259`, `:280`) and `session://<id>/health` escapes it. Recorded in
  `session.md` with the measured numbers.
- **The window between the check and the lane open (part of F7).** A resource
  removed after the guard and before `LaneAdmin::open` still yields a lane
  (`src/application/subscription_admin.rs:144-147`). The feature narrows the
  window; it does not close it.
- **A lifecycle-`Closed` resource with a live repository row is accepted.**
  `sub_open` never calls `LifecyclePolicyPort::on_subscribe`
  (`src/application/subscription_admin.rs:135-162`), so it cannot observe the
  lifecycle state and does not return `RESOURCE_GONE` today or after this
  feature; only the repository row decides. Recorded in `session.md`
  ("`sub_open` never calls `LifecyclePolicyPort::on_subscribe`") and told
  wrongly by `docs/OPERATIONS.md:176-213` and `docs/LLM_GUIDE.md:1469-1472`,
  which FR-006 corrects in prose without wiring the lifecycle call.
- **A typo in a permissive kind still opens a dead lane.** Forward and rsync
  ids live in `DashMapForwardRepo` (`src/adapters/repo/dashmap/forward.rs:57`)
  and `DashMapRsyncRepo` (`src/adapters/repo/dashmap/rsync.rs:28`), both behind
  ports (`src/ports/forward_repo.rs:18-19`, `src/ports/rsync_repo.rs:25-26`),
  so the application layer can ask; serial lives in the process-wide
  `SERIAL_REGISTRY` (`src/adapters/serial/state.rs:325`) with no port. The
  permissive arm is therefore a choice for forward and rsync and a necessity
  for serial; the policy of `resources/subscribe` stays as specified. Recorded
  in `session.md`.
- **`scripts/chaos_v5_subscribe.py` is broken on the unchanged tree.** It
  asserts `SSH_SUB_LIST: OK` (`:118`) and `SSH_SUB_STATS: OK` (`:285`) against
  the current `SUB_LIST: OK` / `SUB_STATS: OK`; `7 failed`. Recorded in
  `session.md`; not repaired here, and not a gate command.
- **The frozen descriptions (F8).** `make tools-export-check` must stay
  `unchanged`; the `RESOURCE_NOT_FOUND` line stays false and joins the open
  item `session.md` already keeps about that description.
- **Every other recorded defect** and the conformity work on the baseline
  errors.

## Functional Requirements

### FR-001 A missing repository-backed resource is refused with the per-kind variant

`SubscribeUseCase::execute` calls the shared policy before opening a lane and
returns `DomainError::ShellNotFound`, `CommandNotFound`, `TransferNotFound` or
`SessionNotFound` for a missing id of the matching kind. The variants are
declared at `src/domain/error.rs:17-31` and are the ones
`SubscribeResourceUseCase` already returns (`src/application/subscribe_resource.rs:192-233`).
The id in the variant is the parsed id, not the URI.

**Acceptance.** Four `#[tokio::test]`s in
`crate::application::subscription_admin::tests` build the guarded use case over
real `DashMap` repositories: `subscribe_unknown_shell_is_refused` (asserts
`ShellNotFound`), `subscribe_unknown_command_is_refused`
(`CommandNotFound`), `subscribe_unknown_transfer_is_refused`
(`TransferNotFound`) and `subscribe_unknown_session_is_refused`
(`SessionNotFound`). Each asserts the variant and the carried id. They fail on
the base tree with `Ok(SubscribeOutcome { … })`
(`subscribe_unknown_resource_returns_resource_not_found`, not in the tree;
`/var/tmp/ssh-mcp/sub-repro.patch`).

### FR-002 A refused call opens no lane and starts no debouncer

The policy runs before `LaneAdmin::open` (`src/application/subscription_admin.rs:144-147`).
After a refusal `LaneAdmin::list` (`src/ports/subscriber_lane.rs`) is empty,
`LaneAdmin::stats` for the id is `None`, and the `DebouncerActivator`
(`src/ports/notifier.rs:86-90`) recorded no `ensure_for_uri` call. No per-URI
or global reservation is taken (`src/adapters/subscription/subscriber_lane.rs:811`,
`:509-540`) and no debouncer task is spawned
(`src/adapters/subscription/memory_registry.rs:371-410`).

**Acceptance.**
`crate::application::subscription_admin::tests::refused_sub_open_leaves_no_lane_and_no_debouncer`
uses a real `SubscriberLaneAdapter` and a `DebouncerActivator` stub that
records URIs; it asserts the error, `list_subs().is_empty()`, no recorded URI
and that the per-URI cap is still fully available (the same URI opens after
the resource is seeded). It is a `#[tokio::test]`. Replaces
`unknown_resource_lane_still_consumes_a_cap_slot_and_a_stats_row` (not in the
tree; `/var/tmp/ssh-mcp/sub-repro.patch`).

### FR-003 Forward, serial and rsync stay permissive

`ResourceKind::Forward`, `Serial` and `Rsync` pass without a repository probe,
for the reasons at `src/application/subscribe_resource.rs:175-188`. A guarded
`sub_open` on `forward://f/events`, `serial://s/output` and
`rsync://r/progress` returns `SUB_OPEN: OK` with a fresh `SubId` even when no
producer ever wrote the id.

**Acceptance.**
`crate::application::subscription_admin::tests::forward_serial_and_rsync_stay_permissive`
runs the guarded use case for the three schemes and asserts one lane each. It
is a `#[tokio::test]`. `subscribe_to_forward_uri_always_succeeds`
(`src/application/subscribe_resource.rs:545`) stays green for the shared
policy.

### FR-004 The kind policy exists once and both use cases call it

The table "Shell, Command, Transfer, Session checked; Forward, Serial, Rsync
pass" is implemented in one `src/` location. `SubscribeResourceUseCase` keeps
its five existing tests green. No adapter imports `crate::application`; the
policy is either an application module called by both use cases or a
`src/ports/` trait implemented by a composition-built adapter. A design that
makes `SubscribeUseCase` generic must adapt the 26 `new` call sites listed
above; a design that keeps `new` permissive must install the guard on the
production object in `src/composition/prod.rs:544-547`.

**Acceptance.** `grep -rn 'ResourceKind::Forward | ResourceKind::Serial' src`
returns one match, and so does each per-kind probe; the five
`subscribe_resource` `ensure_*`/`ensure_exists` tests and the four FR-001
tests pass. It is proven by the test set, not by a new test.

### FR-005 The tool answer carries the per-kind code

A refused `sub_open` returns a successful `CallToolResult` with
`is_error: Some(true)` (`src/infra/mcp/helpers/structured.rs:105-111`) whose
first line is `SUB_OPEN: ERROR` and whose `REASON: [<CODE>] <reason>` and
`DETAIL: <cure>; <id>` lines are the table above. The structured twin carries
`tool: "sub_open"`, `status: "error"`, the same `code`, `reason` and `detail`.
The four codes are not new: they are the mapping at
`src/infra/mcp/tool_router.rs:788-807` and the cures at
`src/infra/mcp/error_detail.rs:83-92`.

The frozen `sub_open` description lists `RESOURCE_NOT_FOUND`
(`src/infra/mcp/tool_router.rs:2389`; `docs/TOOL_CATALOG.md:1475`;
`docs/tool-catalog.json:1501`), which no path emits. The description is not
edited; the mismatch joins the item `session.md` already keeps about it.
Choosing the per-kind code instead of a new `RESOURCE_NOT_FOUND` is recorded in
ADR 0007 as a decision registered for the owner's review, with the alternative.

**Acceptance.**
`crate::infra::mcp::tool_router::tests::sub_open_refusal_answers_per_kind_code`
is the proving test. It builds the guarded use case over real `DashMap`
repositories, calls the same helper `sub_open` calls,
`run_sub_subscribe` (`src/infra/mcp/tool_router.rs:1301-1322`), for the four
missing ids, and asserts the returned `CallToolResult` is an error, that its
first line is `SUB_OPEN: ERROR`, that `REASON: [<CODE>]` and `DETAIL: <cure>;
<id>` are the table rows, and that the structured twin carries the same
`code`. It is a `#[tokio::test]` and fails on the base tree with
`Ok(CallToolResult { … })`.

The plain `#[test]`
`crate::infra::mcp::tool_router::tests::sub_open_refusal_renders_per_kind_code`
is kept as a mapping pin only: it renders the four `DomainError` variants
through `render_tool_error` and **passes on the base tree**, so it proves the
code strings, not that `sub_open` produces them. The proof that the use case
produces the variant is FR-001; the proof that the `sub_open` path renders it
is this FR's `run_sub_subscribe` test. `make tools-export-check` reports
`unchanged` for both catalogue files.

### FR-006 The documents that are false state the truth

| Document | Line | Today | Required |
| --- | --- | --- | --- |
| `docs/API.md` | 1106 | "**Errors**: `RESOURCE_GONE`, `MAX_SUBS_PER_URI_EXCEEDED`, `MAX_SUBS_TOTAL_EXCEEDED`, `INVALID_ARGUMENT`." | add `SHELL_NOT_FOUND`, `COMMAND_NOT_FOUND`, `TRANSFER_NOT_FOUND`, `SESSION_NOT_FOUND`; keep `RESOURCE_GONE` (still not emitted by this path) |
| `docs/API.md` | 1084-1087 | the URI row and the lifetime table | state that a missing resource is refused for the four repository kinds and that forward, serial and rsync attach before the producer |
| `docs/OPERATIONS.md` | 53, 176-213 | section "`sub_open` returns `RESOURCE_GONE`" | a missing id returns the per-kind `*_NOT_FOUND`; `sub_open` never returns `RESOURCE_GONE` because it never calls `LifecyclePolicyPort::on_subscribe`; a resource whose lifecycle is `Closed` while its repository row still exists is still accepted. `RESOURCE_GONE` is the lifecycle refusal of `resources/subscribe` (`src/application/subscribe_resource.rs:164`), and the section title stays as the operational shape of that path |
| `docs/LLM_GUIDE.md` | 921, 1429, 1439, 1449, 1469-1472 | `sub_open` on reconnect is listed with `RESOURCE_GONE` (`:921`); the four `*_NOT_FOUND` sections speak only of `ssh_shell_*`, `ssh_exec_*` and `ssh_transfer_progress` (`:1429`, `:1439`, `:1449`); `RESOURCE_GONE` says a subscribe to a `Closed` resource is refused (`:1469-1472`) | a missing id from `sub_open` is the per-kind code; `sub_open` does not return `RESOURCE_GONE`; the `*_NOT_FOUND` "When" lines also name `sub_open`; the `RESOURCE_GONE` text keeps `resources/subscribe` as its subject |
| `docs/RESOURCES.md` | 24-29, 98 | per-scheme subscribe notes | the four repository kinds need a live id; forward, serial and rsync do not |
| `CHANGELOG.md` | `[Unreleased]` → `### Changed` | — | one bullet: `sub_open` refuses a missing repository-backed resource with the per-kind code; a refused call opens no lane |
| `docs/MIGRATION.md` | `## v7.1.3 → unreleased (dependency security update)` (heading at 880), new bullet beside the feature-006 bullet at 882 | — | one bullet under the current section: a missing id is refused with the per-kind code and a refused call opens no lane; the wire is unchanged |
| `doc/arch/functional/functional.md` | 247-251 | "`sub_open` accepts a URI whose resource does not exist … `RESOURCE_NOT_FOUND` … is not raised" | repaired: the four repository kinds are checked, the per-kind code is raised, three kinds stay permissive |
| `doc/arch/functional/functional.md` | F8 table (after 341) | no row | one row "a `sub_open` of a missing repository-backed resource is refused with the per-kind code and opens no lane" bound to the FR-001/FR-002 tests |
| `doc/arch/runbooks/incident-push-delivery-lost.md` | 11, 101-111 | "the call returned an id" | a refused call returns no id; triage starts with the `*_NOT_FOUND` code |

`docs/MIGRATION.md:625`, `:641` and `:826` and
`docs/adr/0004-channel-mux-fairness.md:136-137` are history and stay as
written. `docs/MIGRATION.md:826` sits under `## v4 → v5` (heading at `:567`),
so the new behavior is recorded under the current section at `:880` instead.

Section check for the other rows: `docs/API.md` is the current tool reference
(`### sub_open` at `:1078`); `docs/OPERATIONS.md` is the current runbook
(`## Common failure shapes` at `:67`); `docs/LLM_GUIDE.md` is the current
guide (`### v6.1 / ADR 0010` at `:925` for the reconnect recipe and `##
Error handbook` at `:1225` for the code sections); `docs/RESOURCES.md` is the
current scheme reference; `CHANGELOG.md` has `## [Unreleased]` at `:8`;
`doc/arch/functional/functional.md` (`### F8 — Subscribe and push` at `:197`)
and `doc/arch/runbooks/incident-push-delivery-lost.md` are the current corpus.
None of them is a release-history section except the MIGRATION line above.

The generated catalogue files stay unchanged (FR-008).

**Acceptance.** The ten edits read as required; every cited line was read from
the file in the same step. The scenario stays `@planned`.

### FR-007 `session.md` records the out-of-scope defects and the stale item is corrected

`session.md` gains, in "Defects verified in the code during the session", one
entry per defect of the measurement that the list does not have yet, each with
its `file:line` and, where a reproduction exists, the test name, the statement
that the test is **not in the tree**, its path `/var/tmp/ssh-mcp/sub-repro.patch`,
and what it pins or its failure message. At least:

- the debouncer that `sub_open` starts and nothing stops
  (`src/application/subscription_admin.rs:148-150`,
  `src/adapters/subscription/memory_registry.rs:651-657`, `:371-410`, `:459-468`);
- lanes that outlive the shell, the command and the peer, with the measured
  numbers against `SSH_MAX_SUBS_PER_URI` 16 and `SSH_MAX_SUBS_TOTAL` 1024
  (`src/adapters/config/internal/mod.rs:200`, `:205`) and the four tests;
- the `session://<id>/health` lane the disconnect cascade does not close
  (`src/application/disconnect_session.rs:175-188`, `:243`, `:259`, `:280`)
  and its test;
- `sub_open` never calling `LifecyclePolicyPort::on_subscribe`
  (`src/application/subscription_admin.rs:135-162`) although
  `docs/ARCHITECTURE.md:422` and
  `docs/adr/0004-channel-mux-fairness.md:153` say it does;
- the daemon `unsubscribe` op that sends the `sub_id` as a URI
  (`src/embed/dispatcher.rs:625-627`,
  `src/infra/mcp/resource_handlers.rs:840-843`);
- `docs/DAEMON.md:131` promising a `sub_id` in the `subscribe` ack while the
  dispatcher hard-codes `sub_id: None` (`src/embed/dispatcher.rs:587-595`);
- `docs/API.md:1168` promising a `BYTES` line in the `sub_replay` answer while
  the renderer prints `FROM_CURSOR` (`src/infra/mcp/render/subscription.rs:331-339`);
- `docs/CONFIGURATION.md:153` naming a `FILTER_INVALID` code while the compile
  error is `INVALID_ARGUMENT` (`src/adapters/subscription/filter.rs:56-59`).

Existing entries are extended, not duplicated: the lifetime entry gains the
`lease` variant and the `sub_list`-only reader; the replay entry gains the
never-started drain (`src/adapters/subscription/channel_mux.rs:120`), the
dropped receiver (`src/composition/prod.rs:344`), the queue-fill consequence
and the two tests. The `sub_open` existence entry stays an **open defect**
during this phase and is marked repaired only when the `implement` phase lands
the code. Item 6 of "Update plan" is corrected now (the sentence is stale
regardless of the feature): the trace layer is installed at the line
`grep -n 'TraceLayer::new_for_http' src/composition/prod.rs` prints on the
final tree, `:804` after the eight lines this feature inserts (`:796` before
them), with the import at `:27`, while the corpus cites `:786` at
`doc/arch/threat-model/threat-model.md:58`, `:104` and
`doc/arch/observability/observability.md:181`.

**Acceptance.** Each entry exists with its `file:line`; every patch test is
marked not in the tree; item 6 names the line
`grep -n 'TraceLayer::new_for_http' src/composition/prod.rs` prints, the
import at `:27`, and the three corpus citations at `:58`, `:104` and `:181`.
The scenario stays `@planned`.

### FR-008 No other wire change

No argument, descriptor, notification method, status value or environment
variable changes. The only new observable is the refusal of a call that
returned `OK`. `make tools-export-check` reports `unchanged` for both
catalogue files, and the two catalogue files are byte-identical.

**Acceptance.** `make tools-export-check` exits 0 with `unchanged` twice;
`git diff --stat docs/TOOL_CATALOG.md docs/tool-catalog.json` is empty. The
scenario stays `@planned`.

### FR-009 The gate is green with the reference counts plus the new tests

The verified-change gate of P-06 (`doc/arch/memory/constitution.md:109-112`)
passes, run from `/root/dev/ssh-mcp`:

- `cargo fmt --all -- --check`
- `cargo check --all-features --all-targets`
- `cargo clippy --all-features -- -D warnings`
- `cargo build --bins`
- `cargo nextest run --test-threads num-cpus --lib` (at least 2154 passing)
- `cargo nextest run --test-threads num-cpus --tests --features test-fixtures`
  (at least 2329 passing)
- `cargo build --no-default-features --target-dir target/no-default-features`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_http.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_transfer_progress.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v7_rsync_http.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v47_subscriptions.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v47_idempotency.py`
- `PYTHONPATH=scripts SSH_MCP_STDIO_BIN=$PWD/target/debug/ssh-mcp-stdio SSH_MCP_BIN=$PWD/target/debug/ssh-mcp SSH_MCP_TAIL_BIN=$PWD/target/debug/ssh-mcp-tail /root/dev/ssh-mcp/.venv/bin/python -m pytest -q scripts/test_v5_adversarial.py` (63 passed)
- `PYTHONPATH=scripts SSH_MCP_STDIO_BIN=$PWD/target/debug/ssh-mcp-stdio SSH_MCP_BIN=$PWD/target/debug/ssh-mcp SSH_MCP_TAIL_BIN=$PWD/target/debug/ssh-mcp-tail /root/dev/ssh-mcp/.venv/bin/python -m pytest -q scripts/test_v51_flush_bytes.py` (1 passed)
- `cargo audit`

No lint is disabled and no `#[allow]` or `#[expect]` is added. The two
nextest floors are 2154 and 2329: the feature-006 totals plus the twelve new lib tests and the one integration test.
The fate table has no removal. The adapted Python cases are exactly the five
adversarial cases and the one flush-bytes case of the fate table; the two files
are gate commands so the adaptation is proven. `scripts/chaos_v5_subscribe.py`
is not a gate command: it already fails on the unchanged tree
(`session.md`).

**Acceptance.** Each command exits 0; the two nextest runs report at least the
floors; `git diff` shows no `#[ignore]`, no removed test outside the tables,
and no new `#[allow]` or `#[expect]`. The scenario stays `@planned`.

### FR-010 `speckit validate` adds no finding to the baseline

The baseline is the error set on the feature-006 tree with the binary in use,
`speckit v0.49.0` (`cdb45907 2026-11-22T08:38:26Z`): exit 6 with 68 errors and
no warning. Rules: `fqcn.relative-super` 46, `branch.else` 6,
`pin.ci.unpinned-action` 5, `pin.docker.unpinned-image` 2, and one each of
`readme.mermaid.missing`, `readme.index.missing`, `readme.image.missing`,
`readme.arch-link.missing`, `pin.cargo.lockfile-missing`,
`llms-txt.file.missing`, `fqcn.glob-import`, `crew.roster.dangling` and
`ci.gitlab.missing`. No rule is waived and no feature-007 file is named.

**Acceptance.** `speckit validate` reports 68 errors with that rule set and no
warning, and no finding names a file of this feature or `session.md`. The
scenario stays `@planned`.

### FR-011 The production composition installs the guard

The object `build_use_cases` puts in `UseCases::sub_subscribe`
(`src/composition/mod.rs:192-194`, built at `src/composition/prod.rs:544-547`)
is guarded. A call through it refuses a missing Shell, Command, Transfer and
Session with the FR-001 variants and opens a Forward lane. This requirement
exists because the FR-001/FR-002 tests build the use case by hand: a design
that keeps `SubscribeUseCase::new` permissive and installs the guard with a
builder would keep every one of them green while production still accepted
missing ids.

**Acceptance.**
`tests/sub_open_resource_check.rs::production_sub_subscribe_refuses_missing_resources_and_opens_forward`
calls `ssh_mcp::composition::prod::build_use_cases()`, takes
`use_cases.sub_subscribe`, and asserts the four refusals with their variants
and one `forward://f/events` success with a `SubId`. It is a `#[tokio::test]`
and fails on the base tree because all five calls return `Ok`. Where the test
lives (integration file versus a `#[cfg(test)] mod tests` in
`src/composition/prod.rs`, which has none today) is Q-7; both routes add the
standard test attribute every test module of the crate already carries
(`#[cfg(test)] #[allow(clippy::unwrap_used, reason = "tests use unwrap for
brevity per AGENTS.md test policy")]` in the library, a crate-level
`#![allow(clippy::unwrap_used, …)]` in `tests/`).


### FR-012 "Exists" means the repository row is present, whatever its status

The guard asks the repository `get`. A row is removed only by an explicit
`remove` (disconnect, close, cancel); the command, transfer and shell TTLs at
`src/ports/config.rs:35`, `:38`, `:41` have no consumer. A terminal row is
therefore present and accepted: a completed command and a completed transfer
both open, and so does a shell row whose close has not removed it yet.

**Acceptance.**
`crate::application::subscription_admin::tests::completed_command_row_is_accepted`
and `...::completed_transfer_row_is_accepted` seed a row through the real
`DashMap` repository, set `Completed` through `update`, run the guarded use
case and assert it opens. Both are `#[tokio::test]` and both pass on the base
tree, pinning the meaning. The scenario stays `@planned`.

### FR-013 A refusal is not stored under its idempotency key

A refusal is a `CallToolResult` with `is_error: Some(true)`
(`src/infra/mcp/helpers/structured.rs:105-111`); `run_claimed` publishes only
results whose `is_error` is not `Some(true)`
(`src/infra/mcp/tool_router.rs:519-533`). A retry with the same
`_meta.idempotency_key` and the same arguments therefore re-runs the guard and
sees the resource once it exists; it does not replay the cached refusal.

**Acceptance.**
`crate::infra::mcp::tool_router::tests::refused_sub_open_is_not_cached_under_its_idempotency_key`
drives `with_idempotency_keyed` (`:452-476`) around `run_sub_subscribe` with
`KeyOutcome::Present("k")`: first call refused, the shell is seeded, the second
call with the same key and fingerprint succeeds. It is a `#[tokio::test]`; it
needs the new builder, so it is added with the implementation. The scenario
stays `@planned`.

## Security Requirements

- **Data sensitivity/classification.** The feature reads repository rows by an
  id already on the wire. No new field and no byte payload; the id is already
  part of the URI the caller supplies.
- **Authentication/authorization.** No authenticated surface is added. The
  guard runs in-process against the same repositories the read paths already
  use; it grants no access the resource read would not grant.
- **Input validation.** The URI parser is unchanged
  (`src/application/read_resource.rs:164-192`); the id charset check still
  runs first. The new check turns a well-formed but unknown id into a typed
  refusal instead of a live lane.
- **Cryptography in transit/at rest.** None is added; no secret is read.
- **Logging/audit.** No new log line. The refusal is visible in the tool
  answer and in `sub_list` by the lane's absence. The existing `sub_open`
  success render is unchanged.
- **Error-handling information exposure.** The refusal names the id the caller
  supplied and a static cure; it exposes no repository content. Detail
  truncation is the existing `DEFAULT_ERROR_DETAIL_MAX_BYTES` behavior
  (`src/infra/mcp/helpers/error.rs:44-50`).

## Acceptance Scenarios

Given a shell id no repository holds
When  `sub_open` runs with `shell://<id>/output`
Then  it returns `DomainError::ShellNotFound` (FR-001).

Given a command id no repository holds
When  `sub_open` runs with `command://<id>/output`
Then  it returns `DomainError::CommandNotFound` (FR-001).

Given a transfer id no repository holds
When  `sub_open` runs with `transfer://<id>/progress`
Then  it returns `DomainError::TransferNotFound` (FR-001).

Given a session id no repository holds
When  `sub_open` runs with `session://<id>/health`
Then  it returns `DomainError::SessionNotFound` (FR-001).

Given a refused `sub_open`
When  `sub_list` runs
Then  no lane exists and no debouncer was started (FR-002).

Given a `forward://`, `serial://` or `rsync://` URI with no producer
When  `sub_open` runs
Then  it answers OK with a `SubId` (FR-003).

Given a missing shell
When  the tool answer is rendered
Then  it carries `REASON: [SHELL_NOT_FOUND]` and the shell cure, and the
structured twin carries the same code (FR-005).

Given the repaired tree
When  `make tools-export-check` runs
Then  both catalogue files are `unchanged` (FR-008).

Given the repaired tree
When  the gate commands of FR-009 run
Then  each exits 0 with at least 2154 lib tests and 2329 tests with the
integration suites, and the two adapted scripts report 63 passed and 1 passed
(FR-009).

Given the feature's artifacts on disk
When  `speckit validate` runs over the corpus
Then  the error count stays at 68 with the same rule set and no warning fires
(FR-010).

Given the change landed
When  a reader opens `session.md`
Then  every out-of-scope defect of the measurement carries its `file:line` and
its reproduction note (FR-007).

Given the production composition built by `build_use_cases`
When  `UseCases::sub_subscribe` runs for a missing shell, command, transfer and
session and for a `forward://` URI
Then  the four are refused with their per-kind variant and the forward opens
(FR-011).

Given a command and a transfer row in a terminal status
When  `sub_open` runs for each
Then  both open (FR-012).

Given a refused `sub_open` and its `_meta.idempotency_key`
When  the same key is retried after the resource exists
Then  the retry re-runs the guard and opens (FR-013).

## Accepted Risks

- **AR-1 A synthetic id that used to open now fails.** Accepted and
  registered for the owner's review (Q-1): the tests and scripts that use a
  placeholder shell or command URI must seed a resource or switch to a
  permissive scheme. The alternative (a bypass flag) would leave two
  behaviors again.
- **AR-2 An id removed between the check and the open still yields a lane.**
  Accepted: the window is one `await` wide and F7 closes it later. Recorded in
  Out of Scope.
- **AR-3 A host that relied on subscribing before the resource exists for the
  four repository kinds loses that.** Accepted: the create tools return
  synchronously before the subscribe call in every documented workflow
  (`docs/LLM_GUIDE.md:141-152`), and the permissive kinds cover the
  producer-later cases.
- **AR-4 The frozen description stays false about `RESOURCE_NOT_FOUND`.**
  Accepted: the catalogue is frozen by policy; the mismatch is recorded in
  `session.md` and ADR 0007 for the owner.

## Observability

No metric, log event or trace span is added or removed. A refused call is
visible as a tool error with a per-kind code; a successful call renders exactly
as before. `sub_list` and `sub_stats` show only lanes bound to a resource that
existed at open time. Conventions stay those of
`doc/arch/observability/observability.md`.

## Clarifications

### Session 2026-11-22

All ten questions are answered by measurement or by a line of code, on the
feature-006 tree and in the throwaway copy `/var/tmp/ssh-mcp/guard-probe`. The
prototype installs a guard only in `src/composition/prod.rs`; `SubscribeUseCase::new`
stays permissive, so the probe measures the production change alone. Logs are
under `/var/tmp/ssh-mcp/logs/guard-probe-*.log`.

### Q-1 Which tests and scripts depend on a missing id being accepted

**Baseline, unmodified copy.** Commands run from
`/var/tmp/ssh-mcp/guard-probe/scripts` with
`PYTHONPATH=. SSH_MCP_STDIO_BIN=… SSH_MCP_BIN=… SSH_MCP_TAIL_BIN=…` pointing at
the probe's `target/debug` binaries and `/root/dev/ssh-mcp/.venv/bin/python -m
pytest <file> -q --tb=line`:

| Script | Exit | Summary |
| --- | --- | --- |
| `test_v5_adversarial.py` | 0 | `63 passed in 40.38s` |
| `test_v51_flush_bytes.py` | 0 | `1 passed in 0.37s` |
| `chaos_v5_subscribe.py` | 1 | `7 failed in 14.54s`; all seven fail at `:118` asserting `SSH_SUB_LIST: OK` against `SUB_LIST: OK` (the filter test also at `:285` with `SSH_SUB_STATS`). Pre-existing and not repaired here. |

**With the prototype guard.** Same commands:

| Script | Exit | Summary | Dependent cases |
| --- | --- | --- | --- |
| `test_v5_adversarial.py` | 1 | `5 failed, 58 passed in 46.63s` | the five below |
| `test_v51_flush_bytes.py` | 1 | `1 failed in 15.12s` | `test_subscribe_hint_mentions_flush_bytes_knob` |
| `chaos_v5_subscribe.py` | 1 | same seven failures, same sites | no new failure |

The five adversarial cases, each failing on the guard:

| Test | Site | Assertion that failed | Fate |
| --- | --- | --- | --- |
| `test_subscribe_invalid_filter_regex` | `:665` | `assert 'SHELL_NOT_FOUND' == 'INVALID_ARGUMENT'` | adapted — `forward://x/events` |
| `test_max_subs_per_uri_exceeded_with_low_cap` | `:195` | `subscribe failed: 'SUB_OPEN: ERROR … SHELL_NOT_FOUND … overflow-test'` | adapted — `forward://overflow-test/events` |
| `test_subscribe_unsubscribe_concurrent_burst` | `:195` via `:768` | `subscribe failed: … SHELL_NOT_FOUND … burst-22` | adapted — `forward://burst-<i>/events` |
| `test_subscribe_returns_required_hint_line` | `:195` | `subscribe failed: … SHELL_NOT_FOUND … hint-test` | adapted — `forward://hint-test/events` |
| `test_sub_list_during_burst_churn` | `:195` | `subscribe failed: … SHELL_NOT_FOUND … churn-0` | adapted — `forward://churn-<i>/events` |

`test_v51_flush_bytes.py:87` fails with `AssertionError: SUB_OPEN: ERROR`
(`COMMAND_NOT_FOUND` on `command://placeholder/output`); fate: adapted to
`forward://placeholder/events`. The chaos shell cases are masked by the
pre-existing prefix failure; they stay as they are.

**Constructor-guarded cost.** The 26 `new` references are one declaration
(`src/application/subscription_admin.rs:93`) and 25 call sites: 17 in
`subscription_admin` unit tests (`:702`, `:723`, `:740`, `:757`, `:1092`,
`:1109`, `:1126`, `:1143`, `:1160`, `:1177`, `:1194`, `:1211`, `:1231`,
`:1272`, `:1533`, `:1552`, `:1571`), 5 in `tests/v5_smoke.rs` (`:88`, `:577`,
`:688`, `:786`, `:910`) and 3 in `tests/chaos_inline_push.rs` (`:221`, `:406`,
`:447`). All 25 would fail to compile if `new` took a guard. At runtime 24
exercise checked kinds with no seeded row and would fail unless seeded; the
single forward URI (`subscription_admin.rs:1314-1317`) would pass. Kinds across
the 25: shell 20, command 1, transfer 1, session 1, forward 1, and one
`ftp://` that fails at parse before the guard. The chosen design (guard only in
composition) keeps all 25 as they are.

### Q-2 Idempotency

A refusal is a `CallToolResult` with `is_error: Some(true)`
(`src/infra/mcp/helpers/structured.rs:105-111`). `run_claimed` publishes to the
cache only when `response.is_error != Some(true)`
(`src/infra/mcp/tool_router.rs:519-533`). Probe test
`crate::infra::mcp::tool_router::tests::refused_sub_open_is_not_cached_under_its_idempotency_key`
drives `with_idempotency_keyed` (`:452-476`) with `KeyOutcome::Present("k")`
around `run_sub_subscribe`: first call refused, the shell is seeded, the second
call with the same key and fingerprint succeeds. Answer: a refusal is not
cached; a retry re-runs the check and sees the resource once it exists.

### Q-3 Embedded daemon

The `subscribe` op sends only the URI
(`src/embed/dispatcher.rs:552`, `:570-603`); the guard runs in the server use
case, and the ack/err shape is unchanged (`Event::Ack` at `:587-595`,
`SUBSCRIBE_FAILED` at `:596-602`). The op enum has no `sub_*` tool
(`src/embed/parser.rs:44-246`). The daemon tests `tests/v5_daemon_smoke.rs::t01_daemon_connect_exec_subscribe_drain`,
`::t02_daemon_signal_shutdown`, `::t05_daemon_buffer_pressure_does_not_deadlock`
and the dispatcher unit tests pass in the probe (`guard-probe-tests.log`,
2316 passed). A daemon subscribe to a missing id now fails `SUBSCRIBE_FAILED`
with the per-kind message in `detail`; no NDJSON field changes.

### Q-4 HTTP

`build_http_service` builds one server per session through the factory
(`src/composition/prod.rs:754-765`), and `build_use_cases` allocates fresh
repositories per call (`:227-231`). The guard is a field of the use case
holding those `Arc`s; it adds no process-wide state. The only global is the
pre-existing producer forwarder (`:541-543`), untouched by this feature.

### Q-5 Where the policy lives — plan decision

Constraints, each from the code:

- One implementation of the kind table in `src/` (FR-004).
- Layer table: `application` may depend on `domain` and `ports`
  (`docs/ARCHITECTURE.md:112`); `ports` holds pure traits and may depend on
  `domain` (`:111`); no adapter imports `crate::application`.
- P-01 (`doc/arch/memory/constitution.md:89-91`): imports point inward through
  the six layers; ports are pure traits; use cases stay generic over their
  ports; "No `Box<dyn Trait>` on a hot path"; an adapter never calls another
  adapter directly.
- `SubscribeUseCase` already holds `Arc<dyn LaneAdmin>`
  (`src/application/subscription_admin.rs:82`), so a cold-path dyn port has
  precedent; `SubscribeResourceUseCase` is generic over four repositories
  (`src/application/subscribe_resource.rs:66-80`) and keeps its behavior and
  tests.
- No non-test `application` type implements a `ports` trait today; the only
  implementors of ports in `src/application/` are `#[cfg(test)]` stubs
  (`grep -rn 'impl .*Port for' src/application` lists only test modules). The
  nearest precedent is `NoopLifecycle`, a `ports` type implementing a `ports`
  trait (`src/ports/lifecycle_policy.rs:163-165`).
- No new lock on a producer path; production code panic-free; no `super::`
  import and no `else` after an early return in new production code.

The plan chooses among: an application module generic over the four
repositories, a static-dispatch `ports` trait with a composition-built
adapter, or the probe's cold-path dyn port. Registered as a plan decision.

### Q-6 Exact totals

No test is removed. Tests added: FR-001 four (`subscribe_unknown_shell_is_refused`,
`subscribe_unknown_command_is_refused`, `subscribe_unknown_transfer_is_refused`,
`subscribe_unknown_session_is_refused`), FR-002 two
(`refused_sub_open_leaves_no_lane_and_no_debouncer`,
`refused_then_created_resource_can_be_subscribed`), FR-003 one
(`forward_serial_and_rsync_stay_permissive`), FR-005 two
(`sub_open_refusal_answers_per_kind_code`,
`sub_open_refusal_renders_per_kind_code`), FR-011 one integration
(`production_sub_subscribe_refuses_missing_resources_and_opens_forward`).

Arithmetic: lib `2142 + 12 = 2154` (four FR-001, two FR-002, one FR-003, two
FR-005, one FR-013 and two FR-012 tests); with integration `2316 + 13 = 2329`
(the integration file adds only to the second). FR-009, its scenario, the feature
file and the schema say "at least" those numbers.

### Q-7 Where the FR-011 test lives

`Cargo.toml` has no `autotests = false`; only `lockfree_invariants` has an
explicit `[[test]]` block (`:54-57`), so a new `tests/*.rs` is auto-discovered
and needs no manifest entry and no feature: `build_use_cases` is not
feature-gated. It is `pub` (`src/composition/prod.rs:227`), `composition::prod`
is `pub` (`src/composition/mod.rs:28`), `UseCases::sub_subscribe` is `pub`
(`:194`), and every type in the return tuple is public; the private
`ProdWiring` alias does not block destructuring. Proven by
`tests/sub_open_resource_check.rs`, which passes in the probe
(`Summary [0.005s] 1 test run: 1 passed`). Calling it installs the
process-wide producer forwarder (`src/composition/prod.rs:541-543`,
`src/adapters/subscription/legacy.rs:193-195`); in a dedicated test binary the
last call wins and earlier registries stop receiving forwarded pokes, so the
file keeps one composition test. Decision: integration file under `tests/`.

### Q-8 Production callers of `SubscribeUseCase::execute`

`findReferences` on `:135` returns 31 references; the only production one is
`src/infra/mcp/tool_router.rs:1313` inside `run_sub_subscribe`. The others are
the 20 unit-test calls and the 10 integration calls listed in Q-1. Nothing
under `src/embed/` names the type or the method.

### Q-9 The three permissive kinds

Forward ids live in `DashMapForwardRepo` (`src/adapters/repo/dashmap/forward.rs:57`)
behind `ForwardRepository` (`src/ports/forward_repo.rs:18-19`); rsync ids live
in `DashMapRsyncRepo` (`src/adapters/repo/dashmap/rsync.rs:28`) behind
`RsyncRepository` (`src/ports/rsync_repo.rs:25-26`); serial lives in the
process-wide `SERIAL_REGISTRY` (`src/adapters/serial/state.rs:325`) with no
port. The application layer can ask forward and rsync today; serial it cannot.
The permissive arm is a choice for two kinds and a necessity for serial, and
the policy of `resources/subscribe` is kept. The dead lane a typo in those
three kinds still opens is recorded in Out of Scope and in `session.md`.

### Q-10 What "exists" means

The guard is a repository `get`. Rows are removed only by an explicit
`remove` (disconnect, close, cancel); the command, transfer and shell TTLs at
`src/ports/config.rs:35`, `:38`, `:41` have no consumer, so a terminal row
stays until the resource is removed. Probe tests
`crate::application::subscription_admin::tests::completed_command_row_is_accepted`
and `...::completed_transfer_row_is_accepted` set a terminal status through the
real `DashMap` repositories and the guarded use case still opens; both pass.
"Exists" therefore means "the row is present", terminal or not.

## Owner review

- The answer code (per-kind, not `RESOURCE_NOT_FOUND`) is the decision
  registered in ADR 0007.
- The six script edits of Q-1 are the only non-Rust changes; they are
  registered here because a script that no gate runs cannot prove the feature.
