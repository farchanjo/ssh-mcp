# Tasks: Resources Subscribe Lane Removal

## Task Breakdown

Fifteen tasks, in the order they run. The six tests land first **in their
base form** and are observed failing (four) or passing (two) on the unchanged
tree; the port and the registry follow; the composition stops calling the
builders before the builders are removed; each removal carries the tests that
reference the removed symbol; the documents and `session.md` follow; the gate,
the validation and the closing equality against the spike close. Every file
named below is in the file map of `plan.md`.

**Base form of T001.** The six tests are added exactly as the spike's final
tests except in three places that the later removals delete:

- every `UnsubscribeResourceRequest` constructor carries `sub_id: None`
  (`resub_unsub`, and the three explicit literals in the two unsubscribe
  tests);
- both harnesses build **both** use cases with
  `.with_subscriber_lane(...)`, through a `let lane: Arc<dyn LaneAdmin> =
  Arc::clone(&lanes) as Arc<dyn LaneAdmin>;` coercion, because production
  wires them today;
- the two modules import `LaneAdmin`.

T005 removes the subscribe builder and the `LaneAdmin` wiring in both
harnesses; T006 removes the unsubscribe builder, the `sub_id: None` fields and
the remaining `LaneAdmin` import. Without this base form the two "fails today"
results of `research.md` §2 cannot be reproduced.

The three mutation checks are **not** a task: they were run in the spike and
recorded in `research.md` §5. Every task below was replayed in order in
`/var/tmp/ssh-mcp/lane-replay`; the exit statuses and summary lines are in
`research.md` §9.

Build rules for every task that builds: `RUSTC_WRAPPER=sccache` and
`CARGO_BUILD_JOBS=4` stay exported; no `-j`, no `--release`, no
`nice`/`taskset`; tests run with `cargo nextest run --test-threads num-cpus`,
never `cargo test`; no `#[allow]` and no `#[expect]` is added; no test is
ignored or weakened. The spike at `/var/tmp/ssh-mcp/lane-spike` is the
reference for every hunk; the closing task compares against it.

- [x] **T001 Add the six proving tests in their base form and record the base
  results.**
  - **Files:** `src/application/subscribe_resource.rs` (test module:
    `resources_subscribe_records_no_lane_and_delivers_once`,
    `repeated_resources_subscribe_counts_once_in_the_lifecycle`,
    `lifecycle_refusal_leaves_no_registry_entry_and_no_delivery`,
    `seventeenth_resources_subscribe_with_interleaved_unsubscribe_succeeds`),
    `src/application/unsubscribe_resource.rs` (test module:
    `resources_unsubscribe_removes_the_registry_entry`,
    `resources_unsubscribe_leaves_a_sub_open_lane_open`), plus the local
    harnesses (recording notifier, `StubPeer`, seeded shell, real
    `MemoryRegistry`, `SubscriberLaneAdapter`, `RefcountedLifecycleAdapter`).
  - **Requirements:** FR-001, FR-002, FR-003, FR-004, FR-005, FR-011.
  - **Do:** the base form described above.
  - **Check:** `cargo check --all-features --all-targets` exits 0; the
    six-test filter of `research.md` §2 exits 100 with `2 passed, 4 failed`
    and the four recorded messages (`no lane may be opened`; `a refresh must
    not increment the lifecycle`, left 2 right 1; `assertion failed:
    snapshot_subscribers(...).is_empty()`; `subscribe round 16 failed: max
    subs per URI exceeded … (limit 16)`). Replay: check 0, tests 100,
    `2 passed, 4 failed`.
  - **Compiles:** yes; measured.

- [x] **T002 Add `SubscribeDisposition` and return it from the registry.**
  - **Files:** `src/ports/subscriber_registry.rs` (enum and `subscribe`
    return type), `src/adapters/subscription/memory_registry.rs` (`existed`
    under the shard guard, `insert_subscriber` returning
    `(bool, SubscribeDisposition)`, `subscribe` forwarding it), and the
    `subscribe` method of each `RecordingRegistry` stub (return type and
    `Ok(SubscribeDisposition::Created)`; `unsubscribe`, `drop_peer` and
    `NotifierPort` are unchanged).
  - **Requirements:** FR-003.
  - **Do:** `plan.md` §Technical Approach 1 and D-1; no `#[must_use]`.
  - **Check:** `cargo check --all-features --all-targets` exits 0;
    `test(repeated_resources_subscribe_counts_once)` still fails with
    `left: 2`, `right: 1`. Replay: check 0, test 100, left 2 right 1.
  - **Compiles:** yes; measured.

