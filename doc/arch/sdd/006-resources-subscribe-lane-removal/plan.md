# Implementation Plan: Resources Subscribe Lane Removal

## Overview

Stop `resources/subscribe` from opening a lane, remove the now-dead `sub_id`
surface from both resource use cases, make the registry report whether a
`(peer, URI)` entry was created or refreshed so the lifecycle refcount stays
balanced, and roll a created entry back when `on_subscribe` refuses the
resource. The subscription keeps its registry entry, cursors and debouncer;
`sub_open` lanes are untouched.

The spike at `/var/tmp/ssh-mcp/lane-spike` carries the whole change — code,
tests and documents — to a green gate. `research.md` carries the base results
of the six tests, the three mutation checks, the gate table and the patch.

## Technical Approach

### 1. The disposition (FR-003, D-1)

`SubscriberRegistryAsync::subscribe` returns a new
`SubscribeDisposition { Created, Refreshed }`:

- declared in `src/ports/subscriber_registry.rs` next to the port;
- `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`, no `#[must_use]`;
- `MemoryRegistry::insert_subscriber` computes `existed` from the entries it
  already has under the `subscribers` shard guard, before the `retain`, so no
  second map lookup and no lock are added; it returns
  `(first, disposition)` and `subscribe` forwards the disposition;
- `subscribe_with_sub_id` keeps its `SubId` return and ignores the
  disposition internally.

Callers, all read with a text search (the trait method is macro-generated, so
`goToImplementation` finds nothing): production `subscribe_resource.rs:137-140`
reads it; every other call site keeps its `()`-era shape because no
`#[must_use]` is added. Only the two `RecordingRegistry` stubs change
signature.

### 2. `execute` order and rollback (FR-001, FR-004, D-2)

`SubscribeResourceUseCase::execute` becomes, in order: parse → `ensure_exists`
→ `subscribe` (capturing `peer_id` before the move) → on `Created`, call
`on_subscribe`; on any error from it, `unsubscribe(&peer_id, &canonical)`
(bind the bool with `let _ =`), then return the original error → otherwise
return `SubscribeResourceOutcome { uri }`.

The rollback removes a created record unconditionally, including one a
concurrent refresh has replaced; the refresh client's success is not
re-validated. Consistency, not quiescence, is the reason: every later
subscribe of a refused resource fails the same way while the retained
`Closed` entry lives (`refcount.rs:371-380`, `:419-424`). `Closed` does not
stop the producer (`grace_timer.rs:148`, `refcount.rs:564`), so the refresh
caller can miss notifications; that is AR-4.

Alternatives: leave the entry on error (fails FR-004); re-read the entry after
the rollback to decide whether to keep it (an extra registry read that changes
nothing observable, because the record has no owner to attribute it to).

### 3. Removal map (FR-001, FR-002)

| File | Leaves | Unused imports / doc comments |
| --- | --- | --- |
| `src/application/subscribe_resource.rs` | `sub_id` field of the outcome, `lane` field, `with_subscriber_lane`, `open_lane_if_wired` | `SubId`, `LaneAdmin`, `LanePolicy`; outcome doc comment |
| `src/application/unsubscribe_resource.rs` | `sub_id` field, `lane` field, `with_subscriber_lane`, the lane-close branch | `SubId`, `LaneAdmin`; module and field docs |
| `src/infra/mcp/resource_handlers.rs` | `log_subscribe_outcome` and its call | `SubscribeResourceOutcome` |
| `src/composition/prod.rs` | both `.with_subscriber_lane(...)` calls | — |
| `src/adapters/subscription/memory_registry.rs` (test module) | the pinned test | — |

`LaneAdmin`, `LanePolicy`, `SubscriberLaneAdapter` and the `sub_*` tools stay.

### 4. Tests (D-4)

All six live in application test modules and build real adapters
(`MemoryRegistry`, `SubscriberLaneAdapter`, `RefcountedLifecycleAdapter`) plus
a local recording notifier and `StubPeer`; no test under `src/adapters/`
imports `crate::application`.

| Test | Module | Builds | Asserts | FR |
| --- | --- | --- | --- | --- |
| `resources_subscribe_records_no_lane_and_delivers_once` | `crate::application::subscribe_resource::tests` | real registry + lane adapter (detached), seeded shell, tracked lifecycle | one registry entry, `list_subs()` empty, one flush records one peer event, a 100 ms settle still reads 1, no lane counters | FR-001, FR-005 |
| `resources_unsubscribe_removes_the_registry_entry` | `crate::application::unsubscribe_resource::tests` | real registry + lane adapter | snapshot empty after unsubscribe; a second call succeeds | FR-002 |
| `resources_unsubscribe_leaves_a_sub_open_lane_open` | same | plus a directly opened `sub_open` lane | the lane survives the use case | FR-002 |
| `repeated_resources_subscribe_counts_once_in_the_lifecycle` | `crate::application::subscribe_resource::tests` | real registry + lifecycle | one entry, refcount 1 after two subscribes, 0 after one unsubscribe, stays 0 after a second | FR-003 |
| `lifecycle_refusal_leaves_no_registry_entry_and_no_delivery` | same | tracked shell force-closed first | `ResourceGone`, empty snapshot, refcount 0, no event on a flush | FR-004 |
| `seventeenth_resources_subscribe_with_interleaved_unsubscribe_succeeds` | same | lane adapter at the default cap 16 | 17 cycles all succeed | FR-011 |

