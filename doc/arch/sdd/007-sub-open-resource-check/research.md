# Research: Sub Open Resource Check

Evidence for `plan.md`. Every measurement was taken on 2026-11-22 in
`/var/tmp/ssh-mcp/guard-spike` with `RUSTC_WRAPPER=sccache` and
`CARGO_BUILD_JOBS=4` as exported, one cargo invocation at a time, no `-j`, no
`--release`, no `nice` and no `taskset`. `df -h /tmp /var/tmp` was read before
the first build and after the last; the spike's `target` was deleted when the
results were recorded.

## 1. Setup

```
rm -rf /var/tmp/ssh-mcp/guard-spike
rsync -a --exclude target --exclude .git --exclude .venv \
  /root/dev/ssh-mcp/ /var/tmp/ssh-mcp/guard-spike/
```

The new tests were added first to the unchanged tree and run there (§2). Then
the production change, the script adaptations and the document changes were
made (§3, §5, §6). The repository's Rust files do not change in this phase.

## 2. The new tests on the unchanged tree

Command, run twice (the nine application tests, then the two tool-router
tests and the integration file):

```
cargo nextest run --lib --test-threads num-cpus --no-fail-fast \
  'application::subscription_admin::tests::subscribe_unknown_' \
  'application::subscription_admin::tests::refused_' \
  'application::subscription_admin::tests::forward_serial' \
  'application::subscription_admin::tests::completed_'
cargo nextest run --lib --test-threads num-cpus --no-fail-fast \
  'infra::mcp::tool_router::tests::sub_open_refusal'
cargo nextest run --test sub_open_resource_check --test-threads num-cpus
```

First: exit 100, `Summary [0.011s] 9 tests run: 3 passed, 6 failed, 2142
skipped` (`logs/guard-spike-base-1.log`). Second: exit 100, `Summary [0.017s]
2 tests run: 1 passed, 1 failed, 2151 skipped`
(`logs/guard-spike-base-2.log`). Third: exit 100, `Summary [0.006s] 1 test
run: 0 passed, 1 failed, 0 skipped` (`logs/guard-spike-base-3.log`).

| Test | Base | Message / what it pins |
| --- | --- | --- |
| `subscribe_unknown_shell_is_refused` | FAIL | `called Result::unwrap_err() on an Ok value: SubscribeOutcome { … }` |
| `subscribe_unknown_command_is_refused` | FAIL | same |
| `subscribe_unknown_transfer_is_refused` | FAIL | same |
| `subscribe_unknown_session_is_refused` | FAIL | same |
| `refused_sub_open_leaves_no_lane_and_no_debouncer` | FAIL | `assertion failed: uc.execute(req("shell://ghost/output")).await.is_err()` |
| `refused_then_created_resource_can_be_subscribed` | FAIL | `assertion failed: uc.execute(req("shell://later/output")).await.is_err()` |
| `forward_serial_and_rsync_stay_permissive` | PASS | three permissive kinds open; pins behavior that stays |
| `completed_command_row_is_accepted` | PASS | a terminal command row is accepted; pins FR-012 |
| `completed_transfer_row_is_accepted` | PASS | a terminal transfer row is accepted; pins FR-012 |
| `sub_open_refusal_answers_per_kind_code` | FAIL | the helper returns `Ok` on the base |
| `sub_open_refusal_renders_per_kind_code` | PASS | the four codes already render; pins the mapping |
| `production_sub_subscribe_refuses_missing_resources_and_opens_forward` | FAIL | `assertion failed: matches!(uc.execute(req("shell://missing/output")).await, Err(DomainError::ShellNotFound(_)))` |

The FR-013 test needs the new builder, so it was added with the implementation
and passes there (§3).

After the change all thirteen pass: `Summary [0.008s] 12 tests run: 12
passed` on `--lib` filters and `Summary [0.005s] 1 test run: 1 passed` on the
integration file.

## 3. Production change

`/var/tmp/ssh-mcp/guard-spike.patch` holds the full delta. The production
hunks:

- `src/application/resource_guard.rs` (new) — `ResourceGuard`,
  `RepositoryGuard`, `GuardFuture`.
- `src/application/mod.rs` — `pub mod resource_guard;`.
- `src/application/subscribe_resource.rs` — `ensure_exists` becomes
  `pub(crate)`.
- `src/application/subscription_admin.rs` — `guard` field, `new`/`with_activator`
  initializers, `with_resource_guard`, `ensure_exists` private method, one call
  in `execute`.
