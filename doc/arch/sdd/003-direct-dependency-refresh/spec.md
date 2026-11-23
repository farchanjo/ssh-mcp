---
id: 01a201ed-1c85-7943-93c4-e993ca8e84db
number: 003
slug: direct-dependency-refresh
status: implemented
created_at: 2026-11-22T06:24:06.27724013Z
---
# Feature Specification: Direct Dependency Refresh

Feature: 003-direct-dependency-refresh
Created: 2026-11-22

Every fact below about the tree or the dependency graph was measured on
2026-11-22 in the throwaway copy `/tmp/ssh-mcp-bumps-probe` made from
`/root/dev/ssh-mcp` at `aac7fc4`, and the command that produced it is named
beside it. A fact taken from upstream documentation says so and was not
measured here. The plan phase re-measures every line on the tree it receives.
No production code changes in this phase.

## Why the move, measured

Four direct requirements sit on the previous release line. Three of them keep a
second copy of the same crate in the graph because the current `russh` 0.65.4
and `rmcp` 3.11.0 already resolve the newer line transitively. The move deletes
those copies, deletes the whole `digest` 0.10 family behind them, and puts the
four crates on their current line.

### Versions and requirers

`cargo tree -i <crate>@<version>` on the base lock, one call per crate:

| Crate | Manifest | Locked today | Required by (base) | Latest, candidate line | Candidate `rust-version` |
| --- | --- | --- | --- | --- | --- |
| `tower-http` | `Cargo.toml:89` | 0.6.11 | `ssh-mcp` only | 0.7.1 | 1.65 |
| `base64` | `Cargo.toml:110` | 0.22.1 | `ssh-mcp` only | 0.23.1 | 1.71.0 |
| `sha2` | `Cargo.toml:129` | 0.10.9 | `ssh-mcp` only | 0.11.1 | 1.88 |
| `md-5` | `Cargo.toml:131` | 0.10.6 | `ssh-mcp` only | 0.11.0 | 1.85 |

The second copies are transitive and already resolved today
(`cargo tree -i` on each): `base64 0.23.1` by `rmcp 3.11.0`; `sha2 0.11.1` by
`russh 0.65.4`, `rmcp 3.11.0`, `ssh-key 0.7.0-rc.12`, `rsa 0.10.0` and the
`p256`/`p384`/`p521` family; `digest 0.11.3` by the same tree. `md-5 0.10.6`
and `digest 0.10.7` are reachable only through this package.

`cargo info <crate>@<line>` gives the `rust-version` column. The project pins
toolchain 1.99.1 (`rust-toolchain.toml:2`) and `rust-version = "1.99"`
(`Cargo.toml:16`), so every candidate line is inside the toolchain.

Release dates, from `https://crates.io/api/v1/crates/<crate>` (upstream
documentation, not measured here): `tower-http` 0.7.1 on 2026-08-31, with no
0.6.x release after 0.6.11 on 2026-05-18; `base64` 0.23.1 on 2026-08-04, with
no 0.22.x release after 0.22.1 on 2024-04-30; `sha2` 0.11.1 on 2026-10-12,
with no 0.10.x release after 0.10.9 on 2025-04-30; `md-5` 0.11.0 on
2026-03-27, with no 0.10.x release after 0.10.6 on 2023-09-22. No advisory is
open on any of the four (FR-007).

### Coupling

`sha2` and `md-5` share the `digest` crate and are the only two users of
`digest 0.10.7` (`cargo tree -i digest@0.10.7`). Moving one alone keeps
`digest 0.10.7`, `block-buffer 0.10.4`, `crypto-common 0.1.7`,
`generic-array 0.14.7`, `cpufeatures 0.2.17` and `version_check 0.9.5` in the
graph for the other. They are therefore one requirement, not two. `base64` and
`tower-http` are independent.

`cargo tree -d` before the move reports two copies of `base64`, `sha2` and
`digest`; after it reports one of each and no other duplicate of the four.

### Call sites

