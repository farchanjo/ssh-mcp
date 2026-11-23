# Research: Subscription Pause Delivery

Evidence for `plan.md`. Every measurement was taken on 2026-11-22 in
`/var/tmp/ssh-mcp/` with `RUSTC_WRAPPER=sccache` and `CARGO_BUILD_JOBS=4` as
exported, one cargo invocation at a time, no `-j`, no `--release`, no `nice`
and no `taskset`. `df -h /tmp /var/tmp` was read before the first build and
after the last.

## 1. Setup

```
rm -rf /var/tmp/ssh-mcp/pause-spike
rsync -a --exclude target --exclude .git --exclude .venv --exclude dist \
  --exclude '*.tar.gz' --exclude doc/.specify --exclude .pytest_cache \
  /root/dev/ssh-mcp/ /var/tmp/ssh-mcp/pause-spike/
rm -rf /var/tmp/ssh-mcp/pause-base
rsync -a ... /root/dev/ssh-mcp/ /var/tmp/ssh-mcp/pause-base/
```

`pause-spike` carries the whole change. `pause-base` carries the nine new
tests and no production change, and was used to prove which tests fail on the
untouched tree (§7). Both keep their sources; their `target` directories were
deleted after the results were recorded. The repository is the source of the
patch.

## 2. What the documents say a `seq` gap means

- `docs/API.md:1329` — "`seq` | `u64` | Monotonic per-lane sequence (starts
  at `0`). Use to detect drops."
- `docs/adr/0012-inline-push-notifications.md:710` — "The host reassembles
  by concatenating bytes in `seq` order."
- `docs/adr/0012-inline-push-notifications.md:711` — a host that wakes after
  a lag receives the marker and "the next inline payload starts from a new
  `seq` with `cursor_after` reflecting the post-drop server cursor."
- `docs/LLM_GUIDE.md:522` — "Fall back to
  `resources/read?cursor=<cursor_after>` if you missed a notification (queue
  overflow, transport reconnect)."

A pause is a drop with a known recovery anchor. Composing and dropping keeps
both halves of the contract: `seq` shows the gap, `cursor_after` is the
absolute offset the host reads from. Advancing only the cursor would leave
`seq` contiguous, so the documented detection signal would not fire, and
`cursor_after` would under-count every paused byte for the rest of the lane
life. Chosen: compose and drop (`plan.md` D-2).

## 3. Who sends the catch-up

Two designs were built and measured.

| | A, adapter-owned `Arc<dyn LaneResumeSink>` | B, resume use case |
| --- | --- | --- |
| Cycle | bridge -> adapter (strong) and adapter -> bridge (strong) | use case -> bridge -> adapter |
| Release test | fails, "adapter retained" | passes |
| Wiring | inside `with_inline_max`, a constructor side effect | `prod.rs:551`, next to `install_lane_bridge` |
| Port change | new trait, one implementor | `LaneAdmin::resume` returns `bool` (one implementor), `LaneNotifierBridge::notify_lane` |
| Call sites | none | `subscription_admin.rs:812`, `:843`, `prod.rs:551`, `tests/v5_smoke.rs:312` |
| Layering | adapter names a trait | use case orchestrates two ports (P-01 literal) |

Design A was the first spike. The release test
`adapter_and_bridge_are_released_after_drop` failed on it:

```
FAIL [0.004s] ssh-mcp adapters::subscription::lane_bridge::tests::adapter_and_bridge_are_released_after_drop
panicked at src/adapters/subscription/lane_bridge.rs:1189: adapter retained
```

On B the same test passes. Neither existing sink creates a cycle: `rx_sink`
and `close_sink` capture `Arc<ChannelMuxAdapter>` (`prod.rs:333-343`), and
`MemoryRegistry::install_lane_bridge` installs a bridge that does not hold the
registry (`memory_registry.rs:185`, `:261`). Chosen: B (`plan.md` D-1).

Bridge construction sites, production and tests:

