# Implementation Plan: Sub Open Resource Check

## Overview

`sub_open` gains one guard: after the URI parses and before any lane exists,
the four repository-backed kinds are checked and a missing `shell`, `command`,
`transfer` or `session` id returns the per-kind not-found variant. `forward`,
`serial` and `rsync` stay permissive. The guard is installed by the production
composition; `SubscribeUseCase::new` stays permissive, so the 25 existing
`new` call sites and the 25 tests that use them do not change.

The spike at `/var/tmp/ssh-mcp/guard-spike` carries code, tests, scripts and
documents to a green gate. `research.md` carries the base results, the gate
table, the four mutation checks, the validate result and the patch.

## Technical Approach

### 1. Where the policy lives and how the use case reaches it (FR-004, D-1)

Chosen: **a cold-path, object-safe trait and its implementation both in
`src/application/resource_guard.rs`**, with the guard injected through an
optional builder (`with_resource_guard`) beside `with_activator`.

The alternatives, each measured against the layer table
(`docs/ARCHITECTURE.md:110-114`) and P-01
(`doc/arch/memory/constitution.md:89-91`):

- **(a) Port trait plus adapter holding the four repositories.** The kind
  table moves into an adapter and `SubscribeResourceUseCase` loses its four
  repository parameters. Cost: new port file, new adapter file, both use cases
  and their tests re-parameterized; the table leaves the layer that owns the
  policy. P-01 is satisfied, but the four-repository generic of
  `SubscribeResourceUseCase` (`src/application/subscribe_resource.rs:66-80`)
  becomes dead and would be removed, widening the diff.
- **(b) Four repositories as type parameters of `SubscribeUseCase`.** P-01's
  static-dispatch preference is met, but all 25 `new` sites in three files
  (`src/application/subscription_admin.rs`, `tests/v5_smoke.rs`,
  `tests/chaos_inline_push.rs`) change, 24 of them on a checked kind with no
  seeded row, so each also needs a repository harness. Feature 006 had to undo
  25 mechanical edits; (b) repeats that.
- **(c) Port trait implemented by an `application` type.** Same shape as (d)
  with the trait in `src/ports/`. The layer table does not forbid it, but no
  non-test `application` type implements a `ports` trait today (only
  `#[cfg(test)]` stubs; the nearest precedent is `NoopLifecycle`, a `ports`
  type), so it would be the first and the trait would invite an adapter
  implementor that does not exist.
- **(d) Trait and implementation in `src/application/`.** The guard is
  application policy over repository ports; no adapter implements it. P-01
  allows one cold-path `dyn` (`SubscribeUseCase` already holds
  `Arc<dyn LaneAdmin>`, `src/application/subscription_admin.rs:82`), the kind
  table stays in one application file, and no existing call site changes.

(d) costs: one new file (~95 lines), one field, one three-line builder, one
private six-line method, eight lines in `src/composition/prod.rs` and one
`pub(crate)` on `ensure_exists`. All 25 `new` sites stay byte-identical.

The guard is **optional** on `new` and mandatory in production through
`with_resource_guard`; FR-011 is the proof that composition installs it, and
the mutation of §5(c) shows the unit tests stay green when it is removed.

### 2. Order inside `execute` (FR-001, FR-002, D-2)

`SubscribeUseCase::execute` becomes: parse → `self.ensure_exists(kind, id)` →
canonical URI → `LaneAdmin::open` → activator → inline-push handshake →
outcome.

A refusal returns the variant from the guard (`ShellNotFound`,
`CommandNotFound`, `TransferNotFound`, `SessionNotFound`) with the parsed id.
It happens before `open`, so there is no lane row, no per-URI or global slot,
no stats row and no debouncer call; `ensure_exists` also runs before the
regex compile inside `open`, so a bad filter on a missing id reports the
missing id first. `NoopLifecycle` is not involved (the use case holds none).

### 3. `ensure_exists` (FR-004, D-3)

