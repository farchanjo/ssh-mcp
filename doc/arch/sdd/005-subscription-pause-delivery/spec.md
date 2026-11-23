---
id: 01a20248-1453-70e1-8c63-b3418c3db16c
number: 005
slug: subscription-pause-delivery
status: implemented
created_at: 2026-11-22T08:03:27.955380677Z
---
# Feature Specification: Subscription Pause Delivery

Feature: 005-subscription-pause-delivery
Created: 2026-11-22

`sub_pause` sets `LaneState::paused` and nothing reads it for delivery. The
field is written by `pause_lane` and `resume_lane`
(`src/adapters/subscription/subscriber_lane.rs:832`, `:842`) and read only by
the `Debug` impl (`:218`) and the `sub_list` summary (`:299`); a
`findReferences` on the field returns those five lines and the declaration
(`:160`). Two delivery paths reach a lane peer and neither loads the flag:
the debouncer-driven legacy `notifications/resources/updated`
(`LaneFanoutBridge::notify_one_lane`, `src/adapters/subscription/lane_bridge.rs:280`,
ending at `ship_legacy_notify`, `:327`) and the producer-driven inline
`notifications/ssh/output` (`notify_lanes_inline`, `:512`). A paused lane
therefore keeps receiving.

The repair gates both paths on the lane and adds the one catch-up signal a
paused consumer needs at resume. Every fact below was measured on 2026-11-22
in the throwaway copy `/var/tmp/ssh-mcp/pause` made from `/root/dev/ssh-mcp`
at `aac7fc4`; every `file:line` is the repository line of that tree, checked
by printing it. No production code changes in this phase.

## Why the repair

- The tool is documented as a delivery gate: "Suspend lane drain. Subsequent
  events accumulate in the lane mpsc until sub_resume"
  (`src/infra/mcp/tool_router.rs:2441`), and
  `docs/OPERATIONS.md:103` gives `sub_resume` as the cure when a lane
  delivers nothing. A host that pauses gets traffic anyway, and a host that
  trusts the cure learns nothing.
- The flag, the port methods and the response lines already exist
  (`src/ports/subscriber_lane.rs:128-133`, `:201-214`;
  `src/application/subscription_admin.rs:314-320`, `:341-347`). The repair is
  a read on the hot path and one cold-path signal, not a new surface.
- The reproduction test
  (`crate::adapters::subscription::lane_bridge::tests::paused_lane_receives_no_delivery_until_resume`,
  added by `/var/tmp/ssh-mcp/defects-repro.patch`) fails on today's tree with
  "legacy notify must not reach a paused lane".

## Delivery paths, measured

Both paths run in the process for every transport (stdio, HTTP and the
embedded NDJSON daemon), because all three build the same composition root
(`src/composition/prod.rs:361-363`, `:544`) and the daemon's embedded rmcp
client receives the same notifications through `EmbedClient`.

| Path | Trigger | Call chain | Transport | Reads lane state today |
| --- | --- | --- | --- | --- |
| Legacy lane notify | debouncer wake, 5 s force flush, 30 s keepalive | `MemoryRegistry::broadcast` (`src/adapters/subscription/memory_registry.rs:962`) → `LaneNotifierBridge::notify_lanes` (`:973`) → `notify_one_lane` (`lane_bridge.rs:280`) → `notify_resource_updated` (`src/ports/notifier.rs:41`) | stdio, HTTP, daemon | no |
| Inline lane push | every producer byte tail | `SubscriptionRegistry::record_bytes_with_tail` (`src/adapters/subscription/legacy.rs:458`) → `ProducerForwarder::forward_record_bytes_with_tail` (`:461`) → `MemoryRegistry::record_bytes_with_tail` (`memory_registry.rs:700`) → `notify_lanes_inline` (`:718`) → per-URI FIFO consumer (`lane_bridge.rs:440`) → `notify_ssh_output` (`src/ports/notifier.rs:72`) | stdio, HTTP, daemon (relay env-gated) | no |
| Legacy per-peer fan-out | same debouncer ticks | `MemoryRegistry::broadcast` → `registry.subscribers` loop (`memory_registry.rs:985-987`) and the v4 singleton `broadcast_resource_updated` (`legacy.rs:600`) | stdio, HTTP, daemon | no |

