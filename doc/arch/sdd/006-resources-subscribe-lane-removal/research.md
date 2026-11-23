# Research: Resources Subscribe Lane Removal

Evidence for `plan.md`. Every measurement was taken on 2026-11-22 in
`/var/tmp/ssh-mcp/lane-spike` with `RUSTC_WRAPPER=sccache` and
`CARGO_BUILD_JOBS=4` as exported, one cargo invocation at a time, no `-j`, no
`--release`, no `nice` and no `taskset`. `df -h /tmp /var/tmp` was read before
the first build and after the last; the spike's `target` was deleted when the
results were recorded.

## 1. Setup

```
rm -rf /var/tmp/ssh-mcp/lane-spike
rsync -a --exclude target --exclude .git --exclude .venv \
  /root/dev/ssh-mcp/ /var/tmp/ssh-mcp/lane-spike/
```

The six new tests were added first to the unchanged tree and run there (§2).
Then the production change and the document changes were made (§3, §6). The
repository's Rust files do not change in this phase.

## 2. The six tests on the unchanged tree

Command:

```
cargo nextest run --lib --test-threads num-cpus \
  -E 'test(resources_subscribe_records_no_lane) or test(repeated_resources_subscribe_counts_once) \
      or test(lifecycle_refusal_leaves_no_registry) or test(seventeenth_resources_subscribe) \
      or test(resources_unsubscribe_removes_the_registry_entry) or test(resources_unsubscribe_leaves_a_sub_open)'
```

Exit 100, `Summary [0.009s] 6 tests run: 2 passed, 4 failed, 2141 skipped`.
Log `/var/tmp/ssh-mcp/logs/lane-spike-base.log`.

| Test | Base | Message / what it pins |
| --- | --- | --- |
| `resources_subscribe_records_no_lane_and_delivers_once` | FAIL | `no lane may be opened` (`subscribe_resource.rs:794`) — the defect |
| `repeated_resources_subscribe_counts_once_in_the_lifecycle` | FAIL | `assertion left == right failed: a refresh must not increment the lifecycle`, left 2, right 1 (`:823`) — the defect |
| `lifecycle_refusal_leaves_no_registry_entry_and_no_delivery` | FAIL | `assertion failed: h.registry.snapshot_subscribers(RESUB_URI).is_empty()` (`:866`) — the defect |
| `seventeenth_resources_subscribe_with_interleaved_unsubscribe_succeeds` | FAIL | `subscribe round 16 failed: max subs per URI exceeded for shell://sh-resub/output (limit 16)` (`:894`) — the defect |
| `resources_unsubscribe_removes_the_registry_entry` | PASS | the entry is removed and a second call succeeds; pins behaviour that stays |
| `resources_unsubscribe_leaves_a_sub_open_lane_open` | PASS | a `sub_open` lane is not touched; pins behaviour that stays |

After the change all six pass: exit 0, `Summary [0.111s] 6 tests run: 6
passed, 2137 skipped`, log `/var/tmp/ssh-mcp/logs/lane-spike-fixed.log`.

## 3. Production change

`/var/tmp/ssh-mcp/lane-spike.patch` holds the full delta. The essential
hunks:

- `src/ports/subscriber_registry.rs` — `SubscribeDisposition`, `#[must_use]`,
  and the `subscribe` return type.
- `src/adapters/subscription/memory_registry.rs` — `existed` under the
  existing shard guard, disposition returned, `let _ =` at the test call
  sites.
- `src/application/subscribe_resource.rs` — disposition-driven
  `on_subscribe`, rollback, DTO and removal.
- `src/application/unsubscribe_resource.rs` — DTO and removal.
- `src/infra/mcp/resource_handlers.rs` — log removal and request
  construction.
- `src/composition/prod.rs` — wiring removal.
- no `let _ =` bindings: the disposition carries no `#[must_use]`.
- the documents of §6 and the `session.md` repair mark.

## 4. Gate

| Command | Exit | Result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | clean |
| `cargo check --all-features --all-targets` | 0 | clean |
| `cargo clippy --all-features -- -D warnings` | 0 | clean |
| `cargo build --no-default-features --target-dir target/no-default-features` | 0 | clean |
| `cargo build --bins` | 0 | clean |
| `cargo nextest run --test-threads num-cpus --lib` | 0 | 2142 passed |
| `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` | 0 | 2316 passed |
| `cargo audit` | 0 | no vulnerability; the allowed `chrono 0.4.45` unmaintained warning |
| `make tools-export-check` | 0 | both `unchanged` |

