# Research: Debouncer Consumer Lifetime

Evidence for `plan.md`. Measurements were taken on 2026-11-22 in
`/var/tmp/ssh-mcp/deb-spike` (spike), `/var/tmp/ssh-mcp/deb-base5` (base copy)
and `/var/tmp/ssh-mcp/mut-<letter>` (one copy per mutation) with
`RUSTC_WRAPPER=sccache` and `CARGO_BUILD_JOBS=4` as exported, one cargo
invocation at a time, except in the two full-suite runs of delivery 3 that
shared the host with the stability loops of this same session (§7); no `-j`,
no `--release`, no `nice`, no `taskset`. `df -h /tmp /var/tmp` was read before
the first build and after the last; every `target` and every tree copy was
deleted when its result was recorded. Mutations never run in the gate tree.

## 1. Setup

```
rm -rf /var/tmp/ssh-mcp/deb-spike
rsync -a --exclude target --exclude .git --exclude .venv \
  /root/dev/ssh-mcp/ /var/tmp/ssh-mcp/deb-spike/
```

The base run of §2 was made on a copy of the spike with the base rule inside.
The spike carries the production change, the documents and the `session.md`
repair marks (§3, §6, §9); the repository's `session.md` keeps the defect
records without the marks, and the patch is generated from a copy of the
repository working tree. The repository's Rust files do not change in this
phase.

## 2. The new tests on the base behaviour

`/var/tmp/ssh-mcp/deb-base5` is a copy of the spike with the base rule put
back: `consumers_empty` ignores the presence query (a stop fires whenever the
peer list is empty), `ensure_debouncer` is `contains_key` then insert with no
re-entry guard and no pre-spawn decision, and the removal is entry first, then
the maps. Command:

```
cargo nextest run --test-threads num-cpus --lib -E '<the twenty names>'
```

Exit 100, `Summary [4.286s] 20 tests run: 14 passed, 6 failed, 2154 skipped`
(`/var/tmp/ssh-mcp/logs/deb-base6-tests.log`). The six failures pin the
change:

| Test | Base | Message / what it pins |
| --- | --- | --- |
| `peer_unsubscribe_keeps_the_debouncer_a_lane_still_needs` | FAIL | `assertion failed: reg.has_debouncer(… "sh-d2")` — the stop ignores lanes |
| `ensure_after_the_last_consumer_left_spawns_nothing` | FAIL | `left: 2, right: 1` on the strong count — the base ensure spawns with no consumer |
| `ensure_racing_stop_never_splits_the_task_from_its_waker` | FAIL | `left: 1, right: 0`, `no consumer left, so no task` — a task survives its consumer |
| `open_racing_last_lane_close_keeps_one_debouncer` | FAIL | `assertion failed: reg.debounce_tasks.contains_key(… "race")` — the close stops a task the opener needs |
| `peer_unsubscribe_racing_sub_open_keeps_one_debouncer` | FAIL | same |
| `a_stop_that_holds_the_entry_makes_the_opener_rebuild_every_map` | FAIL | `assertion failed: reg.debounce_tasks.contains_key(… "race")` — the stop evaluated outside the entry |

Two more are weak on the base tree and pass in the batch run only by timing:
`resources_subscribe_racing_last_lane_close_keeps_one_debouncer` passed the
batch and failed 1 of 10 runs alone, and `concurrent_ensure_spawns_one_task`
passed the batch and failed 10 of 10 runs alone (`left: 20, right: 1`).
`last_lane_close_stops_the_debouncer` passes on the base copy because a lane
close already reaches the base rule once the sink exists; its proof is the
mutation table of §5, not this run. The remaining eleven pass in both.

## 3. Production change

`/var/tmp/ssh-mcp/deb-spike.patch` holds the full delta. The production hunks
are in `memory_registry.rs` (presence field and install, `release_if_idle_uri`,
the entry guard, the pre-spawn consumer decision, removal order),
`subscriber_lane.rs` (URI in `CloseSink`, one fire site), `lane_bridge.rs` (the
one `install_lane_lifetime` builder, weak handles) and `prod.rs` (calls the
builder, composes the mux unregister, `build_use_cases_inner`), plus
`src/adapters/subscription/fake.rs` (shared `StubNotifier`, gated like
`src/adapters/ssh/fake.rs`) and the two cascade test modules. No use case
signature changes; `install_close_sink` has one caller (`lane_bridge.rs:566`)
and `fire_close_sink` one call site (`subscriber_lane.rs:830`).

