# Tasks: Russh Advisory Upgrade

## Task Breakdown

Eleven tasks, in the order they run. No task is merged and none is split: the
boundary is one concern per task, which is what D-12 asks for and what makes each
one verifiable alone. Every file named below is in the file map of `plan.md`.

- [x] **T001 Move the two dependency requirements and re-resolve the lock.**
  - **Files:** `Cargo.toml` (lines 96-97); `Cargo.lock` (resolved in place, untracked).
  - **Requirements:** FR-001.
  - **Do:** set `russh = "0.65"` and `russh-sftp = "4"`, then run
    `cargo update -p russh -p russh-sftp`. No other requirement is edited.
  - **Check:** `grep -A1 -E '^name = "(russh|russh-sftp)"$' Cargo.lock` prints
    `russh` with `version = "0.65.4"` and `russh-sftp` with `version = "4.0.0"`,
    and the `Cargo.toml` diff holds those two lines and no other.

- [x] **T002 Adapt `check_server_key` and add its tests with it.**
  - **Files:** `src/adapters/ssh/internal/session.rs`.
  - **Requirements:** FR-003, FR-004, FR-005.
  - **Do:** replace the override at `:122-131` with the body in `plan.md`: the
    `&keys::PublicKeyOrCertificate` argument, the `skip_verification` early
    return, a two-arm `match` with no wildcard arm, the plain-key arm calling
    `verify_host_key` unchanged, and the certificate arm logging at `warn` with
    `host` and `port` before returning `Ok(false)`. Add
    `certificate_is_rejected_and_not_recorded` in the same change, because it is
    the proof of the behaviour the override now states. Leave
    `skip_flag_accepts_any_host_key`, `recorded_key_mismatch_is_rejected` and
    `unknown_host_is_learned_via_tofu` unedited.
  - **Check:** `cargo check --all-features --all-targets` exits 0 — before this
    task the moved library fails with `E0053` at `session.rs:124:28`, so the exit
    is the FR-003 proof. Then
    `cargo nextest run --lib --test-threads num-cpus -E 'test(skip_flag_accepts_any_host_key) | test(recorded_key_mismatch_is_rejected) | test(unknown_host_is_learned_via_tofu) | test(certificate_is_rejected)'`
    reports 4 passed, 0 failed.

- [x] **T003 Offer no host-certificate algorithm, and pin it with a test.**
  - **Files:** `src/adapters/ssh/internal/client.rs`.
  - **Requirements:** FR-004.
  - **Do:** add `host_key_certificates: Cow::Borrowed(&[])` to the `Preferred`
    literal in `build_client_config` (`:89-118`) with `use std::borrow::Cow;`
    added to the `std` import block, so the empty list is the project's own
    statement and not the library default. Add
    `test_no_host_certificate_algorithm_is_offered` to the `client_config` module
    in the same change.
  - **Check:**
    `cargo nextest run --lib --test-threads num-cpus -E 'test(test_no_host_certificate_algorithm_is_offered) | test(test_compression_enabled_includes_zlib) | test(test_builds_config_with_keepalive)'`
    reports 3 passed, 0 failed.

- [x] **T004 Classify channel-open failures on the typed error, at both call sites.**
  - **Files:** `src/adapters/ssh/internal/client.rs`.
  - **Requirements:** FR-006.
  - **Do:** replace `is_transient_open_error(msg: &str) -> bool` (`:684-694`)
    with the typed `const fn` of `plan.md`; change `open_session_with_retry`
    (`:526`) to pass `&e` and drop its now-unused `let msg = e.to_string();`;
    change `handle_open_error` (`:672`) to pass `err`, keeping `msg` for the
    `warn!` above it. Replace the five string assertions at `:1398-1414` with
    `exhaustion_reasons_are_transient` and `other_reasons_and_errors_are_not_transient`,
    which build real `russh::Error` values; keep
    `backoff_starts_at_100ms_and_caps_at_one_second` unchanged. The removal and
    its replacement land in the same task.
  - **Check:**
    `cargo nextest run --lib --test-threads num-cpus -E 'test(exhaustion_reasons_are_transient) | test(other_reasons_and_errors_are_not_transient) | test(backoff_starts_at_100ms_and_caps_at_one_second)'`
    reports 3 passed, 0 failed, and
    `grep -n 'contains("connectfailed")\|contains("resource_shortage")\|contains("administratively")' src/adapters/ssh/internal/client.rs`
    prints nothing.

