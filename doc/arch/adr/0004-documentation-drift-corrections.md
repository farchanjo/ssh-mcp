---
status: proposed
date: 2026-11-22
deciders: [Fabricio Archanjo]
consulted: []
informed: []
---

# Documentation Drift Corrections

## Context and Problem Statement

Three statements in the published documentation contradict the code or a
measurement. They were found by a review, and each was re-established before it
was accepted:

1. `docs/MIGRATION.md:882` tells an operator that a stale `Cargo.lock` "must be
   re-resolved (`cargo update -p russh -p russh-sftp`) before it builds against
   the new lines". Measured on 2026-11-22 in a throwaway copy with the
   pre-change lock (`russh 0.60.3`, `rmcp 1.8.0`), plain `cargo metadata`
   exits 0, rewrites the lock, and `diff -q` against the repository lock is
   silent. The same section already says the later moves need no manual step
   (`docs/MIGRATION.md:888`).
2. `docs/OPERATIONS.md:409` says `INTERNAL_ERROR` is "currently never raised —
   `subscribe` is infallible at runtime". `map_resource_error` builds
   `McpError::internal_error(err.to_string(), None)`
   (`src/infra/mcp/resource_handlers.rs:72-79`): the JSON-RPC class follows
   `resource_error_category` and the message is the domain `Display`, with no
   `data`. The internal class covers `DomainError::MaxSubsPerUriExceeded` and
   `DomainError::MaxSubsTotalExceeded` (`:130-131`), reached through the
   registry (`src/adapters/subscription/memory_registry.rs:551-560`) and the
   lane (`src/adapters/subscription/subscriber_lane.rs:470`, `:531`) at the
   configured caps (`src/adapters/config/internal/mod.rs:200`, `:205`). Measured on
   2026-11-22 by driving `ssh-mcp-tail daemon` with 17 `subscribe` ops on
   `forward://x/events`: the 17th prints
   `"code":"SUBSCRIBE_FAILED"` and
   `"detail":"Mcp error: -32603: max subs per URI exceeded for forward://x/events (limit 16)"`.
3. The documentation names error codes that exist nowhere in `src`:
   `LANE_LIMIT_PER_URI` and `LANE_LIMIT_TOTAL` (`docs/CONFIGURATION.md:149`,
   `:150`), `FILTER_INVALID` (`docs/API.md:1106`, `:1153`,
   `doc/arch/threat-model/threat-model.md:755`) and `INLINE_PUSH_BAD_PARAMS` as
   a `sub_open` error (`docs/API.md:1106`). The lane emits
   `MAX_SUBS_PER_URI_EXCEEDED` and `MAX_SUBS_TOTAL_EXCEEDED`
   (`src/infra/mcp/tool_router.rs:883-892`) and a failed regex compile is
   `INVALID_ARGUMENT` (`src/adapters/subscription/filter.rs:56-59`).
   `INLINE_PUSH_BAD_PARAMS` is a channel-mux event code
   (`src/embed/event_mux.rs:326`).

`docs/DAEMON.md:147` names `SUB_NOT_FOUND`, `INVALID_LIFETIME` and
`INVALID_LAG_POLICY` for the daemon `subscribe` op. The dispatcher emits one
event code, `SUBSCRIBE_FAILED`, and puts the rmcp error text in `detail`
(`src/embed/dispatcher.rs:596-601`); the capture above shows that text. The
daemon op also parses `lifetime`, `grace_ms`, `lag_policy`, `filter` and
`start_cursor` (`src/embed/parser.rs:95-108`) and sends only the URI
(`src/embed/dispatcher.rs:578-583`).

`docs/CONFIGURATION.md:153` promises that a pattern over `SSH_FILTER_REGEX_MAX`
is rejected. `resolve_filter_regex_max` (`src/adapters/config/internal/mod.rs:906-911`)
has no production caller, so the cap is a code defect a later feature repairs:
the operator-facing promise stays as written, and the threat model records the
gap.

The published surface is frozen (P-04 and P-05,
`doc/arch/memory/constitution.md:101-108`). A code change for these sentences
would either edit the tool catalogue or add codes to the wire, so the
correction belongs in the documents.

## Decision Drivers

- A host branches on the error code it reads; a code that never arrives is a
  dead branch, and an omitted code is an unhandled failure.
- An operator should not run a manual step cargo already performs.
- The documents must describe the code of the release that ships them, without
  moving the frozen surface.
- One concern per change (D-12, `doc/arch/memory/constitution.md:74-77`): prose
  edits only, in seven documents.

## Considered Options

- Option A: correct every occurrence in the seven documents, keep the codes the
  code emits, and record the description and catalogue drift as an open
  question.
- Option B: correct only the code names, and leave the stale-lock and
  `INTERNAL_ERROR` sentences.
- Option C: change the code to emit the documented names, and make `subscribe`
  infallible.
- Option D: leave the documents and file one bug per statement.

## Decision Outcome

Chosen option: "Option A". It fixes all three reported statements and every
occurrence a text search finds, without touching the frozen catalogue. Option B
leaves two false statements in place. Option C changes the wire for every host
that already branches on `MAX_SUBS_PER_URI_EXCEEDED` (ADR 0007 lists it) and is
larger than a documentation correction. Option D leaves the reader with the
wrong text until someone triages the bugs.

The corrected sentences are in the requirements of
`doc/arch/sdd/004-documentation-drift-corrections/spec.md`. Each names the
emitter or the measurement, and each requirement carries the search that finds
the old text.

The research is not lost. The defects it established go to the defect list of
`session.md`, each with its `file:line`: the unapplied `SSH_FILTER_REGEX_MAX`
cap, the daemon `subscribe` fields that are parsed and not sent, and the two
unconstructed validation codes. The catalogue open item goes to "Open items for
the owner". The char cap is therefore a promise kept in
`docs/CONFIGURATION.md:153` and a gap recorded in the threat model; the two API
lines take the emitted code.

The decision was taken in this session on the owner's instruction to carry the
whole update plan forward, and it is recorded for the owner's review.

### Consequences

- Good: the `sub_open` error list names the four codes the handler can emit, so
  a retry branch matches the wire.
- Good: the upgrade guide stops prescribing a manual lock step that changes
  nothing.
- Good: the `resources/*` section stops telling an operator that a reachable
  `INTERNAL_ERROR` cannot happen.
- Bad: the tool descriptions and the generated catalogues still advertise
  `SUB_LIMIT_EXCEEDED` and `SUB_INVALID_FILTER`, so `docs/API.md` and
  `docs/TOOL_CATALOG.md` disagree until the owner accepts a description change.
- Bad: `docs/CONFIGURATION.md:153` keeps a promise the code does not keep, so
  the document is right about the intent and wrong about the release until the
  cap is applied.
- Neutral: `session.md` grows three defect entries and one owner item; the
  uncommitted hunks of earlier features in that file and in
  `docs/MIGRATION.md` and the threat model are kept apart from these edits.
- Neutral: no code, test, manifest or wire byte moves; the gate is unchanged
  and `make tools-export-check` stays `unchanged`.
