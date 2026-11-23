# Tasks: Rmcp Sdk Upgrade

## Task Breakdown

Seventeen tasks, in the order they run. One concern per task (D-12). The
manifest move breaks the build until the adaptations land; T001 opens that
window and T003 closes it, so the red span is two tasks long. Every task after
T003 leaves `cargo check --all-features --all-targets` at exit 0.

The whole change to `Cargo.toml`, `src/` and `tests/` is the measured patch
`/tmp/ssh-mcp-rmcp-spike.patch` (18 files, 649 insertions, 135 deletions),
applied verbatim; T008 is part of it, and its diff check proves the verbatim
application. Every file named below is in the file map
of `plan.md`.

- [x] **T001 Move the dependency requirement and re-resolve the lock.**
  - **Files:** `Cargo.toml:74`; `Cargo.lock` (resolved in place, untracked).
  - **Requirements:** FR-001.
  - **Do:** change `version = "1.6"` to `version = "3.11"`, keep the seven
    feature names, then run `cargo update -p rmcp`. No other requirement is
    edited and no toolchain pin moves.
  - **Check:** `grep -A1 '^name = "rmcp"$' Cargo.lock` prints
    `version = "3.11.0"`. The tree is **red** from here: measured,
    `cargo check --all-features --all-targets` exits 101 with 14 errors
    (`E0432` 10, `E0639` 2, `E0271` 2; `research.md` §4b).
  - **Environment:** `RUSTC_WRAPPER=sccache` and `CARGO_BUILD_JOBS=4` stay
    exported; no `-j`, no `--release`.

- [x] **T002 Rename the model types and rebuild the two notification
  constructors.**
  - **Files:** `src/infra/mcp/helpers/structured.rs`,
    `src/infra/mcp/idempotency.rs`, `src/infra/mcp/prompts.rs`,
    `src/infra/mcp/resource_handlers.rs`,
    `src/infra/mcp/resource_templates.rs`,
    `src/infra/mcp/tool_router.rs` (imports, the `get_info` return type and
    the `Content` uses in its test module),
    `src/adapters/notifier/rmcp_adapter.rs`,
    `src/adapters/subscription/legacy.rs`,
    `src/embed/duplex_transport.rs`, `src/embed/event_mux.rs` (aliases only).
  - **Requirements:** FR-003, FR-008.
  - **Do:** `Content` → `ContentBlock`, `Meta` → `MetaObject`,
    `PromptMessageContent` → `ContentBlock`, `PromptMessageRole` → `Role`,
    `RawResource` → `Resource`, `RawResourceTemplate` → `ResourceTemplate`,
    `ClientInfo` → `ClientConfig`, `ServerInfo` → `ServerConfig`; drop the
    `AnnotateAble` imports and the `no_annotation()` calls; turn the two
    `ResourceUpdatedNotificationParam { uri }` literals into `::new(uri)`. The
    prompt test patterns become `let ContentBlock::Text(content) = ... else`
    and read `content.text`.
  - **Check:** `cargo check --all-features --all-targets` still exits 101,
    with only 2 `E0271` errors left (`research.md` §4b).
  - **Environment:** as T001.

- [x] **T003 Return the response enums, add the ceiling and the negotiation,
  and set the legacy session mode. This closes the red window.**
  - **Files:** `src/infra/mcp/tool_router.rs`,
    `src/composition/prod.rs`.
  - **Requirements:** FR-003, FR-004, FR-005, FR-006.
  - **Do:** `read_resource` and `get_prompt` return `ReadResourceResponse` and
    `GetPromptResponse`, converted with `Into::into` at both return points; add
    `MAX_SUPPORTED_PROTOCOL_VERSION = ProtocolVersion::V_2025_11_25`, the
    `supported_protocol_versions` helper and the trait override, `get_info`
    advertises the constant, and `initialize` ends in
    `self.negotiate_initialize(&request)`; in `build_http_service`, build
    `StreamableHttpServerConfig::default()`, set `legacy_session_mode = true`
    and pass it, with the comment of `plan.md` §5.
  - **Check:** `cargo check --all-features --all-targets` exits 0. Two
    deprecation warnings remain (the dispatcher), and T004 removes them.
  - **Environment:** as T001.

