# Tasks: Subscription Pause Delivery

## Task Breakdown

Eleven tasks, in the order they run. The pinning tests and the two port
signatures that let them compile come first, so the tree is green before any
gate lands; the four gates and their tests follow; the loom comment, the
records, the gate and the commit plan close.

Every file named below is in the file map of `plan.md`. Build rules for every
task that builds: `RUSTC_WRAPPER=sccache` and `CARGO_BUILD_JOBS=4` stay
exported; no `-j`, no `--release`, no `nice`/`taskset`; tests run with `cargo
nextest run --test-threads num-cpus`, never `cargo test`; no `#[allow]` and no
`#[expect]` is added; no test is ignored, deleted or weakened.

- [x] **T001 Pin the resume outcome, the release and the residual, with the
  two port signatures they need.**
  - **Files:** `src/ports/subscriber_lane.rs` (`resume` and `resume_lane`
    return `bool`), `src/ports/notifier.rs` (`notify_lane`), the `lane_by_id`
    locator and the `resume_lane` body in
    `src/adapters/subscription/subscriber_lane.rs`, the `ResumeSubUseCase`
    field and `notify_lane` call in `src/application/subscription_admin.rs`,
    the `resume_notifier` in `src/composition/prod.rs`, the constructor in
    `tests/v5_smoke.rs`, and the six pinning tests in the
    `src/adapters/subscription/lane_bridge.rs` and
    `src/adapters/subscription/memory_registry.rs` test modules:
    `adapter_and_bridge_are_released_after_drop`,
    `resume_of_a_running_lane_sends_no_extra_notification`,
    `resume_after_a_quiet_pause_sends_exactly_one_notification`,
    `pause_unknown_lane_returns_sub_not_found`,
    `resume_of_a_lane_closed_while_paused_returns_sub_not_found`,
    `resources_subscribe_lane_is_listed_but_pause_does_not_gate_the_peer_fanout`,
    and the three use-case tests in the
    `src/application/subscription_admin.rs` test module:
    `resume_of_a_paused_lane_notifies_once_with_its_sub_id`,
    `resume_of_a_running_lane_does_not_notify`,
    `resume_unknown_does_not_notify`.
  - **Requirements:** FR-002, FR-005, FR-011.
  - **Do:** as `plan.md` §Technical Approach 2 and D-1. `LaneAdmin::resume`
    returns `true` only on a true-to-false swap;
    `LaneNotifierBridge::notify_lane` is a no-op default implemented by the
    bridge through `ship_legacy_notify`; `ResumeSubUseCase` calls it on
    `true`. No gate is added yet.
  - **Check:** a nine-test filter over the pinning tests of `research.md`
    §7 → 9 passed, exit 0; `cargo check --all-features --all-targets`
    exits 0.
  - **Compiles:** yes.

- [x] **T002 Gate the four delivery sites and add the seven gate tests.**
  - **Files:** `src/adapters/subscription/lane_bridge.rs` — the `Acquire`
    loads in `prepare_inline_dispatch`, `notify_one_lane`,
    `notify_one_lane_inline_only` and `ship_one`; the tests
    `paused_lane_receives_no_delivery_until_resume` (with its final
    `resource_events` assertion adapted from 1 to 2),
    `pause_does_not_gate_another_lane_on_the_same_uri`,
    `paused_inline_lane_keeps_cursor_and_seq_monotonic_across_the_gap`,
    `paused_lane_counters_do_not_advance` (events_sent 2 after resume),
    `paused_lane_inline_only_path_is_gated`,
    `prepare_inline_dispatch_skips_a_paused_lane`,
    `queued_inline_dispatch_is_dropped_when_the_lane_is_paused`.
  - **Requirements:** FR-001, FR-003, FR-004, FR-006.
  - **Do:** as `plan.md` §Technical Approach 1 and 3. The gate reads the lane
    after the composition; nothing else moves.
  - **Check:** the thirteen-test filter of `research.md` §7 → 13 passed,
    exit 0. Each gate is proven by the removal table of `research.md` §5a.
  - **Compiles:** yes.

- [x] **T003 Name the gate in the loom invariant comment.**
  - **Files:** `tests/lockfree_invariants.rs:699-703`.
  - **Requirements:** FR-008.
  - **Do:** replace "production code uses `Notify::notified().await`" with the
    gate wording of `plan.md`; no assertion changes.
  - **Check:** `cargo fmt --all -- --check` exits 0.

- [x] **T004 Record the repair in `CHANGELOG.md`.**
  - **Files:** `CHANGELOG.md:24`.
  - **Requirements:** FR-009.
  - **Do:** add the `### Fixed` bullet of `plan.md`.
  - **Check:** `grep -n 'now stops delivery' CHANGELOG.md` returns
    `:24`.

- [x] **T005 Record the behaviour in `docs/MIGRATION.md`.**
  - **Files:** `docs/MIGRATION.md:641`.
  - **Requirements:** FR-009.
  - **Do:** add the "Pause delivery." paragraph, including the counted
    catch-up, the cursor read and the peerless residual. Rows `:631-632`
    stay as written (v5.0 notes).
  - **Check:** `grep -n 'Pause delivery' docs/MIGRATION.md` returns `:641`;
    `grep -n 'drain loop' docs/MIGRATION.md` returns only `:631` and `:632`.

