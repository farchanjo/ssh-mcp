# Tasks: Direct Dependency Refresh

## Task Breakdown

Ten tasks, in the order they run. The tests come first so they pass on the
crates in use today; each crate move is one task; the records follow; the gate
and the commit plan close. Every task leaves `cargo check --all-features
--all-targets` at exit 0, measured in `research.md` §5 and §6.

Every file named below is in the file map of `plan.md`. Build rules for every
task that builds: `RUSTC_WRAPPER=sccache` and `CARGO_BUILD_JOBS=4` stay
exported; no `-j`, no `--release`, no `nice`/`taskset`; tests run with `cargo
nextest run --test-threads num-cpus`, never `cargo test`; no `#[allow]` and no
`#[expect]` is added; no test is ignored, deleted or weakened.

- [x] **T001 Pin the three wire outputs with known-answer tests, before any
  crate moves.**
  - **Files:** `src/adapters/notifier/rmcp_adapter.rs` (test module),
    `src/adapters/sftp/internal/sftp.rs` (test module),
    `src/adapters/rsync/wire/hash.rs` (test module).
  - **Requirements:** FR-005.
  - **Do:** add `inline_payload_params_known_answers` (empty and
    `0x00..=0xff` through `inline_payload_params`, the second with the
    constant of `research.md` §4), `sha256_local_prefix_hashes_only_the_prefix`
    (a six-byte `tempfile::tempdir()` file hashed at offset 3 and offset 0,
    returning `Result` and using `?`) and `file_hasher_known_answers` (empty
    and `abc` through `FileHasher::for_protocol(0, 31)`).
  - **Check:**
    `cargo nextest run --test-threads num-cpus --lib -E 'test(inline_payload_params_known_answers) | test(file_hasher_known_answers) | test(sha256_local_prefix_hashes_only_the_prefix)'`
    → 3 passed, exit 0, on the old crates (`research.md` §5).
  - **Compiles:** yes, exit 0 (`research.md` §6, step 1).

- [x] **T002 Move `base64` to 0.23.**
  - **Files:** `Cargo.toml:110`; `Cargo.lock`.
  - **Requirements:** FR-002, FR-004.
  - **Do:** `base64 = "0.22"` → `"0.23"`; then
    `cargo update -p base64@0.22.1`. No other requirement is edited.
  - **Check:** `cargo tree -d` no longer lists `base64 0.22.1`; the T001
    filter still reports 3 passed, exit 0.
  - **Compiles:** yes, exit 0 (`research.md` §6, step 2).

- [x] **T003 Move `sha2` and `md-5` to 0.11 and adapt the one formatting
  line.**
  - **Files:** `Cargo.toml:129`, `:131`; `Cargo.lock`;
    `tests/v7_rsync_wire_e2e_vm.rs:500`.
  - **Requirements:** FR-003, FR-004.
  - **Do:** both requirements to `"0.11"`; then
    `cargo update -p sha2@0.10.9 -p md-5@0.10.6`; replace the `{:x}` format of
    the finalized digest with `.iter().map(|b| format!("{b:02x}")).collect()`.
    The assertion at `:509` is unchanged.
  - **Check:** `cargo tree -d` lists no `digest 0.10` and no `sha2`/`md-5`
    duplicate; the T001 filter reports 3 passed, exit 0.
  - **Compiles:** yes, exit 0 (`research.md` §6, step 3).

- [x] **T004 Move `tower-http` to 0.7 and add the trace test. This closes the
  source work.**
  - **Files:** `Cargo.toml:89`; `Cargo.lock`; `tests/v7_protocol_http.rs`.
  - **Requirements:** FR-001, FR-006.
  - **Do:** `tower-http = { version = "0.6", features = ["trace"] }` → `"0.7"`;
    then `cargo update -p tower-http@0.6.11`. Refactor the harness to one
    `Server::launch` and one `Server::request`; add `start_logged` (with a
    `tempfile::tempdir()`), `strip_ansi`, `trace_lines` and `wait_for_lines`;
    add
    `trace_layer_emits_six_debug_lines_for_two_responses_and_none_under_info`,
    which asserts the six DEBUG lines in order and, on a second run with
    `RUST_LOG` removed, the `ssh-mcp listening on` line and no `tower_http`
    line.
  - **Check:** the trace test filter reports 1 passed, exit 0. On the base
    crates the same test fails after 10 s with five lines, the sixth missing
    (`research.md` §5).
  - **Closing check:** the four `diff` commands against
    `/var/tmp/ssh-mcp/bumps-spike` (`src`, `tests`, `Cargo.toml`,
    `Cargo.lock`) print nothing.
  - **Compiles:** yes, exit 0 (`research.md` §6, step 4).

- [x] **T005 Record the release in `CHANGELOG.md`.**
  - **Files:** `CHANGELOG.md`.
  - **Requirements:** FR-009.
  - **Do:** add the release entry: the four bumps, the ten packages removed
    from the lock, the wire unchanged, and one DEBUG log line added for a
    fixed-length response.
  - **Check:** `grep -n 'base64' CHANGELOG.md` names the new entry.