## 4. `scripts/test_v47_subscriptions.py::test_s07…`

The test opens two shells on one peer, subscribes both URIs, waits for both
notifications (which arrive), then reads each URI once with `?cursor=auto` and
asserts the marker is in the text (`scripts/test_v47_subscriptions.py:648`).
The shell reader may not have flushed the marker into the history yet, so the
read returns only the tty banner.

Twenty runs each, sequential, same host:

| Copy | Passed | Failed | Message |
| --- | --- | --- | --- |
| unmodified (`deb-probe-base`) | 16 | 4 | all `assert 'S7_B_MARK' in "/bin/sh: 0: can't access tty; job control turned off\r\n$ "` at `:648` |
| prototype (`deb-probe`) | 18 | 2 | same |
| spike | 19 | 1 | same |

The rates are comparable and the notification assertions pass; the test is
flaky on its own. Recorded in `session.md`; a bounded re-read would cure it.

## 5. Mutation checks

Each was applied in its own copy of the tree under `/var/tmp/ssh-mcp/mut-<letter>`
with its own `target`, the named test run, then the copy deleted. The loop
tests were repeated ten times; a mutation that survived would mean the test is
rewritten. Mutation (i) is a reorder plus its own `#[cfg(test)]` 5 ms pause
between the entry removal and the map removals: with the pause removed, the
plain reorder is caught 1 of 10 runs by `a_stop_that_holds_the_entry_…` and 0
of 10 by `ensure_racing_stop_…`, which is why the pause lives in the mutant
and not in production code. The deterministic test also waits for the opener
task to start and park on the shard before it releases the blocked query.

| # | Mutation | Test | Result |
| --- | --- | --- | --- |
| a | stop on peer-list emptiness without asking for lanes | `peer_unsubscribe_keeps_the_debouncer_a_lane_still_needs` | FAIL 1/1, `assertion failed: reg.has_debouncer(… "sh-d2")` (`deb-spike16-mut-a.log`) |
| b | never remove (removal deleted) | `last_lane_close_stops_the_debouncer` | FAIL 1/1, `assertion failed: !reg.has_debouncer(… "sh-last")` (`deb-spike16-mut-b.log`) |
| c | remove the composition link | `production_wiring_…` (adapter test stays green) | production FAIL 1/1; adapter PASS (`deb-spike16-mut-c.log`) |
| d | restore the non-atomic ensure (test, then insert; no re-check) | `concurrent_ensure_spawns_one_task` | FAIL 7/10, `left: 20, right: 1` (`deb-spike16-mut-d.log`) |
| e | make the registry-side handle strong | `registry_is_released_after_the_last_consumer_leaves` | FAIL 1/1, `the registry must be released once the last consumer is gone` (`deb-spike16-mut-e.log`) |
| f | remove the no-query rule | `ensure_without_a_presence_query_keeps_the_task` | FAIL 1/1, `assertion failed: reg.has_debouncer(… "no-query")` (`deb-spike16-mut-f.log`) |
| g | evaluate the consumers before taking the entry | `a_stop_that_holds_the_entry_makes_the_opener_rebuild_every_map` | FAIL 10/10, `assertion failed: reg.debounce_tasks.contains_key(… "race")` (`deb-spike16-mut-g.log`) |
| h | remove the pre-spawn consumer check | `ensure_after_the_last_consumer_left_spawns_nothing` (1 run) and `ensure_racing_stop_never_splits_the_task_from_its_waker` (10 runs) | FAIL 11/11, `left: 2, right: 1` on the strong count and `no consumer left, so no task` (`deb-spike16-mut-h.log`) |
| i | remove the three wakeup maps after the entry, with a 5 ms pause between | `a_stop_that_holds_the_entry_makes_the_opener_rebuild_every_map` | FAIL 10/10, `assertion failed: reg.wakers.contains_key(… "race")` (`deb-spike16-mut-i.log`) |

