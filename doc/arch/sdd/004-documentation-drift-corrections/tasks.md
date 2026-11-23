# Tasks: Documentation Drift Corrections

## Task Breakdown

Nine tasks, in the order they run: one per document, one for `session.md`, one
for the gate, and the commit plan as text. Every task is prose; no task builds.
The exact old and new text is in `plan.md` under the file name; the evidence is
in `research.md`. Every file named below is in the file map of `plan.md`, and
nothing else is edited.

The gate is the one of `plan.md` "Verification plan". No test is added, so no
scenario binds: every scenario of
`doc/arch/specs/features/documentation-drift-corrections.feature` stays
`@planned` throughout.

- [x] **T001 Correct the stale-lock sentence in the upgrade guide.**
  - **Files:** `docs/MIGRATION.md:882`.
  - **Requirements:** FR-001.
  - **Do:** replace the last sentence of the bullet with the new sentence of
    `plan.md` "`docs/MIGRATION.md:882`".
  - **Check:** `grep -n 'cargo update -p russh -p russh-sftp' docs/MIGRATION.md`
    prints nothing (exit 1); `grep -c 'needs no manual step' docs/MIGRATION.md`
    returns 2.
  - **Hunk:** line 882 is this feature's only line in the file; the feature 001
    and 002 hunks elsewhere are untouched.

- [x] **T002 Correct the `INTERNAL_ERROR` bullet in the operations guide.**
  - **Files:** `docs/OPERATIONS.md:409`.
  - **Requirements:** FR-002.
  - **Do:** replace the bullet with the new bullet of `plan.md`
    "`docs/OPERATIONS.md:409`".
  - **Check:** `grep -c 'currently never raised' docs/OPERATIONS.md` returns 0;
    `grep -c '17th' docs/OPERATIONS.md` returns 0; `sed -n '409p'
    docs/OPERATIONS.md` contains `SSH_MAX_SUBS_PER_URI` and
    `max subs per URI exceeded`.

- [x] **T003 Correct the two cap code names in the configuration table.**
  - **Files:** `docs/CONFIGURATION.md:149`, `:150`.
  - **Requirements:** FR-003.
  - **Do:** `LANE_LIMIT_PER_URI` → `MAX_SUBS_PER_URI_EXCEEDED` and
    `LANE_LIMIT_TOTAL` → `MAX_SUBS_TOTAL_EXCEEDED`, as in `plan.md`
    "`docs/CONFIGURATION.md:149` and `:150`". Line 153 keeps
    `FILTER_INVALID`.
  - **Check:** `grep -c 'LANE_LIMIT_PER_URI' docs/CONFIGURATION.md` and
    `grep -c 'LANE_LIMIT_TOTAL' docs/CONFIGURATION.md` return 0;
    `sed -n '153p' docs/CONFIGURATION.md` still contains `FILTER_INVALID`.

- [x] **T004 Correct the two `sub_*` error lists in the API reference.**
  - **Files:** `docs/API.md:1106`, `:1153`.
  - **Requirements:** FR-004, FR-005.
  - **Do:** apply the two replacements of `plan.md` "`docs/API.md:1106` and
    `:1153`". Line 1106 keeps `RESOURCE_GONE` and drops the three non-emitted
    names; line 1153 takes `INVALID_ARGUMENT`.
  - **Check:** `sed -n '1106p' docs/API.md` contains `RESOURCE_GONE`,
    `MAX_SUBS_PER_URI_EXCEEDED`, `MAX_SUBS_TOTAL_EXCEEDED` and
    `INVALID_ARGUMENT`; `grep -c
    'LANE_LIMIT_PER_URI\|LANE_LIMIT_TOTAL\|FILTER_INVALID\|INLINE_PUSH_BAD_PARAMS'
    docs/API.md` returns 0.