- [x] **T006 Correct `docs/API.md`.**
  - **Files:** `docs/API.md:1126`, `:1138`.
  - **Requirements:** FR-009, FR-011.
  - **Do:** replace both false sentences and keep the two added paragraphs
    (the `sub_open`-only gate and the inline cursor read).
  - **Check:** `grep -n 'drain loop' docs/API.md` prints nothing;
    `grep -n 'Gates the lanes opened by .sub_open.' docs/API.md` returns
    `:1128`.

- [x] **T007 Correct `docs/OPERATIONS.md`.**
  - **Files:** `docs/OPERATIONS.md:79`, `:261`.
  - **Requirements:** FR-009.
  - **Do:** the two wording replacements of `plan.md`.
  - **Check:** `grep -n 'drain loop' docs/OPERATIONS.md` prints nothing.

- [x] **T008 Correct `docs/ARCHITECTURE.md`.**
  - **Files:** `docs/ARCHITECTURE.md:376`.
  - **Requirements:** FR-009.
  - **Do:** "suspend drain" → "suspend delivery".
  - **Check:** `grep -n 'suspend delivery without disconnecting' docs/ARCHITECTURE.md`
    returns `:376`.

- [x] **T009 Record the outcome and the residual in `session.md`.**
  - **Files:** `session.md:108-115`, `:171-176`.
  - **Requirements:** FR-009, FR-011.
  - **Do:** mark the defect repaired with feature 005, add the residual
    defect entry, and extend the open item with the four untrue sentences and
    `NOOP`.
  - **Check:** `grep -n 'Repaired in feature 005' session.md` returns `:109`;
    `grep -n 'does not gate' session.md` returns `:115`.

- [x] **T009A Reconcile the standing corpus.**
  - **Files:** `doc/arch/functional/functional.md:220-236`,
    `doc/arch/runbooks/incident-push-delivery-lost.md:101-111`.
  - **Requirements:** FR-009, FR-011.
  - **Do:** replace the "no delivery path reads the flag" limitation with the
    current behaviour and the open residual; make step 8 say the cure works
    for a `sub_open` lane and keep the workaround for the residual only.
  - **Check:** `grep -n 'no delivery path' doc/arch/functional/functional.md
    doc/arch/runbooks/incident-push-delivery-lost.md` prints nothing.

- [x] **T010 Run the gate.**
  - **Files:** none.
  - **Requirements:** FR-007, FR-008, FR-010.
  - **Do:** run the numbered verification plan of `plan.md` (18 commands).
  - **Check:** every command exits 0 with the expected result; 2141 lib and
    2315 integration tests passed; `make tools-export-check` reports both
    `unchanged`; `cargo audit` reports the `chrono` warning only;
    `test_transfer_progress.py` reports 2 passed.

- [x] **T011 Prepare the commit plan.**
  - **Files:** none (text only).
  - **Requirements:** FR-009.
  - **Do:** one commit per task group below; every header accepted by
    `speckit commit check --message`.
  - **Check:** each `speckit commit check --message` exits 0.

## Commit Plan

The owner commits; no task stages, commits or pushes. Each row is one commit,
its files, and the state at that commit: the tree compiles and the gate holds.

| # | Header | Files |
| --- | --- | --- |
| 0 | `chore(speckit): regenerate AGENTS.md for speckit 0.49` | `AGENTS.md` |
| 1 | `docs(arch): specify the subscription pause delivery` | `doc/arch/sdd/005-subscription-pause-delivery/`, `doc/arch/adr/0005-subscription-pause-delivery.md`, `doc/arch/schemas/subscription-pause-delivery.cue`, `doc/arch/specs/features/subscription-pause-delivery.feature` |
| 2 | `fix(subscription): gate lane delivery on sub_pause` | T001, T002 and T003 files: the two ports, the adapter, the use case, `prod.rs`, `v5_smoke.rs`, `lane_bridge.rs`, `memory_registry.rs`, `tests/lockfree_invariants.rs` |
| 3 | `docs: record the sub_pause repair` | `CHANGELOG.md`, `docs/MIGRATION.md`, `docs/API.md`, `docs/OPERATIONS.md`, `docs/ARCHITECTURE.md`, `session.md` |

Files that already carry uncommitted hunks of features 001 to 004, and the
lines that are this feature's:

- `CHANGELOG.md` — the new `### Fixed` bullet at `:24` only.
- `session.md` — `:108-115` (repair mark and residual entry) and `:171-176`
  (open-item extension) only.
- `docs/API.md` — `:1126-1141` only.
- `docs/OPERATIONS.md` — `:79` and `:261` only.
- `docs/ARCHITECTURE.md` — `:376` only.
- `docs/MIGRATION.md` — `:641` only.

## Dependencies

- Commit 0 precedes everything: `AGENTS.md` is regenerated for the binary in
  use (`v0.49.0`) and the baseline of FR-010 is measured after it. Commit 1
  precedes the code, because `doc/arch` is the source of truth (D-01).
- T002 needs T001; T003 needs T002; T004 to T009 need T002; T010 needs
  T001 to T009.
- No external system, no credential and no toolchain change is involved.
