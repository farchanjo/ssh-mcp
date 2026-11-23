# Implementation Plan: Direct Dependency Refresh

## Overview

Move four direct requirements to their current release line and prove the three
wire outputs did not move. `base64` 0.22, `sha2` 0.10, `md-5` 0.10 and
`tower-http` 0.6 become 0.23, 0.11, 0.11 and 0.7. The first three delete the
duplicate copies `russh 0.65.4` and `rmcp 3.11.0` already resolve and, together,
the whole `digest` 0.10 family. `tower-http` removes no duplicate; it leaves a
line with no release since 2026-05-18.

The change is four manifest lines, the lock resolution, one formatting line in
an existing test, three known-answer tests through the project's producers and
one process test for the trace layer. No production code changes. The spike
reaches a green gate; `research.md` carries the evidence.

## Technical Approach

### 1. Manifest and resolution (FR-001, FR-002, FR-003, FR-004)

`Cargo.toml:89` `version = "0.6"` → `"0.7"`; `:110` `"0.22"` → `"0.23"`;
`:129` and `:131` `"0.10"` → `"0.11"`. The `trace` feature stays. Then:

```
cargo update -p base64@0.22.1 -p sha2@0.10.9 -p md-5@0.10.6 -p tower-http@0.6.11
```

The lock difference is exactly the ten removals and two additions of
`research.md` §9; no other entry moves.

### 2. The one test-line adaptation (FR-003)

`tests/v7_rsync_wire_e2e_vm.rs:500` formats the finalized `digest` 0.11 output
with `.iter().map(|b| format!("{b:02x}")).collect()` instead of `{:x}`. The
assertion (`:509`) is unchanged.

### 3. Known-answer tests (FR-005)

Each goes through the producer:

- `inline_payload_params_known_answers` in
  `src/adapters/notifier/rmcp_adapter.rs::tests`: empty, `hello` and
  `0x00..=0xff` through `inline_payload_params`.
- `sha256_local_prefix_hashes_only_the_prefix` in
  `src/adapters/sftp/internal/sftp.rs::tests::verify_helpers`: a six-byte file
  hashed at offset 3 (the digest of `abc`) and at offset 0 (the digest of the
  empty input). `#[tokio::test]`, because the producer is async; the test
  returns `Result` and uses `?`, so no lint attribute is added; the file lives
  in a `tempfile::tempdir()` that removes itself.
- `file_hasher_known_answers` in `src/adapters/rsync/wire/hash.rs::tests`:
  empty and `abc` through `FileHasher::for_protocol(0, 31)`.

Constants and their independent checks are in `research.md` §4. All three
tests pass on the base crates unchanged (`research.md` §5).

### 4. Trace test (FR-006)

`tests/v7_protocol_http.rs` already spawns the binary and speaks HTTP over
`TcpStream`. One `Server::launch` helper replaces the spawn and readiness
bodies of `start`; one `Server::request` helper backs `post` and `get`. The
public shape of the existing tests is unchanged. Add `start_logged` (a
`tempfile::tempdir()` and a log file), `strip_ansi`, `trace_lines` and
`wait_for_lines`, then
`trace_layer_emits_six_debug_lines_for_two_responses_and_none_under_info`:

1. start the binary with `RUST_LOG=debug` and the temp log;
2. `GET /` → 406, then `initialize` `POST` → 200;
3. poll the log until six `tower_http::trace` lines are present or 10 s
   elapse, then assert them in order: `on_request`, `on_response status=406`,
   `on_eos`, `on_request`, `on_response status=200`, `on_eos`;
4. start a second binary with `RUST_LOG` removed, answer both requests, poll
   for the server's `ssh-mcp listening on` line, then assert no `tower_http`
   line;
5. both temp directories drop at the end, so nothing is left behind.

The extra `on_eos` is the 0.7.1 fixed-length difference; `OnEos` receives no
response headers, so it cannot be filtered without losing the streamed line
(`research.md` §2). `docs/MIGRATION.md` records it.

### 5. Gate and records (FR-007 to FR-011)

Run the gate of §Verification. Record the release in `CHANGELOG.md`, the
operator facts in `docs/MIGRATION.md`, the outcome in `session.md:77`, and the
re-anchoring list under `session.md:83-86`. The two historical entries
(`docs/adr/0012-inline-push-notifications.md:867`,
`docs/MIGRATION.md:1372`) stay as written.