The third path is not a lane path: it resolves peers registered by
`resources/subscribe`, while that use case opens a lane with
`LanePolicy::default()`, whose `peer` is `None`
(`src/application/subscribe_resource.rs:182-183`,
`src/ports/subscriber_lane.rs:33-46`). `notify_one_lane` returns early for a
peerless lane (`lane_bridge.rs:281-283`). A pause must stop the first two
paths for the paused lane and must leave the third, the other lanes on the
same URI and every other peer untouched.

## User Stories

- As a host implementer, I want `sub_pause` to stop delivery to the lane it
  names, so that a slow consumer can stop reading without losing the lane.
- As a host implementer, I want `sub_resume` to tell me to drain, so that the
  bytes produced during the pause are not silently lost.
- As an operator, I want a pause on one lane to leave the other lanes and
  peers on the same URI delivering, so that one slow consumer does not
  degrade the others.
- As a reviewer, I want the pause to add no buffer of its own, so that a long
  pause is bounded by the resource ring that already exists.

## Out of Scope

Each item is a recorded defect (`session.md:100-144`) or an adjacent
behaviour, with the reason it is not repaired here.

- **Lane lifetime (`auto_close`, `lease`).** Stored and never enforced
  (`src/adapters/subscription/subscriber_lane.rs:156`, `session.md:110-111`).
  A paused lane is not reaped by either, so the pause does not interact; the
  two lifetime knobs stay unenforced.
- **`resources/unsubscribe` closing the lane it opened.** It does not
  (`session.md:116-117`). Pause does not depend on it.
- **`sub_replay` delivery and the undrained mux queue.** `sub_replay` sends a
  `LaneMsg::Snapshot` into the lane mpsc
  (`src/adapters/subscription/subscriber_lane.rs:855-882`), and
  `ChannelMuxAdapter::spawn_drain` has no production caller (the only callers
  are `src/adapters/subscription/channel_mux.rs:278`, `:325`, `:362`, `:394`
  and `tests/chaos/chaos23_channel_mux_fairness_n_lanes.rs:51`), so the mpsc
  is never drained. The repair therefore delivers through the notifier, not
  through the mpsc, and a resumed lane is not made to depend on replay.
- **The inline dispatch channel dropping fragments when full**
  (`lane_bridge.rs:402-414`). A paused lane never enqueues, so it cannot
  fill the channel; after resume the existing bound applies unchanged.
- **Lag policy and `LAG_DETECTED` emission.** `lane_dispatch` runs only from
  `SubscriberLaneAdapter::produce`, which also has no production caller, so
  `lagged_drops` and `LAG_DETECTED` do not move in production. The pause gate
  is placed before the dispatch enqueue and does not change that.
- **The peerless lane opened by `resources/subscribe`.** Its delivery is the
  per-peer fan-out, which is keyed by `(peer, uri)` and not by `sub_id`; the
  wire does not return the `sub_id` either (`subscribe_impl` returns `()`,
  `src/infra/mcp/resource_handlers.rs:795-818`). Making that path
  pause-aware changes how the use case opens the lane and is a separate
  change; FR-011 pins today's behaviour and records the residual so the
  later feature must change it knowingly.
- **The frozen tool descriptions and argument descriptors.** They stay
  byte-identical; see Clarifications for the one internal wording that stays
  false.
- **Every other recorded defect**, and the conformity work on the 70 baseline
  errors.

## Functional Requirements

### FR-001 A pause stops both delivery methods for that lane

While `LaneState::paused` is `true`, neither `notify_resource_updated` nor
`notify_ssh_output` is called for that lane. The gate runs on both producer
entries: `notify_one_lane` (`lane_bridge.rs:280`) and
`notify_one_lane_inline_only` (`:291`), and again at the dispatch consumer
(`ship_one`, `:470`) so a dispatch already queued when the pause lands is
dropped. The producer itself keeps running and the resource ring keeps
filling.

