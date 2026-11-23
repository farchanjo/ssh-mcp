# Implementation Plan: Subscription Pause Delivery

## Overview

Make `sub_pause` gate the two lane notification paths on the lane it names,
and make `sub_resume` emit one catch-up notification so the consumer reads
the missed range from its own cursor. The flag, the ports and the response
lines already exist; the change is a hot-path `Acquire` load, a cold-path
sink, the tests of the specification and six document corrections.

The spike at `/var/tmp/ssh-mcp/pause-spike` carries the whole change to a
green gate; `research.md` carries the evidence, the two decisions and the
test results, including the tests run against the untouched tree.

## Technical Approach

### 1. The gate (FR-001, FR-004, FR-011)

`LaneFanoutBridge` loads `lane.paused` with `Acquire` on both producer
entries and at the dispatch consumer, after composing the inline payload:

- `notify_one_lane` (`src/adapters/subscription/lane_bridge.rs:280` today):
  compose, then return before either send;
- `notify_one_lane_inline_only` (`:291` today): the same;
- `prepare_inline_dispatch` (`:256` today): compose for every lane, then
  `continue` on a paused lane so no dispatch is enqueued;
- `ship_one` (`:470` today): return on a paused lane so a dispatch already
  queued when the pause landed is dropped.

The gate reads the lane, so another lane on the URI and another peer keep
delivering (FR-004). A peerless lane is skipped exactly as today (FR-011).

### 2. The catch-up signal (FR-002)

`LaneAdmin::resume` (and `SubscriberLaneAsync::resume_lane`) returns `bool`:
`true` only when this call flipped a paused lane. `ResumeSubUseCase` holds an
`Arc<dyn LaneNotifierBridge>` next to the lane admin, and on `true` calls
`notify_lane(&sub_id)`. The bridge resolves the lane by id
(`SubscriberLaneAdapter::lane_by_id`), takes its peer and sends through
`ship_legacy_notify`, so the catch-up counts as one delivered event with zero
bytes. Composition passes the bridge at `prod.rs:551`; a test double
implements the port in the two test modules that build the use case.

No strong cycle exists: the use case holds the bridge, the bridge holds the
adapter, and the adapter holds neither. `adapter_and_bridge_are_released_after_drop`
pins it.

### 3. Cursor and seq (FR-003)

While paused the composition still runs and the fragments are dropped. `seq`
gaps by the dropped fragment count and `cursor_after` stays the absolute
produced offset, which is the documented drop contract (`research.md` §2).
The `seq` and `cursor` arithmetic is unchanged: `compose_inline_payload`
already mints both (`lane_bridge.rs:176-212`).

### 4. Observability (FR-006)

No new metric, log event, span or NDJSON event. `sub_list` keeps rendering
`paused=true|false`; the four delivery counters freeze while paused and move
on the next delivered fragment. The catch-up counts as one delivered event
with zero bytes.

### 5. Records and documents (FR-009, FR-011)

`CHANGELOG.md` under `[Unreleased]` → `### Fixed`; a "Pause delivery"
paragraph in `docs/MIGRATION.md` after the `sub_*` table; the two API
sentences; the two OPERATIONS wordings; the ARCHITECTURE field wording; the
`session.md` repair mark, residual defect entry and open-item extension. The
exact old and new text is below.

## Decisions

### D-1 Who sends the catch-up: the resume use case

Chosen: `LaneAdmin::resume` reports the transition and `ResumeSubUseCase`
calls `LaneNotifierBridge::notify_lane`.

Evidence:

- The first spike had the adapter hold `Arc<dyn LaneResumeSink>`
  implemented by the bridge. The bridge already holds
  `Arc<SubscriberLaneAdapter>`, so both stayed alive for ever. The HTTP
  transport builds one composition per session, so every ended session
  retained its adapter, bridge and lanes. `adapter_and_bridge_are_released_after_drop`
  fails on that design ("adapter retained") and passes on this one.
- Neither existing sink creates a cycle: `rx_sink` captures
  `Arc<ChannelMuxAdapter>` and `close_sink` does too
  (`prod.rs:333-343`); `install_lane_bridge` installs
  `Arc<dyn LaneNotifierBridge>` on `MemoryRegistry`, which the bridge does
  not hold. The adapter is held by the bridge, not the reverse.
- P-01 puts the seam at the use case: the resume use case orchestrates
  `LaneAdmin` and `LaneNotifierBridge`; no adapter names another adapter.
