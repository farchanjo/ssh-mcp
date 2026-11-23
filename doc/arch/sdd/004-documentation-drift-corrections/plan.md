# Implementation Plan: Documentation Drift Corrections

## Overview

Feature 004 corrects seven documents against the code they describe. No
production source, test, manifest or script moves, and no tool description
changes. The edits are prepared in
`/var/tmp/ssh-mcp/docs-drift/`, a copy that keeps the repository-relative
paths, and exported as `/var/tmp/ssh-mcp/docs-drift.patch` (`a/<path>`,
`b/<path>`). Every acceptance command of the specification passed against the
copy before this plan was written; the implement phase applies the same edit to
the repository and proves each touched document equal to its copy.

## Technical Approach

Prose only. Each edit replaces one statement with the sentence the evidence
supports. The requirements are FR-001 to FR-010 of
`doc/arch/sdd/004-documentation-drift-corrections/spec.md`; the evidence, the
daemon capture and the `DomainError` table are in `research.md`. Nothing under
`src/`, `tests/`, `Cargo.toml`, `Cargo.lock` or `scripts/` is opened for
writing.

## File map

| Document | Requirement | Lines of 2026-11-22 |
| --- | --- | --- |
| `docs/MIGRATION.md` | FR-001 | 882 |
| `docs/OPERATIONS.md` | FR-002 | 409 |
| `docs/CONFIGURATION.md` | FR-003 | 149, 150 |
| `docs/API.md` | FR-004, FR-005 | 1106, 1153 |
| `docs/DAEMON.md` | FR-006 | 147 |
| `doc/arch/threat-model/threat-model.md` | FR-004 | 754-758 |
| `session.md` | FR-009 | 100-150 |

No other document is edited. `docs/CONFIGURATION.md:153` stays as written and
is listed under Excluded statements in the specification.

## Exact edits

### `docs/MIGRATION.md:882` (FR-001)

Old, last sentence of the bullet:

```
A stale `Cargo.lock` must be re-resolved (`cargo update -p russh -p russh-sftp`) before it builds against the new lines.
```

New:

```
A stale `Cargo.lock` needs no manual step: cargo re-resolves it on the next invocation, and the result is identical to the repository lock.
```

### `docs/OPERATIONS.md:409` (FR-002)

Old:

```
- `INTERNAL_ERROR` for registry-level failures (currently never raised — `subscribe` is infallible at runtime).
```

New:

```
- `INTERNAL_ERROR` for registry-level failures — returned when a lane cap is reached: `SSH_MAX_SUBS_PER_URI` (default 16) on one URI, or `SSH_MAX_SUBS_TOTAL` server-wide. The message carries the domain display, for example `max subs per URI exceeded for forward://x/events (limit 16)`.
```

### `docs/CONFIGURATION.md:149` and `:150` (FR-003)

Old `:149` tail: `Returns `LANE_LIMIT_PER_URI` once exhausted.`
New `:149` tail: `Returns `MAX_SUBS_PER_URI_EXCEEDED` once exhausted.`

Old `:150` tail: `Returns `LANE_LIMIT_TOTAL` once exhausted.`
New `:150` tail: `Returns `MAX_SUBS_TOTAL_EXCEEDED` once exhausted.`

### `docs/API.md:1106` and `:1153` (FR-004, FR-005)

Old `:1106`:

```
**Errors**: `RESOURCE_GONE`, `LANE_LIMIT_PER_URI`, `LANE_LIMIT_TOTAL`, `FILTER_INVALID`, `INLINE_PUSH_BAD_PARAMS`, `INVALID_ARGUMENT`.
```

New `:1106`:

```
**Errors**: `RESOURCE_GONE`, `MAX_SUBS_PER_URI_EXCEEDED`, `MAX_SUBS_TOTAL_EXCEEDED`, `INVALID_ARGUMENT`.
```

Old `:1153`:

```
**Response**: `SUB_FILTER: OK`. **Errors**: `SUB_NOT_FOUND`, `FILTER_INVALID`.
```

New `:1153`:

```
**Response**: `SUB_FILTER: OK`. **Errors**: `SUB_NOT_FOUND`, `INVALID_ARGUMENT` (regex compile).
```

### `docs/DAEMON.md:147` (FR-006)

Old:

```
Errors: `SUB_NOT_FOUND` (parent resource gone), `RESOURCE_GONE`, `MAX_SUBS_PER_URI_EXCEEDED`, `MAX_SUBS_TOTAL_EXCEEDED`, `INVALID_LIFETIME`, `INVALID_LAG_POLICY`.
```

New:

```
Errors: `SUBSCRIBE_FAILED` — the dispatcher collapses every `resources/subscribe` failure into this code, and the domain message (`max subs per URI exceeded ...`, `resource gone ...`, `... not found ...`) is carried in `detail`.
```

### `doc/arch/threat-model/threat-model.md:754-758` (FR-004)

Old:

```
- The pattern length is capped at 1024 characters by default, in the range 16
  to 65536, and an over-length pattern is rejected with `FILTER_INVALID`
  (`src/adapters/config/internal/mod.rs:217`, `docs/CONFIGURATION.md:153`).
