# Session resume — ssh-mcp v7.1.2 shell-open race + dev-loop hardening

**Last touched:** 2026-11-16
**Repo:** `/root/dev/ssh-mcp`
**Branch:** `master` (in sync with `origin/master` at session start)
**Latest tag:** `v7.1.2`

## What is in flight (uncommitted, verified)

Patch series in the working tree. Gates and the touched Python files are green;
see "Verification run" below.

### 1. Shell-open race fix (functional)

`OpenShellUseCase::execute` and `RusshAdapter::open_shell`'s detached registration
sink both write the shell row. Whichever landed second met `Entry::Occupied`, and
`ssh_shell_open` surfaced `INTERNAL_ERROR: shell <id> already exists` under load.
The new `persist_shell` helper treats an occupied row as success and overwrites it
with the use-case entity (the adapter builds its copy before `apply_overrides`, so
keeping the sink row would persist the default TTL instead of the caller's
override), re-raising only genuine write failures. Scenario 13
(`row_already_written_by_registration_sink_is_not_a_conflict`) pins both halves —
id coexistence and override authority — and fails with the original
`Internal("... already exists")` when the helper is reverted.

### 2. Dev-loop / CI hardening

- `JOBS` pinned to 4 everywhere (Makefile default, `.cargo/config.toml` `[build] jobs`,
  `.config/nextest.toml` `test-threads`, CI `CARGO_BUILD_JOBS`). Every cargo
  invocation is pinned; unset would mean "all cores".
- `make gates` (fmt + dev clippy + both nextest suites) and `make lint-dev` added.
  Release builds belong to the owner — see AGENTS.md → Working agreement.
- CI runs lib/integration through `cargo nextest` (`ci` profile, JUnit) and
  installs nextest when absent.
- The Python suites resolve binaries via the new `scripts/helpers/paths.py` (dev
  profile default; `SSH_MCP_BUILD_PROFILE` / `SSH_MCP_*_BIN` overrides).
  `run_all.sh` builds the 38-tool surface into `target/no-default-features` so it
  can no longer overwrite the full binary.
- `helpers/local_sshd.py`: one process-wide host key (port recycling no longer
  trips `known_hosts`), `EOFError` added to the tolerated transport errors, and
  the `_pty_to_channel` pump no longer dies silently under a flooding child.
- `test_http.py` ctrl-C case drives a throttled loop instead of `yes`.
- `test_v5_adversarial.py`: `rejected_at_schema` helper plus a `forward_client`
  fixture that skips on 38-tool builds; forward tests use the split
  `remote_address` / `remote_port` arguments; the tail fixture resolves through
  `helpers.paths` so the Windows `.exe` is found.
- CI `build-test` pins `SSH_MCP_BUILD_PROFILE=release`, matching its release
  build and the `helpers.paths` docstring instead of silently testing the debug
  tree nextest leaves behind.

### 3. File-transfer session-limit fixes

- Per-session caps now count **running** work instead of history. Terminal
  transfer rows stay in the repository for `ssh_transfer_progress` /
  `transfer://<id>/progress` readers, but release the slot; the same applies to
  rsync via `count_active_by_session`. Before this, the 11th upload/download on
  a long-lived session (5th rsync) failed with `MAX_TRANSFERS_EXCEEDED` while
  nothing was in flight. New port methods `count_running_by_session` /
  `count_active_by_session`, and `insert_if_under_cap` ignores terminal rows.
- `open_sftp_session` now uses the shared `open_session_with_retry`, so a
  transient `MaxSessions` / `RESOURCE_SHORTAGE` refusal backs off (100 ms → 1 s,
  25 attempts) instead of failing the transfer. `is_transient_open_error` is now
  case-insensitive.
- Regression coverage: repo-level tests for both adapters, use-case tests for
  upload/download/rsync, classifier/backoff unit tests, and an HTTP integration
  test that drives 12 sequential uploads on one session
  (`test_more_than_ten_sequential_uploads_on_one_session`).

### 4. Disconnect counters stop reporting finished work as interrupted

- `ssh_disconnect` / `ssh_disconnect_agent` counted every removed row, so a
  session whose ten uploads had completed answered `TRANSFERS_ABORTED: 10`
  (and a session with sixteen finished execs answered `COMMANDS_CANCELLED: 16`).
  Terminal rows are still purged; only running resources increment the
  counters. Verified by mutation: forcing the unconditional increment makes the
  live test report `transfers_aborted: 12`.