**Acceptance.** The reproduction test
`crate::adapters::subscription::lane_bridge::tests::paused_lane_receives_no_delivery_until_resume`
fails on the base tree with "legacy notify must not reach a paused lane" and
passes after the repair; with the lane paused, a write to its URI records
neither a resource event nor an inline event, and the write after resume
records one of each. Each further gate has its own test:
`...::paused_lane_inline_only_path_is_gated`,
`...::prepare_inline_dispatch_skips_a_paused_lane` and
`...::queued_inline_dispatch_is_dropped_when_the_lane_is_paused`. All are
`#[tokio::test]`; the scenario stays `@planned`.

### FR-002 A resume emits exactly one catch-up notification, and only on a true-to-false transition

`resume_lane` swaps the flag and, when the previous value was `true`, emits
exactly one `notifications/resources/updated` for the lane's URI to the
lane's peer. A resume of a running lane (`false` to `false`) emits nothing.
`LaneAdmin::resume` reports the true-to-false transition as a `bool`; the
resume use case holds the `LaneNotifierBridge` and calls its `notify_lane`,
and composition passes the bridge next to the lane admin. No adapter holds
another adapter, and no strong cycle exists. The catch-up goes through the
same delivery helper as any legacy notification, so it counts as one
delivered event with zero bytes. A failed send is logged at debug and does
not fail `sub_resume`.

**Acceptance.**
`crate::adapters::subscription::lane_bridge::tests::resume_after_a_quiet_pause_sends_exactly_one_notification`
asserts one recorded resource event after `pause` then `resume` with no
intervening write, and
`crate::adapters::subscription::lane_bridge::tests::resume_of_a_running_lane_sends_no_extra_notification`
asserts none. `crate::adapters::subscription::lane_bridge::tests::adapter_and_bridge_are_released_after_drop`
holds weak handles to the adapter and the bridge, drops both, and asserts the
weak handles are dead. The use case's own rule is proved in its module:
`crate::application::subscription_admin::tests::resume_of_a_paused_lane_notifies_once_with_its_sub_id`
records exactly one `notify_lane` with that `sub_id`,
`...::resume_of_a_running_lane_does_not_notify` records none, and
`...::resume_unknown_does_not_notify` returns `SUB_NOT_FOUND` and records
none. All are `#[tokio::test]`, so the scenarios stay `@planned`.

### FR-003 Inline `seq` and the absolute byte cursor stay monotonic across the pause

While paused, the inline payload is still composed so `seq` and
`LaneState::cursor` advance; the fragments are dropped unsent. This keeps
`cursor_after` equal to the absolute produced offset instead of drifting by
the paused byte count. After resume, the next shipped fragment carries a
`seq` that jumped by the dropped fragment count and a `cursor_after` that
covers the paused bytes; a consumer that reads the gap from its own cursor
and then resumes the inline stream sees each byte once.

The documents tell a host what a gap means. `docs/API.md:1329` says `seq` is
a "Monotonic per-lane sequence (starts at `0`). Use to detect drops."
`docs/adr/0012-inline-push-notifications.md:710` says the host reassembles by
concatenating the bytes in `seq` order, and `:711` says a host that wakes
after a lag receives the marker and the next payload starts from a new `seq`
with `cursor_after` reflecting the post-drop cursor. `docs/LLM_GUIDE.md:522`
says to fall back to `resources/read?cursor=<cursor_after>` on a missed
notification. A paused window is a drop of exactly that shape, so the
composition runs, `seq` gaps by the dropped fragment count, and
`cursor_after` stays the absolute produced offset.

**Acceptance.**
`crate::adapters::subscription::lane_bridge::tests::paused_inline_lane_keeps_cursor_and_seq_monotonic_across_the_gap`
writes 5 bytes, pauses, writes 5 more, resumes, writes 4 and asserts two
inline events: `seq` 0 then 2, `cursor_after` 5 then 14, the second payload
`cccc`. It is a `#[tokio::test]`; the scenario stays `@planned`.