Python suites, from the spike with `SSH_MCP_BIN`, `SSH_MCP_STDIO_BIN` and
`SSH_MCP_TAIL_BIN` on its `target/debug` binaries:

| Suite | Result |
| --- | --- |
| `scripts/test_http.py` | 15 passed |
| `scripts/test_transfer_progress.py` | 2 passed |
| `scripts/test_v7_rsync_http.py` | 10 passed |
| `scripts/test_v47_subscriptions.py` | 20 passed |
| `scripts/test_v47_idempotency.py` | 9 passed |

Totals: 2141 − 5 removed-or-replaced + 6 added = 2142 lib; 2315 − 5 + 6 =
2316 with integration.

## 5. Mutation checks

Each was applied, the named test run, then the files restored.

| Mutation | Test | Result |
| --- | --- | --- |
| call `on_subscribe` on a refresh too | `repeated_resources_subscribe_counts_once_in_the_lifecycle` | FAIL, `left: 2`, `right: 1` (`lane-spike-mut-a.log`) |
| remove the rollback | `lifecycle_refusal_leaves_no_registry_entry_and_no_delivery` | FAIL, `assertion failed: h.registry.snapshot_subscribers(RESUB_URI).is_empty()` (`lane-spike-mut-b.log`) |
| re-wire the lane in the use case, composition and test harness | `resources_subscribe_records_no_lane_and_delivers_once`, `seventeenth_resources_subscribe_with_interleaved_unsubscribe_succeeds` | FAIL, `no lane may be opened`; `subscribe round 16 failed: max subs per URI exceeded … (limit 16)` (`lane-spike2-mut-c.log`) |
| duplicate the fan-out notify (proves the settle re-read, not part of the feature) | `resources_subscribe_records_no_lane_and_delivers_once` | FAIL, `left: 2`, `right: 1` at the `delivered`/`exactly one notification` assertions (`lane-spike2-mut-c2b.log`) |

## 6. Validate in the spike

`SPECKIT_PROJECT_ROOT=/var/tmp/ssh-mcp/lane-spike speckit validate` exits 6
with 68 errors and no warning, the same rule set as the repository baseline
(46 `fqcn.relative-super`, 6 `branch.else`, 5 `pin.ci.unpinned-action`, 2
`pin.docker.unpinned-image`, one each of the remaining nine). No finding names
a file of the feature. Log `/var/tmp/ssh-mcp/logs/lane-spike-validate.log`.
The change removes no baseline finding and adds none.

## 7. Patch

`/var/tmp/ssh-mcp/lane-spike.patch` is generated from the pre-feature tree
`/var/tmp/ssh-mcp/final-005/tree` to the spike in `a/<path>` and `b/<path>`
form, excluding the feature's own artifacts (`doc/arch/sdd/006-…`, ADR 0006,
the schema, the feature file), `Cargo.lock`, `target`, `.venv`, `.git`,
caches, the release tarball and databases. The differing-file list minus those
exclusions equals the patch file list (`diff` prints nothing).
`patch -p1 --dry-run` in a scratch copy of that tree under `/var/tmp` exits 0;
a real `patch -p1` in the same scratch copy followed by `diff -r` of `src/`,
`tests/` and `docs/` against the spike prints nothing. The scratch copy is
deleted afterwards.

```
 17 files changed, 570 insertions(+), 360 deletions(-)
```
SHA-256 `42c479ceaa66cfdd5957e91ede9bef327ee5506bde5851f890b6c042797cae8e`.
The headers are `--- a/<path>` / `+++ b/<path>`.

## 8. Cleanup

`/var/tmp/ssh-mcp/lane-spike/target` deleted after the last build;
`/var/tmp/ssh-mcp/lane-probe` and `/var/tmp/ssh-mcp/resub-probe` deleted
entirely. `/var/tmp/ssh-mcp/resub-repro.patch` and the logs stay.

## 9. Order replay

The order of `tasks.md` was replayed in `/var/tmp/ssh-mcp/lane-replay`, a
copy of the repository tree, one task at a time. Every task ran
`cargo check --all-features --all-targets` and the task's own check command.
The replay directory was deleted after the table was recorded.