- The alternative (adapter-owned `Weak<dyn LaneResumeSink>`) adds a `Weak`
  upgrade on the cold path and keeps the notifier reachable from an adapter.
  It touches the port trait, the adapter, the bridge impl, the constructor
  and every test helper that builds a bridge. This option touches
  `LaneAdmin::resume` (one implementor), `ResumeSubUseCase` (field,
  constructor, `execute`), `prod.rs`, the two admin tests and
  `tests/v5_smoke.rs:312`. The locator `lane_by_id` is added to the adapter.
  Both were measured; the use case is the layering the constitution asks for
  and carries no cycle.

### D-2 What `seq` does while paused: compose and drop

Chosen: compose the fragments and drop them.

Evidence, from the project's own host guidance:

- `docs/API.md:1329` — "`seq` | `u64` | Monotonic per-lane sequence (starts
  at `0`). Use to detect drops."
- `docs/adr/0012-inline-push-notifications.md:710` — the host reassembles by
  concatenating the bytes in `seq` order.
- `docs/adr/0012-inline-push-notifications.md:711` and
  `docs/LLM_GUIDE.md:522` — after a gap the next payload starts from a new
  `seq` with `cursor_after` reflecting the post-drop cursor, and the host
  reads from that cursor.

A paused window is a drop of exactly that shape. Advancing only the cursor
would keep `seq` contiguous and hide the gap from a host told to detect
drops with `seq`; it would also under-count `cursor_after` by every paused
byte for the rest of the lane life. The cost is one transient payload per
producer write while paused; nothing is stored.

### D-3 The window after `sub_pause` returns (AR-3)

At most one legacy notification and one inline dispatch can land after the
store takes effect. Each path is serialized per URI: one debouncer task per
`(kind, id)` (`spawn_debouncer` returns early when the task exists,
`memory_registry.rs:399-403`) and one consumer per URI (`inline_channel_for`
inserts one sender, `lane_bridge.rs:384-394`). A send already awaiting the
notifier when the flag is stored completes; every later dispatch is dropped
at `ship_one`. The inline dispatch may carry several fragments, so the bound
is two notifications, not one fragment. No lock is added.

## File Map

| File | Requirement |
| --- | --- |
| `src/ports/subscriber_lane.rs` | FR-002 (`resume` returns `bool`) |
| `src/ports/notifier.rs` | FR-002 (`notify_lane`) |
| `src/adapters/subscription/subscriber_lane.rs` | FR-002 (`lane_by_id`, `resume` swap result) |
| `src/adapters/subscription/lane_bridge.rs` | FR-001, FR-002, FR-003, FR-004, FR-006 |
| `src/application/subscription_admin.rs` | FR-002 (`ResumeSubUseCase`) |
| `src/composition/prod.rs` | FR-002 (wiring) |
| `src/adapters/subscription/memory_registry.rs` (test module) | FR-011 |
| `tests/lockfree_invariants.rs` (comment) | FR-008 |
| `tests/v5_smoke.rs:312` (constructor) | FR-002 |
| `CHANGELOG.md` | FR-009 |
| `docs/MIGRATION.md` | FR-009 |
| `docs/API.md` | FR-009 |
| `docs/OPERATIONS.md` | FR-009 |
| `docs/ARCHITECTURE.md` | FR-009 |
| `session.md` | FR-009, FR-011 |
| `doc/arch/functional/functional.md` | FR-009, FR-011 |
| `doc/arch/runbooks/incident-push-delivery-lost.md` | FR-009 |
| `doc/arch/sdd/005-subscription-pause-delivery/{spec,plan,research,tasks}.md`, `doc/arch/adr/0005-subscription-pause-delivery.md`, `doc/arch/schemas/subscription-pause-delivery.cue`, `doc/arch/specs/features/subscription-pause-delivery.feature` | FR-009, FR-010 |

No other file changes.

## Test Edits and Additions