- [x] **T005 Run the project gate.**
  - **Files:** none; this task only runs commands.
  - **Requirements:** FR-007.
  - **Do:** run the four gate commands with `RUSTC_WRAPPER=sccache` and
    `CARGO_BUILD_JOBS=4` exported.
  - **Check:** `cargo fmt --all -- --check` exits 0;
    `cargo clippy --all-features -- -D warnings` exits 0;
    `cargo nextest run --test-threads num-cpus --lib` reports at least 2114
    passed and 0 failed;
    `cargo nextest run --test-threads num-cpus --tests --features test-fixtures`
    reports at least 2281 passed and 0 failed. No lint is added to an allow list,
    no `#[ignore]` is added and no test file is deleted anywhere in the diff.

- [x] **T006 Audit the resolved dependency set.**
  - **Files:** none; this task only reads the resolved set.
  - **Requirements:** FR-002.
  - **Check:** `cargo audit` names no advisory for `russh`, `rsa` or
    `crypto-bigint`; the only vulnerability it prints is RUSTSEC-2026-0462 on
    `rmcp 1.8.0` and the only warning is the unmaintained `chrono 0.4.45`. The
    command exits non-zero because that one vulnerability remains, which is the
    accepted residual AR-5 of the specification, so the check is the content of
    the report and not the exit status.

- [x] **T007 Build the dev-profile binaries and run the Python integration suites.**
  - **Files:** none; this task writes only `target/debug/`.
  - **Requirements:** FR-008.
  - **Do:** `cargo build --bins`, then run the three suites with the repository
    interpreter: `/root/dev/ssh-mcp/.venv/bin/python -m pytest
    scripts/test_http.py scripts/test_transfer_progress.py
    scripts/test_v7_rsync_http.py`, from the repository root so `scripts/conftest.py`
    makes `helpers` importable.
  - **Check:** each suite exits 0. The SSH endpoint comes from
    `SSH_MCP_TEST_TARGET` when it is set and otherwise from the in-process
    paramiko fixture, so no system `sshd` is required.

- [x] **T008 Prove the published tool catalogue did not move.**
  - **Files:** none; this task only compares the committed export with the binary.
  - **Requirements:** FR-009.
  - **Check:** `make tools-export-check` exits 0, which means the export
    regenerated from the `target/debug` binary matches `docs/TOOL_CATALOG.md` and
    `docs/tool-catalog.json` byte for byte. No change under `src/infra/mcp/` or
    `src/adapters/config/` appears in the diff.

- [x] **T009 Record the change in the four documents.**
  - **Files:** `CHANGELOG.md`, `docs/MIGRATION.md`,
    `doc/arch/threat-model/threat-model.md`, `session.md`.
  - **Requirements:** FR-010.
  - **Do:** under `## [Unreleased]` in `CHANGELOG.md` (`:8`), add a `### Security`
    entry naming the four `russh` advisories plus RUSTSEC-2023-0071 and
    RUSTSEC-2026-0387 as closed, a `### Changed` entry naming the dependency move
    and the host-certificate rejection, and a `### Fixed` entry for the retry
    classification; add the operator section to `docs/MIGRATION.md` and its row in
    the table at `:32`; update entry TM-021 to say the `russh` half of the cure
    landed and that RUSTSEC-2026-0462 and the `chrono` warning stay open; note the
    outcome in item 1 of the update plan in `session.md` (`:45-48`).
  - **Check:** `grep -c 'RUSTSEC-2026-0492' CHANGELOG.md` is at least 1,
    `grep -n 'RESOURCE_SHORTAGE' docs/MIGRATION.md` shows the new section,
    `grep -n 'RUSTSEC-2026-0462' doc/arch/threat-model/threat-model.md` shows the
    updated TM-021, and `session.md` item 1 names `0.65.4` and `4.0.0`.