Text search for the `use` lines (`grep -rn`), `LSP findReferences` for
`FileHasher` (49 references in 5 files) and `sha256_local_prefix` (2 references
in 1 file). `LSP findReferences` on `inline_payload_params` returned nothing;
the text search found its two uses, listed here.

| Crate | Production call sites | Test call sites |
| --- | --- | --- |
| `base64` | `src/adapters/notifier/rmcp_adapter.rs:16`, `:17` (imports), `:119` (`STANDARD.encode`), `:125` (`bytes_b64` field) | `tests/property_inline_push.rs:42`, `:43`, `:380`; `tests/v5_daemon_smoke.rs:262`, `:263`, `:305` |
| `sha2` | `src/adapters/sftp/internal/sftp.rs:29`, `:192`, `:202` | `tests/v7_rsync_e2e_vm.rs:248`, `:249`; `tests/v7_rsync_wire_e2e_vm.rs:497`, `:498`, `:500` |
| `md-5` (as `md5`) | `src/adapters/rsync/wire/hash.rs:50`, `:260`, `:272`, `:298`, `:312` | `src/adapters/rsync/wire/hash.rs:469`, `:470`, `:597`, `:598`; `src/adapters/rsync/wire/tokens.rs:377`, `:378`; `src/adapters/rsync/wire/match_.rs:1029`, `:1030`; `tests/property_rsync.rs:368`, `:375`, `:376` |
| `tower-http` | `src/composition/prod.rs:27` (import), `:792` (`TraceLayer::new_for_http()`) | none |

The inline decoder at `src/infra/mcp/tool_router.rs:3209` is hand-rolled and
does not use the `base64` crate.

### Upstream breaking items

Quoted from the upstream changelogs and release notes (upstream documentation,
not measured here):

- `sha2` 0.11.0, https://github.com/RustCrypto/hashes/blob/master/sha2/CHANGELOG.md
  — "Update to `digest` v0.11", "Replace type aliases with newtypes", "Edition
  changed to 2024 and MSRV bumped to 1.85", and the `asm`, `asm-aarch64`,
  `loongarch64_asm`, `compress`, `soft`, `force-soft-compact` and `std`
  features are removed.
- `md-5` 0.11.0, https://docs.rs/crate/md-5/0.11.0/source/CHANGELOG.md — the
  same `digest` v0.11 and newtype changes, and the same removed `std` feature.
- `base64` 0.23.0, https://docs.rs/crate/base64/0.23.1/source/RELEASE-NOTES.md
  — "Update MSRV to 1.71.0", "Added SIMD-accelerated engines behind the
  default-on `simd-unsafe` feature", and `DecodeError::InvalidLastSymbol` now
  includes the decoded value. No breaking change is labelled.
- `tower-http` 0.7.0, https://github.com/tower-rs/tower-http/blob/master/tower-http/CHANGELOG.md
  — six "**breaking:**" items, all in `compression`, `follow-redirect`,
  `services` and the `GrpcCode`/`GrpcFailureClass` enums; the `trace` default
  callbacks "now explicitly parent their tracing events to the request span"
  (non-breaking), and `on_eos` fires on a precise content-length end of stream.

### Probe

Manifest lines moved to the four candidate lines, then
`cargo update -p base64@0.22.1 -p sha2@0.10.9 -p md-5@0.10.6 -p tower-http@0.6.11`
(exit 0) and `cargo check --all-features --all-targets --keep-going`. The check
stopped at one error:

- `E0277` at `tests/v7_rsync_wire_e2e_vm.rs:500`: `format!("{:x}", hasher.finalize())`
  needs `LowerHex`, which the `digest` 0.11 output no longer implements.

With that one line changed in the probe to
`hasher.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>()`, the
same command exits 0 with no warning, and `cargo check --no-default-features`
exits 0. The library needed no change. `python3 scripts/export_tool_catalog.py
--check` prints `unchanged` for both catalogues. `cargo audit` on the moved lock
reports no vulnerability and the one allowed `chrono 0.4.45` unmaintained
warning under RUSTSEC-2026-0543.

No test run happened in this phase, and the `e2e-vm` targets that carry the one
error are `#[ignore]`d, so the compile is not a run.