`ensure_exists` and its four helpers stay in
`src/application/subscribe_resource.rs`; the function becomes `pub(crate)`
(`:156`), the helpers keep `async fn` privacy. `ResourceGuard::ensure` is the
only new caller; `SubscribeResourceUseCase::execute` keeps its existing call
(`:124`). `grep -rn 'ResourceKind::Shell => ensure_' src` finds one table, and
`grep -rn 'ensure_exists' src` finds the declaration, the two callers and the
guard. No behavior of `resources/subscribe` changes and its five tests stay
green.

### 4. Tests (D-4)

Every new test builds real `DashMap` repositories and a real
`SubscriberLaneAdapter`; a `RecordingActivator` is the only double, for FR-002.
No test under `src/adapters/` imports `crate::application`.

| Test | Module | Builds | Asserts | FR | Base |
| --- | --- | --- | --- | --- | --- |
| `subscribe_unknown_shell_is_refused` | `crate::application::subscription_admin::tests` | guarded use case, empty repos | `ShellNotFound` with the id | FR-001 | FAIL |
| `subscribe_unknown_command_is_refused` | same | same | `CommandNotFound` with the id | FR-001 | FAIL |
| `subscribe_unknown_transfer_is_refused` | same | same | `TransferNotFound` with the id | FR-001 | FAIL |
| `subscribe_unknown_session_is_refused` | same | same | `SessionNotFound` with the id | FR-001 | FAIL |
| `refused_sub_open_leaves_no_lane_and_no_debouncer` | same | lane adapter + `RecordingActivator`; shell seeded after the refusal | error, `list_subs` empty, no recorded URI, the same URI opens after the seed | FR-002 | FAIL |
| `refused_then_created_resource_can_be_subscribed` | same | shared shell repo | first call refused, second accepted | FR-001, FR-002 | FAIL |
| `forward_serial_and_rsync_stay_permissive` | same | empty repos | three lanes open | FR-003 | PASS (pins) |
| `completed_command_row_is_accepted` | same | command row, then `Completed` | the guarded call opens | FR-012 | PASS (pins) |
| `completed_transfer_row_is_accepted` | same | transfer row, then `Completed` | the guarded call opens | FR-012 | PASS (pins) |
| `sub_open_refusal_answers_per_kind_code` | `crate::infra::mcp::tool_router::tests` | guarded use case through `run_sub_subscribe` | `SUB_OPEN: ERROR`, per-kind `REASON`/`DETAIL`, structured `code` | FR-005 | FAIL |
| `sub_open_refusal_renders_per_kind_code` | same | `render_tool_error` only | the four `REASON` codes | FR-005 | PASS (pins) |
| `refused_sub_open_is_not_cached_under_its_idempotency_key` | same | `with_idempotency_keyed`, same key before and after the seed | refusal not cached; retry re-runs | FR-013 | FAIL (new API) |
| `production_sub_subscribe_refuses_missing_resources_and_opens_forward` | `tests/sub_open_resource_check.rs` | `build_use_cases` | four refusals, one forward success | FR-011 | FAIL |

The integration file carries the crate-level `#![allow(clippy::unwrap_used,
reason = "…")]` every integration file carries; it holds one test because
`build_use_cases` replaces the process-wide forwarder
(`src/composition/prod.rs:542-544`).

First fate table: every row keeps its subject. The 17 unit `new` calls, the
five `v5_smoke` and the three `chaos_inline_push` calls stay as they are; the
five adversarial cases and the flush-bytes case are adapted to
`forward://…/events`; `chaos_v5_subscribe.py` is not a gate command (it is
already broken, `session.md`).

### 5. Documents (FR-006, D-5)