- [x] **T010 Bind every scenario to its proof, where the resolver can see it.**
  - **Files:** `doc/arch/specs/features/russh-advisory-upgrade.feature`.
  - **Requirements:** FR-004 and FR-006 for the bound scenarios; FR-001,
    FR-002, FR-003, FR-005, FR-007, FR-008, FR-009, FR-010 and FR-011 keep
    their `@planned` tag.
  - **Do:** restate as the prose trio of `speckit manual --topic verify-steps`
    only the scenarios whose proof is a `#[test]` function: FR-004 "the client
    offers no host-certificate algorithm", citing
    `crate::adapters::ssh::internal::client::tests::client_config::test_no_host_certificate_algorithm_is_offered`,
    and FR-006, citing
    `crate::adapters::ssh::internal::client::tests::transient_open_errors::exhaustion_reasons_are_transient`
    and `::other_reasons_and_errors_are_not_transient`, one `Then` per cited
    test and each `And` continuing that `Then`. Keep FR-005 and the FR-004
    certificate scenario in prose steps with `@planned`: their proofs are
    `#[tokio::test]` functions (`src/adapters/ssh/internal/session.rs:164`,
    `:177`, `:200`, `:218`) and `speckit verify` resolves a citation only to a
    `#[test]` function (AR-6 of the specification). The eight scenarios whose
    proof is a command — the resolved version lines, the audit report,
    `cargo check`, the gate, the Python suites, `make tools-export-check`, the
    four documents and the SFTP call scan — keep `@planned`: the engine binds a
    `Then` only to a test citation, and inventing a test for a command would
    assert something the repository does not run.
  - **Check:** `grep -c '@planned'` on the feature file returns 10;
    `grep -c 'proved by'` returns 3;
    `speckit verify --changed` prints no `failed` line, prints `passed` for
    FR-004 and FR-006, and ends `scenario(s): 2 passed, 0 failed, 10 unbound`;
    `speckit nexttest` reports no `unproven-fr` item and no FR-004, FR-005 or
    FR-006 item as a failed scenario (its one aggregate `failing-scenario` item
    counts the unbound scenarios of the last recorded run fragment, so it stays
    non-zero by design); `speckit validate` reports no finding on the file.

- [x] **T011 Run `speckit validate` on the finished change.**
  - **Files:** none; this task only runs the validator.
  - **Requirements:** every FR, through the artifacts it leaves.
  - **Check:** `speckit validate` exits 6 with 70 errors of the baseline rule set
    (`fqcn.relative-super` 46, `branch.else` 8, `pin.ci.unpinned-action` 5,
    `pin.docker.unpinned-image` 2 and one each of `ci.gitlab.missing`,
    `crew.roster.dangling`, `fqcn.glob-import`, `llms-txt.file.missing`,
    `pin.cargo.lockfile-missing`, `readme.arch-link.missing`,
    `readme.image.missing`, `readme.index.missing`, `readme.mermaid.missing`), no
    warning, no finding naming a file of this feature, and no rule that did not
    fire before the feature.

## Dependencies

- The plan is `plan.md` and its evidence is `research.md`; both are accepted
  before T001 starts.
- T001 precedes T002 to T006: the source compiles only against the moved library,
  and the audit reads the lock it resolves.
- T002, T003 and T004 precede T005: the gate covers the tests those tasks add.
- T002 to T004 precede T010: the citations of T010 name the tests those tasks
  create.
- T001 precedes T006 and T007: both read the moved resolution and the binaries
  built from it.
- T007 needs the binaries from its own `cargo build --bins`; T008 needs the same
  `target/debug` tree.
- T009 depends on nothing but T001 and has no successor dependency.
- T011 runs last, after every artifact it validates exists.
- Environment: `RUSTC_WRAPPER=sccache` and `CARGO_BUILD_JOBS=4` are exported for
  every build; nextest takes `--test-threads num-cpus` on the command line, which
  overrides the `test-threads = 4` of `.config/nextest.toml`; the Python suites
  use `/root/dev/ssh-mcp/.venv/bin/python`; the owner lands the commit, and no
  task stages, commits, pushes or merges.
- No external system, actor or credential is needed. `cargo audit` reads the
  RustSec advisory database, which the operator command fetches; the task does
  not run inside the change loop.
- Crew class: the tasks scaffold asks for none, and the project's roster is
  `crew.roster.dangling` in the validate baseline, so no task names a class.