- [x] **T004 Replace the daemon's deprecated subscribe path and name its
  revision.**
  - **Files:** `src/embed/dispatcher.rs`,
    `src/embed/duplex_transport.rs`, `src/embed/event_mux.rs`.
  - **Requirements:** FR-006, FR-008.
  - **Do:** both dispatcher sites use
    `send_request(ClientRequest::{Subscribe,Unsubscribe}Request(Request::new(...)))`
    and the shared `expect_empty_result(&result, op)` helper, which returns
    `Ok` only for `ServerResult::EmptyResult`; add
    `EMBED_PROTOCOL_VERSION = ProtocolVersion::V_2025_11_25` in
    `duplex_transport.rs` and use it at both client sites.
  - **Check:** `cargo clippy --all-features -- -D warnings` exits 0 with no
    deprecation warning, and
    `cargo nextest run --test-threads num-cpus --test v5_daemon_smoke
    --features test-fixtures` reports 7 passed, 0 failed.
  - **Environment:** nextest only, never `cargo test`.

- [x] **T005 Add the unit tests.**
  - **Files:** `src/infra/mcp/tool_router.rs`,
    `src/embed/dispatcher.rs`, `src/embed/duplex_transport.rs`,
    `src/embed/event_mux.rs`.
  - **Requirements:** FR-006, FR-008, FR-011.
  - **Do:** add `supported_versions_stop_at_2025_11_25`,
    `a_newer_client_is_negotiated_down_to_2025_11_25`,
    `capability_set_is_the_frozen_one`,
    `expect_empty_result_accepts_empty_and_reports_other_variants`,
    `embed_client_info_names_the_embedded_revision` and
    `default_client_reports_the_embedded_revision`, each in its module. No
    `#[allow]` is added.
  - **Check:** `cargo nextest run --test-threads num-cpus --lib -E
    'test(supported_versions_stop_at_2025_11_25) | test(a_newer_client_is_negotiated_down_to_2025_11_25) | test(capability_set_is_the_frozen_one) | test(expect_empty_result_accepts_empty_and_reports_other_variants) | test(embed_client_info_names_the_embedded_revision) | test(default_client_reports_the_embedded_revision)'`
    reports 6 passed, 0 failed, 2116 skipped.

- [x] **T006 Add the stdio protocol test file.**
  - **Files:** `tests/v7_protocol_negotiation.rs` (new).
  - **Requirements:** FR-004, FR-006, FR-011.
  - **Do:** `request(message, id)` reads the id and compares it with the
    response; `initialize(asked)` builds the body and calls `request`; one
    capability helper. `initialize_answers_each_revision` covers
    `2024-11-05`, `2025-03-26`, `2025-06-18`, `2025-11-25`, `2026-07-28`,
    `2024-10-07`, `1999-01-01`, `2027-01-01`, `garbage`;
    `discover_without_initialize_is_refused_with_unsupported_version` expects
    `-32022`. No `#[allow]`.
  - **Check:** `cargo nextest run --test-threads num-cpus --test
    v7_protocol_negotiation --features test-fixtures` reports 2 passed, 0
    failed.

- [x] **T007 Add the HTTP protocol test file.**
  - **Files:** `tests/v7_protocol_http.rs` (new).
  - **Requirements:** FR-004, FR-005, FR-006, FR-011.
  - **Do:** spawn `env!("CARGO_BIN_EXE_ssh-mcp")` on a free port with
    `MCP_HOST=127.0.0.1`, fail with the port named after 15 s, set a 10 s read
    timeout, speak HTTP/1.1 over `TcpStream`. The `post` helper takes the
    `Host`; the four tests cover the revision table with `Mcp-Session-Id`,
    served session with a non-empty `tools` array / 404 made-up id, 403
    non-loopback `Host` / 200 loopback, and the discover refusal.
  - **Check:** `cargo nextest run --test-threads num-cpus --test
    v7_protocol_http --features test-fixtures` reports 4 passed, 0 failed.