### FR-004 Isolation: another lane on the URI and another peer keep delivering

The gate reads the lane, not the URI and not the peer. On one fan-out tick,
a paused lane is skipped and every other lane is delivered normally. A
second lane opened by another peer on the same URI is unaffected.

**Acceptance.**
`crate::adapters::subscription::lane_bridge::tests::pause_does_not_gate_another_lane_on_the_same_uri`
pauses one of two lanes on `shell://x/output`, writes once and asserts the
single recorded event belongs to the unpaused peer. It is a `#[tokio::test]`;
the scenario stays `@planned`.

### FR-005 The edge cases keep today's outcome

- Pause of an already paused lane stores `true` again and returns `OK`.
- Resume of a lane that is not paused returns `OK` and sends nothing (FR-002).
- Pause or resume of an unknown `sub_id` returns `SUB_NOT_FOUND` (rendered
  from `DomainError::SubNotFound` by `render_tool_error`, as today).
- A lane closed while paused is removed; resume returns `SUB_NOT_FOUND`.
- A resource that ends while the lane is paused is covered by the resume
  notification: the terminal wake is suppressed like any other, and the
  consumer's read after resume returns the terminal `status`.
- The documented `NOOP` status stays unimplemented: the render always emits
  `OK` (`src/infra/mcp/render/subscription.rs:278-287`). FR-007 freezes that.

**Acceptance.**
`crate::adapters::subscription::lane_bridge::tests::pause_unknown_lane_returns_sub_not_found`
and `crate::adapters::subscription::lane_bridge::tests::resume_of_a_lane_closed_while_paused_returns_sub_not_found`
assert the two `SUB_NOT_FOUND` cases; the existing
`crate::adapters::subscription::subscriber_lane::tests::pause_resume_round_trip`
still asserts the `OK` round trip and stays green. All are `#[tokio::test]`;
the scenarios stay `@planned`.

### FR-006 Observability: `sub_list` shows the pause and delivery counters freeze

`sub_list` already renders `paused=true|false`
(`src/infra/mcp/render/subscription.rs:378-379`, structured twin `:395`).
While paused, `events_sent`, `bytes_sent`, `inline_events_sent` and
`inline_bytes_sent` stay at their pre-pause values, because the counters
move only in `record_notify` and `ship_inline_fragments`. The catch-up counts
as one delivered event with zero bytes; `bytes_sent` moves on the next
delivered fragment. `sub_stats_all` and `queue_depth` are unchanged, because
the mpsc is not fed (output of scope).

**Acceptance.**
`crate::adapters::subscription::lane_bridge::tests::paused_lane_counters_do_not_advance`
asserts all four counters at zero after a paused write, then `events_sent`
and `bytes_sent` at 2 and 2 and the inline counters at 1 and 2 after resume
and one 2-byte write. `crate::infra::mcp::render::subscription::tests::list_render_emits_one_line_per_summary`
already asserts the two `paused=` renderings and stays green. The counter
test is a `#[tokio::test]`; its scenario stays `@planned`. The render
scenario is bound because the test is a synchronous `#[test]`.

### FR-007 No wire change: tools, arguments, responses and errors keep their bytes

No tool, argument, descriptor, response line, status value, error code,
notification method or environment variable is added, removed or renamed.
`sub_pause` keeps `SUB_PAUSE: OK`, `SUB_ID: <id>`, `STATE: paused` and the
structured twin `{"tool":"sub_pause","status":"ok","sub_id":...,"paused":true}`;
`sub_resume` keeps `STATE: resumed` and `"paused":false`. The two argument
descriptors (`src/infra/mcp/args/subscription.rs:116-128`) and every tool
description (`src/infra/mcp/tool_router.rs:2441`, `:2465`) stay
byte-identical, so both generated catalogue files stay unchanged.

