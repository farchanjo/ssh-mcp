Feature: Direct Dependency Refresh
  As the maintainer of the dependency surface
  I want the four direct requirements on their current release line
  So that the graph carries one copy of each and stays on a maintained line

  # One scenario per FR of spec.md, named with the FR id so the link is
  # machine-checkable. Tags are optional and closed: `@serial` runs the
  # scenario alone, `@security` counts it into the advisory security tally,
  # and `@planned` marks a scenario whose proof does not exist yet, because the
  # test that proves it belongs to the implement phase. A step outside the
  # published vocabulary (`speckit manual --topic verify-steps`) is reported
  # unbound. A scenario cites a test only when the test proves its Then.

  @planned
  Scenario: FR-001 tower-http moves to the 0.7 line
    Given Cargo.toml line 89 requiring tower-http 0.6 with the trace feature
    When  the requirement moves to the 0.7 line
    Then  Cargo.lock resolves 0.7.1 or a later 0.7.x
    And   TraceLayer::new_for_http at src/composition/prod.rs:792 compiles unchanged
    And   the only tower-http change against the received manifest is line 89

  @planned
  Scenario: FR-002 base64 moves to the 0.23 line
    Given Cargo.toml line 110 requiring base64 0.22
    When  the requirement moves to the 0.23 line
    Then  Cargo.lock resolves 0.23.1 or a later 0.23.x
    And   cargo tree -d reports one base64 copy
    And   the bytes_b64 text is unchanged

  @planned
  Scenario: FR-003 sha2 and md-5 move together to their 0.11 lines
    Given Cargo.toml lines 129 and 131 requiring sha2 0.10 and md-5 0.10
    When  both requirements move to the 0.11 line
    Then  the lock resolves sha2 0.11.1 or later and md-5 0.11.0 or later
    And   cargo tree -d reports no digest 0.10 copy
    And   the only test edit is the formatting line at tests/v7_rsync_wire_e2e_vm.rs:500

  @planned
  Scenario: FR-004 the lock diff is confined
    Given the base lock and the moved lock
    When  their package lists are diffed
    Then  only the ten removals and two additions of the specification appear
    And   Cargo.toml changed only at lines 89, 110, 129 and 131

  Scenario: FR-005 the bytes_b64 text is standard base64 and the hello vector stays
    Given the feature specification mandates "the bytes_b64 text is standard base64"
    When  the implementation satisfies: inline_payload_params encodes hello as aGVsbG8=
    Then  the observable contract holds: proved by crate::adapters::notifier::rmcp_adapter::tests::inline_payload_params_shape_matches_adr0012

  Scenario: FR-005 the bytes_b64 known-answer set covers empty and all byte values
    Given the feature specification mandates "the bytes_b64 known-answer set covers empty and all byte values"
    When  the implementation satisfies: inline_payload_params encodes the empty input and every byte value
    Then  the observable contract holds: proved by crate::adapters::notifier::rmcp_adapter::tests::inline_payload_params_known_answers

  @planned
  Scenario: FR-005 the resume-prefix sha256 has a known-answer test
    Given the producer sha256_local_prefix
    When  it hashes a six-byte file at offset 3 and at offset 0
    Then  each result equals a constant written in the test
    And   the scenario stays planned because the proof is a tokio test

  Scenario: FR-005 the rsync MD5 trailer has a known-answer test
    Given the feature specification mandates "the rsync MD5 trailer has a known-answer test"
    When  the implementation satisfies: FileHasher hashes the empty input and abc at protocol 31
    Then  the observable contract holds: proved by crate::adapters::rsync::wire::hash::tests::file_hasher_known_answers

  Scenario: FR-006 the trace layer keeps the streamed lines and gains one fixed-length line
    Given the feature specification mandates "the trace layer keeps the streamed lines and gains one fixed-length line"
    When  the implementation satisfies: the binary answers GET / and an initialize POST under RUST_LOG=debug
    Then  the observable contract holds: proved by tests/v7_protocol_http.rs::trace_layer_emits_six_debug_lines_for_two_responses_and_none_under_info

  @security
  @planned
  Scenario: FR-007 no advisory is open and cargo audit stays clean
    Given the moved dependency set
    When  cargo audit reads Cargo.lock
    Then  no vulnerability is reported
    And   the only finding is the chrono unmaintained warning under RUSTSEC-2026-0543

  @planned
  Scenario: FR-008 the gate is green on the moved set
    Given the moved set and the current source tree
    When  the gate commands run
    Then  each exits 0 with at least 2122 lib tests and 2295 tests with the integration suites
    And   no lint is disabled, no allow attribute is added and no test is ignored or removed
    And   the only test edit is the formatting line at tests/v7_rsync_wire_e2e_vm.rs:500

  @planned
  Scenario: FR-009 the change is recorded and historical records stay as written
    Given the change landed
    When  a reader opens CHANGELOG.md, docs/MIGRATION.md or session.md
    Then  each records the move
    And   docs/adr/0012-inline-push-notifications.md:867 and docs/MIGRATION.md:1372 stay as written
    And   session.md lists the three re-anchoring cases under the conformity item

  @planned
  Scenario: FR-010 speckit validate adds no finding to the baseline
    Given the feature's artifacts on disk
    When  speckit validate runs over the corpus
    Then  the error count stays at 70 with the same rule set
    And   no warning fires

  @planned
  Scenario: FR-011 no other dependency and no new surface
    Given the moved set
    When  make tools-export-check runs and the initialize result is read
    Then  both catalogue files are unchanged
    And   no tool, argument, error code, environment variable or metric was added
