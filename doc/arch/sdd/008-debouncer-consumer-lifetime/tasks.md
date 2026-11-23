# Tasks: Debouncer Consumer Lifetime

## Task Breakdown

Eight tasks, in the order they run. The production change and its tests come
from the measured spike at `/var/tmp/ssh-mcp/deb-spike`; the reference patch is
`/var/tmp/ssh-mcp/deb-spike.patch`, SHA-256
`8737f2c57e4538ffed147550d6ff0d0f21e9353adc250ead77def92d2e1ecc4f`.
Every file named below is in the file map of `plan.md`.

Build rules for every task that builds: `RUSTC_WRAPPER=sccache` and
`CARGO_BUILD_JOBS=4` stay exported; no `-j`, no `--release`, no
`nice`/`taskset`; tests run with `cargo nextest run --test-threads num-cpus`,
never `cargo test`; no test is ignored, removed or weakened. The standard test
module allowance in `plan.md` §5 is the only new lint allowance.

- [x] **T001 Extend the lane close signal with the removed lane URI.**
  - **Files:** `src/adapters/subscription/subscriber_lane.rs`,
    `src/composition/prod.rs` (the existing mux close closure only).
  - **Requirements:** FR-002, FR-004.
  - **Do:** make `remove_lane` return the removed `Arc<LaneState>`, extend
    `CloseSink` and `fire_close_sink` with the URI, fire only after a successful
    removal, and adapt the existing mux closure without changing its behavior.
  - **Check:** `cargo check --all-features --all-targets` exits 0 with 0
    warnings; the existing lane-close tests stay green.

- [x] **T002 Make the debouncer follow both consumer kinds atomically.**
  - **Files:** `src/adapters/subscription/memory_registry.rs`,
    `src/adapters/subscription/lane_bridge.rs`,
    `src/adapters/subscription/fake.rs` (new),
    `src/adapters/subscription/mod.rs`.
  - **Requirements:** FR-001, FR-002, FR-003, FR-004, FR-005, FR-011, FR-012,
    FR-013, FR-014.
  - **Do:** add the weak lane-presence query, `consumers_empty`,
    `release_if_idle_uri`, the single `remove_debouncer`, the entry-guarded
    ensure and the pre-spawn consumer check; add `install_lane_lifetime`, the
    shared gated `StubNotifier`, and the seventeen registry tests of `plan.md`
    §6. Remove wakeup maps before the task entry, under the same entry guard.
  - **Check:** the seventeen registry tests pass; `concurrent_ensure_spawns_one_task`
    reports one task; `adapter_and_bridge_are_released_after_drop` stays green;
    `cargo check --all-features --all-targets` exits 0 with 0 warnings.

- [x] **T003 Install the lifetime link in the production composition.**
  - **Files:** `src/composition/prod.rs`.
  - **Requirements:** FR-005, FR-006.
  - **Do:** call `install_lane_lifetime`, compose the mux unregister through
    its close callback, split `build_use_cases_inner` for the private test
    handle, and add
    `production_wiring_stops_the_debouncer_when_the_last_lane_closes`.
  - **Check:** the production-composition test passes;
    `registry_is_released_after_the_last_consumer_leaves` stays green.

- [x] **T004 Prove both disconnect cascades release child debouncers.**
  - **Files:** `src/application/disconnect_session.rs`,
    `src/application/disconnect_agent.rs`.
  - **Requirements:** FR-002.
  - **Do:** add `session_cascade_stops_child_debouncers_and_spares_the_health_one`
    and `agent_cascade_stops_child_debouncers_and_spares_the_health_one` exactly
    as measured in the spike.
  - **Check:** both tests pass and each leaves the health-lane debouncer alive.

- [x] **T005 Update the lifetime documentation and defect records.**
  - **Files:** `docs/OPERATIONS.md`, `CHANGELOG.md`, `session.md`.
  - **Requirements:** FR-007, FR-010.
  - **Do:** replace the peer-only stop statement with the last-consumer rule,
    add the single `### Changed` bullet, mark the measured debouncer defects
    repaired, and re-anchor every citation into a changed file on the final
    tree. Do not change the tool catalogues.
  - **Check:** the documents match the spike; `git diff --stat
    docs/TOOL_CATALOG.md docs/tool-catalog.json` is empty.

- [x] **T006 Run the complete feature gate.**
  - **Files:** none.
  - **Requirements:** FR-007, FR-008, FR-011.
  - **Do:** run every command of `plan.md` §Verification Plan, one cargo
    invocation at a time, including the catalogue check, all seven Python
    suites and `cargo audit`.
  - **Check:** every command exits 0; nextest reports at least 2174 lib tests
    and 2349 tests with integration; the adversarial and flush-bytes scripts
    report 63 and 1 passed; both catalogue files are `unchanged`.

- [x] **T007 Validate the corpus and verify the changed scenarios.**
  - **Files:** none.
  - **Requirements:** FR-009.
  - **Do:** run `speckit validate` and `speckit verify --changed` from the
    repository root.
  - **Check:** validation has the FR-009 landing baseline of 63 errors, the
    reduced rule set, no warning and no finding on a feature-008 artifact or `session.md`;
    changed-scenario verification reports `0 failed`.

- [x] **T008 Prove that the delivered tree matches the measured spike.**
  - **Files:** none.
  - **Requirements:** FR-001 to FR-014.
  - **Do:** compare every file carried by `deb-spike.patch` with its counterpart
    in `/var/tmp/ssh-mcp/deb-spike`; check `git diff --check`; confirm that no
    file outside the patch and feature artifacts changed during implementation.
  - **Check:** every comparison is empty and the patch checksum remains the
    value recorded above.

## Commit Plan

The owner commits; no task stages, commits or pushes.

| # | Header | Files |
| --- | --- | --- |
| 0 | `docs(arch): specify the debouncer consumer lifetime` | `doc/arch/sdd/008-debouncer-consumer-lifetime/`, `doc/arch/adr/0008-debouncer-consumer-lifetime.md`, `doc/arch/schemas/debouncer-consumer-lifetime.cue`, `doc/arch/specs/features/debouncer-consumer-lifetime.feature` |
| 1 | `fix(subscription): tie debouncers to every consumer` | T001–T004 source files |
| 2 | `docs: record the debouncer consumer lifetime` | `docs/OPERATIONS.md`, `CHANGELOG.md`, `session.md` |

## Coverage

| FR | Tasks |
| --- | --- |
| FR-001 | T002, T006, T008 |
| FR-002 | T001, T002, T003, T004, T006, T008 |
| FR-003 | T002, T006, T008 |
| FR-004 | T001, T002, T006, T008 |
| FR-005 | T002, T003, T006, T008 |
| FR-006 | T003, T006, T008 |
| FR-007 | T005, T006, T008 |
| FR-008 | T006 |
| FR-009 | T007 |
| FR-010 | T005, T008 |
| FR-011 | T002, T006, T008 |
| FR-012 | T002, T006, T008 |
| FR-013 | T002, T006, T008 |
| FR-014 | T002, T006, T008 |

## Dependencies

- T002 needs T001; T003 needs T002; T004 needs T003; T005 needs T004; T006
  needs T001 to T005; T007 and T008 need T006.
- The nine mutation checks and stability runs are already recorded in
  `research.md` §5 and §7; they are evidence, not additional tasks.
- No external system, credential or toolchain change is involved.