### Recommendation

Move all four. `base64`, `sha2` and `md-5` each delete a duplicate copy and the
`digest` 0.10 family with it, need no production edit, and stay on the line
`russh` and `rmcp` already use. `tower-http` deletes no duplicate and closes no
advisory; it moves one constructor's dependency off a line with no release since
2026-05-18, and the compile shows no source change. Leaving it would cost
nothing today and would leave one more breaking release for later; this
specification recommends the move and records the trace measurement it carries
(FR-006).

## User Stories

- As an operator, I want the HTTP trace layer on the current `tower-http` line,
  so that the one layer the server installs is not left on a line with no
  release since 2026-05-18.
- As a maintainer, I want the duplicate `base64`, `sha2` and `digest` copies
  gone from the graph, so that one build, one audit and one advisory feed cover
  what the process actually links.
- As a host implementer, I want `bytes_b64`, the resume sha256 and the rsync MD5
  trailer to keep their exact bytes, so that the refresh is a rebuild and not a
  protocol change.
- As a reviewer, I want the one test line the `digest` 0.11 surface breaks
  named in advance, so that the change lands without a silenced lint or a
  weakened assertion.

## Out of Scope

- Every other dependency, direct or transitive.
- The `rmcp` and `russh` lines, already moved by features 002 and 001.
- The defect list of `session.md:88-124` (update plan item 4).
- Channel support (item 5) and the conformity work on the 70 baseline errors
  (item 6).

## Functional Requirements

### FR-001 `tower-http` moves to the 0.7 line

`Cargo.toml:89` changes `version = "0.6"` to `version = "0.7"`; the `trace`
feature stays and no other feature is added. The lock resolves 0.7.1 or a later
0.7.x. The only production use, `TraceLayer::new_for_http()` at
`src/composition/prod.rs:792`, compiles unchanged. `tower-http 0.7.1` declares
`rust-version: 1.65` (`cargo info tower-http@0.7`), inside the pin.

**Acceptance.** `Cargo.toml:89` names the 0.7 line with `features = ["trace"]`;
`Cargo.lock` resolves 0.7.1 or later 0.7.x; `cargo check --all-features
--all-targets` exits 0; against the manifest this feature receives (HEAD plus
features 001 and 002), the only `tower-http` change is line 89.

### FR-002 `base64` moves to the 0.23 line

`Cargo.toml:110` changes `version = "0.22"` to `version = "0.23"`. The used
surface is the `Engine` trait and `engine::general_purpose::STANDARD`
(`src/adapters/notifier/rmcp_adapter.rs:16-17`), whose signatures the probe
found unchanged. `base64 0.23.1` declares `rust-version: 1.71.0`
(`cargo info base64@0.23`), inside the pin. The default feature set gains
`simd-unsafe`; no manifest feature change is required by the used surface.

**Acceptance.** `Cargo.toml:110` names the 0.23 line; `Cargo.lock` resolves
0.23.1 or later 0.23.x; `cargo tree -d` shows one `base64` copy; the
`bytes_b64` text is unchanged (FR-005).

### FR-003 `sha2` and `md-5` move together to their 0.11 lines

`Cargo.toml:129` changes to `version = "0.11"` and `Cargo.toml:131` to
`version = "0.11"`. The lock resolves `sha2 0.11.1` or later 0.11.x and
`md-5 0.11.0` or later 0.11.x. Both crates declare an MSRV inside the pin
(1.88 and 1.85, `cargo info`). The `Digest` trait imports and the
`Sha256`/`Md5` constructors stay as they are; the probe compiled them
unchanged. One test line follows the `digest` 0.11 output type:
`tests/v7_rsync_wire_e2e_vm.rs:500` stops using `{:x}` on the finalized output
and formats the bytes in lowercase hex. That is the only test edit the probe
found, and it weakens no assertion.

Moving both is what removes `digest 0.10.7`, `block-buffer 0.10.4`,
`crypto-common 0.1.7`, `generic-array 0.14.7`, `cpufeatures 0.2.17` and
`version_check 0.9.5` (`cargo tree -i digest@0.10.7`).