- [x] **T008 Correct the source comments of the FR-015 table.**
  - **Files:** `Cargo.toml:85-86`, `src/infra/mcp/server.rs:22`,
    `src/infra/mcp/helpers/structured.rs:11`,
    `src/infra/mcp/tool_router.rs:3963-3964`,
    `src/ports/subscriber_registry.rs:143`,
    `src/adapters/notifier/rmcp_peer.rs:139`,
    `src/adapters/subscription/legacy.rs:300`, `:620`,
    `src/embed/event_mux.rs:235`, `src/embed/duplex_transport.rs:49`, `:112`,
    `:118`.
  - **Requirements:** FR-015.
  - **Do:** apply the corrected text of the FR-015 table in `plan.md`. The
    `Cargo.toml` text rests on `cargo tree -i axum` (`axum 0.8.9` under
    `ssh-mcp` only) and `cargo tree -p rmcp --depth 1` (`tower-service`, no
    axum). The `tool_router` text names the docs.rs `Implementation` builder
    and no registry path.
  - **Check:** `grep -rn 'rmcp 1\.\|rmcp-1\.' Cargo.toml src/` prints
    nothing and `grep -rn 'ClientInfo' src/` prints nothing. The tree now
    equals the spike: `diff -r /tmp/ssh-mcp-rmcp-spike-b/src src`,
    `diff -r /tmp/ssh-mcp-rmcp-spike-b/tests tests`,
    `diff /tmp/ssh-mcp-rmcp-spike-b/Cargo.toml Cargo.toml` and
    `diff /tmp/ssh-mcp-rmcp-spike-b/Cargo.lock Cargo.lock` each print
    nothing.

- [x] **T009 Record the release in `CHANGELOG.md`.**
  - **Files:** `CHANGELOG.md`.
  - **Requirements:** FR-015.
  - **Do:** under `## [Unreleased]`, add a `### Security` entry naming
    RUSTSEC-2026-0462 and the move, and a `### Changed` entry naming the
    protocol ceiling and the two changed answers.
  - **Check:** `grep -c 'RUSTSEC-2026-0462' CHANGELOG.md` is at least 1 and
    `grep -n '2025-11-25' CHANGELOG.md` shows the new entry.

- [x] **T010 Write the operator section in `docs/MIGRATION.md`.**
  - **Files:** `docs/MIGRATION.md`.
  - **Requirements:** FR-015, FR-007.
  - **Do:** extend the v7.1.3 → unreleased section with the FR-006 table, the
    statement that the `2026-07-28` and unknown-revision answers are the only
    differences, the `-32022` refusal for a discover client, the
    `legacy_session_mode` setting and its 3.x name, and one sentence that the
    loopback `Host` rule is unchanged.
  - **Check:** `grep -n '2026-07-28' docs/MIGRATION.md` and
    `grep -n 'legacy_session_mode' docs/MIGRATION.md` each show the new
    paragraph.

- [x] **T011 Correct the five current-state documents.**
  - **Files:** `docs/API.md:3`, `docs/DEVELOPMENT.md:19`,
    `docs/ARCHITECTURE.md:133`, `:785`, `docs/OPERATIONS.md:710`, `:773`,
    `docs/CONFIGURATION.md:139`.
  - **Requirements:** FR-015.
  - **Do:** change the version to `rmcp 3.11`; keep each workaround sentence,
    which stays true.
  - **Check:** `grep -rn 'rmcp 1\.6' docs/API.md docs/DEVELOPMENT.md
    docs/ARCHITECTURE.md docs/OPERATIONS.md docs/CONFIGURATION.md` prints
    nothing.

