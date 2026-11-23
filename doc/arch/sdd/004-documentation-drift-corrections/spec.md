---
id: 01a20237-b1d4-7cd3-b7ea-99ab1333250d
number: 004
slug: documentation-drift-corrections
status: implemented
created_at: 2026-11-22T07:45:34.164918675Z
---
# Feature Specification: Documentation Drift Corrections

Feature: 004-documentation-drift-corrections
Created: 2026-11-22

Three statements in the published documentation contradict the code or a
measurement. Each was reported by a review, re-established here, and is
corrected in place; no code, test, manifest or published surface moves. Every
`file:line` below is the repository line of 2026-11-22 at `aac7fc4`, checked by
printing it. Every statement about the code is read from the project or
measured; nothing is copied from the review.

The three candidates and what the evidence decided:

| # | Statement | Verdict |
| --- | --- | --- |
| 1 | A stale lock must be re-resolved by hand before the `russh` 0.65 build | False — plain `cargo metadata` rewrites it, and the result is byte-identical |
| 2 | `INTERNAL_ERROR` is never raised for `resources/*` | False — a lane-cap refusal becomes a JSON-RPC internal error and is reachable |
| 3 | The lane-cap and filter failures carry other code names | False — the names do not exist in the code |

## Why the corrections

- An operator who follows the stale-lock instruction runs a manual step cargo
  already performs; the same section says so for the later moves
  (`docs/MIGRATION.md:888`).
- A host that reads the error list of `sub_open` branches on three codes the
  server never emits (`LANE_LIMIT_PER_URI`, `LANE_LIMIT_TOTAL`,
  `FILTER_INVALID`) and is not told the two cap codes it does emit.
- `docs/OPERATIONS.md:409` tells an operator that `INTERNAL_ERROR` cannot
  happen on `resources/*`; it can, as soon as a lane cap is reached.

## User Stories

- As an operator, I want the upgrade guide to say which lock step cargo does by
  itself, so that I do not run a command that changes nothing.
- As a host implementer, I want the `sub_open` error list to name the codes the
  server emits, so that a retry branch matches the wire.
- As a maintainer, I want the documentation to agree with the code, so that a
  reader does not file a bug against correct behaviour.

## Out of Scope

- Code, tests, manifests, scripts. No production source line changes.
- The text of tool descriptions and argument descriptors in the source
  (`src/infra/mcp/tool_router.rs:2389`, `:2489`, `src/infra/mcp/args/`). They
  are part of the published tool catalogue, frozen by P-04 and P-05
  (`doc/arch/memory/constitution.md:101-108`). The names they carry are listed
  under Open Questions, not corrected here.
- The generated catalogue files that copy those descriptions
  (`docs/TOOL_CATALOG.md`, `docs/tool-catalog.json`). They follow a description
  change, which this feature does not make.
- Every statement whose truth depends on a defect a later feature repairs. The
  documents describe the intended behaviour and the code will be brought to
  them; they are listed under Excluded statements below.
- Historical records: decision records under `doc/arch/adr/`, release notes of
  past versions in `docs/MIGRATION.md` and `CHANGELOG.md`.
- The corpus line citations into source files, for example
  `src/composition/prod.rs:786`; they are scheduled for the conformity work.

## Excluded statements

Each is met by a text search and left as written, so that its absence from the
requirements is not read as an oversight.

- `sub_pause` stops delivery, `auto_close` and `lease` close a lane, `sub_replay`
  delivers, `resources/unsubscribe` closes the lane and `release_when_no_subs`
  releases the resource: intended behaviour, repaired by a later feature.
- The three cleanup TTLs, `SSH_INACTIVITY_TIMEOUT` and `persistent` reaching the
  transport: intended behaviour, same reason.
- The close reasons listed in `docs/DAEMON.md`: intended behaviour, same reason.
- `docs/adr/0007-error-taxonomy.md:14`, `:104` and `:105` name
  `MAX_SUBS_PER_URI_EXCEEDED` and `MAX_SUBS_TOTAL_EXCEEDED`; both names are
  correct, and the record is history.