Existing tests: the four that read a removed symbol are removed; the six in
`unsubscribe_resource.rs` that only use `req()` are adapted (the helper loses
the field); `release_resource_reaches_closed_via_production_unsubscribe_path`
is adapted the same way; the pinned memory-registry test is deleted because
the application test proves its replacement. Five tests leave, six arrive.

### 5. Documents (FR-005, FR-006)

| File | Old | New |
| --- | --- | --- |
| `docs/LLM_GUIDE.md:69` | "Every `sub_open` (or legacy `resources/subscribe`) returns a `sub_id` …" | only `sub_open` returns an id; the legacy method is peer-keyed |
| `docs/LLM_GUIDE.md:73` | "only explicit `sub_close` or peer-transport disconnect closes it" | `sub_close` closes a lane; `resources/unsubscribe` removes the legacy subscription |
| `docs/RESOURCES.md:291-292` | `lane_admin.close(sub_id)` for both | only `sub_close` reaches a lane |
| `docs/ARCHITECTURE.md:780` | "Per `(SubId, Uri)` cursor …" | `(PeerId, Uri)` only; the legacy method opens no lane |
| `doc/arch/functional/functional.md:203-205` | "share the registry" | uses the registry only |
| `doc/arch/functional/functional.md:229-236` | "Open limitation: … does not gate" | opens no lane; fan-out delivers; new pin named |
| `doc/arch/functional/functional.md:341` | residual row, unproven | no lane; new test; proven |
| `docs/OPERATIONS.md:71` | "the call returned a `sub_id`" | only `sub_open` returns one |
| `docs/LLM_GUIDE.md:236-238` | "Track every sub_id returned by sub_open (or the legacy resources/subscribe)" | `sub_open` returns the id `sub_close` takes; legacy ends with `resources/unsubscribe` |
| `docs/RESOURCES.md:266-272` | one message, "subscribe / register lane", "ack (SUB_ID)" | two paths, only `sub_open` acks an id |
| `README.md:32` | "Each `resources/subscribe` mints a `SubId` …" | `sub_open` mints the id and lane |
| `docs/API.md:1128` | "A lane opened by `resources/subscribe` … keeps delivering while paused." | no lane; the fan-out delivers; no pause target |
| `doc/arch/runbooks/incident-push-delivery-lost.md:106-111` | "Open residual … close the lane with `sub_close` and open a new one" | no residual; `sub_list` has no row |
| `CHANGELOG.md` | — | bullet under `[Unreleased]` → `### Changed` |
| `docs/MIGRATION.md` | — | paragraph under `## v7.1.3 → unreleased` |
| `session.md:112-117` | residual entry | marked repaired by feature 006 |

### 6. Gate (FR-008)

The command list of FR-008 plus `cargo check --all-features --all-targets`,
`cargo build --bins` and `make tools-export-check`, run from the repository
root, one cargo invocation at a time. FR-007 is proved by row 9 (both
catalogue files `unchanged`) and by the unchanged responses; FR-009 by
`speckit validate` after the gate.

### 7. Re-anchoring (FR-005, FR-006)

Every `file:line` written by this feature into a standing document is read
from the final spike tree after the code change, not from the pre-change
tree: the two new citations in `functional.md` and the runbook
(`memory_registry.rs:995-1000`) and every entry of the `session.md` defect
list that cites a file this feature changes. Check: `grep -n` each cited line
on the spike tree and confirm the symbol is there. Citations in `spec.md`,
`research.md` and the ADR describe the pre-change tree and stay. The
conformity item 6 of the `session.md` update plan is extended to re-anchor
the citations into the files changed by features 005 and 006.

## Decisions

### D-1 The disposition type

Chosen: `SubscribeDisposition { Created, Refreshed }`, without `#[must_use]`,
in the port module.

A `bool` is ambiguous with the existing `first` return of
`insert_subscriber` (`memory_registry.rs:596`), which means "first subscriber
on the resource" and is true both for a refresh of the sole peer and false
for a new second peer. A second `contains` call would re-read the shard. The
enum names the two facts.