**Acceptance.** Both manifest lines name the 0.11 line; the lock resolves
`sha2` 0.11.1 or later 0.11.x and `md-5` 0.11.0 or later 0.11.x;
`cargo tree -d` reports no `digest` 0.10 copy and one `sha2` and one `md-5`
copy; the digests are unchanged (FR-005); the only test edit is the
`tests/v7_rsync_wire_e2e_vm.rs:500` formatting line, or a smaller one.

### FR-004 The lock diff is confined to the four crates and the `digest` 0.10 family

On the probe, `cargo update` removed exactly `base64 0.22.1`,
`block-buffer 0.10.4`, `cpufeatures 0.2.17`, `crypto-common 0.1.7`,
`digest 0.10.7`, `generic-array 0.14.7`, `md-5 0.10.6`, `sha2 0.10.9`,
`tower-http 0.6.11` and `version_check 0.9.5`, and added exactly `md-5 0.11.0`
and `tower-http 0.7.1`. Every other lock entry kept its version.

**Acceptance.** The `Cargo.lock` diff names no package outside that set, and
the `Cargo.toml` diff touches only lines 89, 110, 129 and 131.

### FR-005 The three wire outputs keep their exact bytes, and their proof is stated per output

The outputs are frozen (P-04, `doc/arch/memory/constitution.md:101-104`). For
each, the table says what exists today and what is missing.

| Output | Producer | What the named test proves today | Missing |
| --- | --- | --- | --- |
| standard base64 text of `bytes_b64` | `inline_payload_params` (`src/adapters/notifier/rmcp_adapter.rs:119`) | `crate::adapters::notifier::rmcp_adapter::tests::inline_payload_params_shape_matches_adr0012` (`:283`) builds an `InlinePayload` for `hello` through the producer and asserts `bytes_b64 == "aGVsbG8="` (`:306`): a known answer, through the producer. `tests/property_inline_push.rs::prop_inline_push_event_bytes_b64_is_passthrough` (`:361`) and `tests/v5_daemon_smoke.rs:305` decode the shipped text with the crate and compare the bytes: round-trip, not a constant. | the empty input and all 256 byte values as constants |
| sha256 of the resume prefix, 64 lowercase hex | `sha256_local_prefix` (`src/adapters/sftp/internal/sftp.rs:185`) | no test calls `sha256_local_prefix` (`LSP findReferences`: 2 references, the definition `:185` and the call `:363`). The five named tests do not compute a SHA-256: `parse_sha256_round_trip` (`:1493`) and `parse_sha256_handles_sha256sum_prefix` (`:1502`) parse hex text; `upload_verify_command_uses_sha256sum_full_file` (`:1526`) and `download_verify_command_uses_dd_pipe_sha256sum` (`:1533`) build command strings; `hex_encode_matches_lowercase_format` (`:1540`) encodes a fixed array. | a known-answer SHA-256 through `sha256_local_prefix` |
| 16-byte MD5 trailer at protocol 30 and above | `FileHasher::for_protocol` (`src/adapters/rsync/wire/hash.rs:270`) | `file_hasher_for_protocol_31_uses_md5_no_seed` (`:461`), `file_hasher_for_protocol_30_uses_md5` (`:590`) and `file_hasher_md5_plain_ignores_seed` (`:495`) hash through the producer and compare with `Md5::new()` (`:469-473`): the trailer is MD5 of the bytes, but both sides are the same crate. | a known-answer MD5 through `FileHasher::for_protocol` |

The new vectors go through the producer, not through a direct call to
`base64`, `sha2` or `md5`: a direct call pins the dependency, not the wire
output. The `bytes_b64` vector is a synchronous `#[test]` in the existing test
module; the MD5 vector is a synchronous `#[test]` in `hash.rs::tests`; the
sha256 producer is async, so its vector is a `#[tokio::test]`.

**Acceptance.** The named tests stay green with their assertions unchanged, and
the three missing vectors exist with constants written in the test. A constant
that disagrees with the producer fails the feature.

