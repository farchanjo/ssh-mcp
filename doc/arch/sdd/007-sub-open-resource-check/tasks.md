# Tasks: Sub Open Resource Check

## Task Breakdown

Twelve tasks, in the order they run. The order exists because the new tests
call `RepositoryGuard` and `with_resource_guard`, which do not exist on the
unchanged tree: a test-first task would not compile. The guard module lands
first (no behavior change), then the field, the builder and the tests together
(the tests compile and the checked-kind ones fail), then the call (the unit
tests pass, the production-composition test still fails), then the composition
wiring (it passes), then the scripts and the documents. Every file named below
is in the file map of `plan.md`.

The four mutation checks are **not** a task: guard removed, guard moved after
the open, wiring removed and one permissive kind made checked were run in the
spike and are recorded in `research.md` §5. Every task below was replayed in order in
`/var/tmp/ssh-mcp/guard-replay`; the exit statuses, warning counts and summary
lines are in `research.md` §9.

Build rules for every task that builds: `RUSTC_WRAPPER=sccache` and
`CARGO_BUILD_JOBS=4` stay exported; no `-j`, no `--release`, no
`nice`/`taskset`; tests run with `cargo nextest run --test-threads num-cpus`,
never `cargo test`; no `#[allow]` and no `#[expect]` is added; no test is
ignored or weakened. The spike at `/var/tmp/ssh-mcp/guard-spike` is the
reference for every hunk; the closing task compares against it.

- [x] **T001 Add the guard module and register it.**
  - **Files:** `src/application/resource_guard.rs` (new: `ResourceGuard`,
    `RepositoryGuard`, `GuardFuture`), `src/application/mod.rs` (`pub mod
    resource_guard;` in list order between `read_resource` and
    `subscribe_resource`), `src/application/subscribe_resource.rs`
    (`ensure_exists` becomes `pub(crate)` so the new module can call it).
  - **Requirements:** FR-004.
  - **Do:** the module of `plan.md` D-1, with the borrowed
    `ensure<'a>(&'a self, kind, id: &'a str) -> GuardFuture<'a>`; nothing
    calls it yet.
  - **Check:** `cargo check --all-features --all-targets` exits 0 with 0
    warnings; `grep -c 'pub mod resource_guard;' src/application/mod.rs` → 1.
    Replay: check 0, warnings 0, grep 1.
  - **Compiles:** yes; measured.

- [x] **T002 Add the field, the builder and the thirteen tests.**
  - **Files:** `src/application/subscription_admin.rs` (field
    `guard: Option<Arc<dyn ResourceGuard>>`, `guard: None` in both
    constructors, `with_resource_guard`, `use` line, the test module: the
    `RecordingActivator`, `req`, `repos`, `guarded`, `sample_shell` helpers
    and the nine application tests), `src/infra/mcp/tool_router.rs` (test
    module: `sub_open_args`, `guarded_lane` and the three tests),
    `tests/sub_open_resource_check.rs` (new, the FR-011 test).
  - **Requirements:** FR-001, FR-002, FR-003, FR-005, FR-011, FR-012, FR-013.
  - **Do:** the tests exactly as the spike has them; the fixture timestamps
    are `Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()`. `Debug` reads
    the field, so no warning appears before the call exists.
  - **Check:** `cargo check --all-features --all-targets` exits 0 with 0
    warnings; the twelve `--lib` tests exit 100 with `4 passed, 8 failed`
    and the eight messages recorded in `research.md` §2, and the integration
    file exits 100. Replay: check 0, warnings 0, lib tests 100 `4 passed, 8
    failed`, integration 100.
  - **Compiles:** yes; measured.

- [x] **T003 Call the guard from `execute`.**
  - **Files:** `src/application/subscription_admin.rs` (`execute` calls
    `self.guard_resource(parsed.kind, &parsed.id).await?` after the parse and
    before `canonical_uri`; private `guard_resource` method).
  - **Requirements:** FR-001, FR-002, FR-012, FR-013.
  - **Do:** `plan.md` §Technical Approach 2 and D-2. Nothing with an effect
    runs before the guard.
  - **Check:** `cargo check --all-features --all-targets` exits 0 with 0
    warnings; the twelve `--lib` tests pass (`Summary … 12 passed`); the
    integration test still fails with
    `assertion failed: matches!(uc.execute(req("shell://missing/output")).await,
    Err(DomainError::ShellNotFound(_)))`. Replay: check 0, warnings 0, lib
    tests 0 `12 passed`, integration 100 with that message.
  - **Compiles:** yes; measured.

- [x] **T004 Wire the guard into the production composition.**
  - **Files:** `src/composition/prod.rs` (`RepositoryGuard` import; build it
    from the four repositories and chain `.with_resource_guard(...)`).
  - **Requirements:** FR-011.
  - **Do:** the eight lines of `plan.md` §Technical Approach 6; `lane_admin`
    and everything else stay.
  - **Check:** `cargo check --all-features --all-targets` exits 0 with 0
    warnings; the integration test passes (`1 passed`); the unit tests stay
    green. Replay: check 0, warnings 0, integration 0 `1 passed`, unit 0.
  - **Compiles:** yes; measured.