| File:line | Old | New |
| --- | --- | --- |
| `docs/API.md:1084` | URI row | adds that the four repository kinds are refused and the three permissive kinds are not |
| `docs/API.md:1106` | errors list starts `RESOURCE_GONE` | four `*_NOT_FOUND` codes; `RESOURCE_GONE` noted as not emitted here and a `Closed` row accepted |
| `docs/OPERATIONS.md:176-178` | section says `sub_open` returns `RESOURCE_GONE` | the section keeps `resources/subscribe` as the subject; `sub_open` returns the per-kind code and never `RESOURCE_GONE` |
| `docs/LLM_GUIDE.md:921` | reconnect failure mode `RESOURCE_GONE` | four `*_NOT_FOUND`; `sub_open` does not return `RESOURCE_GONE` |
| `docs/LLM_GUIDE.md:1429` | `ssh_shell_*` only | adds `sub_open` |
| `docs/LLM_GUIDE.md:1439` | `ssh_exec_output` / `ssh_exec_cancel` | adds `sub_open` |
| `docs/LLM_GUIDE.md:1449` | `ssh_transfer_progress` | adds `sub_open` |
| `docs/LLM_GUIDE.md:1469-1470` | a subscribe to `Closed` is refused | `resources/subscribe` refuses; `sub_open` accepts a live row |
| `docs/RESOURCES.md:96` | — | the four kinds need a live id; the three permissive kinds do not |
| `CHANGELOG.md:15` | — | one `### Changed` bullet |
| `docs/MIGRATION.md:882` | — | one bullet under the current section |
| `doc/arch/functional/functional.md:247-251` | "`sub_open` accepts a URI whose resource does not exist" | repaired; the description mismatch stays |
| `doc/arch/functional/functional.md:343` | — | one F8 row bound to the two application tests |
| `doc/arch/runbooks/incident-push-delivery-lost.md:11` | "the call returned an id" | OK, id only for `sub_open` |
| `doc/arch/runbooks/incident-push-delivery-lost.md:110` | — | a missing id is refused; the check/open window still returns an id |

`docs/MIGRATION.md:826` is history and stays. The catalogue stays
`unchanged`.

### 6. Gate (FR-008, FR-009, FR-010, D-6)

The command list of FR-009 plus `cargo check --all-features --all-targets`,
`cargo build --bins` and `make tools-export-check`, run from the repository
root, one cargo invocation at a time, then `speckit validate`. Expected:
2154 lib and 2329 with integration (2142 + 12 lib tests: four FR-001, two
FR-002, one FR-003, two FR-005, one FR-013 and two FR-012; 2316 + 13 with the
integration file), and the two adapted scripts at 63 and 1 passed.

### 7. Re-anchoring (D-7)

Every `file:line` this feature writes into a standing document is read from
the final spike tree after the code change: the two `functional.md` anchors,
the runbook line and every `session.md` entry that cites
`src/application/subscription_admin.rs` or `src/composition/prod.rs`. Check:
`grep -n` each cited line on the spike and confirm the symbol. Citations in
`spec.md`, `research.md` and the ADR describe the pre-change tree.

## Decisions

### D-1 Placement and injection

Chosen: (d), optional builder. The guard is application policy over repository
ports, no adapter implements it, and `SubscribeUseCase` already carries a
cold-path `dyn` handle. Cost: one application file, one field, one builder,
one private method, eight composition lines. Alternatives (a), (b) and (c) are
compared in §1; (b) is rejected mainly for the 25-site mechanical edit.

### D-2 Refusal before side effects

The guard call is the first statement after the parse and before the canonical
URI is built. A refusal returns the variant with the parsed id; no other early
return is added. This is what makes FR-002 true by construction rather than
by cleanup.

### D-3 One table, one module

The kind table stays in `subscribe_resource.rs`; the guard is a thin caller.
No adapter imports `crate::application`; `SubscribeResourceUseCase` keeps its
four generics and its five tests.

### D-4 Tests and their base result

The table of §4 is the complete set. Tests that pass on the base tree stay as
pins (permissive kinds, terminal rows, renderer mapping); the rest fail on the
base and prove the change. The integration test is the FR-011 proof and lives
alone in its binary.

