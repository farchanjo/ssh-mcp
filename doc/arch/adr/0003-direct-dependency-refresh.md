---
status: proposed
date: 2026-11-22
deciders: [Fabricio Archanjo]
consulted: []
informed: []
---

# Direct Dependency Refresh

## Context and Problem Statement

Four direct requirements sit on the previous release line of their crates:
`tower-http` 0.6 (`Cargo.toml:89`, locked 0.6.11), `base64` 0.22 (`:110`,
locked 0.22.1), `sha2` 0.10 (`:129`, locked 0.10.9) and `md-5` 0.10 (`:131`,
locked 0.10.6). The current `russh` 0.65.4 and `rmcp` 3.11.0 already resolve
`base64 0.23.1`, `sha2 0.11.1` and `digest 0.11.3` transitively, so the four
old requirements keep a second copy of three crates and the whole `digest` 0.10
family in the graph (`cargo tree -d`, `cargo tree -i digest@0.10.7`).

No advisory is open on any of the four (`cargo audit`, 2026-11-22). The reason
is the maintained line, not a fix: the last 0.6 release of `tower-http` is
2026-05-18, the last 0.22 release of `base64` is 2024-04-30, the last 0.10
release of `sha2` is 2025-04-30 and the last 0.10 release of `md-5` is
2023-09-22 (crates.io API, upstream documentation). `md-5` and `digest 0.10.7`
are reachable only through this package; every other copy in the graph is
transitive and already on the newer line.

`sha2` and `md-5` are coupled: they are the only users of `digest 0.10.7`.
Moving one alone keeps `digest 0.10.7`, `block-buffer 0.10.4`,
`crypto-common 0.1.7`, `generic-array 0.14.7`, `cpufeatures 0.2.17` and
`version_check 0.9.5` for the other. Moving both removes the family.

A compile probe in a throwaway copy, with the four lines moved and
`cargo check --all-features --all-targets`, stopped at one error: `E0277` at
`tests/v7_rsync_wire_e2e_vm.rs:500`, where `format!("{:x}", hasher.finalize())`
needs a `LowerHex` implementation that the `digest` 0.11 output no longer has.
With that one line formatting the bytes instead, the same command exits 0 with
no warning and the library needed no change. `--no-default-features` exits 0,
`make tools-export-check` reports `unchanged`, and `cargo audit` on the moved
lock reports no vulnerability.

The published surface is frozen (P-04 and P-05,
`doc/arch/memory/constitution.md:101-108`). The three wire outputs are the
base64 text of `bytes_b64` (`src/adapters/notifier/rmcp_adapter.rs:119`), the
sha256 of the resume prefix (`src/adapters/sftp/internal/sftp.rs:192`) and the
MD5 trailer of the rsync wire transport
(`src/adapters/rsync/wire/hash.rs:312`).

## Decision Drivers

- Stay on a maintained release line for four crates instead of one that has
  had no release for between seven months and three years.
- Delete the duplicate `base64`, `sha2` and `digest` copies the graph carries
  today, so one build and one audit cover what the process links.
- Keep the three wire outputs byte-identical, because hosts and rsync peers
  read them.
- One concern per change (D-12, `doc/arch/memory/constitution.md:74-77`): four
  mechanical version moves, one formatting line and nothing else.

## Considered Options

- Option A: move all four to their current lines, keep every default feature,
  adapt the one `LowerHex` line, and add the known-answer vectors and the trace
  test the tree lacks.
- Option B: move `base64`, `sha2` and `md-5` only, and leave `tower-http` on
  0.6.
- Option C: leave all four on their current lines.
- Option D: move only `base64`, the single independent crate with a duplicate.

## Decision Outcome

Chosen option: "Option A". It takes the whole update plan entry in one pass,
removes every duplicate the plan named, and leaves no line for a later change
to pick up. Option B would also remove the duplicates, since `tower-http`
contributes none; it is the fallback if the trace measurement of Q-1 finds a
difference that cannot be recorded. Option C leaves three duplicate copies and
the `digest` 0.10 family in every build. Option D leaves the coupled pair
split across two `digest` lines.

The move changes no production source line. The one test edit is the
`LowerHex` formatting at `tests/v7_rsync_wire_e2e_vm.rs:500`; the assertion is
unchanged. `sha2` and `md-5` move together because only the pair removes
`digest 0.10.7`.

The three wire outputs keep their exact bytes. The base64 `hello` vector is
already a known answer through the producer
(`src/adapters/notifier/rmcp_adapter.rs:306`); the sha256 and MD5 paths compare
the crate against itself or have no digest test at all and gain known-answer
vectors through their producers.

The trace layer was captured on 2026-11-22 on both binaries, with `RUST_LOG`
unset and with `RUST_LOG=debug`, for a streamed `initialize` response and for a
fixed-length `GET /` response. It logs at DEBUG, so the default
`EnvFilter::new("info")` (`src/composition/prod.rs:733`) hides it. With
`RUST_LOG=debug`, the streamed response produces the same three events
(`on_request`, `on_response`, `on_eos`) on both versions, and the fixed-length
response gains one `on_eos` line on 0.7.1; the normalized `diff` has exactly
that added line. `OnEos` receives no response headers (upstream documentation
of 0.7.1), so a custom callback cannot filter the fixed-length line without
dropping the streamed one. The extra line is a listed difference in
`docs/MIGRATION.md`, and FR-006 pins both response kinds.

The default features stay on. `base64 0.23.1` enables `simd-unsafe` by default
and `sha2`/`md-5` 0.11 enable `oid` by default; `simd-unsafe` is already active
in the base graph through `rmcp 3.11.0`, and disabling the defaults would
remove a feature `rmcp` already turns on. The workspace `unsafe_code = "deny"`
applies to this package, not to a dependency.

The decision was taken in this session on the owner's instruction to carry the
whole update plan forward, and it is recorded for the owner's review.

### Consequences

- Good: `base64 0.22.1`, `sha2 0.10.9`, `md-5 0.10.6`, `digest 0.10.7`,
  `block-buffer 0.10.4`, `crypto-common 0.1.7`, `generic-array 0.14.7`,
  `cpufeatures 0.2.17`, `version_check 0.9.5` and `tower-http 0.6.11` leave the
  lock; the graph keeps one copy of `base64`, `sha2`, `md-5` and `digest`.
- Good: the process links the same lines `russh` and `rmcp` already use, so one
  audit result covers the whole graph.
- Good: the missing known-answer vectors and the missing trace test become part
  of the tree, so a later hash or trace regression has a pin.
- Bad: `tower-http` 0.7 is a breaking release. Six breaking items exist, all in
  features this package does not enable (`compression`, `follow-redirect`,
  `services`, gRPC classification).
- Bad: a fixed-length response gains one DEBUG `on_eos` line. It is invisible
  under the default `info` filter and cannot be removed without losing the
  streamed `on_eos`, so it is recorded in `docs/MIGRATION.md`.
- Bad: one test line changes for `digest` 0.11, so the change is not purely a
  manifest edit.
- Neutral: `base64` 0.23.1 enables `simd-unsafe` by default and `sha2`/`md-5`
  0.11 enable `oid` by default. `simd-unsafe` is already active through
  `rmcp`, so the move adds no feature; `unsafe_code = "deny"` covers this
  package, not a dependency.
- Neutral: no advisory is closed, because none is open. The benefit is the
  maintained line.
- Neutral: no toolchain change is needed; the highest candidate MSRV is 1.88,
  the project pins 1.99 (`Cargo.toml:16`, `rust-toolchain.toml:2`).