`#[must_use]` was tried and dropped: it buys nothing here. The enum has one
production caller (`subscribe_resource.rs:137-140`), which reads it and is
pinned by the FR-003 test and its mutation check, while the attribute forces
`let _ =` on 25 call sites in three files the feature otherwise would not
touch (21 in the `memory_registry.rs` test module, 3 in `peer_gc.rs`, 1 in
`tests/chaos/chaos37_...rs`), against D-12.

### D-2 Rollback on every error

Chosen: remove the created record on any `on_subscribe` error and return that
error. Today the adapter returns only `ResourceGone`
(`refcount.rs:419-424`, `:482-488`, with its own increment undone at `:483`),
but the rule is written for the port, not the implementor. If the rollback's
`unsubscribe` returns `false` the record is already gone; the original error
is still returned.

### D-3 No new port for the removal

The two use cases simply stop holding the lane. A `LaneCleanup` port would
have one implementor and no caller after the removal, so it is not added.

## File Map

| File | Requirement |
| --- | --- |
| `src/ports/subscriber_registry.rs` | FR-003 (enum, signature) |
| `src/adapters/subscription/memory_registry.rs` | FR-003 (compute the disposition under the guard) |
| `src/application/subscribe_resource.rs` | FR-001, FR-003, FR-004 (order, rollback, removal) |
| `src/application/unsubscribe_resource.rs` | FR-002 (removal) |
| `src/infra/mcp/resource_handlers.rs` | FR-001 (log removal, request construction) |
| `src/composition/prod.rs` | FR-001, FR-002 (wiring removal) |
| `CHANGELOG.md`, `docs/MIGRATION.md`, `docs/API.md`, `docs/OPERATIONS.md`, `docs/ARCHITECTURE.md`, `docs/LLM_GUIDE.md`, `docs/RESOURCES.md`, `README.md` | FR-005, FR-006 |
| `doc/arch/functional/functional.md`, `doc/arch/runbooks/incident-push-delivery-lost.md` | FR-005 |
| `session.md` | FR-005, FR-010 |

No other file changes.

## Verification Plan

| # | Command | Expected |
| --- | --- | --- |
| 1 | `cargo fmt --all -- --check` | exit 0 |
| 2 | `cargo check --all-features --all-targets` | exit 0 |
| 3 | `cargo clippy --all-features -- -D warnings` | exit 0 |
| 4 | `cargo build --no-default-features --target-dir target/no-default-features` | exit 0 |
| 5 | `cargo build --bins` | exit 0 |
| 6 | `cargo nextest run --test-threads num-cpus --lib` | 2142 passed |
| 7 | `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` | 2316 passed |
| 8 | `cargo audit` | exit 0; the allowed `chrono` warning only |
| 9 | `make tools-export-check` | both `unchanged` |
| 10–14 | the five Python suites | 15 / 2 / 10 / 20 / 9 passed; a fast-machine run may report 1 passed + 1 skip on row 11 (`scripts/test_transfer_progress.py:192`) |

The spike ran all fourteen: see `research.md` §4. Mutation checks: calling
`on_subscribe` on a refresh fails FR-003; removing the rollback fails FR-004;
re-wiring the lane fails FR-001 and FR-011.

## Constitution

- **D-01 spec first** — the change follows the clarified specification.
- **D-03 layering** — the new type is in `ports`; no adapter names another.
- **D-05 calisthenics** — six small tests; the schema stays under the
  definition and field ceilings.
- **D-06 FQCN** — every new import is a full `crate::…` path.
- **D-07 lints** — no lint disabled, no `#[allow]`/`#[expect]` added.
- **D-08 fail loud** — production stays panic-free; the rollback propagates.
- **D-11 stable contracts** — no wire change.
- **D-12 small changes** — one concern, one revertible unit.
- **P-01 inward dependencies** — no test under `src/adapters/` imports
  `crate::application`; the pin moves to the application module.
- **P-02 lock-free** — the disposition comes from the guard already held.
- **P-03 suppressions** — none added.
- **P-04/P-05 frozen wire** — catalogue and responses untouched.
- **P-06 gate** — the verification plan is the gate.

## Rollback

Reverse-apply `/var/tmp/ssh-mcp/lane-spike.patch` as one unit (17 files,
570 insertions and 360 deletions, `a/` and `b/` prefixes, SHA-256
`42c479ceaa66cfdd5957e91ede9bef327ee5506bde5851f890b6c042797cae8e`). The lane
returns, the two DTO fields return, and the document corrections revert with
the code.

## Open Decisions

None. Q-1 stays the owner-review risk (AR-4 is the accepted consequence of
D-2); Q-4 is answered by D-1.

## Companion Artifacts

- `research.md` — base results, mutation table, gate table, validate result
  and the patch.