| File:line (spike) | Kind | Requirement |
| --- | --- | --- |
| `src/adapters/subscription/lane_bridge.rs:1024` | new tokio test `paused_lane_receives_no_delivery_until_resume` | FR-001 |
| `src/adapters/subscription/lane_bridge.rs:1059` | new tokio test `pause_does_not_gate_another_lane_on_the_same_uri` | FR-004 |
| `src/adapters/subscription/lane_bridge.rs:1076` | new tokio test `resume_of_a_running_lane_sends_no_extra_notification` | FR-002 |
| `src/adapters/subscription/lane_bridge.rs:1096` | new tokio test `resume_after_a_quiet_pause_sends_exactly_one_notification` | FR-002 |
| `src/adapters/subscription/lane_bridge.rs:1113` | new tokio test `paused_inline_lane_keeps_cursor_and_seq_monotonic_across_the_gap` | FR-003 |
| `src/adapters/subscription/lane_bridge.rs:1147` | new tokio test `paused_lane_counters_do_not_advance` | FR-006 |
| `src/adapters/subscription/lane_bridge.rs:1178` | new tokio test `paused_lane_inline_only_path_is_gated` | FR-001 |
| `src/adapters/subscription/lane_bridge.rs:1202` | new tokio test `prepare_inline_dispatch_skips_a_paused_lane` | FR-001, FR-003 |
| `src/adapters/subscription/lane_bridge.rs:1227` | new tokio test `queued_inline_dispatch_is_dropped_when_the_lane_is_paused` | FR-001 |
| `src/adapters/subscription/lane_bridge.rs:1256` | new tokio test `pause_unknown_lane_returns_sub_not_found` | FR-005 |
| `src/adapters/subscription/lane_bridge.rs:1268` | new tokio test `resume_of_a_lane_closed_while_paused_returns_sub_not_found` | FR-005 |
| `src/adapters/subscription/lane_bridge.rs:1289` | new tokio test `adapter_and_bridge_are_released_after_drop` | FR-002 |
| `src/application/subscription_admin.rs:896` | new tokio test `resume_of_a_paused_lane_notifies_once_with_its_sub_id` | FR-002 |
| `src/application/subscription_admin.rs:926` | new tokio test `resume_of_a_running_lane_does_not_notify` | FR-002 |
| `src/application/subscription_admin.rs:948` | new tokio test `resume_unknown_does_not_notify` | FR-005 |
| `src/adapters/subscription/memory_registry.rs:1788` | new tokio test `resources_subscribe_lane_is_listed_but_pause_does_not_gate_the_peer_fanout` | FR-011 |
| `tests/v5_smoke.rs:312` | constructor edit | FR-002 |
| `src/adapters/subscription/lane_bridge.rs:1024` | one-line edit of the reproduction test's final `resource_events` assertion, 1 → 2 | FR-001, FR-008 |
| `tests/lockfree_invariants.rs:700` | comment names the gate instead of `Notify` | FR-008 |

No test is ignored, removed or weakened; no other assertion changes. All
sixteen new tests are `#[tokio::test]`, so every scenario stays `@planned`
except the two already bound to synchronous render tests.

## Verification Plan

Run from the repository root, one cargo invocation at a time, with
`RUSTC_WRAPPER=sccache` and `CARGO_BUILD_JOBS=4` as exported.

| # | Command | Expected |
| --- | --- | --- |
| 1 | `cargo fmt --all -- --check` | exit 0 |
| 2 | `cargo check --all-features --all-targets` | exit 0 |
| 3 | `cargo clippy --all-features -- -D warnings` | exit 0 |
| 4 | `cargo build --no-default-features --target-dir target/no-default-features` | exit 0 |
| 5 | `cargo build --bins` | exit 0 |
| 6 | `cargo nextest run --test-threads num-cpus --lib` | 2141 passed |
| 7 | `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` | 2315 passed |
| 8 | `cargo audit` | exit 0; `chrono` warning only |
| 9 | `make tools-export-check` | both `unchanged` |
| 10 | `…/.venv/bin/python -m pytest -rs scripts/test_http.py` | 15 passed |
| 11 | `… scripts/test_transfer_progress.py` | 2 passed |
| 12 | `… scripts/test_v7_rsync_http.py` | 10 passed |
| 13 | `… scripts/test_v47_subscriptions.py` | 20 passed |
| 14 | `… scripts/test_v47_idempotency.py` | 9 passed |
| 15 | `diff -r /var/tmp/ssh-mcp/pause-spike/src src` | no output |
| 16 | `diff -r /var/tmp/ssh-mcp/pause-spike/tests tests` | no output |
| 17 | `diff /var/tmp/ssh-mcp/pause-spike/Cargo.toml Cargo.toml` | no output |
| 18 | `diff` of each touched document against its spike copy | no output |

The five Python suites run with `SSH_MCP_BIN`, `SSH_MCP_STDIO_BIN` and
`SSH_MCP_TAIL_BIN` on the spike's `target/debug` binaries. The reference
gate is 2125 lib and 2299 with integration; the feature adds sixteen lib tests.

## Documentation Changes