### FR-006 The trace layer keeps the lines it emits for a streamed response, and the one new line for a fixed-length response is listed

Measured on 2026-11-22 with
`/var/tmp/ssh-mcp/trace-probe2.sh <binary> <log> <unset|debug>` on
`127.0.0.1`, after `cargo build --bin ssh-mcp` in both copies. Each run sends
`GET /` (406, `content-length: 52`) and then one `initialize` `POST` (200,
streamed). The layer logs at DEBUG: with `RUST_LOG` unset the default filter
`EnvFilter::new("info")` (`src/composition/prod.rs:733`) hides every line of
it. With `RUST_LOG=debug`:

| Response | 0.6.11 (base) | 0.7.1 (moved) |
| --- | --- | --- |
| `GET /`, 406, `content-length: 52` | `on_request`, `on_response ... status=406` | `on_request`, `on_response ... status=406`, `on_eos: end of stream stream_duration=N` |
| `POST /mcp`, `initialize`, 200 streamed | `on_request`, `on_response ... status=200`, `on_eos` | the same three lines |

`diff -u` between the normalized captures (timestamp, duration, port and
session id removed) reports exactly one added line, the `on_eos` of the
fixed-length response. The line cannot be removed without also removing the
streamed `on_eos`: the `OnEos` callback receives `trailers`, `stream_duration`
and `span`, and no response headers (upstream documentation of 0.7.1, not
measured here), so the default callback cannot tell the two responses apart; a
no-op callback would drop both. The extra line is therefore a listed difference
in `docs/MIGRATION.md`, justified by the SDK's precise content-length end of
stream (upstream changelog).

The requirement: for a streamed response the three events (`on_request`,
`on_response`, `on_eos`), their targets, their DEBUG level and the span name
and fields (`method`, `uri`, `version`) stay as captured; for a fixed-length
response the two events of 0.6.11 stay and the extra `on_eos` is recorded. A
test covers both response kinds through the binary, with `RUST_LOG=debug` and
with `RUST_LOG` unset.

**Acceptance.** `tests/v7_protocol_http.rs` asserts the five base lines and the
extra sixth line for the fixed-length response, and that no `tower_http` line
appears with `RUST_LOG` unset; `docs/MIGRATION.md` records the extra line and
the reason it stays.

### FR-007 No advisory is open and `cargo audit` stays clean

`cargo audit` on the base lock and on the moved lock reports no vulnerability.
The only finding on both is the unmaintained `chrono 0.4.45` warning under
RUSTSEC-2026-0543, with no fixed upgrade.

**Acceptance.** `cargo audit` on the moved lock exits 0 and its only finding is
that warning.

### FR-008 The gate is green on the moved set, with the reference counts

The verified-change gate of P-06 (`doc/arch/memory/constitution.md:109-112`)
passes: `cargo fmt --all -- --check`, `cargo clippy --all-features -- -D
warnings`, `cargo nextest run --test-threads num-cpus --lib` (at least 2122
passing), `cargo nextest run --test-threads num-cpus --tests --features
test-fixtures` (at least 2295 passing), `cargo build --no-default-features --target-dir target/no-default-features`, the
five Python suites under `/root/dev/ssh-mcp/.venv/bin/python -m pytest -rs`
(`scripts/test_http.py`, `scripts/test_transfer_progress.py`,
`scripts/test_v7_rsync_http.py`, `scripts/test_v47_subscriptions.py` and
`scripts/test_v47_idempotency.py`), and `cargo audit`. No lint is disabled, no
`#[allow]` is added, no test is ignored, removed or weakened. The one test line
of FR-003 is the only permitted edit to an existing test.

**Acceptance.** Each command exits 0; the two nextest runs report at least the
counts above; `git diff` shows no `#[ignore]`, no removed test and no
`#[allow]`.

### FR-009 The change is recorded, and historical records stay as written

`CHANGELOG.md` records the release that carries the move, `docs/MIGRATION.md`
records what an operator and a contributor must know (the DEBUG level of the
trace layer, its invisibility under the default `info` filter, and the extra
fixed-length `on_eos` line with the reason it stays, FR-006), and item 3 of the
update plan in `session.md:77` records the outcome.