## Answers to the Questions

- **Q-1, trace output.** Streamed lines identical; a fixed-length 406 gains one
  DEBUG `on_eos`; none visible under the default `info` filter. Listed in
  `docs/MIGRATION.md`; pinned by the process test above. `research.md` §2.
- **Q-2, defaults.** Keep. `simd-unsafe` is already active through `rmcp`; the
  sha2 feature set is unchanged; disabling would remove features for no gain.
  `research.md` §3.
- **Q-3, test location.** Base64 in `rmcp_adapter.rs::tests` (sync), MD5 in
  `hash.rs::tests` (sync), sha256 in `sftp.rs::tests::verify_helpers`
  (`#[tokio::test]`). `research.md` §5.

## Lock Difference

Ten removals (`base64 0.22.1`, `block-buffer 0.10.4`, `cpufeatures 0.2.17`,
`crypto-common 0.1.7`, `digest 0.10.7`, `generic-array 0.14.7`,
`md-5 0.10.6`, `sha2 0.10.9`, `tower-http 0.6.11`, `version_check 0.9.5`),
two additions (`md-5 0.11.0`, `tower-http 0.7.1`). `cargo tree -d` after:
one `base64`, one `sha2`, one `md-5`, one `digest`.

## File Map

| File | Requirement |
| --- | --- |
| `Cargo.toml:89`, `:110`, `:129`, `:131` | FR-001, FR-002, FR-003 |
| `Cargo.lock` | FR-004 |
| `src/adapters/notifier/rmcp_adapter.rs` (test module) | FR-005 |
| `src/adapters/sftp/internal/sftp.rs` (test module) | FR-005 |
| `src/adapters/rsync/wire/hash.rs` (test module) | FR-005 |
| `tests/v7_protocol_http.rs` | FR-006 |
| `tests/v7_rsync_wire_e2e_vm.rs:500` | FR-003 |
| `CHANGELOG.md`, `docs/MIGRATION.md`, `session.md` | FR-009 |
| `doc/arch/sdd/003-direct-dependency-refresh/{spec,plan,research,tasks}.md`, `doc/arch/adr/0003-direct-dependency-refresh.md`, `doc/arch/schemas/direct-dependency-refresh.cue`, `doc/arch/specs/features/direct-dependency-refresh.feature` | FR-009, FR-010 |

No other file changes.

## Test Edits and Additions

| File:line (after change) | Kind | Requirement |
| --- | --- | --- |
| `src/adapters/notifier/rmcp_adapter.rs:310` | new sync test | FR-005 |
| `src/adapters/sftp/internal/sftp.rs:1553` | new tokio test | FR-005 |
| `src/adapters/rsync/wire/hash.rs:604` | new sync test | FR-005 |
| `tests/v7_protocol_http.rs:26`, `:65`, `:71`, `:318`-`:360` | new helpers | FR-006 |
| `tests/v7_protocol_http.rs:364`-`:399` | new sync test | FR-006 |
| `tests/v7_rsync_wire_e2e_vm.rs:500` | formatting edit | FR-003 |

No test is ignored, removed or weakened; no assertion changes.

## Verification Plan

Run from the repository root, one cargo invocation at a time. Expected in the
last column.

| # | Command | Expected |
| --- | --- | --- |
| 1 | `cargo fmt --all -- --check` | exit 0 |
| 2 | `cargo check --all-features --all-targets` | exit 0 |
| 3 | `cargo clippy --all-features -- -D warnings` | exit 0 |
| 4 | `cargo build --no-default-features --target-dir target/no-default-features` | exit 0 |
| 5 | `cargo build --bins` | exit 0 |
| 6 | `cargo nextest run --test-threads num-cpus --lib` | 2125 passed |
| 7 | `cargo nextest run --test-threads num-cpus --tests --features test-fixtures` | 2299 passed |
| 8 | `cargo audit` | exit 0; no open advisory; `chrono` warning only |
| 9 | `make tools-export-check` | both `unchanged` |
| 10 | `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs scripts/test_http.py` | 15 passed |
| 11 | `… scripts/test_transfer_progress.py` | 1 passed, 1 skipped |
| 12 | `… scripts/test_v7_rsync_http.py` | 10 passed |
| 13 | `… scripts/test_v47_subscriptions.py` | 20 passed |
| 14 | `… scripts/test_v47_idempotency.py` | 9 passed |
| 15 | `diff -r /var/tmp/ssh-mcp/bumps-spike/src src` | no output |
| 16 | `diff -r /var/tmp/ssh-mcp/bumps-spike/tests tests` | no output |
| 17 | `diff /var/tmp/ssh-mcp/bumps-spike/Cargo.toml Cargo.toml` | no output |
| 18 | `diff /var/tmp/ssh-mcp/bumps-spike/Cargo.lock Cargo.lock` | no output |