**Acceptance.** `make tools-export-check` reports `unchanged` for both
catalogue files; the three synchronous render tests
`crate::infra::mcp::render::subscription::tests::pause_resume_renders_distinct_state_labels`,
`crate::infra::mcp::render::subscription::tests::pause_structured_paused_is_true`
and
`crate::infra::mcp::render::subscription::tests::resume_structured_paused_is_false`
stay green with their assertions unchanged. The scenario is bound to
`pause_resume_renders_distinct_state_labels`.

### FR-008 The gate is green with the reference counts plus the new tests

The verified-change gate of P-06 (`doc/arch/memory/constitution.md:109-112`)
passes, run from `/root/dev/ssh-mcp`:

- `cargo fmt --all -- --check`
- `cargo clippy --all-features -- -D warnings`
- `cargo nextest run --test-threads num-cpus --lib` (at least 2141 passing;
  2125 base plus the sixteen tests of FR-001 to FR-006 and FR-011)
- `cargo nextest run --test-threads num-cpus --tests --features test-fixtures`
  (at least 2315 passing)
- `cargo build --no-default-features --target-dir target/no-default-features`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_http.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_transfer_progress.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v7_rsync_http.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v47_subscriptions.py`
- `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_v47_idempotency.py`
- `cargo audit`

No lint is disabled, no `#[allow]` or `#[expect]` is added, no test is
ignored, removed or weakened. The only edit to the reproduction test is its
final assertion, which counts the resume notification (FR-002).

P-02 (`doc/arch/memory/constitution.md:93-96`) asks a change that touches a
hot-path atomic for a loom invariant. The gate is an `Acquire` load of the
existing `paused` flag, paired with the `Release` store in `pause_lane`; the
existing `loom_lane_pause_resume_no_loss` (`tests/lockfree_invariants.rs:687`)
already models that pair and stays, with its comment updated to name the
delivery gate. Loom is not part of the gate because full mode is blocked
upstream (`docs/DEVELOPMENT.md:137`); the file compiles to empty without
`--cfg loom`.

**Acceptance.** Each command exits 0; the two nextest runs report at least
the counts above; `git diff` shows no `#[ignore]`, no removed test and no new
`#[allow]` or `#[expect]`. The scenario stays `@planned` (it is decided by
commands, not by a test).

### FR-009 The change is recorded and the documents that describe the pause are corrected

`CHANGELOG.md` records the repair under `[Unreleased]` → `### Fixed`,
`docs/MIGRATION.md` records what a host and an operator must know, the defect
entry at `session.md:108-109` is marked repaired with this feature while
keeping its text and `file:line`, and the standing corpus is reconciled:
`doc/arch/functional/functional.md:220-236` replaces the "no delivery path
reads the flag" limitation with the current behaviour and the open residual,
and `doc/arch/runbooks/incident-push-delivery-lost.md:101-111` says the
`sub_resume` cure works for a `sub_open` lane and keeps the workaround for
the residual only. The sentences that describe the
implementation rather than the observable and would otherwise stay false are
corrected:

| Document | Line | Today | Required |
| --- | --- | --- | --- |
| `docs/API.md` | 1126 | "Suspend the lane drain loop. Producer keeps emitting; the mpsc fills under the lane's lag policy." | delivery stops for the lane; the producer and the resource ring keep running; resume sends one catch-up notification and the consumer reads from its cursor |
| `docs/API.md` | 1138 | "Resume the drain loop after `sub_pause`." | resume delivery and emit the catch-up notification |
| `docs/OPERATIONS.md` | 79 | "suspended the drain loop" | "suspended delivery for that lane" |
| `docs/OPERATIONS.md` | 261 | "the subscriber lane mpsc is paused (`sub_pause`)" | delivery for that lane is paused |
| `docs/ARCHITECTURE.md` | 376 | "suspend drain without disconnecting" | "suspend delivery without disconnecting" |