Two records name a superseded line but are history and stay as written:
`docs/adr/0012-inline-push-notifications.md:867`, an entry of the References
list of an accepted decision record, and `docs/MIGRATION.md:1372`, inside the
release notes of `v7.0.0-alpha.X — openrsync port slice 5` (heading `:1367`).
A text search over `*.md`, `*.rs`, `*.toml` and `*.py` finds no other statement
that names one of the four old lines outside `Cargo.toml:89`, `:110`, `:129`
and `:131`.

Under the update plan item for the conformity work (`session.md:83-86`),
`session.md` also records that the corpus line citations into files changed by
features 001, 002 and 003 are re-anchored in that work, naming the three known
cases: `doc/arch/observability/observability.md:181` and
`doc/arch/threat-model/threat-model.md:57`, `:104` all cite
`src/composition/prod.rs:786`, and feature 002 moved the layer to `:792`.

**Acceptance.** `CHANGELOG.md`, `docs/MIGRATION.md` and `session.md` carry the
record; the two historical entries are unchanged; `session.md` lists the three
re-anchoring cases under the conformity item.

### FR-010 `speckit validate` adds no finding to the baseline

The baseline measured before this feature is exit 6 with 70 errors and no
warning: `fqcn.relative-super` 46, `branch.else` 8, `pin.ci.unpinned-action` 5,
`pin.docker.unpinned-image` 2, and one each of `readme.mermaid.missing`,
`readme.index.missing`, `readme.image.missing`, `readme.arch-link.missing`,
`pin.cargo.lockfile-missing`, `llms-txt.file.missing`, `fqcn.glob-import`,
`crew.roster.dangling` and `ci.gitlab.missing`.

**Acceptance.** `speckit validate` reports 70 errors with that rule set and no
warning, and no finding names a file of this feature.

### FR-011 The feature adds no other dependency and no new surface

The change adopts four releases on their current lines and nothing else: no new
tool, argument, error code, environment variable, metric or configuration key.
`make tools-export-check` keeps reporting `unchanged` for both catalogue files
(measured on the probe).

**Acceptance.** `make tools-export-check` exits 0 with `unchanged` on both
files; the `initialize` result and the tool schemas keep their bytes.

## Security Requirements

- **Data sensitivity/classification.** The feature reads and writes no data of
  its own. The values it hashes or encodes are the bytes the process already
  carries: the inline-push payload, the local resume prefix and the rsync file
  body.
- **Authentication/authorization.** No authenticated surface and no credential
  moves. The HTTP transport stays unauthenticated as TM-001 records.
- **Input validation.** The untrusted inputs are the bytes handed to the base64
  encoder and the two hash functions. The encoders and digests are total over
  byte slices; the base64 decode side is only used by tests and keeps its
  error type.
- **Cryptography in transit/at rest.** SHA-256 and MD5 are used as wire
  checksums, not as a security boundary, and no key material is involved. The
  algorithms do not change; only their implementation release does (FR-005).
- **Logging/audit.** No new log line and no new destination. The trace layer
  keeps its fields and levels; the one parenting difference is recorded
  (FR-006).
- **Error-handling information exposure.** No error code or message changes.
  The `digest` 0.11 output type is internal to the hashing helpers.

## Acceptance Scenarios

Given the manifest and the lock after the change
When  the resolver reads them
Then  `tower-http` resolves to 0.7.1 or a later 0.7.x with only the `trace`
feature and no other `tower-http` line changes (FR-001).

Given the manifest and the lock after the change
When  the resolver reads them
Then  `base64` resolves to 0.23.1 or a later 0.23.x and the graph holds one
`base64` copy (FR-002).

Given the manifest and the lock after the change
When  the resolver reads them
Then  `sha2` resolves to 0.11.1 or a later 0.11.x and `md-5` to 0.11.0 or a
later 0.11.x, the graph holds one copy of each and no `digest` 0.10 copy, and
the only test edit is the `tests/v7_rsync_wire_e2e_vm.rs:500` formatting line
(FR-003).