## 6. Gate

One cargo invocation at a time, `--test-threads num-cpus` on every nextest
invocation, no `--release`, no `--no-capture`, no throttle.

| Command | Exit | Result | Log |
| --- | --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | — | `deb-spike20-fmt.log` |
| `cargo check --all-features --all-targets` | 0 | 59 crates checked, `Checking ssh-mcp` at `:256` | `deb-spike20-check.log` |
| `cargo clippy --all-features -- -D warnings` | 0 | — | `deb-spike20-clippy.log` |
| `cargo build --no-default-features --target-dir target/no-default-features` | 0 | — | `deb-spike20-nodefault.log` |
| `cargo build --bins` | 0 | `Compiling ssh-mcp` | `deb-spike20-bins.log` |
| `cargo nextest run --test-threads num-cpus --lib` | 0 | `Summary [   5.644s] 2174 tests run: 2174 passed, 0 skipped` | `deb-spike20-lib.log` |
| `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` | 0 | `Summary [  26.581s] 2349 tests run: 2349 passed, 0 skipped` | `deb-spike20-tests.log` |
| `cargo audit` | 0 | `warning: 1 allowed warning found` | `deb-spike20-audit.log` |
| `make tools-export-check` | 0 | both `unchanged` | `deb-spike20-tools.log` |

The lib suite ran from an empty `target` in 26 s wall including the build,
5.64 s of it in the tests themselves.

Python suites, from the spike through the repository virtualenv; the last two
carry the three `SSH_MCP_*_BIN` variables on its `target/debug` binaries:

| Suite | Exit | Result | Log |
| --- | --- | --- | --- |
| `scripts/test_http.py` | 0 | `15 passed` | `deb-spike20-test_http.log` |
| `scripts/test_transfer_progress.py` | 0 | `1 passed, 1 skipped` | `deb-spike20-test_transfer_progress.log` |
| `scripts/test_v7_rsync_http.py` | 0 | `10 passed` | `deb-spike20-test_v7_rsync_http.log` |
| `scripts/test_v47_subscriptions.py` | 0 | `20 passed` | `deb-spike20-test_v47_subscriptions.log` |
| `scripts/test_v47_idempotency.py` | 0 | `9 passed` | `deb-spike20-test_v47_idempotency.log` |
| `scripts/test_v5_adversarial.py` | 0 | `63 passed` | `deb-spike20-test_v5_adversarial.log` |
| `scripts/test_v51_flush_bytes.py` | 0 | `1 passed` | `deb-spike20-test_v51_flush_bytes.log` |

The `scripts/test_v47_subscriptions.py` row is the gate run; the s07 flake of
§4 did not fire.

Totals: 2154 + 20 lib tests = 2174; 2329 + 20 with the integration run = 2349.
The twenty are the seventeen registry tests of §2 plus the two cascade tests
and the production test; they are listed by name once, in §2.
`scripts/chaos_v5_subscribe.py` is not a gate command (already broken,
`session.md`).

## 7. Stability

The counts below were taken on the final tree, one log per run, kept under
`/var/tmp/ssh-mcp/logs/deb-spike18-stab-<test>-<n>.log`, sequentially on the
same host, after the last source edit; the first and last file of each family
carry the timestamps quoted with the table.

| Test | Passed | Failed | Log family | First / last |
| --- | --- | --- | --- | --- |
| `open_racing_last_lane_close_keeps_one_debouncer` | 200 | 0 | `deb-spike18-stab-open-racing-1..200.log` | 18:17:14 / 18:32:31 |
| `resources_subscribe_racing_last_lane_close_keeps_one_debouncer` | 200 | 0 | `deb-spike18-stab-resources-subscribe-1..200.log` | 18:33:05 / 18:48:25 |
| `peer_unsubscribe_racing_sub_open_keeps_one_debouncer` | 500 | 0 | `deb-spike18-stab-peer-unsubscribe-1..500.log` | 18:48:40 / 19:27:04 |
| `concurrent_ensure_spawns_one_task` | 200 | 0 | `deb-spike18-stab-concurrent-ensure-1..200.log` | 19:27:25 / 19:28:49 |
| `ensure_racing_stop_never_splits_the_task_from_its_waker` | 200 | 0 | `deb-spike18-stab-ensure-racing-1..200.log` | 19:28:50 / 19:32:23 |
| `a_stop_that_holds_the_entry_makes_the_opener_rebuild_every_map` | 300 | 0 | `deb-spike18-stab-a-stop-1..300.log` | 19:32:23 / 19:34:20 |
| `ensure_after_the_last_consumer_left_spawns_nothing` | 200 | 0 | `deb-spike18-stab-ensure-after-1..200.log` | 19:34:20 / 19:35:34 |