Three records name the old behaviour but are history and stay as written:
`docs/adr/0004-channel-mux-fairness.md:136-137`, and `docs/MIGRATION.md:631-632`
inside the v5.0 release notes (heading `## v4 → v5`, subsection
`### Additive surface`, `#### Nine new tools (Phase 3)`). A later release-notes
edit does not rewrite the v5.0 note, so the five sentences above are the
complete correction set. `docs/TOOL_CATALOG.md` and `docs/tool-catalog.json` are generated
from descriptions that do not change, so they stay as written (FR-007).

`docs/API.md` also gains the host-facing consequence: under `sub_pause` it
names `sub_open` as the gated path and states that a lane opened by
`resources/subscribe` is not gated (FR-011), and under `sub_resume` it tells
a host with an inline lane that the next fragment's `cursor_after` jumps over
the paused bytes and that it must read the gap with
`resources/read?cursor=<last cursor_after>`. `session.md` gains one defect
entry for the `resources/subscribe` residual (FR-011) and extends the
existing "Open items for the owner" item about tool descriptions with the
three sentences that stay false (the `sub_pause` "flush queued events in
order", the `sub_resume` "Queued events flush in order, subject to
lag_policy" and "the next push emits `LAG_DETECTED`") and with the `NOOP`
status both descriptions list and the server never returns.

**Acceptance.** The three files carry the record; the five corrected sentences
read as required; the API guidance and the two `session.md` additions exist;
the historical entries are unchanged. `grep -n 'drain loop' docs/API.md
docs/OPERATIONS.md` prints nothing; `grep -n 'drain loop' docs/MIGRATION.md`
prints only `:631` and `:632`, under the v5.0 heading above;
`grep -n 'no delivery path' doc/arch/functional/functional.md
doc/arch/runbooks/incident-push-delivery-lost.md` prints nothing. The
scenario stays `@planned`.

### FR-010 `speckit validate` adds no finding to the baseline

The baseline is the error set `speckit validate` reports on the tree before
this feature with the binary in use, `speckit v0.49.0`
(`cdb45907 2026-11-22T08:38:26Z`): exit 6 with 68 errors and no warning.
Rules: `fqcn.relative-super` 46, `branch.else` 6,
`pin.ci.unpinned-action` 5, `pin.docker.unpinned-image` 2, and one each of
`readme.mermaid.missing`, `readme.index.missing`, `readme.image.missing`,
`readme.arch-link.missing`, `pin.cargo.lockfile-missing`,
`llms-txt.file.missing`, `fqcn.glob-import`, `crew.roster.dangling` and
`ci.gitlab.missing`. The earlier features record 70 because the binary was
replaced during this session (they ran `v0.48.2`): `v0.49.0` no longer
reports the two `let ... else { return; }` sites as `branch.else`, and it
adds one `llm-context.regeneration.drift` error against `AGENTS.md`, which
`speckit generate agents` closed before this baseline. No rule is waived and
no feature-005 file is named.

**Acceptance.** `speckit validate` reports 68 errors with that rule set and
no warning, and no finding names a file of this feature. The scenario stays
`@planned`.

### FR-011 The `resources/subscribe` residual is pinned and recorded

A host can reach the peerless lane: `sub_list` renders every lane through
`SubscriberLanePort::list_subs` (`src/adapters/subscription/subscriber_lane.rs:708-713`),
including the one `resources/subscribe` opens with `LanePolicy::default()`
(`src/application/subscribe_resource.rs:182-183`). Pausing that `sub_id`
returns `OK`, but its delivery is the per-peer fan-out
(`memory_registry.rs:985-987`), which the gate does not reach, so it keeps
delivering. The residual is stated in Out of Scope, recorded in the
`session.md` defect list, and pinned by a test that must keep passing until
the feature that associates a `(peer, uri)` pair with its lane changes it.

**Acceptance.**
`crate::adapters::subscription::memory_registry::tests::resources_subscribe_lane_is_listed_but_pause_does_not_gate_the_peer_fanout`
subscribes a peer, opens a peerless lane for the same URI, asserts `sub_list`
contains the `sub_id`, pauses it, triggers a byte-threshold flush and asserts
the peer event arrives while the lane counters stay at zero. It is a
`#[tokio::test]`; the scenario stays `@planned`.

## Security Requirements

- **Data sensitivity/classification.** The pause copies no bytes. The
  notification carries the URI only; the shell and command bytes stay in the
  resource ring that already holds them (default cap 10 MiB,
  `src/adapters/config/internal/mod.rs:93`, `:97`).
- **Authentication/authorization.** No authenticated surface and no credential
  moves; the pause gate is in-process state.
- **Input validation.** `sub_id` is an opaque string. An unknown value takes
  the existing `DomainError::SubNotFound` path; no new parser is added.
- **Cryptography in transit/at rest.** None is added; the notifications keep
  their transport.
- **Logging/audit.** No new log line. A failed catch-up send reuses the
  existing debug line in `ship_legacy_notify` (`lane_bridge.rs:335-337`) and
  does not fail the tool.
- **Error-handling information exposure.** No error code or message changes.
  The catch-up notification is at-most-once, like the rest of the notifier
  port; a missing peer returns `Ok`.

## Acceptance Scenarios

Given a lane with inline push on and its peer
When  the lane is paused and a write reaches its URI
Then  neither a resource event nor an inline event is recorded for it (FR-001)
And   the write after resume records one of each (FR-001).

Given a paused lane
When  `sub_resume` runs
Then  exactly one `notifications/resources/updated` is recorded for that lane (FR-002).

Given a running lane
When  `sub_resume` runs
Then  no catch-up notification is recorded (FR-002).

Given an inline lane that produced 5 bytes before the pause and 5 during it
When  4 more bytes are produced after resume
Then  the shipped fragments are `seq` 0 / `cursor_after` 5 and `seq` 2 /
`cursor_after` 14 (FR-003).

Given two lanes from two peers on one URI
When  one is paused and the URI is fanned out
Then  only the unpaused peer is recorded (FR-004).

Given an unknown `sub_id`
When  `sub_pause` or `sub_resume` runs
Then  the response carries `SUB_NOT_FOUND` (FR-005).

Given a lane paused with no production
When  `sub_resume` runs
Then  `events_sent` and `bytes_sent` stay 0 and 0, and `sub_list` renders
`paused=false` (FR-006).

Given the repaired tree
When  `make tools-export-check` and the three render tests run
Then  both catalogue files are `unchanged` and the render assertions hold (FR-007).

Given the repaired tree
When  the gate commands of FR-008 run
Then  each exits 0 with at least 2141 lib tests and 2315 tests with the
integration suites (FR-008).

Given the change landed
When  a reader opens `CHANGELOG.md`, `docs/MIGRATION.md` or `session.md`
Then  each carries its record and the six listed sentences read as required (FR-009).

Given a peer subscribed through `resources/subscribe` and a peerless lane on the same URI
When  the lane is paused and a byte-threshold flush fires
Then  the peer event arrives and the lane counters stay at zero (FR-011).

Given the feature's artifacts on disk
When  `speckit validate` runs over the corpus
Then  the error count stays at 70 with the same rule set and no warning fires (FR-010).

## Accepted Risks

- **AR-1 The catch-up signal is a legacy notification, not an inline
  fragment.** A host that ignores `notifications/resources/updated` on an
  inline lane gets no explicit resume signal; it resumes on the next producer
  write, which is exactly when new bytes exist, and the debouncer's 5 s force
  flush also fires one. Accepted: the inline mode is documented as additive
  (`src/infra/mcp/tool_router.rs:2441`), and a quiet lane has nothing to
  miss. Blocks FR-002.
- **AR-2 A resume of a lane with nothing missed sends one spurious
  notification.** Accepted: one event per resume is cheaper than a per-lane
  "produced while paused" flag, and the read that follows returns an empty
  slice. Blocks FR-002.
- **AR-3 At most one in-flight fragment can land after `sub_pause` returns.**
  A fragment already awaiting `notify_ssh_output` when the flag is stored
  completes; the gate at `ship_one` drops the queued ones. Accepted: the
  delivery port is at-most-once and a per-lane barrier would need a lock on
  the hot path. Blocks FR-001.
- **AR-4 `events_sent` counts the catch-up notification.** Accepted: it
  counts delivered notifications and the catch-up is one, so a host that
  compares the counter with what it received is off by one only per resume.
  `bytes_sent` is unaffected. Blocks FR-006.
- **AR-5 The reproduction test's final assertion changes from 1 to 2 resource
  events.** The resume notification sends one before the post-resume write
  sends another. Accepted and recorded; the inline assertion stays 1. Blocks
  FR-001 and FR-008.

## Observability

The feature adds no metric, log event, NDJSON event or trace span and
changes no target, level, format or destination. `sub_list` keeps rendering
`paused=true|false`; `sub_stats` keeps its counters and freezes the four
delivery counters while the lane is paused; `sub_stats_all` aggregates the
same values. Conventions stay those of
`doc/arch/observability/observability.md`.

## Clarifications

### Session 2026-11-22

- **Decision provenance.** Every decision in this specification was taken in
  this session on the owner's instruction to carry the whole update plan
  forward, and it is recorded for the owner's review.
- **Q-1 answered: the frozen wording is only partly true, and it stays.**
  The descriptions say more than "accumulate". A lane without inline push
  observes the equivalent of the sentence: nothing arrives while paused, one
  notification arrives at resume, and `resources/read?cursor=auto` returns
  every missed byte from its own cursor. A lane with inline push does not:
  the paused bytes are never sent as `notifications/ssh/output`, the next
  fragment's `cursor_after` jumps over them, and the host must read the gap
  itself. The descriptions also say events "flush in order" and that "the
  next push emits `LAG_DETECTED`", and neither happens literally (no mpsc
  drain, no dispatch). The descriptions are frozen by P-04 and P-05;
  `session.md` records the three sentences and the unimplemented `NOOP`
  status under the owner's open items (FR-009), and `docs/API.md` tells the
  inline host what it observes and what to do. No owner question.