Given the base and moved locks
When  their package lists are diffed
Then  only the ten removals and two additions of FR-004 appear, and
`Cargo.toml` changed only at lines 89, 110, 129 and 131 (FR-004).

Given the feature specification mandates "the three wire outputs keep their exact bytes"
When  the implementation satisfies: the existing pins stay green and known-answer vectors are added
Then  the observable contract holds: proved by crate::adapters::notifier::rmcp_adapter::tests::inline_payload_params_shape_matches_adr0012
And   the observable contract holds: proved by crate::adapters::sftp::internal::sftp::tests::verify_helpers::parse_sha256_round_trip
And   the observable contract holds: proved by crate::adapters::sftp::internal::sftp::tests::verify_helpers::upload_verify_command_uses_sha256sum_full_file
And   the observable contract holds: proved by crate::adapters::sftp::internal::sftp::tests::verify_helpers::download_verify_command_uses_dd_pipe_sha256sum
And   the observable contract holds: proved by crate::adapters::sftp::internal::sftp::tests::verify_helpers::hex_encode_matches_lowercase_format
And   the observable contract holds: proved by crate::adapters::rsync::wire::hash::tests::file_hasher_for_protocol_31_uses_md5_no_seed
And   the observable contract holds: proved by crate::adapters::rsync::wire::hash::tests::file_hasher_for_protocol_30_uses_md5
And   the observable contract holds: proved by crate::adapters::rsync::wire::hash::tests::file_hasher_md5_plain_ignores_seed

Given the moved `tower-http`
When  the trace test captures the events
Then  the span name, the `method`, `path`, `latency` and `status` fields and the
levels are unchanged, and the parenting difference is absent or recorded in
`docs/MIGRATION.md` (FR-006).

Given the moved dependency set
When  `cargo audit` reads `Cargo.lock`
Then  no vulnerability is reported and the only finding is the `chrono`
unmaintained warning under RUSTSEC-2026-0543 (FR-007).

Given the moved set and the current source tree
When  the gate commands of FR-008 run
Then  each exits 0 with at least 2122 lib tests and 2295 tests with the
integration suites, and no lint, `#[allow]`, `#[ignore]` or removed test
appears (FR-008).

Given the change landed
When  a reader opens `CHANGELOG.md`, `docs/MIGRATION.md` or `session.md`
Then  each carries its record
And   the two historical entries stay as written
And   `session.md` lists the three re-anchoring cases under the conformity item
(FR-009).

Given the feature's artifacts on disk
When  `speckit validate` runs over the corpus
Then  the error count stays at 70 with the same rule set and no warning fires
(FR-010).

Given the moved set
When  `make tools-export-check` runs and the `initialize` result is read
Then  both catalogue files are `unchanged`, the `initialize` result keeps its
bytes, and no tool, argument, error code, environment variable or metric was
added (FR-011).

## Accepted Risks

- **AR-1 A fixed-length response gains one DEBUG `on_eos` line.** Measured with
  `GET /` (406) and one `initialize` POST on both binaries. Accepted: the line
  is DEBUG-only, invisible under the default `info` filter, and cannot be
  filtered without also dropping the streamed `on_eos`, because `OnEos`
  receives no response headers. FR-006 pins all six lines and
  `docs/MIGRATION.md` records the difference. It blocks FR-006 only.
- **AR-2 `digest` 0.11 drops `LowerHex` from the output type.** Measured as
  `E0277` at `tests/v7_rsync_wire_e2e_vm.rs:500`. Accepted: one formatting line
  changes, and the assertion keeps comparing the same hex string. It blocks
  FR-003 and FR-008.
- **AR-3 The probe is a dev-profile `cargo check`; no test ran.** Accepted: the
  plan phase runs the gate before coding, and compile success is not treated as
  test success. It blocks FR-008.
- **AR-4 The `e2e-vm` targets are `#[ignore]`d.** Their compile is proven, their
  run is not. Accepted: they are not part of the gate; a break in them is found
  by the next VM run.