- [x] **T012 Correct `doc/arch/functional/functional.md`.**
  - **Files:** `doc/arch/functional/functional.md:122`.
  - **Requirements:** FR-015.
  - **Do:** say the manifest requests `rmcp 3.11` and the lock resolves
    `3.11.0`.
  - **Check:** `grep -n '3.11' doc/arch/functional/functional.md` shows the
    corrected line.

- [x] **T013 Record TM-001 and close the `rmcp` half of TM-021.**
  - **Files:** `doc/arch/threat-model/threat-model.md`.
  - **Requirements:** FR-015.
  - **Do:** TM-001 records, as a measured fact, that the HTTP transport
    validates `Host` against a loopback-only list, quotes that the docs say it
    protects against DNS rebinding, and states that any other `Host` receives
    403; TM-021 records RUSTSEC-2026-0462 closed and the `chrono` warning
    still open.
  - **Check:** `grep -n 'Host header is not allowed' doc/arch/threat-model/threat-model.md`
    shows the TM-001 text and `grep -n 'RUSTSEC-2026-0462'` shows TM-021 with
    the closure.

- [x] **T014 Record the outcome and the open item in `session.md`.**
  - **Files:** `session.md`.
  - **Requirements:** FR-015.
  - **Do:** item 2 records the outcome; the open items under the HTTP default
    exposure record that the server binds `0.0.0.0` and serves only a loopback
    `Host`, that no project document said so, and that a configurable list is a
    later feature.
  - **Check:** `grep -n 'loopback' session.md` shows the open item.

- [x] **T015 Bind the scenarios a synchronous test proves.**
  - **Files:** `doc/arch/specs/features/rmcp-sdk-upgrade.feature`.
  - **Requirements:** FR-004, FR-005, FR-006, FR-007, FR-011.
  - **Manual forms:** the prose trio's `Then` "end[s] it with `proved by
    tests/<file>.rs::<test name>` or `proved by crate::<module>::<test name>`"
    (`speckit manual --topic verify-steps`, first paragraph), and the step
    production `Then the observable contract holds: proved by
    <tests/file.rs::test_name | crate::module::test_name>`. A citation that
    matches no test, or more than one, fails.
  - **Do:** bind the four scenarios below that a synchronous `#[test]` proves,
    each `Then` followed by `And the observable contract holds: proved by ...`
    per cited test; the other thirteen keep `@planned`.
  - **Scenario list, in file order:**
    1. FR-001 manifest — `@planned`; proof is `cargo update` and a lock grep.
    2. FR-002 audit — `@planned`; `cargo audit`.
    3. FR-003 compile — `@planned`; `cargo check --all-features --all-targets`.
    4. FR-004 negotiation — **bound**; `tests/v7_protocol_negotiation.rs::discover_without_initialize_is_refused_with_unsupported_version`
       and `tests/v7_protocol_http.rs::discover_without_initialize_is_refused_with_unsupported_version`.
    5. FR-005 sessions — **bound**; `tests/v7_protocol_http.rs::a_returned_session_is_served_and_a_made_up_id_is_refused`.
    6. FR-006 ceiling — **bound**;
       `tests/v7_protocol_negotiation.rs::initialize_answers_each_revision`,
       `tests/v7_protocol_http.rs::initialize_answers_each_revision_and_returns_a_session`,
       `crate::infra::mcp::tool_router::tests::supported_versions_stop_at_2025_11_25`,
       `crate::infra::mcp::tool_router::tests::a_newer_client_is_negotiated_down_to_2025_11_25`,
       `crate::embed::duplex_transport::tests::embed_client_info_names_the_embedded_revision`,
       `crate::embed::event_mux::tests::inline_push_translation::default_client_reports_the_embedded_revision`.
    7. FR-007 render — bound already; keep the two render citations.
    8. FR-007 capabilities — bound already; add
       `crate::infra::mcp::tool_router::tests::capability_set_is_the_frozen_one`.
    9. FR-007 whole surface — `@planned`; `make tools-export-check` and the
       push suites.
    10. FR-008 deprecations — `@planned`; `cargo clippy -D warnings`. The
        helper test is not cited because the proof is the command.
    11. FR-009 gate — `@planned`; the gate commands.
    12. FR-010 no-default-features — `@planned`; the build command.
    13. FR-011 protocol tests — **bound**; every test of items 4 to 6, in
        `tests/v7_protocol_negotiation.rs` and `tests/v7_protocol_http.rs`.
    14. FR-012 push — `@planned`; the tests are `#[tokio::test]` (AR-5).
    15. FR-013 Python — `@planned`; external pytest.
    16. FR-014 validate — `@planned`; the validator.
    17. FR-015 documents — `@planned`; grep checks.
    18. FR-016 boundary — `@planned`; inspection.
  - **Check:** `grep -c '@planned'` returns 13 (was 17);
    `speckit verify --changed` prints no `failed` line and reports
    `8 passed, 0 failed, 22 unbound`. Arithmetic: today the changed slice is
    30 scenarios (12 of feature 001, 18 of feature 002) with 4 passed and 26
    unbound, two bound in each feature; four more bound scenarios of feature
    002 give 8 passed and 22 unbound. The slice covers both features while
    feature 001 is uncommitted, so the count drops by its 12 scenarios once
    that feature is committed. `speckit validate` reports no finding on the
    file.