```

New:

```
- The pattern length is intended to be capped at 1024 characters by default,
  in the range 16 to 65536, but no production caller applies
  `resolve_filter_regex_max` today; a pattern that fails to compile is rejected
  with `INVALID_ARGUMENT` (`src/adapters/config/internal/mod.rs:217`,
  `docs/CONFIGURATION.md:153`).
```

### `session.md` (FR-009)

Three entries are appended to "Defects verified in the code during the session",
after the last existing bullet and before "### Open items for the owner":

```
- `src/adapters/config/internal/mod.rs:906`: `SSH_FILTER_REGEX_MAX` is resolved
  by `resolve_filter_regex_max` and applied by no production caller.
- `src/embed/parser.rs:95` and `src/embed/dispatcher.rs:578`: the daemon
  `subscribe` op parses `lifetime`, `grace_ms`, `lag_policy`, `filter` and
  `start_cursor` and sends only the URI.
- `src/domain/error.rs:209` and `:214`: `INVALID_LIFETIME` and
  `INVALID_LAG_POLICY` are documented in `docs/LLM_GUIDE.md:1665` and `:1675`
  and constructed by no production path.
```

One entry is appended to "Open items for the owner", after the last bullet:

```
- The `sub_open` and `sub_filter` descriptions in the tool catalogue name codes
  the server does not emit, and changing a description moves the frozen
  catalogue (`src/infra/mcp/tool_router.rs:2389`, `:2489`).