- `src/composition/prod.rs` — build `RepositoryGuard` from the four
  repositories and install it.

No existing call site changes. The private method keeps `execute` under the
30-line clippy ceiling (`too_many_lines` was hit at 31 and fixed by the
extraction).

## 4. Gate

| Command | Exit | Result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | clean |
| `cargo check --all-features --all-targets` | 0 | clean |
| `cargo clippy --all-features -- -D warnings` | 0 | clean |
| `cargo build --no-default-features --target-dir target/no-default-features` | 0 | clean |
| `cargo build --bins` | 0 | clean |
| `cargo nextest run --test-threads num-cpus --lib` | 0 | `Summary [5.448s] 2154 tests run: 2154 passed, 0 skipped` |
| `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` | 0 | `Summary [25.661s] 2329 tests run: 2329 passed, 0 skipped` |
| `cargo audit` | 0 | `warning: 1 allowed warning found` (`chrono` unmaintained) |
| `make tools-export-check` | 0 | `unchanged: docs/TOOL_CATALOG.md`; `unchanged: docs/tool-catalog.json` |

Python suites, from the spike with the three `SSH_MCP_*_BIN` variables on its
`target/debug` binaries:

| Suite | Exit | Result |
| --- | --- | --- |
| `scripts/test_http.py` | 0 | `15 passed in 210.20s` |
| `scripts/test_transfer_progress.py` | 0 | `1 passed, 1 skipped in 2.12s` (the suite's fast-machine skip) |
| `scripts/test_v7_rsync_http.py` | 0 | `10 passed in 145.75s` |
| `scripts/test_v47_subscriptions.py` | 0 | `20 passed in 39.21s` |
| `scripts/test_v47_idempotency.py` | 0 | `9 passed in 7.80s` |
| `scripts/test_v5_adversarial.py` (adapted) | 0 | `63 passed in 47.74s` |
| `scripts/test_v51_flush_bytes.py` (adapted) | 0 | `1 passed in 0.37s` |

Totals: 2142 + 12 lib tests = 2154; 2316 + 13 with the integration file =
2329. The twelve lib tests are four FR-001, two FR-002, one FR-003, two
FR-005, one FR-013 and two FR-012. `scripts/chaos_v5_subscribe.py` is not a gate
command (already broken, `session.md`).

## 5. Mutation checks

Each was applied, the named test run, then the files restored.

| Mutation | Test | Result |
| --- | --- | --- |
| remove `self.ensure_exists(...)` from `execute` | four `subscribe_unknown_*` | FAIL, `called Result::unwrap_err() on an Ok value: SubscribeOutcome { … }`; `4 tests run: 0 passed, 4 failed` (`guard-spike-mut-a.log`) |
| move the guard after `LaneAdmin::open` | `refused_sub_open_leaves_no_lane_and_no_debouncer` | FAIL, `assertion failed: adapter.list_subs().is_empty()` (`guard-spike-mut-b.log`) |
| remove `.with_resource_guard(...)` from `src/composition/prod.rs` | unit `subscribe_unknown_shell_is_refused` / integration `production_sub_subscribe_refuses_missing_resources_and_opens_forward` | unit PASS (`1 passed`); integration FAIL, `assertion failed: matches!(uc.execute(req("shell://missing/output")).await, Err(DomainError::ShellNotFound(_)))` (`guard-spike-mut-c-unit.log`, `guard-spike-mut-c-int.log`) |
| make `forward` a checked kind (`ensure_shell` on the forward arm) | `forward_serial_and_rsync_stay_permissive` | FAIL, `called Result::unwrap() on an Err value: ShellNotFound(ShellId("f"))` (`guard-spike-mut-d.log`) |

## 6. Validate in the spike

`SPECKIT_PROJECT_ROOT=/var/tmp/ssh-mcp/guard-spike speckit validate` exits 6
with 68 errors and no warning, the same rule set as the repository baseline
(46 `fqcn.relative-super`, 6 `branch.else`, 5 `pin.ci.unpinned-action`, 2
`pin.docker.unpinned-image`, one each of the remaining nine). No finding names
a file of the feature or `session.md`. Log
`/var/tmp/ssh-mcp/logs/guard-spike-validate.log`. The change removes no
baseline finding and adds none.

## 7. Patch

`/var/tmp/ssh-mcp/guard-spike.patch` is generated from the pre-feature tree
`/var/tmp/ssh-mcp/final-006/tree` to the spike in `a/<path>` / `b/<path>`
form. Excluded: the feature's own artifacts (`doc/arch/sdd/007-…`, ADR 0007,
the schema, the feature file), `Cargo.lock`, `target`, `.venv`, `.git`,
caches, `dist`, the release tarball and databases. The differing-file list
minus those exclusions equals the patch file list (`diff` prints nothing).