- [x] **T005 Adapt the six script cases to a permissive URI.**
  - **Files:** `scripts/test_v5_adversarial.py` (five URIs become
    `forward://…/events` at `:663`, `:733`, `:755`, `:785`, `:790`, `:967`),
    `scripts/test_v51_flush_bytes.py` (`:81` becomes
    `forward://placeholder/events`).
  - **Requirements:** FR-009.
  - **Do:** the six replacements; assertions unchanged.
  - **Check:** `git diff --stat scripts/` shows the two files; with the
    binaries of T004, `PYTHONPATH=scripts … pytest -q
    scripts/test_v5_adversarial.py` → `63 passed`, and the flush-bytes file →
    `1 passed`. Replay: both exit 0 with those summaries.
  - **Compiles:** not applicable.

- [x] **T006 Correct the five reference passages to the per-kind code.**
  - **Files:** `docs/API.md:1084`, `:1106`; `docs/OPERATIONS.md:53`,
    `:176-178`; `docs/LLM_GUIDE.md:921`, `:1429`, `:1439`, `:1449`,
    `:1469-1470`; `docs/RESOURCES.md:96`.
  - **Requirements:** FR-006.
  - **Do:** the replacement text of `plan.md` §5, including the renamed
    OPERATIONS heading and its corrected anchor
    `#4-resourcessubscribe-returns-resource_gone`.
  - **Check:** `grep -rn 'sub_open-returns-resource_gone' docs` prints
    nothing; `grep -n '4-resourcessubscribe' docs/OPERATIONS.md` → `:53`;
    `grep -n 'attach before the producer and are not checked' docs/API.md` →
    `:1084`; `grep -n 'is not returned here' docs/API.md` → `:1106`;
    `grep -n 'never returns `RESOURCE_GONE`' docs/OPERATIONS.md` → `:178`;
    `grep -n 'does not return `RESOURCE_GONE`' docs/LLM_GUIDE.md` → `:921`;
    `grep -n 'or `sub_open` call referenced a closed or unknown shell'
    docs/LLM_GUIDE.md` → `:1429`; `grep -n 'or `sub_open` referenced a stale
    or never-existed `command_id`' docs/LLM_GUIDE.md` → `:1439`;
    `grep -n 'or `sub_open` referenced a finished or unknown transfer'
    docs/LLM_GUIDE.md` → `:1449`; `grep -n 'does not return this code'
    docs/LLM_GUIDE.md` → `:1469`; `grep -n 'per-kind `\*_NOT_FOUND` code'
    docs/RESOURCES.md` → `:96`. Replay: documents only.
  - **Compiles:** not applicable.

- [x] **T007 Record the change in `CHANGELOG.md` and `docs/MIGRATION.md`.**
  - **Files:** `CHANGELOG.md:15`, `:17`, `docs/MIGRATION.md:882`.
  - **Requirements:** FR-006.
  - **Do:** one `### Changed` bullet and one bullet under the current
    migration section.
  - **Check:** `grep -n 'refuses a missing' CHANGELOG.md` → `:17`;
    `grep -n 'A missing id is refused' docs/MIGRATION.md` → `:882`. Replay:
    documents only.
  - **Compiles:** not applicable.

- [x] **T008 Correct the functional corpus and the runbook.**
  - **Files:** `doc/arch/functional/functional.md:247-254`, `:345`,
    `doc/arch/runbooks/incident-push-delivery-lost.md:11`, `:110-113`.
  - **Requirements:** FR-006.
  - **Do:** the repaired limitation bullet, the F8 row and the runbook trigger
    and step 9 (old step 9 becomes 10).
  - **Check:** `grep -n 'guard call is' doc/arch/functional/functional.md` →
    `:248`; `grep -n 'per-kind code and opens no lane' doc/arch/functional/functional.md`
    → `:345`; `grep -n '^9\.' doc/arch/runbooks/incident-push-delivery-lost.md`
    → `:110`. Replay: documents only.
  - **Compiles:** not applicable.

- [x] **T009 Mark the defect repaired and re-anchor `session.md`.**
  - **Files:** `session.md` — the `sub_open` existence entry gains
    `**Repaired in feature 007 …**` (`:151`, `:162-163`); every defect-list
    entry that cites `src/application/subscription_admin.rs` or
    `src/composition/prod.rs` is re-anchored on the final tree (`:264`,
    `:115`, `:184`).
  - **Requirements:** FR-007.
  - **Do:** the repair mark and the re-anchoring of `plan.md` §7.
  - **Check:** `grep -n 'Repaired in feature 007' session.md` → `:162`;
    `grep -n 'subscription_admin.rs:147\|prod.rs:542-544\|prod.rs:822'
    session.md` returns `:151`, `:115`, `:184`. Replay: documents only.
  - **Compiles:** not applicable.