- [x] **T016 Run the gate.**
  - **Files:** none; this task only runs commands.
  - **Requirements:** FR-002, FR-009, FR-010, FR-012, FR-013, FR-014, FR-016.
  - **Do:** run the twelve steps of the verification plan of `plan.md` in
    order, from the repository root, with `RUSTC_WRAPPER=sccache` and
    `CARGO_BUILD_JOBS=4` exported, nextest with `--test-threads num-cpus`, no
    `-j`, no `--release`.
  - **Check:** steps 1 to 5 exit 0; step 6 reports 2122 passed, 0 failed; step
    7 reports 2295 passed, 0 failed; step 8 reports no vulnerability and the
    `chrono` warning; step 9 prints `unchanged` twice; step 10 reports 55
    passed, 1 skipped, 0 failed; step 11 exits 6 with 70 errors, 46 of them
    `fqcn.relative-super`, and no warning; step 12 prints no `failed` line. No
    `#[allow]`, no `#[ignore]`, no deleted or weakened test.

- [x] **T017 Write the commit plan as text.**
  - **Files:** none; the plan is reported in the session.
  - **Requirements:** every FR, through the artifacts.
  - **Do:** propose Conventional Commits headers that `speckit commit suggest`
    and `speckit commit check` accept, one concern per commit — the dependency
    move with its adaptations, the tests, the documentation and the feature
    corpus files under `doc/arch/` — with no attribution trailer. State for
    each commit that the tree compiles and the gate of T016 holds at that
    commit; a commit that leaves the gate red is not proposed. The owner
    commits; no task stages, commits or pushes.
  - **Check:** each header matches `<type>(<scope>): <subject>`,
    `speckit commit check` accepts the list, and every commit carries the
    compile-and-gate sentence.

## Dependencies

- The plan is `plan.md` and its evidence is `research.md`; both are accepted
  before T001 starts.
- T001 opens the red window; T002 continues it; T003 closes it. No later task
  leaves `cargo check --all-features --all-targets` at a non-zero exit.
- T002 and T003 precede T004 to T008; the binary and the tests compile only
  against the adapted library.
- T005 to T007 precede T015: the citations of T015 name the tests those tasks
  create.
- T008 to T014 depend only on T001 and have no successor dependency beyond
  T016, which covers everything.
- T016 runs after every artifact exists; T017 runs last.
- Environment: `RUSTC_WRAPPER=sccache` and `CARGO_BUILD_JOBS=4` are exported
  for every build; nextest is the only runner and takes
  `--test-threads num-cpus`; the Python suites use
  `/root/dev/ssh-mcp/.venv/bin/python`. Code is navigated with the `LSP` tool;
  text search is for strings and the second pass of a rename.
- No external system, actor or credential is needed. `cargo audit` reads the
  RustSec advisory database, which the operator command fetches.