### D-5 Document scope

Only the passages that become false or silent are edited. Release history
(`docs/MIGRATION.md:826`) is not corrected; the new behavior goes under the
current section. The frozen catalogue is not edited.

### D-6 Gate

The gate is FR-009 plus the two build commands and the catalogue check. Any
count difference from 2154/2329 is explained before the next step.

### D-7 Re-anchoring

All citations into changed files are read from the final spike tree; the
`session.md` defect list is re-anchored in the same step that adds the repair
mark.

## File Map

| File | Requirement |
| --- | --- |
| `src/application/resource_guard.rs` | FR-004 (trait and implementation) |
| `src/application/mod.rs` | FR-004 (module) |
| `src/application/subscribe_resource.rs` | FR-004 (`pub(crate) ensure_exists`) |
| `src/application/subscription_admin.rs` | FR-001, FR-002, FR-005 (field, builder, call, tests) |
| `src/composition/prod.rs` | FR-011 (guard wiring) |
| `src/infra/mcp/tool_router.rs` | FR-005, FR-013 (tests) |
| `tests/sub_open_resource_check.rs` | FR-011 |
| `scripts/test_v5_adversarial.py`, `scripts/test_v51_flush_bytes.py` | FR-009 (five and one case to `forward://`) |
| the documents of §5 | FR-006 |
| `session.md` | FR-007 |

No other file changes.

## Verification Plan

| # | Command | Expected |
| --- | --- | --- |
| 1 | `cargo fmt --all -- --check` | exit 0 |
| 2 | `cargo check --all-features --all-targets` | exit 0 |
| 3 | `cargo clippy --all-features -- -D warnings` | exit 0 |
| 4 | `cargo build --no-default-features --target-dir target/no-default-features` | exit 0 |
| 5 | `cargo build --bins` | exit 0 |
| 6 | `cargo nextest run --test-threads num-cpus --lib` | 2154 passed |
| 7 | `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` | 2329 passed |
| 8 | `cargo audit` | exit 0, the allowed `chrono` warning |
| 9 | `make tools-export-check` | both `unchanged` |
| 10–14 | the five Python suites | 15 / 2 / 10 / 20 / 9 passed |
| 15 | the two adapted scripts | 63 passed; 1 passed |

Mutation checks: §5 of `research.md` (guard removed, guard moved after open,
wiring removed, one permissive kind checked).

## Constitution

- **D-01 spec first** — the change follows the clarified specification.
- **D-03/P-01 layering** — one cold-path `dyn` on an existing cold-path use
  case; no adapter imports `crate::application`; no new port.
- **D-05 calisthenics** — small files; the guard method keeps `execute` under
  the 30-line ceiling.
- **D-06 FQCN** — new imports use `crate::…`.
- **D-07 lints** — no lint disabled, no `#[allow]`/`#[expect]` added.
- **D-08/P-03 fail loud** — production stays panic-free; the guard propagates.
- **D-11 stable contracts** — the only wire change is the new refusal.
- **D-12 small changes** — one concern, revertible as one patch.
- **P-02 lock-free** — the guard runs on the cold path and adds no lock.
- **P-04/P-05 frozen wire** — catalogue untouched.
- **P-06 gate** — the verification plan is the gate.

## Rollback

Reverse-apply `/var/tmp/ssh-mcp/guard-spike.patch` as one unit. The patch is
generated from `/var/tmp/ssh-mcp/final-006/tree` in `a/` and `b/` form and
carries 18 files, 752 insertions and 42 deletions; SHA-256
`7145a0581f5050043ee9308a8984d540acff6759d25ebdc4533cb0a56b172360`;
`patch -p1 --dry-run` in a scratch copy of that tree exits 0. The guard file,
the field, the builder, the call and the eight composition lines leave; the
documents revert with them.

## Open Decisions

Q-5 is settled by D-1. The owner-review items stay: the per-kind answer code
(ADR 0007) and the six script edits.