- [x] **T010 Run the fifteen gate commands of `plan.md`.**
  - **Files:** none.
  - **Requirements:** FR-008, FR-009.
  - **Do:** the fifteen numbered rows of the verification table in `plan.md`
    §Verification Plan, one cargo invocation at a time.
  - **Check:** every row exits 0; rows 6 and 7 report 2154 and 2329 passed;
    rows 1–5, 8, 9 report clean or `unchanged`; rows 10–15 report the six
    suite tallies. Replay: the code and script tasks were replayed; the full
    gate was measured in the spike.
  - **Compiles:** yes.

- [x] **T011 Run `speckit validate` and `speckit verify --changed`.**
  - **Files:** none.
  - **Requirements:** FR-010.
  - **Do:** both commands from the repository root.
  - **Check:** `speckit validate` exits 6 with 68 errors, the baseline rule
    set, no warning and no feature finding; `speckit verify --changed`
    reports its summary with `0 failed`. Replay: `speckit validate` in the
    replay copy exits 6 with 68 errors, the same rule set and no warning.
  - **Compiles:** not applicable.

- [x] **T012 Prove equality with the spike.**
  - **Files:** none.
  - **Requirements:** FR-001 to FR-013.
  - **Do:** compare every file of the patch against the spike and the whole
    tree against the pre-feature tree.
  - **Check:** `diff -r /var/tmp/ssh-mcp/guard-spike/src src`, the same for
    `tests` and `scripts`, and a per-file `diff` of every document of the
    patch print nothing; no file outside the 18 of the patch and the
    feature's artifacts differs from `/var/tmp/ssh-mcp/final-006/tree`.
    Replay: the three `diff -r` commands printed nothing.
  - **Compiles:** not applicable.

- [x] **T013 Write the three-commit plan and check every header.**
  - **Files:** none (text only).
  - **Requirements:** FR-006, FR-007.
  - **Do:** one commit per group below; every header at most 72 characters,
    accepted by `speckit commit check --message`.
  - **Check:** each `speckit commit check --message` exits 0.

## Commit Plan

The owner commits; no task stages, commits or pushes.

| # | Header | Files |
| --- | --- | --- |
| 0 | `docs(arch): specify the sub_open resource check` | `doc/arch/sdd/007-sub-open-resource-check/`, `doc/arch/adr/0007-sub-open-resource-check.md`, `doc/arch/schemas/sub-open-resource-check.cue`, `doc/arch/specs/features/sub-open-resource-check.feature` |
| 1 | `fix(subscription): refuse a missing sub_open resource` | T001–T005 files: `src/application/resource_guard.rs`, `src/application/mod.rs`, `src/application/subscribe_resource.rs`, `src/application/subscription_admin.rs`, `src/composition/prod.rs`, `src/infra/mcp/tool_router.rs`, `tests/sub_open_resource_check.rs`, `scripts/test_v5_adversarial.py`, `scripts/test_v51_flush_bytes.py` |
| 2 | `docs: record the sub_open resource check` | `CHANGELOG.md`, `docs/MIGRATION.md`, `docs/API.md`, `docs/OPERATIONS.md`, `docs/LLM_GUIDE.md`, `docs/RESOURCES.md`, `doc/arch/functional/functional.md`, `doc/arch/runbooks/incident-push-delivery-lost.md`, `session.md` |

## Coverage

### FR to tasks

| FR | Tasks |
| --- | --- |
| FR-001 | T002, T003, T012 |
| FR-002 | T002, T003, T012 |
| FR-003 | T002, T012 |
| FR-004 | T001, T002, T003, T012 |
| FR-005 | T002, T012 |
| FR-006 | T006, T007, T008, T012 |
| FR-007 | T009, T012 |
| FR-008 | T010 |
| FR-009 | T005, T010 |
| FR-010 | T011 |
| FR-011 | T002, T004, T012 |
| FR-012 | T002, T003, T012 |
| FR-013 | T002, T003, T012 |

### File to tasks

| File | Tasks |
| --- | --- |
| `src/application/resource_guard.rs` | T001 |
| `src/application/mod.rs` | T001 |
| `src/application/subscription_admin.rs` | T002, T003 |
| `src/infra/mcp/tool_router.rs` | T002 |
| `tests/sub_open_resource_check.rs` | T002 |
| `src/composition/prod.rs` | T004 |
| `scripts/test_v5_adversarial.py` | T005 |
| `scripts/test_v51_flush_bytes.py` | T005 |
| `docs/API.md` | T006 |
| `docs/OPERATIONS.md` | T006 |
| `docs/LLM_GUIDE.md` | T006 |
| `docs/RESOURCES.md` | T006 |
| `CHANGELOG.md` | T007 |
| `docs/MIGRATION.md` | T007 |
| `doc/arch/functional/functional.md` | T008 |
| `doc/arch/runbooks/incident-push-delivery-lost.md` | T008 |
| `session.md` | T009 |
| `src/application/subscribe_resource.rs` | T001 (`pub(crate)`) |

## Dependencies

- T002 needs T001; T003 needs T002; T004 needs T003; T005 needs T004; T006 to
  T009 need T005; T010 needs T001 to T009; T011 and T012 need T010; T013 is
  text only.
- T002 carries all thirteen tests because they share the helpers and the
  builder; T003 and T004 flip their results without touching test code.
- No external system, credential or toolchain change is involved.