- `CHANGELOG.md`, inserted under `[Unreleased]` → `### Fixed` before the
  `RESOURCE_SHORTAGE` bullet:
  old — none; new — "**`sub_pause` now stops delivery to the lane it
  names.** … A lane opened by `resources/subscribe` is still delivered by
  the per-peer fan-out and is not gated (`src/adapters/subscription/lane_bridge.rs`,
  `src/adapters/subscription/subscriber_lane.rs`,
  `src/ports/subscriber_lane.rs`)."
- `docs/MIGRATION.md`, after the `sub_*` table (today `:639`):
  old — none; new — the "Pause delivery." paragraph at spike `:641`. Rows
  `:631-632` stay as written: they sit under `## v4 → v5` →
  `### Additive surface` → `#### Nine new tools (Phase 3)`, v5.0 history.
- `docs/API.md:1126`: old — "Suspend the lane drain loop. Producer keeps
  emitting; the mpsc fills under the lane's lag policy."; new — "Suspend
  delivery for the lane. The producer and the resource ring keep running;
  the consumer reads the missed range from its cursor at resume." followed
  by "Gates the lanes opened by `sub_open`. A lane opened by
  `resources/subscribe` is delivered by the per-peer fan-out and keeps
  delivering while paused."
- `docs/API.md:1138`: old — "Resume the drain loop after `sub_pause`."; new
  — "Resume delivery after `sub_pause` and emit one catch-up
  `notifications/resources/updated` for the lane." followed by the
  inline-host paragraph.
- `docs/OPERATIONS.md:79`: old — "suspended the drain loop … the lane mpsc
  fills under its lag policy"; new — "suspended delivery for that lane …
  the resource ring fills under its cap".
- `docs/OPERATIONS.md:261`: old — "the subscriber lane mpsc is paused
  (`sub_pause`)"; new — "delivery for that lane is paused (`sub_pause`)".
- `docs/ARCHITECTURE.md:376`: old — "suspend drain without disconnecting";
  new — "suspend delivery without disconnecting".
- `session.md:108-109`: append the "Repaired in feature 005" sentence.
- `session.md`, after that entry: the residual defect entry at spike `:112`.
- `session.md:160-162`: extend the open item with the four untrue sentences
  and the `NOOP` status, at spike `:171`.

## Constitution

- **D-01 spec first** — the change follows the clarified specification and
  the spike evidence in `research.md`.
- **D-02 dogfooding** — `speckit validate` and `verify --changed` close the
  loop; no rule is weakened.
- **D-03 layering** — the new seam is a port trait; the adapter names the
  trait, not the bridge.
- **D-04 DDD** — the CUE schema tags every element `ValueObject`.
- **D-05 calisthenics** — the new tests are small and single-purpose; the
  schema stays under the definition and field ceilings.
- **D-06 FQCN** — every new import is a full `crate::…` path.
- **D-07 lints and pinning** — no lint disabled, no `#[allow]` or
  `#[expect]` added; no dependency moves.
- **D-08 fail loud** — no `unwrap`, `expect` or `panic!` in production.
- **D-09 licence** — untouched.
- **D-10 guard-mediated writes** — every write goes through the guard.
- **D-11 stable contracts** — tools, arguments, response lines, error codes
  and environment variables are unchanged.
- **D-12 small changes** — one concern, one revertible unit.
- **D-13 offline** — no network path is added.
- **D-17 dependency surface** — no dependency changes.
- **P-01 inward dependencies** — the resume use case orchestrates `LaneAdmin`
  and `LaneNotifierBridge`; no adapter names another adapter.
- **P-02 lock-free hot paths** — one `Acquire` load on the existing
  `AtomicBool`; the cold-path sink lives behind `ArcSwap`.
- **P-03 panic-free and justified suppressions** — no suppression is added;
  the production path propagates or logs.
- **P-04 frozen wire** — both notification methods, the response lines and
  the structured twins keep their bytes.
- **P-05 additive wire** — no tool, argument, error code or environment
  variable is added or edited.
- **P-06 verified-change gate** — the verification plan is the gate.
- **P-07 dev profile** — no release build.
- **P-08 commits and decisions** — ADR 0005 records the decision; one commit.
- **P-09 en-US** — every artifact is en-US.

## Rollback

Reverse-apply `/var/tmp/ssh-mcp/pause-spike.patch` as one unit. The flag
returns to being cosmetic, the `resume` return value and `notify_lane` leave, and the six
document corrections revert with the code. No data, configuration or wire
state is involved.

## Open Decisions

None. The three spike questions are answered in `research.md`; the
`resources/subscribe` residual is pinned by a test and recorded.

## Companion Artifacts

- `research.md` — the evidence: path inventory, decision evidence, the
  in-flight bound, the base-copy results, the gate table and the patch.