- [x] **T006 Record the operator facts in `docs/MIGRATION.md`.**
  - **Files:** `docs/MIGRATION.md`.
  - **Requirements:** FR-006, FR-009.
  - **Do:** state that the trace layer logs at DEBUG, is invisible under the
    default `info` filter, and gains one `on_eos` line for a fixed-length
    response on 0.7.1, with the reason it stays.
  - **Check:** `grep -n 'on_eos' docs/MIGRATION.md` names the new passage.

- [x] **T007 Record the outcome and the re-anchoring list in `session.md`.**
  - **Files:** `session.md:77`; `session.md` under the conformity item
    (`:84-86`).
  - **Requirements:** FR-009.
  - **Do:** append the outcome of item 3; add the re-anchoring record with the
    three known cases (`doc/arch/observability/observability.md:181`,
    `doc/arch/threat-model/threat-model.md:57`, `:104`, all
    `src/composition/prod.rs:786`; the layer is at `:792`).
  - **Check:** `grep -n 're-anchor' session.md` returns the new line.

- [x] **T008 Bind the feature scenarios.**
  - **Files:** `doc/arch/specs/features/direct-dependency-refresh.feature`.
  - **Requirements:** FR-005, FR-006.
  - **Do:** give every scenario its verdict. A scenario is bound only to a
    synchronous `#[test]` that proves its `Then`; an async proof leaves it
    `@planned`.

    | Scenario | Verdict |
    | --- | --- |
    | FR-001 tower-http moves | `@planned`, manifest and lock only |
    | FR-002 base64 moves | `@planned`, manifest and lock only |
    | FR-003 sha2 and md-5 move | `@planned`, manifest, lock and one formatting line |
    | FR-004 lock diff confined | `@planned`, package-list diff |
    | FR-005 bytes_b64 standard base64 and hello vector | bound to `crate::adapters::notifier::rmcp_adapter::tests::inline_payload_params_shape_matches_adr0012` |
    | FR-005 bytes_b64 empty and all-byte-values vectors | bound to `crate::adapters::notifier::rmcp_adapter::tests::inline_payload_params_known_answers` |
    | FR-005 resume-prefix sha256 vector | `@planned`, the proof is `#[tokio::test]` |
    | FR-005 rsync MD5 vectors | bound to `crate::adapters::rsync::wire::hash::tests::file_hasher_known_answers` |
    | FR-006 trace events | bound to `tests/v7_protocol_http.rs::trace_layer_emits_six_debug_lines_for_two_responses_and_none_under_info` |
    | FR-007 audit clean | `@planned`, command |
    | FR-008 gate green | `@planned`, command |
    | FR-009 records written | `@planned`, document review |
    | FR-010 validate baseline | `@planned`, command |
    | FR-011 no new surface | `@planned`, command |

  - **Check:** after this task `speckit verify --changed` reports 12 passed,
    0 failed, 32 unbound (44 scenarios in the changed slice: 12 in feature
    001, 18 in feature 002 and 14 in feature 003; 12 bound, 4 of them in
    feature 003). Before this task the three new citations are `@planned`.

- [x] **T009 Run the gate.**
  - **Files:** none.
  - **Requirements:** FR-007, FR-008, FR-010, FR-011.
  - **Do:** run the numbered verification plan of `plan.md` (18 commands).
  - **Check:** every command exits 0 with the expected result; 2125 lib and
    2299 integration tests passed; `make tools-export-check` reports both
    `unchanged`; `cargo audit` reports the `chrono` warning only.

- [x] **T010 Prepare the commit plan.**
  - **Files:** none (text only).
  - **Requirements:** FR-009.
  - **Do:** one commit per task above plus the corpus commit; every header
    accepted by `speckit commit check --message`.
  - **Check:** each `speckit commit check --message` exits 0.

## Commit Plan

The owner commits; no task stages, commits or pushes. Each row is one commit,
its files, and the state at that commit: the tree compiles and the gate holds.

| # | Header | Files |
| --- | --- | --- |
| 1 | `docs(arch): specify the direct dependency refresh` | `doc/arch/sdd/003-direct-dependency-refresh/`, `doc/arch/adr/0003-direct-dependency-refresh.md`, `doc/arch/schemas/direct-dependency-refresh.cue`, `doc/arch/specs/features/direct-dependency-refresh.feature` |
| 2 | `test: pin the base64, sha256 and md5 wire outputs` | the three test modules of T001 |
| 3 | `build(deps): move base64 to 0.23` | `Cargo.toml:110`, `Cargo.lock` |
| 4 | `build(deps): move sha2 and md-5 to 0.11` | `Cargo.toml:129`, `:131`, `Cargo.lock`, `tests/v7_rsync_wire_e2e_vm.rs` |
| 5 | `build(deps): move tower-http to 0.7` | `Cargo.toml:89`, `Cargo.lock`, `tests/v7_protocol_http.rs` |
| 6 | `docs: record the direct dependency refresh` | `CHANGELOG.md`, `docs/MIGRATION.md`, `session.md` |

## Dependencies

- The corpus commit (row 1) precedes the code commits, because `doc/arch` is
  the source of truth (D-01).
- T002, T003 and T004 are independent of each other in principle but run in
  this order; each keeps the tree green.
- T005 to T007 need T004.
- T009 needs T001 to T008.
- No external system, no credential and no toolchain change is involved.