| Site | Needs |
| --- | --- |
| `src/composition/prod.rs:362` (`from_env`) | nothing new; the bridge is cloned for `install_lane_bridge` and for `ResumeSubUseCase` |
| `src/adapters/subscription/lane_bridge.rs:672` (test helper) | nothing; `resume_lane` calls `SubscriberLaneAsync::resume_lane` and, on a transition, `LaneNotifierBridge::notify_lane` |
| `src/adapters/subscription/memory_registry.rs:1804` | nothing |
| `tests/chaos_inline_push.rs:190` | nothing |
| `tests/v5_smoke.rs:682`, `:780`, `:896` | nothing; the module has a `NoopLaneBridge` (`:281`) for the one use-case call at `:326` |

No construction installs anything on the adapter any more.

## 4. The window after `sub_pause` returns

Each path is serialized per URI: `spawn_debouncer` returns early when the
task exists (`memory_registry.rs:399-403`), and `inline_channel_for` creates
one channel and one consumer per URI (`lane_bridge.rs:384-394`). A send
already awaiting the notifier when the store lands completes; `ship_one`
(`:470`) drops every dispatch dequeued afterwards. The bound is one legacy
notification plus one inline dispatch, and the dispatch may hold several
fragments. No lock is added (`plan.md` D-3).

## 5. Production changes

The full hunks are in `/var/tmp/ssh-mcp/pause-spike.diff`. The essentials:

- `src/ports/subscriber_lane.rs` — `LaneAdmin::resume` and
  `SubscriberLaneAsync::resume_lane` return `bool`.
- `src/ports/notifier.rs` — `LaneNotifierBridge::notify_lane(sub_id)`, with
  a no-op default.
- `src/adapters/subscription/subscriber_lane.rs` — `lane_by_id`; `resume_lane`
  returns the swap result.
- `src/application/subscription_admin.rs` — `ResumeSubUseCase` holds the
  notifier and calls `notify_lane` on `true`.
- `src/composition/prod.rs` — passes the bridge to the use case.
- `src/adapters/subscription/lane_bridge.rs` — the four `Acquire` gates and
  the `notify_lane` impl, which sends through `ship_legacy_notify`.

## 5a. Each gate is proven

One gate removed at a time, module tests run, then restored:

| Gate removed | Failing test |
| --- | --- |
| `prepare_inline_dispatch` | `prepare_inline_dispatch_skips_a_paused_lane` |
| `notify_one_lane` | `paused_lane_receives_no_delivery_until_resume`, `pause_does_not_gate_another_lane_on_the_same_uri`, `paused_inline_lane_keeps_cursor_and_seq_monotonic_across_the_gap`, `paused_lane_counters_do_not_advance` |
| `notify_one_lane_inline_only` | `paused_lane_inline_only_path_is_gated` |
| `ship_one` | `queued_inline_dispatch_is_dropped_when_the_lane_is_paused` |

## 6. Gate

| Command | Exit | Result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | clean |
| `cargo check --all-features --all-targets` | 0 | clean |
| `cargo clippy --all-features -- -D warnings` | 0 | clean |
| `cargo build --no-default-features --target-dir target/no-default-features` | 0 | clean |
| `cargo build --bins` | 0 | clean |
| `cargo nextest run --test-threads num-cpus --lib` | 0 | 2141 passed |
| `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` | 0 | 2315 passed |
| `cargo audit` | 0 | no vulnerability; the allowed `chrono 0.4.45` unmaintained warning only |
| `make tools-export-check` | 0 | both `unchanged` |

Python suites, run from the spike with `SSH_MCP_BIN`,
`SSH_MCP_STDIO_BIN` and `SSH_MCP_TAIL_BIN` on its `target/debug` binaries:

| Suite | Result |
| --- | --- |
| `scripts/test_http.py` | 15 passed |
| `scripts/test_transfer_progress.py` | 2 passed |
| `scripts/test_v7_rsync_http.py` | 10 passed |
| `scripts/test_v47_subscriptions.py` | 20 passed |
| `scripts/test_v47_idempotency.py` | 9 passed |