- [x] **T003 Order `execute` on the disposition and add the rollback.**
  - **Files:** `src/application/subscribe_resource.rs` (`execute`: capture
    `peer_id`, read the disposition, call `on_subscribe` on `Created` only,
    remove the created entry and return the error on any `on_subscribe`
    error).
  - **Requirements:** FR-003, FR-004.
  - **Do:** `plan.md` §Technical Approach 2 and D-2. The lane stays wired.
  - **Check:** `cargo check --all-features --all-targets` exits 0;
    `test(lifecycle_refusal_leaves_no_registry_entry)` passes;
    `test(repeated_resources_subscribe_counts_once)` still fails on
    `assertion failed: h.lanes.list_subs().is_empty()` until T005. Replay:
    check 0, tests 100, `1 passed, 1 failed` with that assertion.
  - **Compiles:** yes; measured.

- [x] **T004 Stop wiring both use cases in composition.**
  - **Files:** `src/composition/prod.rs` (both `.with_subscriber_lane(...)`
    calls).
  - **Requirements:** FR-001, FR-002.
  - **Do:** remove both calls; `lane_admin` stays for the `sub_*` use cases.
  - **Check:** `cargo check --all-features --all-targets` exits 0;
    `grep -c 'with_subscriber_lane' src/composition/prod.rs` → 0. Replay:
    check 0, grep 0.
  - **Compiles:** yes; measured. The builders still exist, so nothing else
    breaks.

- [x] **T005 Remove the lane surface from `SubscribeResourceUseCase`, its
  handler log, and the tests that read it.**
  - **Files:** `src/application/subscribe_resource.rs` (`sub_id` field,
    `lane` field/`with_subscriber_lane`/`open_lane_if_wired`, unused imports
    and the outcome doc comment; remove
    `subscribe_outcome_sub_id_is_none_when_lane_not_wired` `:615` and
    `subscribe_with_lane_returns_sub_id_on_outcome` `:628`; unwire the
    subscribe builder in **both** harnesses and drop the subscribe-module
    `LaneAdmin` import), `src/infra/mcp/resource_handlers.rs`
    (`log_subscribe_outcome` and its call, the `SubscribeResourceOutcome`
    import).
  - **Requirements:** FR-001, FR-005.
  - **Do:** `plan.md` §Technical Approach 3. One task because the field, the
    log that reads it and the two tests that call the builder cannot compile
    apart.
  - **Check:** `cargo check --all-features --all-targets` exits 0;
    `test(resources_subscribe_records_no_lane)` and
    `test(repeated_resources_subscribe_counts_once)` pass. Replay: check 0,
    tests 0, `2 passed`.
  - **Compiles:** yes; measured.

- [x] **T006 Remove the `sub_id` and lane surface from
  `UnsubscribeResourceUseCase`, its request construction, and the tests that
  read it.**
  - **Files:** `src/application/unsubscribe_resource.rs` (`sub_id` and
    `lane` fields, `with_subscriber_lane`, the lane-close branch, unused
    imports/docs; `req()` `:224` and
    `release_resource_reaches_closed_via_production_unsubscribe_path` `:398`
    lose the field; remove `unsubscribe_with_lane_closes_matching_sub_id`
    `:321` and `unsubscribe_tolerates_unknown_sub_id_when_lane_wired` `:367`;
    remove the remaining `sub_id: None` literals; unwire the unsubscribe
    builder and drop the `LaneAdmin` import), `src/infra/mcp/resource_handlers.rs`
    (the request construction).
  - **Requirements:** FR-002.
  - **Do:** `plan.md` §Technical Approach 3. One task because the request
    field, the use-case field and the tests that build it cannot compile
    apart.
  - **Check:** `cargo check --all-features --all-targets` exits 0;
    `test(resources_unsubscribe_removes_the_registry_entry)` and
    `test(resources_unsubscribe_leaves_a_sub_open_lane_open)` pass. Replay:
    check 0, tests 0, `2 passed`.
  - **Compiles:** yes; measured.