### 5. Docs

AGENTS.md, CI.md, CONTRIBUTING.md, README.md, docs/DEVELOPMENT.md,
docs/MIGRATION.md, docs/OPERATIONS.md and the script docstrings aligned with the
dev-only loop; CHANGELOG `[Unreleased]` entry added.

## Verification run

- `make gates` → green on every run: fmt, dev clippy, 2114 lib + 167
  integration tests (2281 total), 0 failed.
- Transfer-cap mutation checks: reverting `count_running_in_bucket` makes the
  unit tests fail, and the live 12-upload test fails at upload 11 with
  `REASON: [MAX_TRANSFERS_EXCEEDED]`; restored implementation passes.
- Disconnect-counter mutation check: forcing the unconditional increment makes
  the live test read `transfers_aborted: 12` instead of 0.
- Mutation check: with `persist_shell` reverted to a bare `insert`, scenario 13
  fails with `Internal("shell sess-race-shell-1 already exists; ...")`.
- `make tools-export-check` → `unchanged` for both catalogue files.
- `pytest scripts/test_http.py scripts/test_v5_adversarial.py scripts/test_v47_subscriptions.py` → 98 passed.
- `CHAOS_FILTER=chaos05 scripts/chaos_runner.sh` → summary parses, exit 0;
  a non-matching filter exits 1.
- `bash -n` clean on the three modified shell scripts (shellcheck absent).
- CI workflow parses as YAML (ruby) and the pin lands on the `build-test` job.
- macOS cross-compile against `/opt/sdk/MacOSX26.1.sdk` (via `macosx_run`):
  `cargo check --all-features --all-targets` and `cargo build --bins` pass for
  `aarch64-apple-darwin` and `x86_64-apple-darwin`; all six Mach-O binaries
  link. Dev profile only — release stays the owner's path.
- Full modern suite (`pytest scripts/ --ignore=_legacy_v2`) → RUNNING at session pause; see the note below.
- `scripts/_legacy_v2/test_http_v2.py` performs a live handshake at import time and
  needs a server on `:8000`; run it separately from a harness that provides one.

## Open items

1. Commit the series as contextual commits (fix / build / ci / scripts / fixtures / docs / session).
2. `git push origin master`.
3. Owner decision: cut `v7.1.3` (version bump + tag + release notes) or leave the
   fix under `[Unreleased]`.
4. Optional carry-over: `clap` `--version` / `--help` on `ssh-mcp` and
   `ssh-mcp-stdio` so deploy validation has a clean `exit 0` path.

## Ground truth (for any agent dispatched next session)

| key | value | source |
|---|---|---|
| version | 7.1.2 | Cargo.toml |
| lib_tests_passed | 2103 | `cargo nextest run --lib` |
| integration_tests | 167 across 12 binaries | tests/ |
| loom_invariants | 32 across 3 files | tests/lockfree_invariants*.rs |
| rsync_proto_advertised | 32 (downgrades to 31 via min(local, remote)) | src/adapters/rsync/wire/session.rs |
| error_codes_total | 56 | ADR 0007 + 0010 + 0011 |
| msrv | 1.99 | Cargo.toml |
| edition | 2024 | Cargo.toml |
| ADRs | 12 (0001-0012) | docs/adr/ |
| tools_with_port_forward | 39 | `#[tool]` count in src/infra/mcp/tool_router.rs |
| tools_without_port_forward | 38 | |
| `release_when_no_subs` | bool arg — JSON `true`/`false` only | rmcp/serde rejects strings with `-32602` |
| build profile default | `debug` (`SSH_MCP_BUILD_PROFILE=release` retargets) | scripts/helpers/paths.py |
| jobs pin | 4 | Makefile, `.cargo/config.toml`, `.config/nextest.toml`, ci.yml |

## Convention reminders

- Conventional Commits, subject ≤ 72 chars, one context per commit. Never commit
  unless the owner asks.
- `make gates` must stay green after any code change. CI owns the release-profile
  clippy gate; never run a release build from a session.
- En-US on disk; chat follows the conversation language.