The five Python suites run with `SSH_MCP_BIN`, `SSH_MCP_STDIO_BIN` and
`SSH_MCP_TAIL_BIN` on the spike's `target/debug` binaries. The reference gate
is 2122 lib and 2295 with integration; the feature adds three lib tests and one
integration test.

Stage rule for the tasks phase: after the manifest and lock move the tree is
red (`E0277` at `tests/v7_rsync_wire_e2e_vm.rs:500`); after the formatting
line it is green. New tests follow.

## Documentation Changes

- `CHANGELOG.md`: an entry for the release that carries the four bumps, the
  ten packages removed from the lock, and the one trace difference.
- `docs/MIGRATION.md`: the trace layer logs at DEBUG and is invisible under the
  default `info` filter; a fixed-length response emits one extra `on_eos` line
  on 0.7.1, it cannot be filtered without losing the streamed line, and it is
  the only observable difference.
- `session.md:77`: the outcome of update plan item 3.
- `session.md:83-86`: the re-anchoring list for corpus citations into files
  changed by features 001, 002 and 003, with
  `doc/arch/observability/observability.md:181` and
  `doc/arch/threat-model/threat-model.md:57`, `:104` (all
  `src/composition/prod.rs:786`; the layer is at `:792`).

## Constitution

- **D-01 spec first** — the change follows the specified, clarified, planned
  and tasked artifacts and the measurements in `research.md`.
- **D-02 dogfooding** — `speckit validate` and `verify --changed` close the
  loop; no rule is weakened.
- **D-03 layering** — no layer or dependency direction changes; only test code
  and a manifest edit.
- **D-04 DDD** — the CUE schema tags every element `ValueObject`.
- **D-05 calisthenics** — the new tests are small and single-purpose.
- **D-06 FQCN** — no new production import, so no `crate::` question arises.
- **D-07 lints and pinning** — no `#[allow]` and no `#[expect]` is added; the
  direct requirements stay exact lines.
- **D-07 audit** — no advisory is open and `cargo audit` stays clean on the
  resolved set (FR-007).
- **D-08 fail loud** — no production code changes, so no new `unwrap` or
  `expect`.
- **D-09 licence** — the licence files are untouched.
- **D-10 guard-mediated writes** — every write goes through the speckit guard.
- **D-11 stable contracts** — tools, arguments, error codes and environment
  variables are unchanged.
- **D-12 small changes** — seven files, one concern, one revertible unit.
- **D-13 offline** — no network path is added.
- **D-17 dependency surface** — the direct requirements move and the lock is
  the resolved set.
- **P-01 inward dependencies** — no adapter calls another adapter.
- **P-02 lock-free hot paths** — no shared state type changes.
- **P-03 panic-free and justified suppressions** — no suppression is added.
- **P-04 frozen wire** — the wire is unchanged; the only output difference is
  one DEBUG log line.
- **P-05 additive wire** — no tool, argument, error code or environment
  variable is added or edited.
- **P-06 verified-change gate** — the full gate is the verification plan.
- **P-07 dev profile** — no release build.
- **P-08 commits and decisions** — ADR 0003 records the decision; one commit
  per independent move, the records separate.
- **P-09 en-US** — every artifact is en-US.

## Rollback

Revert the seven files as one unit (`git apply -R` of
`/var/tmp/ssh-mcp/bumps-spike.patch`), then let the resolver restore the lock.
No data migration, no configuration and no wire state is involved. The four
requirements return to their previous lines in the same step.

## Open Decisions

None. The three clarify questions are answered in `research.md`; the trace
difference is recorded rather than filtered.

## Companion Artifacts

- `research.md` — the evidence: captures, feature tables, constants and their
  checks, stage and gate results, patch stat.