- `docs/MIGRATION.md:1371-1376` is the release-notes heading of
  `v7.0.0-alpha.X — openrsync port slice 5` and the old `md-5 = "0.10"` line;
  the release notes are history.
- `docs/LLM_GUIDE.md:1161` and `:1539` name `MAX_SUBS_PER_URI_EXCEEDED` and
  describe it correctly.
- `docs/ARCHITECTURE.md:16` names `INLINE_PUSH_OVERSIZE`; the code exists
  (`src/infra/mcp/tool_router.rs:957`).
- `docs/CONFIGURATION.md:153` promises that a pattern over
  `SSH_FILTER_REGEX_MAX` is rejected. The cap is resolved
  (`src/adapters/config/internal/mod.rs:906-911`) and applied by no production
  caller; a later feature applies it, so the row stays as written.

## Functional Requirements

### FR-001 The upgrade guide says a stale lock re-resolves by itself

`docs/MIGRATION.md:882` ends:

> A stale `Cargo.lock` must be re-resolved (`cargo update -p russh -p russh-sftp`) before it builds against the new lines.

Measured 2026-11-22 in `/var/tmp/ssh-mcp/lockcheck3`, a copy of the tree with
the pre-change lock (`russh 0.60.3`, `russh-sftp 2.4.0`, `rmcp 1.8.0`): plain
`cargo metadata --format-version 1` exits 0, prints
`Locking 69 packages to highest Rust 1.99 compatible versions`, rewrites the lock
to `russh 0.65.4`, `russh-sftp 4.0.0`, `rmcp 3.11.0`, `base64 0.23.1`,
`sha2 0.11.1` and `tower-http 0.7.1`, and `diff -q` against the repository lock
is silent (exit 0). The section already says the opposite for the later moves
(`docs/MIGRATION.md:888`: "A `Cargo.lock` left from before the change needs no
manual step"). The verdict is false.

Corrected sentence, replacing the last sentence of the bullet:

> A stale `Cargo.lock` needs no manual step: cargo re-resolves it on the next invocation, and the result is identical to the repository lock.

**Acceptance.** `grep -n 'cargo update -p russh -p russh-sftp' docs/MIGRATION.md`
prints nothing; `grep -c 'needs no manual step' docs/MIGRATION.md` returns 2.

### FR-002 `INTERNAL_ERROR` is reachable on `resources/*`

`docs/OPERATIONS.md:409` reads:

> - `INTERNAL_ERROR` for registry-level failures (currently never raised — `subscribe` is infallible at runtime).

`map_resource_error` (`src/infra/mcp/resource_handlers.rs:72-79`) builds the
error as `McpError::internal_error(err.to_string(), None)`: the JSON-RPC class
follows `resource_error_category` and the message is the `Display` of the
domain error. No code name is printed, and no `data` is attached. The mapping
is exhaustive; the table lists every variant the three handlers reach.

| Request | `DomainError` | Category (`resource_error_category`) | JSON-RPC | Display (`src/domain/error.rs`) |
| --- | --- | --- | --- | --- |
| subscribe | `InvalidArgument` (`src/application/subscribe_resource.rs:146`) | `InvalidParams` (`src/infra/mcp/resource_handlers.rs:98-100`) | -32602 `invalid_params` | `invalid argument: {0}` (`:78-79`) |
| subscribe | `ShellNotFound` (`src/application/subscribe_resource.rs:233`) | `NotFound` (`:103-115`) | -32002 `resource_not_found` | `shell not found: {0}` (`:26-27`) |
| subscribe | `CommandNotFound` (`src/application/subscribe_resource.rs:244`) | `NotFound` | -32002 | `command not found: {0}` (`:22-23`) |
| subscribe | `TransferNotFound` (`src/application/subscribe_resource.rs:255`) | `NotFound` | -32002 | `transfer not found: {0}` (`:30-31`) |
| subscribe | `SessionNotFound` (`src/application/subscribe_resource.rs:266`) | `NotFound` | -32002 | `session not found: {0}` (`:18-19`) |
| subscribe | `ResourceGone` (`src/adapters/lifecycle/refcount.rs:420`, `:484`) | `NotFound` | -32002 | `resource gone: {0}` (`:130-131`) |
| subscribe | `MaxSubsPerUriExceeded` (`src/adapters/subscription/memory_registry.rs:552`; `src/adapters/subscription/subscriber_lane.rs:531`) | `Internal` (`:130-131`) | -32603 `internal_error` | `max subs per URI exceeded for {uri} (limit {limit})` (`:161-167`) |
| subscribe | `MaxSubsTotalExceeded` (`src/adapters/subscription/memory_registry.rs:558`; `src/adapters/subscription/subscriber_lane.rs:470`) | `Internal` | -32603 | `max subs total exceeded (limit {limit})` (`:171-175`) |
| unsubscribe | `InvalidArgument` (`src/application/unsubscribe_resource.rs:100`) | `InvalidParams` | -32602 | `invalid argument: {0}` |
| unsubscribe | `Internal` (`src/adapters/lifecycle/refcount.rs:552-554`) | `Internal` | -32603 | `internal error: {0}` (`:123-124`) |
| read | `InvalidArgument` (`src/application/read_resource.rs:437`, `:472-479`; `src/infra/mcp/resource_handlers.rs:749-751`) | `InvalidParams` | -32602 | `invalid argument: {0}` |
| read | `ShellNotFound` (`src/application/read_resource.rs:501`; `src/adapters/output_stream/russh_output.rs:142`) | `NotFound` | -32002 | `shell not found: {0}` |
| read | `CommandNotFound` (`src/application/read_resource.rs:536`; `src/adapters/output_stream/russh_output.rs:121`) | `NotFound` | -32002 | `command not found: {0}` |
| read | `TransferNotFound` (`src/application/read_resource.rs:594`) | `NotFound` | -32002 | `transfer not found: {0}` |
| read | `SessionNotFound` (`src/application/read_resource.rs:631`) | `NotFound` | -32002 | `session not found: {0}` |
| read | `ForwardNotFound` (`src/application/read_resource.rs:672`) | `NotFound` | -32002 | `forward not found: {0}` (`:34-35`) |
| read | `ResourceGone` (`src/infra/mcp/resource_handlers.rs:531`, rsync read) | `NotFound` | -32002 | `resource gone: {0}` |

Measured on 2026-11-22 with
`printf '{"op":"subscribe","uri":"forward://x/events","id":"cap-NN"}\n' | ./target/debug/ssh-mcp-tail daemon`
for NN 01 to 17. The first 16 print `{"ev":"ack",...}`; the 17th prints
`{"ev":"err","id":"cap-17","code":"SUBSCRIBE_FAILED","reason":"subscribe forward://x/events failed","detail":"Mcp error: -32603: max subs per URI exceeded for forward://x/events (limit 16)"}`.
The default `SSH_MAX_SUBS_PER_URI` is 16
(`src/adapters/config/internal/mod.rs:200`); production wires the lane
(`src/composition/prod.rs:524`). The verdict is false.

Corrected sentence:

> - `INTERNAL_ERROR` for registry-level failures — returned when a lane cap is reached: `SSH_MAX_SUBS_PER_URI` (default 16) on one URI, or `SSH_MAX_SUBS_TOTAL` server-wide. The message carries the domain display, for example `max subs per URI exceeded for forward://x/events (limit 16)`.

**Acceptance.** `grep -c 'currently never raised' docs/OPERATIONS.md` returns 0;
`grep -c '17th' docs/OPERATIONS.md` returns 0; `sed -n '409p' docs/OPERATIONS.md`
contains `INTERNAL_ERROR`, `SSH_MAX_SUBS_PER_URI` and
`max subs per URI exceeded`; the `INVALID_PARAMS` and `RESOURCE_NOT_FOUND`
bullets above it are unchanged.

### FR-003 The per-URI and global cap codes are the ones the code emits

`docs/CONFIGURATION.md:149` says the per-URI cap "Returns `LANE_LIMIT_PER_URI`
once exhausted" and `:150` says the global cap "Returns `LANE_LIMIT_TOTAL` once
exhausted". Neither identifier exists in the tree: `grep -rn 'LANE_LIMIT_PER_URI\|LANE_LIMIT_TOTAL' src`
prints nothing. The lane emits `DomainError::MaxSubsPerUriExceeded`
(`src/adapters/subscription/subscriber_lane.rs:531`) and
`DomainError::MaxSubsTotalExceeded` (`:470`), and `classify_error` renders them
as `MAX_SUBS_PER_URI_EXCEEDED` (`src/infra/mcp/tool_router.rs:883-887`) and
`MAX_SUBS_TOTAL_EXCEEDED` (`:888-892`). The legacy registry emits the same two
variants (`src/adapters/subscription/memory_registry.rs:551-560`). The verdict
is false for both names.

Corrected sentences, in the two table cells:

> Hard cap on lanes per resource URI. Returns `MAX_SUBS_PER_URI_EXCEEDED` once exhausted.

> Hard cap on lanes server-wide. Returns `MAX_SUBS_TOTAL_EXCEEDED` once exhausted.

**Acceptance.** `grep -c 'LANE_LIMIT_PER_URI' docs/CONFIGURATION.md` returns 0;
`grep -c 'LANE_LIMIT_TOTAL' docs/CONFIGURATION.md` returns 0;
`sed -n '149p' docs/CONFIGURATION.md` contains `MAX_SUBS_PER_URI_EXCEEDED`;
`sed -n '150p' docs/CONFIGURATION.md` contains `MAX_SUBS_TOTAL_EXCEEDED`.

### FR-004 The filter compile failure is `INVALID_ARGUMENT`; the char-cap promise stays

`docs/API.md:1106` and `:1153` name `FILTER_INVALID`, and so does
`doc/arch/threat-model/threat-model.md:755`. The identifier exists nowhere in
`src`: a failed compile is `DomainError::InvalidArgument`
(`src/adapters/subscription/filter.rs:56-59`), reached from the open path
(`src/adapters/subscription/subscriber_lane.rs:798-800`) and the filter path,
and `classify_error` falls through to `INVALID_ARGUMENT`
(`src/infra/mcp/tool_router.rs:781-786`; `FILTER_INVALID` is not in the tag
table, `:587-594`). The verdict is false for the three occurrences.

`docs/CONFIGURATION.md:153` also promises that an over-length pattern is
rejected past `SSH_FILTER_REGEX_MAX`. The code never applies that cap:
`resolve_filter_regex_max` (`src/adapters/config/internal/mod.rs:906-911`) has
no production caller (`LSP findReferences`: two references, the definition and
the test at `:1325`). That is a defect of the code, which a later feature
repairs by applying the cap, so the operator-facing promise stays as written and
the line moves to Excluded statements. The threat model is the register of risk
as it stands, and its entries already record gaps of the code as open; its line
is corrected to say the cap is resolved and not applied, and that a failed
compile is `INVALID_ARGUMENT`.

Corrected sentences:

- `docs/API.md:1106`: `FILTER_INVALID` becomes `INVALID_ARGUMENT` in the list.
- `docs/API.md:1153`: "**Errors**: `SUB_NOT_FOUND`, `INVALID_ARGUMENT` (regex compile)."
- `doc/arch/threat-model/threat-model.md:755`: "The pattern length is intended to be capped at 1024 characters by default, in the range 16 to 65536, but no production caller applies `resolve_filter_regex_max` today; a pattern that fails to compile is rejected with `INVALID_ARGUMENT` (`src/adapters/config/internal/mod.rs:217`, `docs/CONFIGURATION.md:153`)."

**Acceptance.** `grep -rn 'FILTER_INVALID' docs doc/arch/threat-model` returns
exactly one line, `docs/CONFIGURATION.md:153`; `sed -n '1106p' docs/API.md` and
`sed -n '1153p' docs/API.md` contain `INVALID_ARGUMENT`; `sed -n '754,758p'
doc/arch/threat-model/threat-model.md` contains `INVALID_ARGUMENT` and
`no production caller applies`; the second threat-model bullet is unchanged
(`sed -n '757,758p'` of the base equals `sed -n '759,760p'` after the edit).

### FR-005 The `sub_open` error list names only emitted codes

`docs/API.md:1106` reads:

> **Errors**: `RESOURCE_GONE`, `LANE_LIMIT_PER_URI`, `LANE_LIMIT_TOTAL`, `FILTER_INVALID`, `INLINE_PUSH_BAD_PARAMS`, `INVALID_ARGUMENT`.

The codes `sub_open` can return, traced from `run_sub_subscribe`
(`src/infra/mcp/tool_router.rs:1301-1321`) through `SubscribeUseCase::execute`
(`src/application/subscription_admin.rs:135-162`), are exactly:

| Code | Emitter |
| --- | --- |
| `INVALID_ARGUMENT` | bad URI (`src/application/subscription_admin.rs:137`); failed regex (`src/adapters/subscription/filter.rs:58`); idempotency key too long or mismatched (`src/infra/mcp/tool_router.rs:495-510`, `:573-580`) |
| `RESOURCE_GONE` | closed resource (`src/adapters/lifecycle/refcount.rs:420`, `:484`) |
| `MAX_SUBS_PER_URI_EXCEEDED` | per-URI lane cap (`src/adapters/subscription/subscriber_lane.rs:531`) |
| `MAX_SUBS_TOTAL_EXCEEDED` | global lane cap (`src/adapters/subscription/subscriber_lane.rs:470`) |

`INLINE_PUSH_BAD_PARAMS` is a channel-mux `Event::Err` code
(`src/embed/event_mux.rs:326`, `:357-384`), never a `sub_open` response; a bad
inline-push request is silently downgraded
(`src/application/subscription_admin.rs:164-183`). The old list also omits the
two cap codes. The verdict is false.

Corrected sentence:

> **Errors**: `RESOURCE_GONE`, `MAX_SUBS_PER_URI_EXCEEDED`, `MAX_SUBS_TOTAL_EXCEEDED`, `INVALID_ARGUMENT`.

**Acceptance.** `sed -n '1106p' docs/API.md` contains the four codes and
`grep -c 'LANE_LIMIT_PER_URI\|LANE_LIMIT_TOTAL\|FILTER_INVALID\|INLINE_PUSH_BAD_PARAMS' docs/API.md`
returns 0.

### FR-006 The daemon `subscribe` op reports `SUBSCRIBE_FAILED`

`docs/DAEMON.md:147` lists the errors of the `subscribe` op as
`SUB_NOT_FOUND`, `RESOURCE_GONE`, `MAX_SUBS_PER_URI_EXCEEDED`,
`MAX_SUBS_TOTAL_EXCEEDED`, `INVALID_LIFETIME`, `INVALID_LAG_POLICY`. The
dispatcher emits one code for every failure of the op: `SUBSCRIBE_FAILED`
(`src/embed/dispatcher.rs:596-601`), with the JSON-RPC cause in `detail`.
`SUB_NOT_FOUND` cannot occur at open (no `sub_id` is an argument,
`src/infra/mcp/args/subscription.rs:69-107`), and `INVALID_LIFETIME` /
`INVALID_LAG_POLICY` are never constructed (they exist only as variants and
match arms: `src/domain/error.rs:209`, `:214`). The verdict is false.

Corrected sentence:

> Errors: `SUBSCRIBE_FAILED` — the dispatcher collapses every `resources/subscribe` failure into this code, and the JSON-RPC cause (`RESOURCE_GONE`, `MAX_SUBS_PER_URI_EXCEEDED`, `MAX_SUBS_TOTAL_EXCEEDED`, `INVALID_ARGUMENT`) is carried in `detail`.

**Acceptance.** `sed -n '147p' docs/DAEMON.md` contains `SUBSCRIBE_FAILED`;
`sed -n '147p' docs/DAEMON.md | grep -c 'INVALID_LIFETIME\|INVALID_LAG_POLICY\|SUB_NOT_FOUND'`
returns 0.

### FR-007 Nothing else changes

No production or test source, no `Cargo.toml`, no `Cargo.lock`, no script and no
tool description is edited. `make tools-export-check` reports `unchanged` for
both catalogue files. The `git diff` of the feature touches only
`docs/MIGRATION.md`, `docs/OPERATIONS.md`, `docs/CONFIGURATION.md`,
`docs/API.md`, `docs/DAEMON.md`, `doc/arch/threat-model/threat-model.md` and
`session.md`, plus the artifacts of this feature.

**Acceptance.** `git diff --name-only` over the feature prints no path outside
that set; `make tools-export-check` exits 0 and prints `unchanged` twice.

### FR-008 `speckit validate` adds no finding to the baseline

The baseline before this feature is exit 6 with 70 errors and no warning:
`fqcn.relative-super` 46, `branch.else` 8, `pin.ci.unpinned-action` 5,
`pin.docker.unpinned-image` 2, and one each of `readme.mermaid.missing`,
`readme.index.missing`, `readme.image.missing`, `readme.arch-link.missing`,
`pin.cargo.lockfile-missing`, `llms-txt.file.missing`, `fqcn.glob-import`,
`crew.roster.dangling` and `ci.gitlab.missing`.

**Acceptance.** `speckit validate` reports 70 errors with that rule set, no
warning, and no finding that names a file of this feature.

### FR-009 The defects and the open item are recorded in `session.md`

The section "Defects verified in the code during the session"
(`session.md:100`) gains three entries, each with its `file:line` and one
sentence, in the form of the entries already there:

- `src/adapters/config/internal/mod.rs:906`: `SSH_FILTER_REGEX_MAX` is resolved
  by `resolve_filter_regex_max` and applied by no production caller.
- `src/embed/parser.rs:95` and `src/embed/dispatcher.rs:578`: the daemon
  `subscribe` op parses `lifetime`, `grace_ms`, `lag_policy`, `filter` and
  `start_cursor` and sends only the URI.
- `src/domain/error.rs:209` and `:214`: `INVALID_LIFETIME` and
  `INVALID_LAG_POLICY` are documented in `docs/LLM_GUIDE.md:1665` and `:1675`
  and constructed by no production path.

The section "Open items for the owner" (`session.md:138`) gains one entry: the
`sub_open` and `sub_filter` descriptions in the tool catalogue name codes the
server does not emit, and changing a description moves the frozen catalogue
(`src/infra/mcp/tool_router.rs:2389`, `:2489`).

**Acceptance.** `grep -c 'SSH_FILTER_REGEX_MAX' session.md`,
`grep -c 'lag_policy' session.md`, `grep -c 'INVALID_LIFETIME' session.md` and
`grep -c 'sub_filter' session.md` each return at least 1; the new entries sit
under the two named headings.

### FR-010 The decision is recorded with its provenance

The specification, the decision record and the feature file state that every
decision was taken in this session on the owner's instruction to carry the whole
update plan forward, and that it is recorded for the owner's review.

**Acceptance.** `grep -c 'owner.s instruction to carry'` returns 1 in each of
the three artifacts.

## Security Requirements

- **Data sensitivity/classification.** No data of its own. The feature corrects
  prose about error codes and a lock step; no payload, file body or credential
  is involved.
- **Authentication/authorization.** No authenticated surface and no credential.
  The HTTP transport stays unauthenticated as TM-001 records.
- **Input validation.** The subject is the validation vocabulary: which code a
  refused `sub_open` returns. The corrected sentences name the code the
  handler already emits; no bound changes.
- **Cryptography in transit/at rest.** None. No algorithm, key or encoding
  moves.
- **Logging/audit.** No log line, target, level or destination changes. The
  corrected `INTERNAL_ERROR` bullet describes an error that reaches the
  JSON-RPC layer, not a new log.
- **Error-handling information exposure.** The feature makes the error
  vocabulary accurate. It exposes no new detail: the code names already cross
  the wire, and `docs/DAEMON.md` keeps the cause in `detail` as the dispatcher
  does.

## Acceptance Scenarios

Given `docs/MIGRATION.md` line 882
When  the stale-lock sentence is read
Then  it says no manual step is needed and names no `cargo update` command
And   `grep -c 'needs no manual step'` returns 2 (FR-001).

Given `docs/OPERATIONS.md` line 409
When  the `INTERNAL_ERROR` bullet is read
Then  it names the per-URI and server-wide lane caps and the message they produce
And   `grep -c 'currently never raised'` and `grep -c '17th'` return 0 (FR-002).

Given `docs/CONFIGURATION.md` lines 149 and 150
When  the two cap rows are read
Then  they name `MAX_SUBS_PER_URI_EXCEEDED` and `MAX_SUBS_TOTAL_EXCEEDED`
And   `grep -c 'LANE_LIMIT_PER_URI'` and `grep -c 'LANE_LIMIT_TOTAL'` return 0 (FR-003).

Given the three wrong `FILTER_INVALID` occurrences
When  they are read after the change
Then  `grep -rn 'FILTER_INVALID' docs doc/arch/threat-model` prints only `docs/CONFIGURATION.md:153`
And   the two API lines and the threat-model line name `INVALID_ARGUMENT` (FR-004).

Given `docs/API.md` line 1106
When  the `sub_open` error list is read
Then  it names `RESOURCE_GONE`, `MAX_SUBS_PER_URI_EXCEEDED`, `MAX_SUBS_TOTAL_EXCEEDED` and `INVALID_ARGUMENT`
And   it names no `LANE_LIMIT_*`, `FILTER_INVALID` or `INLINE_PUSH_BAD_PARAMS` (FR-005).

Given `docs/DAEMON.md` line 147
When  the `subscribe` error list is read
Then  it names `SUBSCRIBE_FAILED` as the event code
And   the JSON-RPC causes are described as `detail` content (FR-006).

Given the feature diff
When  `git diff --name-only` and `make tools-export-check` run
Then  only the six named documents and the feature artifacts appear
And   both catalogues are `unchanged` (FR-007).

Given the feature's artifacts on disk
When  `speckit validate` runs over the corpus
Then  the error count stays at 70 with the same rule set and no warning fires (FR-008).

Given `session.md` after the change
When  the two sections are read
Then  the three defects sit under the defect heading with their `file:line`
And   the catalogue open item sits under the owner heading (FR-009).

Given the three artifacts
When  the provenance sentence is searched
Then  each carries it once (FR-010).

## Accepted Risks

- **AR-1 The corrected pages describe today's code, and a later change can
  re-drift them.** Accepted: the feature corrects the statement to the emitter
  it names, and each requirement carries the search that finds the old text, so
  the next drift is detected by the same search.
- **AR-2 The tool descriptions and the generated catalogues keep the old
  names.** Accepted: the catalogue is frozen (P-04, P-05), so `sub_open` still
  advertises `SUB_LIMIT_EXCEEDED` and `SUB_INVALID_FILTER` while `docs/API.md`
  names the emitted codes. The inconsistency is listed as an open question for
  the owner.
- **AR-3 The threat-model entry now says the cap is not applied, while
  `docs/CONFIGURATION.md:153` keeps the promise.** Accepted: the threat model
  records the risk as it stands; the operator table states the intended
  contract and changes when a later feature applies the cap. Evidence is `LSP
  findReferences` with two references (definition and test).

## Observability

No metric, counter, NDJSON event or trace span is added and no target, level,
format or destination changes. The feature is documentation only. Conventions
stay those of `doc/arch/observability/observability.md`.

## Open Questions

- **Q-1 The frozen tool descriptions name codes the code does not emit.**
  `sub_open` advertises `SUB_INVALID_FILTER`, `SUB_LIMIT_EXCEEDED` and
  `RESOURCE_NOT_FOUND` (`src/infra/mcp/tool_router.rs:2389`), `sub_filter`
  advertises `SUB_INVALID_FILTER` (`:2489`), and the generated
  `docs/TOOL_CATALOG.md:1373`, `:1475` and `docs/tool-catalog.json` copy them.
  Only the owner can decide whether a description change enters a later feature.
  The item is also recorded under "Open items for the owner" in `session.md`
  (FR-009).

## Clarifications

### Session 2026-11-22

- **Decision provenance.** Every decision in this specification was taken in
  this session on the owner's instruction to carry the whole update plan
  forward, and it is recorded for the owner's review.
- **Q-2 and Q-3 are defects, not open questions.** `INVALID_LIFETIME` /
  `INVALID_LAG_POLICY` and the dropped daemon fields are recorded under
  "Defects verified in the code during the session" (FR-009), with their
  `file:line`. Q-1 stays open for the owner and is also recorded there.