```
18 files changed, 752 insertions(+), 42 deletions(-)
```

SHA-256 `7145a0581f5050043ee9308a8984d540acff6759d25ebdc4533cb0a56b172360`.
`patch -p1 --dry-run` in a scratch copy of `final-006/tree` under
`/var/tmp/ssh-mcp/guard-check` exits 0; the scratch copy is deleted
afterwards. Log: `/var/tmp/ssh-mcp/logs/guard-spike-patch-dryrun.log`.

## 8. Cleanup

`/var/tmp/ssh-mcp/guard-spike/target` deleted after the last build;
`/var/tmp/ssh-mcp/guard-probe` deleted entirely. `/var/tmp/ssh-mcp/sub-repro.patch`
and the logs stay.

## 9. Order replay

`tasks.md` was replayed in `/var/tmp/ssh-mcp/guard-replay`, a copy of the
repository tree, one task at a time up to T005. Every task ran
`cargo check --all-features --all-targets` (exit and warning count) and its own
check command. The replay directory was deleted after the table was recorded.

| Task | Applied | `cargo check` | Task check and result |
| --- | --- | --- | --- |
| T001 | guard module, `pub mod resource_guard;`, `pub(crate) ensure_exists` | 0, warnings 0 | `grep -c 'pub mod resource_guard;'` → 1 |
| T002 | field, builder, twelve lib tests, integration file | 0, warnings 0 | lib tests 100, `4 passed, 8 failed`; integration 100, `assertion failed: matches!(…)`; messages in `guard-replay-t002-tests.log` |
| T003 | `guard_resource` call and method | 0, warnings 0 | lib tests 0, `12 passed`; integration 100, same assertion |
| T004 | composition wiring | 0, warnings 0 | integration 0, `1 passed`; unit 0, `1 passed` |
| T005 | six script URI replacements | build 0 | adversarial 0, `63 passed`; flush-bytes 0, `1 passed` |

Closing equality: `diff -r …/guard-spike/src src` and the same for `tests`
printed nothing; `diff -r --exclude __pycache__ …/guard-spike/scripts scripts`
printed nothing (`SRC-IDENTICAL`, `TESTS-IDENTICAL`, `SCRIPTS-IDENTICAL`).
`SPECKIT_PROJECT_ROOT=/var/tmp/ssh-mcp/guard-replay speckit validate` exited 6
with 68 errors, the baseline rule set and no warning.

## 10. Implement phase in the repository

Tasks T001 to T013 were replayed in `/root/dev/ssh-mcp` on branch
`007-sub-open-resource-check`, one task at a time, with `RUSTC_WRAPPER=sccache`
and `CARGO_BUILD_JOBS=4` as exported and one cargo invocation at a time. Logs
are under `/var/tmp/ssh-mcp/impl-logs/`. No task diverged from the spike.

### 10.1 Task replay

| Task | Files | `cargo check` | Task check and result |
| --- | --- | --- | --- |
| T001 | `src/application/resource_guard.rs` (new), `src/application/mod.rs`, `src/application/subscribe_resource.rs` | 0, warnings 0 | `grep -c 'pub mod resource_guard;'` → 1 |
| T002 | `src/application/subscription_admin.rs`, `src/infra/mcp/tool_router.rs`, `tests/sub_open_resource_check.rs` | 0, warnings 0 | lib tests 100, `Summary [0.025s] 12 tests run: 4 passed, 8 failed, 2142 skipped`, the eight messages of §2; integration 100, `assertion failed: matches!(uc.execute(req("shell://missing/output")).await, Err(DomainError::ShellNotFound(_)))` |
| T003 | `src/application/subscription_admin.rs` | 0, warnings 0 | lib tests 0, `Summary [0.023s] 12 tests run: 12 passed, 2142 skipped`; integration 100, same assertion |
| T004 | `src/composition/prod.rs` | 0, warnings 0 | integration 0, `Summary [0.004s] 1 test run: 1 passed, 0 skipped`; unit tests 0, `Summary [0.024s] 12 tests run: 12 passed, 2142 skipped` |
| T005 | `scripts/test_v5_adversarial.py`, `scripts/test_v51_flush_bytes.py` | `cargo build --bins` 0 | adversarial 0, `63 passed in 41.96s`; flush-bytes 0, `1 passed in 0.38s` |
| T006 | `docs/API.md`, `docs/OPERATIONS.md`, `docs/LLM_GUIDE.md`, `docs/RESOURCES.md` | not applicable | the old anchor prints nothing; `4-resourcessubscribe` → `:53`; the `per-kind` lines are the two of the spike |
| T007 | `CHANGELOG.md`, `docs/MIGRATION.md` | not applicable | `refuses a missing` → `:17`; `A missing id is refused` → `:882` |
| T008 | `doc/arch/functional/functional.md`, `doc/arch/runbooks/incident-push-delivery-lost.md` | not applicable | `guard call is` → `:248`; `per-kind code and opens no lane` → `:345`; `^9\.` → `:110` |
| T009 | `session.md` | not applicable | `Repaired in feature 007` → `:162`; the three re-anchored citations → `:115`, `:151`, `:184` |