| Task | Applied | `cargo check` | Task check and result |
| --- | --- | --- | --- |
| T001 | six tests in base form (`sub_id: None` constructors, both harnesses wiring both use cases) | 0 | six-test filter: 100, `2 passed, 4 failed`, `no lane may be opened`; `a refresh must not increment the lifecycle` (left 2, right 1); `assertion failed: snapshot_subscribers(...).is_empty()`; `subscribe round 16 failed … (limit 16)` |
| T002 | disposition enum, registry and stubs | 0 | FR-003 filter: 100, `left: 2`, `right: 1` (still failing) |
| T003 | execute order and rollback | 0 | FR-003 + FR-004 filter: 100, `1 passed, 1 failed`, the failure `assertion failed: h.lanes.list_subs().is_empty()` |
| T004 | composition stops wiring both use cases | 0 | `grep -c with_subscriber_lane prod.rs` → 0 |
| T005 | subscribe removal, handler log, two tests removed, harnesses unwired | 0 | FR-001 + FR-003 filter: 0, `2 passed` |
| T006 | unsubscribe removal, request field, two tests removed, literals cleaned | 0 | FR-002 pair: 0, `2 passed` |
| T007 | pinned test deleted | 0 | full lib: 0, `2142 tests run: 2142 passed, 0 skipped` |
| T008–T011 | documents and `session.md` | not rerun | text only |
| T012–T014 | gate, validate, equality | — | measured in the spike and in the repository |

Closing equality: `diff -r /var/tmp/ssh-mcp/lane-replay/src
/var/tmp/ssh-mcp/lane-spike/src` and the same for `tests` printed nothing
(`SRC-IDENTICAL`, `TESTS-IDENTICAL`). `SPECKIT_PROJECT_ROOT`-less
`speckit validate` in the replay copy exited 6 with 68 errors, the baseline
rule set and no warning.

## 10. Repository run

The tasks were applied to `/root/dev/ssh-mcp` on branch
`006-resources-subscribe-lane-removal`, in order, one task at a time.
`RUSTC_WRAPPER=sccache` and `CARGO_BUILD_JOBS=4` stayed exported; no `-j`, no
`--release`, no `nice`, no `taskset`; tests ran with `cargo nextest run`. The
six tests landed in the base form of `tasks.md` first. `df -h /tmp /var/tmp`
was read before the first build (47 GB free on `/tmp`) and after the last
(47 GB free on `/tmp`).

After the first gate run the layout of the two test modules was corrected (the
feature banners and both orphan v5 banners removed, the imports moved into the
top block of each module, one blank line restored before the new harness) and
the gate was run again: `2142 tests run: 2142 passed, 0 skipped` on `--lib`
and `2316 tests run: 2316 passed, 0 skipped` with `--tests --features
test-fixtures`. The production part of both files is byte-identical to the
first run, so the Python suites were not run again.

### 10.1 Per-task results

| Task | Files changed | `cargo check` | Task check | Result |
| --- | --- | --- | --- | --- |
| T001 | `src/application/subscribe_resource.rs`, `src/application/unsubscribe_resource.rs` (six tests, base form) | 0 | six-test filter | 100; `2 passed, 4 failed`; `no lane may be opened`; `assertion left == right failed: a refresh must not increment the lifecycle` (left 2, right 1); `assertion failed: h.registry.snapshot_subscribers(RESUB_URI).is_empty()`; `subscribe round 16 failed: max subs per URI exceeded for shell://sh-resub/output (limit 16)` |
| T002 | `src/ports/subscriber_registry.rs`, `src/adapters/subscription/memory_registry.rs`, both `RecordingRegistry` stubs | 0 | `test(repeated_resources_subscribe_counts_once)` | 100; `left: 2`, `right: 1` |
| T003 | `src/application/subscribe_resource.rs` (`execute` order and rollback) | 0 | FR-003 + FR-004 filter | 100; `1 passed, 1 failed`; `assertion failed: h.lanes.list_subs().is_empty()` |
| T004 | `src/composition/prod.rs` | 0 | `grep -c with_subscriber_lane prod.rs` | 0 |
| T005 | `src/application/subscribe_resource.rs`, `src/infra/mcp/resource_handlers.rs` | 0 | FR-001 + FR-003 filter | 0; `2 passed` |
| T006 | `src/application/unsubscribe_resource.rs`, `src/infra/mcp/resource_handlers.rs` | 0 | FR-002 pair | 0; `2 passed` |
| T007 | `src/adapters/subscription/memory_registry.rs` | 0 | full lib suite | 0; `2142 tests run: 2142 passed, 0 skipped` |
| T008 | `doc/arch/functional/functional.md`, `doc/arch/runbooks/incident-push-delivery-lost.md` | not run | `grep -n` anchors | `:231` and `:108`; no `Open residual` |
| T009 | `docs/LLM_GUIDE.md`, `docs/RESOURCES.md`, `docs/ARCHITECTURE.md`, `docs/API.md`, `docs/OPERATIONS.md`, `README.md` | not run | `grep` sweep; `speckit mermaid render` of the changed lines | no lane/`sub_id` sentence outside the historical passages; render exit 0 |
| T010 | `CHANGELOG.md`, `docs/MIGRATION.md` | not run | `grep -n` | `CHANGELOG.md:17`, `docs/MIGRATION.md:882` |
| T011 | `session.md` | not run | `grep -n` anchors | the re-anchored lines; `opens no lane` at `:126`, `:136`; the file list at `:95` |