## 7. The new tests with the four gates removed

The tests call the new API (`notify_lane`, the two-argument
`ResumeSubUseCase::new`), so they cannot compile on the pristine tree. The
base copy is the spike with the four gate expressions deleted and everything
else intact; that is the untouched behaviour. Command, filtered to the
sixteen names: `16 tests run: 9 passed, 7 failed`, exit 100.

| Test | Base | Message / what it pins |
| --- | --- | --- |
| `paused_lane_receives_no_delivery_until_resume` | FAIL | `legacy notify must not reach a paused lane` — the defect |
| `pause_does_not_gate_another_lane_on_the_same_uri` | FAIL | 2 ≠ 1 — the gate is per lane |
| `paused_inline_lane_keeps_cursor_and_seq_monotonic_across_the_gap` | FAIL | 3 ≠ 2 — compose and drop |
| `paused_lane_counters_do_not_advance` | FAIL | 1 ≠ 0 — counters freeze |
| `paused_lane_inline_only_path_is_gated` | FAIL | an inline event is delivered while paused |
| `prepare_inline_dispatch_skips_a_paused_lane` | FAIL | `no dispatch is enqueued while paused` |
| `queued_inline_dispatch_is_dropped_when_the_lane_is_paused` | FAIL | the queued dispatch is shipped |
| `adapter_and_bridge_are_released_after_drop` | PASS | no cycle (also fails on design A) |
| `resume_after_a_quiet_pause_sends_exactly_one_notification` | PASS | the catch-up |
| `resume_of_a_running_lane_sends_no_extra_notification` | PASS | no-op resume |
| `pause_unknown_lane_returns_sub_not_found` | PASS | `SUB_NOT_FOUND` |
| `resume_of_a_lane_closed_while_paused_returns_sub_not_found` | PASS | `SUB_NOT_FOUND` |
| `resources_subscribe_lane_is_listed_but_pause_does_not_gate_the_peer_fanout` | PASS | the residual (FR-011) |
| `resume_of_a_paused_lane_notifies_once_with_its_sub_id` | PASS | one `notify_lane` with the `sub_id` |
| `resume_of_a_running_lane_does_not_notify` | PASS | no call |
| `resume_unknown_does_not_notify` | PASS | `SUB_NOT_FOUND`, no call |

## 8. Patch

`/var/tmp/ssh-mcp/pause-spike.patch` is the whole feature, generated from the
pre-feature tree `/var/tmp/ssh-mcp/verify-06/tree` to the repository as it
is, in `a/<path>` and `b/<path>` form over 15 files. `patch -p1 --dry-run`
in a scratch copy of that tree under `/var/tmp` prints one `checking file`
line per file and exits 0; the scratch copy is deleted. SHA-256:
`5d57a96b070911d6abe11471102dab85278dfd37a258dff4cbbba884c566cf4d`.

```
 src/ports/subscriber_lane.rs                 |   11 +
 src/ports/notifier.rs                        |   15 +
 src/adapters/subscription/subscriber_lane.rs |   23 +-
 src/adapters/subscription/lane_bridge.rs     |  284 ++++++++++++++++++++++++++
 src/adapters/subscription/memory_registry.rs |   66 ++++++
 src/application/subscription_admin.rs        |  122 +++++++++++
 src/composition/prod.rs                      |   13 +
 tests/lockfree_invariants.rs                 |    5
 tests/v5_smoke.rs                            |   19 ++
 CHANGELOG.md                                 |    1
 docs/MIGRATION.md                            |    2
 docs/API.md                                  |    8 +
 docs/OPERATIONS.md                           |    4
 docs/ARCHITECTURE.md                         |    2
 session.md                                   |   17 +-
 15 files changed, 558 insertions(+), 34 deletions(-)
```

`Cargo.toml` is identical between the two trees and carries no hunk.