`findReferences` on `ensure_exists` (`src/application/subscribe_resource.rs:156`)
returned `:124` (call) and `:156` (declaration), the two references of the
spec. No other signature or visibility changed.

### 10.2 Gate in the repository

| # | Command | Exit | Result |
| --- | --- | --- | --- |
| 1 | `cargo fmt --all -- --check` | 0 | clean |
| 2 | `cargo check --all-features --all-targets` | 0 | clean, warnings 0 |
| 3 | `cargo clippy --all-features -- -D warnings` | 0 | clean |
| 4 | `cargo build --no-default-features --target-dir target/no-default-features` | 0 | clean |
| 5 | `cargo build --bins` | 0 | clean |
| 6 | `cargo nextest run --test-threads num-cpus --lib` | 0 | `Summary [5.428s] 2154 tests run: 2154 passed, 0 skipped` |
| 7 | `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` | 0 | `Summary [22.170s] 2329 tests run: 2329 passed, 0 skipped` |
| 8 | `cargo audit` | 0 | `warning: 1 allowed warning found` (`chrono` unmaintained) |
| 9 | `make tools-export-check` | 0 | `unchanged: docs/TOOL_CATALOG.md`; `unchanged: docs/tool-catalog.json` |
| 10 | `scripts/test_http.py` | 0 | `15 passed in 210.21s` |
| 11 | `scripts/test_transfer_progress.py` | 0 | `1 passed, 1 skipped in 2.10s` (the known fast-machine skip at `:192`) |
| 12 | `scripts/test_v7_rsync_http.py` | 0 | `10 passed in 145.74s` |
| 13 | `scripts/test_v47_subscriptions.py` | 0 | `20 passed in 39.15s` |
| 14 | `scripts/test_v47_idempotency.py` | 0 | `9 passed in 7.80s` |
| 15 | `scripts/test_v5_adversarial.py` (adapted) | 0 | `63 passed in 47.64s` |
| 16 | `scripts/test_v51_flush_bytes.py` (adapted) | 0 | `1 passed in 0.37s` |

The five Python suites ran as
`/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/<file>`; the two
adapted scripts ran with `PYTHONPATH=scripts` and the three `SSH_MCP_*_BIN`
variables on `target/debug`. `scripts/chaos_v5_subscribe.py` was not run.

### 10.3 Validation and equality

`speckit validate` exited 6 with 68 errors, 0 warnings and the baseline rule
set; the error set on the changed files is the one of the run made before the
first edit (the twelve `src/infra/mcp/tool_router.rs` findings, unchanged).
`speckit verify --changed` exited 0 with `14 scenario(s): 14 passed, 0 failed,
76 unbound`. `speckit generate agents --check` reported
`checked AGENTS.md: up to date`.

The three `diff -r` commands against the spike printed nothing, and each of
the nine documents compared with `diff` printed nothing. Against
`/var/tmp/ssh-mcp/final-006/tree`, exactly the 18 files of the patch and the
seven feature artifacts differ (25 files).

After the first gate run, three wrong `session.md` citations into changed files
were corrected: `src/composition/prod.rs:796` → `:804`,
`src/composition/prod.rs:114` → `:115` and
`src/application/subscribe_resource.rs:164` → `:145`; the spike copy was
aligned and the patch regenerated (SHA-256
`7145a0581f5050043ee9308a8984d540acff6759d25ebdc4533cb0a56b172360`).