```

## Patch and acceptance on the copy

Patch: `/var/tmp/ssh-mcp/docs-drift.patch`. File list and stat, as produced:

```
doc/arch/threat-model/threat-model.md |    8 +++++---
docs/API.md                           |    4 ++--
docs/CONFIGURATION.md                 |    4 ++--
docs/DAEMON.md                        |    2 +-
docs/MIGRATION.md                     |    2 +-
docs/OPERATIONS.md                    |    2 +-
session.md                            |   12 ++++++++++++
7 files changed, 24 insertions(+), 10 deletions(-)
```

Every acceptance command of the specification, run with the working directory
`/var/tmp/ssh-mcp/docs-drift`:

| FR | Command | Result |
| --- | --- | --- |
| FR-001 | `grep -n 'cargo update -p russh -p russh-sftp' docs/MIGRATION.md` | no line, exit 1 |
| FR-001 | `grep -c 'needs no manual step' docs/MIGRATION.md` | 2 |
| FR-002 | `grep -c 'currently never raised' docs/OPERATIONS.md` | 0 |
| FR-002 | `grep -c '17th' docs/OPERATIONS.md` | 0 |
| FR-002 | `sed -n '409p' docs/OPERATIONS.md` | names `INTERNAL_ERROR`, `SSH_MAX_SUBS_PER_URI` and `max subs per URI exceeded` |
| FR-002 | `diff` of bullets 407-408 against the base | identical |
| FR-003 | `grep -c 'LANE_LIMIT_PER_URI' docs/CONFIGURATION.md` | 0 |
| FR-003 | `grep -c 'LANE_LIMIT_TOTAL' docs/CONFIGURATION.md` | 0 |
| FR-003 | `sed -n '149p'` / `sed -n '150p'` | name `MAX_SUBS_PER_URI_EXCEEDED` / `MAX_SUBS_TOTAL_EXCEEDED` |
| FR-004 | `grep -rn 'FILTER_INVALID' docs doc/arch/threat-model` | one line, `docs/CONFIGURATION.md:153` |
| FR-004 | `sed -n '1106p'` / `sed -n '1153p' docs/API.md` | contain `INVALID_ARGUMENT` |
| FR-004 | `sed -n '754,758p' doc/arch/threat-model/threat-model.md` | contains `INVALID_ARGUMENT` and `no production caller applies` |
| FR-004 | base `757,758` vs copy `759,760` | identical |
| FR-005 | `sed -n '1106p' docs/API.md` | four codes, no old name |
| FR-005 | `grep -c 'LANE_LIMIT_PER_URI\|LANE_LIMIT_TOTAL\|FILTER_INVALID\|INLINE_PUSH_BAD_PARAMS' docs/API.md` | 0 |
| FR-006 | `sed -n '147p' docs/DAEMON.md` | contains `SUBSCRIBE_FAILED` |
| FR-006 | `sed -n '147p' docs/DAEMON.md \| grep -c 'INVALID_LIFETIME\|INVALID_LAG_POLICY\|SUB_NOT_FOUND'` | 0 |
| FR-009 | `grep -c 'SSH_FILTER_REGEX_MAX' session.md`, `'lag_policy'`, `'INVALID_LIFETIME'`, `'sub_filter'` | 1 each |
| FR-010 | `grep -c 'owner.s instruction to carry'` in the three artifacts | 1 each |

FR-007 and FR-008 are repository checks and run in the verification plan.

## Verification plan

1. `diff /var/tmp/ssh-mcp/docs-drift/docs/MIGRATION.md docs/MIGRATION.md` —
   no output. Repeat for each of the seven documents against its copy.
2. `make tools-export-check` — exit 0, `unchanged: docs/TOOL_CATALOG.md` and
   `unchanged: docs/tool-catalog.json`.
3. `diff -r /var/tmp/ssh-mcp/bumps-spike/src src` — no output.
4. `diff -r /var/tmp/ssh-mcp/bumps-spike/tests tests` — no output.
5. `diff /var/tmp/ssh-mcp/bumps-spike/Cargo.toml Cargo.toml` — no output.
6. `git diff --name-only` — only the seven documents, plus the untracked
   feature artifacts.
7. Every acceptance command of the table above, run in the repository — same
   results.
8. `speckit validate` — exit 6, 70 errors, the baseline rule set, no warning,
   no finding on a feature-004 file.
9. `speckit verify --changed` — no `failed` line.

## Constitution

- **D-01 Spec first.** The specification, this plan and the tasks precede the
  first document edit; the implement phase follows them in order.
- **D-02 Dogfooding.** `speckit validate` and `speckit verify --changed` run
  after each phase; a finding is fixed in the artifact, not waived.
- **D-10 Guard-mediated writes.** Every artifact is written through the guarded
  tools; the seven documents are written in the implement phase under the same
  scope.
- **D-11 Stable contracts.** The feature changes no exit code, `--json` shape or
  artifact contract; it only makes prose name the existing contract.
- **D-12 Small changes.** Seven documents, one concern: the statements the
  evidence refutes. No code rides along.
- **P-04 The MCP wire is frozen.** The corrected names are the ones the code
  already emits, so the documented surface matches the frozen wire.
- **P-05 Wire additions are additive.** No argument, response line, default or
  notification changes; the catalogue files stay `unchanged`.
- **P-06 The verified-change gate.** No code change, so the gate is unchanged;
  `make tools-export-check` is the one build-adjacent check and stays
  `unchanged`.
- **P-08 Commits and recorded decisions.** The decision is recorded in ADR 0004
  before the edits; the commit plan is in `tasks.md`.
- **P-09 Persisted artifacts are en-US.** Every new sentence is en-US.
- **D-05 Object Calisthenics (advisory).** The schema stays within ten
  definitions and seven fields per type, as validate reports.

## Rollback

Reverse the patch as one unit:
`git apply -R /var/tmp/ssh-mcp/docs-drift.patch`. It removes only this
feature's hunks, so the uncommitted hunks of features 001 to 003 in
`docs/MIGRATION.md`, `session.md` and the threat model survive. The feature
artifacts are untracked and are removed with their directory.

## Open decisions

None new. Q-1 of the specification (the frozen descriptions name non-emitted
codes) stays open and is recorded under "Open items for the owner" in
`session.md`.