### Why the two full-suite runs of delivery 3 failed

`deb-spike7-gate-lib.log` (15:21:28) and `deb-spike8-gate-lib.log` (15:44:17)
fail the same four tests with `assertion failed: reg.wakers.contains_key(…)` at
`memory_registry.rs:2198`, in 0.02 to 0.39 s. Neither log contains a
`Compiling ssh-mcp` line: cargo did not rebuild. The mutations of that delivery
were applied in the gate tree itself and restored with `cp -a`, which preserves
the backup's older mtime, so after each restore the source was older than the
binary. Mutation (d) ran at 15:16:24, the first gate at 15:21:28; mutation (i)
logged at 15:43:45, the second gate 32 s later. Both gates ran a mutant binary
— (d) and (i) respectively — not the final tree. The fix is in place: every
mutation now runs in its own tree copy under `/var/tmp/ssh-mcp/mut-<letter>`
with its own `target`, deleted afterwards, and the gate tree is built from an
empty `target`.

`scripts/test_v47_subscriptions.py` was run 20 times per copy before the last
edits: 4 failed on the unmodified copy, 2 on the prototype and 1 on the spike,
all at `:648` (§4). The gate row of §6 is a later, separate run.

## 8. Validate in the spike

`SPECKIT_PROJECT_ROOT=/var/tmp/ssh-mcp/deb-spike speckit validate` exits 6
with 68 errors and 0 warnings, the same rule set as the repository baseline
(46 `fqcn.relative-super`, 6 `branch.else`, 5 `pin.ci.unpinned-action`, 2
`pin.docker.unpinned-image`, one each of the remaining nine). No error or
warning names a file of the feature or `session.md`; three `info` lines name the
schema (first-class collection), as at the baseline. Log
`/var/tmp/ssh-mcp/logs/deb-spike18-validate.log`. The change removes no
baseline finding and adds none.

## 9. Patch

`/var/tmp/ssh-mcp/deb-spike.patch` is generated from a copy of the repository
working tree (no `target`, no `.git`, no `.venv`, no caches) to the spike in
`a/<path>` / `b/<path>` form, so it applies to the repository as it is.
Excluded: the feature's own artifacts (`doc/arch/sdd/008-…`, ADR 0008, the
schema, the feature file), `Cargo.lock`, `target`, `.venv`, `.git`, caches,
`dist`, the release tarball and databases. The differing-file list minus those
exclusions equals the patch file list (`diff` prints nothing): nine files, the
same nine.

```
11 files changed, 1150 insertions(+), 54 deletions(-)
```

SHA-256 `8737f2c57e4538ffed147550d6ff0d0f21e9353adc250ead77def92d2e1ecc4f`.
`patch -p1 --dry-run` in a scratch copy of the repository working tree under
`/var/tmp/ssh-mcp/deb-check` exits 0; the scratch copy is deleted afterwards.
Log: `/var/tmp/ssh-mcp/logs/deb-spike18-patch-dryrun.log`.

## 10. Cleanup

`/var/tmp/ssh-mcp/deb-spike/target`, `/var/tmp/ssh-mcp/deb-base5`,
`/var/tmp/ssh-mcp/mut-base` and every `/var/tmp/ssh-mcp/mut-<letter>` were
deleted after the last build; `/var/tmp/ssh-mcp/deb-spike.patch`,
`/var/tmp/ssh-mcp/deb-base4-swallow.log`, `/var/tmp/ssh-mcp/unsub-swallow.patch`,
`/var/tmp/ssh-mcp/end-repro.patch` and the logs under
`/var/tmp/ssh-mcp/logs/` stay.
