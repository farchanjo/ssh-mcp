Feature: Rmcp Sdk Upgrade
  As the maintainer of the MCP transport
  I want the MCP SDK on a release without a published advisory
  So that an unbounded HTTP response body is not reachable from the network

  # One scenario per FR of spec.md, named with the FR id so the link is
  # machine-checkable. Tags are optional and closed: `@serial` runs the
  # scenario alone, `@security` counts it into the advisory security tally,
  # and `@planned` marks a scenario whose proof does not exist yet, because the
  # test that proves it belongs to the implement phase. A step outside the
  # published vocabulary (`speckit manual --topic verify-steps`) is reported
  # unbound.

  @planned
  Scenario: FR-001 The manifest and the lock resolve rmcp 3.x at or above 3.11.0
    Given Cargo.toml declaring the rmcp 1.6 line
    When  the manifest is moved to the 3.x line at or above 3.11.0
    Then  Cargo.lock resolves rmcp 3.11.0 or a later 3.x
    And   every feature enabled today is enabled under its 3.x name
    And   no other direct dependency requirement and no toolchain pin changed

  @security
  @planned
  Scenario: FR-002 cargo audit reports no vulnerability on the resolved set
    Given the moved dependency set
    When  cargo audit reads Cargo.lock
    Then  no vulnerability is reported
    And   the only remaining finding is the chrono unmaintained warning under RUSTSEC-2026-0543

  @planned
  Scenario: FR-003 The crate compiles with the three error classes adapted
    Given the moved release resolved
    When  cargo check --all-features --all-targets runs
    Then  the crate compiles
    And   the renamed model types, the notification constructors and the response enums are adapted
    And   a test edit is allowed only where the SDK renamed or reshaped an item the test names, listed in the plan with its file:line
    And   no assertion is weakened, removed or made conditional

  @security
  Scenario: FR-004 A 2026-07-28 discover client is refused with the supported list
    Given the feature specification mandates "a 2026-07-28 discover client is refused with the supported list"
    When  the implementation satisfies: server/discover answers -32022 with {requested, supported}
    Then  the observable contract holds: proved by tests/v7_protocol_negotiation.rs::discover_without_initialize_is_refused_with_unsupported_version
    And   the observable contract holds: proved by tests/v7_protocol_http.rs::discover_without_initialize_is_refused_with_unsupported_version

  @security
  Scenario: FR-005 The HTTP transport keeps protocol-level sessions
    Given the feature specification mandates "the HTTP transport keeps protocol-level sessions"
    When  the implementation satisfies: initialize returns Mcp-Session-Id, the session is served and a made-up id is refused
    Then  the observable contract holds: proved by tests/v7_protocol_http.rs::a_returned_session_is_served_and_a_made_up_id_is_refused

  Scenario: FR-006 The protocol ceiling is 2025-11-25 and answers are fixed per revision
    Given the feature specification mandates "the protocol ceiling is 2025-11-25 and answers are fixed per revision"
    When  the implementation satisfies: a known revision up to the ceiling answers itself and a newer or unknown one answers the ceiling
    Then  the observable contract holds: proved by tests/v7_protocol_negotiation.rs::initialize_answers_each_revision
    And   the observable contract holds: proved by tests/v7_protocol_http.rs::initialize_answers_each_revision_and_returns_a_session
    And   the observable contract holds: proved by crate::infra::mcp::tool_router::tests::supported_versions_stop_at_2025_11_25
    And   the observable contract holds: proved by crate::infra::mcp::tool_router::tests::a_newer_client_is_negotiated_down_to_2025_11_25
    And   the observable contract holds: proved by crate::embed::duplex_transport::tests::embed_client_info_names_the_embedded_revision
    And   the observable contract holds: proved by crate::embed::event_mux::tests::inline_push_translation::default_client_reports_the_embedded_revision

  Scenario: FR-007 One output block and one disconnect response render exactly as before
    Given the feature specification mandates "one output block and one disconnect response render exactly as before"
    When  the implementation satisfies: the output block render and the disconnect response render are unchanged
    Then  the observable contract holds: proved by crate::infra::mcp::helpers::output::tests::render_output_block_with_content
    And   the observable contract holds: proved by crate::infra::mcp::render::connection::tests::disconnect_render_emits_block

  Scenario: FR-007 The tools, resources and prompts capabilities are still advertised
    Given the feature specification mandates "the tools, resources and prompts capabilities are still advertised"
    When  the implementation satisfies: the capability set still carries tools, resources and prompts
    Then  the observable contract holds: proved by crate::infra::mcp::tool_router::tests::server_capabilities_still_advertises_tools_and_resources
    And   the observable contract holds: proved by crate::infra::mcp::tool_router::tests::server_capabilities_keeps_prompts_capability

  @planned
  Scenario: FR-007 The whole catalogue, initialize result and push surface are unchanged
    Given an MCP host of the current release
    When  make tools-export-check runs and the push suites run
    Then  the catalogue is unchanged, or each difference is listed, justified and recorded in docs/MIGRATION.md
    And   the whole initialize result (capabilities including experimental.ssh_inline_push, instructions and server identity) keeps its bytes
    And   every tool's envelope and structured content, resources/subscribe, resources/unsubscribe, notifications/resources/updated, notifications/ssh/output, the prompts and the resource templates keep their bytes

  @planned
  Scenario: FR-008 No deprecated SDK API remains and no lint is silenced
    Given the moved library
    When  the gate compiles the crate with clippy -D warnings
    Then  no deprecation warning is printed
    And   no allow attribute is added
    And   the daemon subscribe and unsubscribe use the non-deprecated request path, or the specification is amended

  @planned
  Scenario: FR-009 The project gate is green on the new library
    Given the moved library and the current source tree
    When  the four gate commands run
    Then  each exits 0 with at least 2116 lib tests and 2283 tests with the integration suites
    And   no lint is disabled, no test is ignored and no test file is removed
    And   a test edit is allowed only where the SDK renamed or reshaped an item the test names, listed in the plan with its file:line
    And   no assertion is weakened, removed or made conditional

  @planned
  Scenario: FR-010 The no-default-features build compiles
    Given the crate on the moved library
    When  cargo build --no-default-features runs
    Then  the 38-tool surface compiles

  Scenario: FR-011 A test proves the protocol answer, the session and the Host list
    Given the feature specification mandates "a test proves the protocol answer, the session and the Host list"
    When  the implementation satisfies: the stdio and HTTP tests assert the answers, the session and the Host list
    Then  the observable contract holds: proved by tests/v7_protocol_negotiation.rs::initialize_answers_each_revision
    And   the observable contract holds: proved by tests/v7_protocol_http.rs::initialize_answers_each_revision_and_returns_a_session
    And   the observable contract holds: proved by tests/v7_protocol_http.rs::a_non_loopback_host_is_refused_and_a_loopback_host_is_served

  @planned
  Scenario: FR-012 The push tests pass with their assertions unchanged
    Given a binary built on the moved library
    When  tests/v5_smoke.rs, tests/chaos_inline_push.rs, tests/property_inline_push.rs and tests/v5_daemon_smoke.rs run
    Then  every test in them passes with its assertions unchanged
    And   an edit is allowed only where the SDK renamed or reshaped an item a test names, listed in the plan with its file:line
    And   no assertion is weakened, removed or made conditional

  @planned
  Scenario: FR-013 The Python suites pass
    Given the debug-profile binaries and an SSH endpoint
    When  the HTTP, transfer-progress, rsync HTTP, subscriptions and idempotency suites run under pytest
    Then  each suite exits 0

  @planned
  Scenario: FR-014 speckit validate adds no finding to the baseline
    Given the feature's artifacts on disk
    When  speckit validate runs over the corpus
    Then  the error count stays at 70 with the same rule set
    And   no warning fires

  @planned
  Scenario: FR-015 The change is recorded and false version statements are corrected
    Given the change landed
    When  a reader opens CHANGELOG.md, docs/MIGRATION.md, entry TM-021 or session.md
    Then  each records the move
    And   every version-specific statement the plan listed is true on 3.11 or corrected

  @planned
  Scenario: FR-016 No newer surface is added
    Given the moved SDK
    When  the feature is inspected for newer surfaces
    Then  no 2026-07-28, task, skill, elicitation or channel support was added