- [x] **T005 Correct the daemon `subscribe` error line.**
  - **Files:** `docs/DAEMON.md:147`.
  - **Requirements:** FR-006.
  - **Do:** replace the line with the new line of `plan.md`
    "`docs/DAEMON.md:147`".
  - **Check:** `sed -n '147p' docs/DAEMON.md` contains `SUBSCRIBE_FAILED`;
    `sed -n '147p' docs/DAEMON.md | grep -c
    'INVALID_LIFETIME\|INVALID_LAG_POLICY\|SUB_NOT_FOUND'` returns 0.

- [x] **T006 Record the unapplied filter cap in the threat model.**
  - **Files:** `doc/arch/threat-model/threat-model.md:754-758`.
  - **Requirements:** FR-004.
  - **Do:** replace the bullet with the new bullet of `plan.md`
    "`doc/arch/threat-model/threat-model.md:754-758`".
  - **Check:** `grep -rn 'FILTER_INVALID' docs doc/arch/threat-model` returns
    only `docs/CONFIGURATION.md:153`; `sed -n '754,758p'
    doc/arch/threat-model/threat-model.md` contains `INVALID_ARGUMENT` and
    `no production caller applies`; the next bullet is unchanged.
  - **Hunk:** TM-016 only; the other threat-model hunks of earlier features
    are untouched.

- [x] **T007 Record the defects and the open item in `session.md`.**
  - **Files:** `session.md:100-150`.
  - **Requirements:** FR-009.
  - **Do:** append the three defect entries and the one owner item of `plan.md`
    "`session.md`" under their headings.
  - **Check:** `grep -c 'SSH_FILTER_REGEX_MAX' session.md`,
    `grep -c 'lag_policy' session.md`, `grep -c 'INVALID_LIFETIME' session.md`
    and `grep -c 'sub_filter' session.md` each return at least 1.
  - **Hunk:** the three new bullets after the last defect entry and the one new
    owner bullet; the earlier uncommitted hunks are untouched.

- [x] **T008 Run the gate.**
  - **Files:** none.
  - **Requirements:** FR-007, FR-008, FR-009, FR-010.
  - **Do:** run the nine numbered commands of `plan.md` "Verification plan".
  - **Check:** each document equals its copy (`diff` silent), `make
    tools-export-check` reports both catalogues `unchanged`, the three `diff`
    commands against `/var/tmp/ssh-mcp/bumps-spike` are silent,
    `git diff --name-only` names only the seven documents, every acceptance
    command gives its planned result, `speckit validate` stays at 70 baseline
    errors with no warning and no feature-004 finding, and `speckit verify
    --changed` reports no `failed` line.

- [x] **T009 Prepare the commit plan.**
  - **Files:** none (text only).
  - **Requirements:** FR-007.
  - **Do:** use the two headers below; the owner commits. No task stages,
    commits or pushes.
  - **Check:** both `speckit commit check --message` calls exit 0 with the
    output below.

## Commit Plan

Two commits, corpus first (D-01). The feature edits documents only, so the
second commit is the whole document change; the uncommitted hunks of features
001 to 003 in `docs/MIGRATION.md`, `session.md` and the threat model stay in
the second commit's files and are listed by line.

| # | Header | Files |
| --- | --- | --- |
| 1 | `docs(arch): specify the documentation drift corrections` | `doc/arch/sdd/004-documentation-drift-corrections/`, `doc/arch/adr/0004-documentation-drift-corrections.md`, `doc/arch/schemas/documentation-drift-corrections.cue`, `doc/arch/specs/features/documentation-drift-corrections.feature` |
| 2 | `docs: correct drifted error names and the lock note` | `docs/MIGRATION.md:882`, `docs/OPERATIONS.md:409`, `docs/CONFIGURATION.md:149-150`, `docs/API.md:1106`, `:1153`, `docs/DAEMON.md:147`, `doc/arch/threat-model/threat-model.md:754-758`, `session.md` (three defect bullets and one owner bullet) |

`speckit commit check --message` for both headers:

```
$ speckit commit check --message "docs(arch): specify the documentation drift corrections"
$ speckit commit check --message "docs: correct drifted error names and the lock note"
```

## Dependencies

- T001 to T007 are independent of each other and run in file order.
- T008 needs T001 to T007.
- T009 needs T008.
- No external system, no credential, no toolchain and no build is involved.