The rendered Mermaid source is the changed block only: the whole diagram uses
`autonumber` and `alt`, which the renderer's subset rejects, so the scratch
`.mmd` carried the changed sequence lines without them.

### 10.2 Gate

| # | Command | Exit | Summary |
| --- | --- | --- | --- |
| 1 | `cargo fmt --all -- --check` | 0 | clean |
| 2 | `cargo check --all-features --all-targets` | 0 | `Finished dev profile` |
| 3 | `cargo clippy --all-features -- -D warnings` | 0 | `Finished dev profile` |
| 4 | `cargo build --no-default-features --target-dir target/no-default-features` | 0 | `Finished dev profile` |
| 5 | `cargo build --bins` | 0 | `Finished dev profile` |
| 6 | `cargo nextest run --test-threads num-cpus --lib` | 0 | `Summary [5.452s] 2142 tests run: 2142 passed, 0 skipped` |
| 7 | `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` | 0 | `Summary [27.015s] 2316 tests run: 2316 passed, 0 skipped` |
| 8 | `cargo audit` | 0 | `warning: 1 allowed warning found` (`chrono` unmaintained) |
| 9 | `make tools-export-check` | 0 | `unchanged: docs/TOOL_CATALOG.md`; `unchanged: docs/tool-catalog.json` |
| 10 | `.venv/bin/python -m pytest -rs scripts/test_http.py` | 0 | `15 passed in 210.22s` |
| 11 | `.venv/bin/python -m pytest -rs scripts/test_transfer_progress.py` | 0 | `1 passed, 1 skipped in 1.58s`; the skip is the suite's fast-machine guard at `scripts/test_transfer_progress.py:192` (`samples=[0, 5242880]`) |
| 12 | `.venv/bin/python -m pytest -rs scripts/test_v7_rsync_http.py` | 0 | `10 passed in 145.75s` |
| 13 | `.venv/bin/python -m pytest -rs scripts/test_v47_subscriptions.py` | 0 | `20 passed in 39.21s` |
| 14 | `.venv/bin/python -m pytest -rs scripts/test_v47_idempotency.py` | 0 | `9 passed in 7.81s` |

The suites ran against the repository's freshly built `target/debug`
binaries, which `scripts/helpers/paths.py` selects by default; no
`SSH_MCP_*_BIN` override and no `SSH_MCP_BUILD_PROFILE` was set.

### 10.3 Validate, verify and agents

- `speckit validate`: exit 6, 68 errors, 0 warnings, the baseline rule set and
  no finding on a feature file.
- `speckit verify --changed`: exit 0, `14 scenario(s): 14 passed, 0 failed,
  63 unbound`.
- `speckit generate agents --check`: exit 0, `checked AGENTS.md: up to date`.
- `speckit commit check` on the three headers of the commit plan: exit 0 each.

### 10.4 Equality

`diff -r /var/tmp/ssh-mcp/lane-spike/src src` and the same for `tests`
printed nothing (`SRC-IDENTICAL`, `TESTS-IDENTICAL`); the eleven documents of
the patch compared with `diff` printed nothing. The file-by-file comparison
with `/var/tmp/ssh-mcp/final-005/tree` over
`git ls-files --cached --others --exclude-standard` reported exactly the 17
patch files as differing and the seven feature artifacts as new, and nothing
else.