- **AR-5 `base64` 0.23.1 enables `simd-unsafe` by default.** It is already
  active in the base graph through `rmcp 3.11.0`
  (`cargo tree -e features -i base64@0.23.1`: `base64 feature "simd-unsafe"`
  under `default`), so the move adds no feature and no unsafe surface. The
  workspace `unsafe_code = "deny"` (`Cargo.toml:217`) applies to this package
  and does not reach a dependency. Accepted: keep the defaults; disabling them
  would remove a feature `rmcp` already turns on.

## Observability

The feature adds no metric, counter, NDJSON event or trace span, and changes no
target, level, format or destination. The trace layer keeps the span name, its
three fields and the DEBUG events captured in FR-006; the only difference is the
extra `on_eos` line of a fixed-length response, recorded in `docs/MIGRATION.md`
and invisible under the default `info` filter. Conventions stay those of
`doc/arch/observability/observability.md`.

## Clarifications

### Session 2026-11-22

- **Decision provenance.** Every decision in this specification was taken in
  this session on the owner's instruction to carry the whole update plan
  forward, and it is recorded for the owner's review.
- **The two digest crates are one requirement (FR-003).** They are the only
  users of `digest 0.10.7`; moving one alone keeps the family in the graph.
- **`tower-http` is recommended even though it removes no duplicate.** It
  closes no advisory and buys no graph reduction; it moves the layer off a line
  with no release since 2026-05-18, and the used surface is one constructor.
- **The one test edit is expected (AR-2).** `digest` 0.11 no longer formats the
  output as `LowerHex`; the assertion is unchanged, only the formatting is.
- **The probe is a lower bound on edits, not an upper bound.** It covered
  `--all-features --all-targets`; a target outside that set is unmeasured.
- **Q-1 answered: the streamed lines are identical, a fixed-length response
  gains one DEBUG `on_eos` line, and nothing is visible by default.** Command:
  `/var/tmp/ssh-mcp/trace-probe2.sh <binary> <log> <unset|debug>` against each
  built binary, with `MCP_HOST=127.0.0.1` and a free `MCP_PORT`; each run sends
  `GET /` (406, `content-length: 52`) and one `initialize` `POST`. With
  `RUST_LOG` unset, no line of the layer appears under the default
  `EnvFilter::new("info")` (`src/composition/prod.rs:733`). With
  `RUST_LOG=debug`, the `diff` between the normalized captures has one added
  line, the fixed-length `on_eos`; the streamed response keeps its three lines
  on both versions. `OnEos` receives no response headers, so a custom callback
  cannot filter the fixed-length line without dropping the streamed one. The
  difference is listed in `docs/MIGRATION.md`. The test is a process test in
  `tests/v7_protocol_http.rs` that spawns the binary, captures its log and
  covers both response kinds; no dependency and no production change is
  needed.
- **Q-2 answered: keep the defaults.** `cargo info` gives `base64 0.23.1`
  default `[std, simd-unsafe]`, `sha2 0.11.1` default `[alloc, oid]` and
  `md-5 0.11.0` default `[alloc, oid]`. `cargo tree -e features -i` shows
  `simd-unsafe` already active in the base graph through `rmcp 3.11.0`, and the
  sha2 feature set `{alloc, default, oid, zeroize}` unchanged before and after;
  md-5 moves from `{default, std}` to `{alloc, default, oid}` because 0.11 has
  no `std`. Disabling defaults would remove features `rmcp` already enables and
  buy nothing.
- **Q-3 answered: through the producers, in the existing modules.** The
  `bytes_b64` vector is a synchronous `#[test]` in
  `crate::adapters::notifier::rmcp_adapter::tests`; the MD5 vector is a
  synchronous `#[test]` in `crate::adapters::rsync::wire::hash::tests`; the
  sha256 vector is a `#[tokio::test]` in
  `crate::adapters::sftp::internal::sftp::tests::verify_helpers` because
  `sha256_local_prefix` is async. The sha256 scenario therefore stays
  `@planned`.