- [x] **T007 Delete the pinned residual test.**
  - **Files:** `src/adapters/subscription/memory_registry.rs` (delete
    `resources_subscribe_lane_is_listed_but_pause_does_not_gate_the_peer_fanout` `:1787`).
  - **Requirements:** FR-005.
  - **Do:** delete only; its replacement is T001's application test.
  - **Check:** `cargo nextest run --lib --test-threads num-cpus` → 2142
    passed, exit 0. Replay: check 0, `2142 tests run: 2142 passed`.
  - **Compiles:** yes; measured.

- [x] **T008 Correct the feature-005 residual passages.**
  - **Files:** `doc/arch/functional/functional.md:203-205`, `:229-236`,
    `:341`; `doc/arch/runbooks/incident-push-delivery-lost.md:106-111`.
  - **Requirements:** FR-005.
  - **Do:** the replacement text of `plan.md` §5.
  - **Check:** `grep -n '995-1000' doc/arch/functional/functional.md
    doc/arch/runbooks/incident-push-delivery-lost.md` returns `:231` and
    `:108`; `grep -n 'Open residual' doc/arch/runbooks/incident-push-delivery-lost.md`
    prints nothing. Replay: documents only.
  - **Compiles:** not applicable.

- [x] **T009 Correct the ten false sentences in `docs/`, `README.md` and the two `doc/arch` documents.**
  - **Files:** `docs/LLM_GUIDE.md:69`, `:73`, `:236-238`;
    `docs/RESOURCES.md:266-272`, `:291-292`; `docs/ARCHITECTURE.md:780`;
    `docs/API.md:1128`; `docs/OPERATIONS.md:71`; `README.md:32`.
  - **Requirements:** FR-006.
  - **Do:** the replacement text of `plan.md` §5, including the two-path
    diagram opening and the four-line close with no semicolon in any message.
  - **Check:** `grep -rn 'resources/subscribe' docs README.md` prints no
    sentence giving the legacy method a lane or a `sub_id` outside the
    historical ADR/release-note passages; `speckit mermaid render` of the
    changed lines exits 0. Replay: documents only.
  - **Compiles:** not applicable.

- [x] **T010 Record the change in `CHANGELOG.md` and `docs/MIGRATION.md`.**
  - **Files:** `CHANGELOG.md` (`[Unreleased]` → `### Changed`),
    `docs/MIGRATION.md` (`## v7.1.3 → unreleased`).
  - **Requirements:** FR-006.
  - **Do:** the two bullets of `plan.md` §5.
  - **Check:** `grep -n 'no longer opens a lane' CHANGELOG.md` returns `:17`;
    `grep -n 'own no lane' docs/MIGRATION.md` returns `:882`. Replay:
    documents only.
  - **Compiles:** not applicable.

- [x] **T011 Mark the two defects repaired and re-anchor the `session.md` citations.**
  - **Files:** `session.md` — mark `:112-117` and `:124-127` repaired; add
    the entry/lifecycle window entry `:152-158`; re-anchor the defect-list
    entries that cite `memory_registry.rs`, `subscribe_resource.rs`,
    `unsubscribe_resource.rs` and `prod.rs` on the final tree; extend item 6
    of the update plan with the files changed by 005 and 006.
  - **Requirements:** FR-005, FR-010.
  - **Do:** the final-tree citations of `plan.md` §7 and the re-anchoring
    table of the handover report.
  - **Check:** `grep -n '995-1000\|133-152\|81-90\|541-543\|425-429\|802-815' session.md`
    returns the re-anchored lines; `grep -n 'opens no lane' session.md`
    returns `:126` and `:136`; `grep -n '001, 002, 003, 005 and 006'
    session.md` returns `:95`. Replay: documents only.
  - **Compiles:** not applicable.

- [x] **T012 Run the gate.**
  - **Files:** none.
  - **Requirements:** FR-007, FR-008.
  - **Do:** the fourteen numbered rows of the verification table in
    `plan.md` §Verification Plan, one cargo invocation at a time.
  - **Check:** every row exits 0; rows 6 and 7 report 2142 and 2316 passed;
    rows 1–5 and 8–14 report the results listed there (`unchanged` twice, the
    `chrono` warning only, the five suite tallies). Replay: the code tasks
    T001–T007 were replayed; the full gate was measured in the spike.
  - **Compiles:** yes.

- [x] **T013 Run `speckit validate` and `speckit verify --changed`.**
  - **Files:** none.
  - **Requirements:** FR-009.
  - **Do:** both commands from the repository root.
  - **Check:** `speckit validate` exits 6 with 68 errors, the baseline rule
    set and no warning; `speckit verify --changed` reports its summary with
    `0 failed`. Replay: `speckit validate` in the replay copy exits 6 with 68
    errors, the same rule set, no warning.
  - **Compiles:** not applicable.