- **Q-2 answered: one catch-up notification at resume, then pull.** It costs
  one notifier call, touches the adapter, the bridge and composition, and
  uses the notifier and the cursor that already exist. Re-sending the paused
  bytes as inline fragments is the recorded alternative. It needs no new
  per-lane store: the bytes are in the resource ring, but the lane cursor is
  relative to the lane, so the missing piece is the mapping from lane cursor
  to ring offset; with that mapping the resume could read the ring and emit
  fragments. It would also duplicate bytes the consumer may already have read.
  It is the fallback if AR-1 is later rejected.
- **Q-3 answered: compose while paused, drop the fragments.** This keeps
  `seq` and `cursor_after` on the documented drop contract (FR-003) at the
  cost of one transient payload per producer write; it stores nothing.
  Advancing only the cursor would make `seq` contiguous, so a host told to
  use `seq` "to detect drops" (`docs/API.md:1329`) would not see the pause
  gap, and `cursor_after` would under-count every paused byte for the rest of
  the lane life.
- **Q-4 answered: the peerless `resources/subscribe` lane is a pinned
  residual, not repaired here.** `sub_list` lists it with its `sub_id`
  (`src/adapters/subscription/subscriber_lane.rs:708-713`), so a host can
  pause it and get `OK` while the per-peer fan-out keeps delivering. FR-011
  pins it and FR-009 records it; the feature that associates `(peer, uri)`
  with its lane closes it.
- **Q-5 answered: `NOOP` stays documented and unimplemented.** Both
  descriptions list it (`src/infra/mcp/tool_router.rs:2441`, `:2465`) and the
  render always emits `OK`; keeping `OK` moves no response byte (FR-007).
  The open item of Q-1 carries it.
- **Q-6 answered: the reproduction test is adapted in one line.** Its final
  `resource_events` count becomes 2 for the resume notification; the inline
  count stays 1 (AR-5).