- [x] **T014 Prove equality with the spike.**
  - **Files:** none.
  - **Requirements:** FR-001 to FR-011.
  - **Do:** compare every file of the patch against the spike.
  - **Check:** `diff -r /var/tmp/ssh-mcp/lane-spike/src src` and the same for
    `tests` print nothing; each document of the patch compared with `diff`
    prints nothing. Replay: both `src` and `tests` diffs printed nothing
    (`SRC-IDENTICAL`, `TESTS-IDENTICAL`).
  - **Compiles:** not applicable.

- [x] **T015 Write the three-commit plan and check every header.**
  - **Files:** none (text only).
  - **Requirements:** FR-005, FR-006.
  - **Do:** one commit per group below; every header accepted by
    `speckit commit check --message`.
  - **Check:** each `speckit commit check --message` exits 0.

## Commit Plan

The owner commits; no task stages, commits or pushes.

| # | Header | Files |
| --- | --- | --- |
| 0 | `docs(arch): specify the resources subscribe lane removal` | `doc/arch/sdd/006-resources-subscribe-lane-removal/`, `doc/arch/adr/0006-resources-subscribe-lane-removal.md`, `doc/arch/schemas/resources-subscribe-lane-removal.cue`, `doc/arch/specs/features/resources-subscribe-lane-removal.feature` |
| 1 | `fix(subscription): stop opening a lane from resources/subscribe` | T001–T007 files: `src/ports/subscriber_registry.rs`, `src/adapters/subscription/memory_registry.rs`, `src/application/subscribe_resource.rs`, `src/application/unsubscribe_resource.rs`, `src/infra/mcp/resource_handlers.rs`, `src/composition/prod.rs` |
| 2 | `docs: record the resources/subscribe lane removal` | `CHANGELOG.md`, `docs/MIGRATION.md`, `docs/API.md`, `docs/OPERATIONS.md`, `docs/ARCHITECTURE.md`, `docs/LLM_GUIDE.md`, `docs/RESOURCES.md`, `README.md`, `doc/arch/functional/functional.md`, `doc/arch/runbooks/incident-push-delivery-lost.md`, `session.md` |

## Coverage

### FR to tasks

| FR | Tasks |
| --- | --- |
| FR-001 | T001, T004, T005, T007, T008, T014 |
| FR-002 | T001, T004, T006, T007, T014 |
| FR-003 | T001, T002, T003, T005, T014 |
| FR-004 | T001, T003, T014 |
| FR-005 | T001, T005, T007, T008, T011, T014 |
| FR-006 | T009, T010, T011, T014 |
| FR-007 | T012 |
| FR-008 | T012 |
| FR-009 | T013 |
| FR-010 | T011 |
| FR-011 | T001, T012, T014 |

### File to tasks

| File | Tasks |
| --- | --- |
| `src/ports/subscriber_registry.rs` | T002 |
| `src/adapters/subscription/memory_registry.rs` | T002, T007 |
| `src/application/subscribe_resource.rs` | T001, T003, T005 |
| `src/application/unsubscribe_resource.rs` | T001, T006 |
| `src/infra/mcp/resource_handlers.rs` | T005, T006 |
| `src/composition/prod.rs` | T004 |
| `doc/arch/functional/functional.md` | T008 |
| `doc/arch/runbooks/incident-push-delivery-lost.md` | T008 |
| `docs/LLM_GUIDE.md` | T009 |
| `docs/RESOURCES.md` | T009 |
| `docs/ARCHITECTURE.md` | T009 |
| `docs/API.md` | T009 |
| `docs/OPERATIONS.md` | T009 |
| `README.md` | T009 |
| `CHANGELOG.md` | T010 |
| `docs/MIGRATION.md` | T010 |
| `session.md` | T011 |

## Dependencies

- T002 needs T001; T003 needs T002; T004 needs T003; T005 needs T004; T006
  needs T005; T007 needs T006; T008 to T011 need T007; T012 needs T001 to
  T011; T013 and T014 need T012; T015 is text only.
- T005 and T006 each cover their use case, its handler side and the tests
  that read the removed symbols, because those cannot compile apart.
- No external system, credential or toolchain change is involved.
