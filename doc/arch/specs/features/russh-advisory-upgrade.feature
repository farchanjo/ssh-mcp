Feature: Russh Advisory Upgrade
  As the maintainer of the SSH transport
  I want the SSH client on a release without published advisories
  So that a flaw in the transport crate is not reachable from a remote host

  # One scenario per FR of spec.md, named with the FR id so the link is
  # machine-checkable. Tags are optional and closed: `@serial` runs the
  # scenario alone, `@security` counts it into the advisory security tally,
  # and the planned tag marks a scenario whose steps do not bind yet, because
  # the test that proves it belongs to the implement phase. A step outside the
  # published vocabulary (`speckit manual --topic verify-steps`) is reported
  # unbound.

  @planned
  Scenario: FR-001 The manifest and the lock resolve russh 0.65.4 and russh-sftp 4.0.0
    Given Cargo.toml declaring the russh 0.60 line and the russh-sftp 2 line
    When  one cargo update moves the two crates
    Then  Cargo.lock resolves russh 0.65.4 or a later 0.65.x and russh-sftp 4.x
    And   the requirement of every other direct dependency is unchanged

  @planned
  Scenario: FR-002 The resolved lock carries no advisory on the SSH transport
    Given the moved dependency set
    When  cargo audit reads Cargo.lock
    Then  no advisory names russh, rsa or crypto-bigint
    And   the only vulnerability reported is RUSTSEC-2026-0462 on rmcp 1.8.0

  @planned
  Scenario: FR-003 The crate compiles with the handler on the new trait signature
    Given the moved russh release resolved
    When  cargo check --all-features --all-targets runs
    Then  the build is clean with check_server_key taking the new argument type

  @security
  Scenario: FR-004 The client offers no host-certificate algorithm
    Given the feature specification mandates "the client offers no host-certificate algorithm"
    When  the implementation satisfies: the built client configuration sets the offered host-certificate list empty
    Then  the observable contract holds: proved by crate::adapters::ssh::internal::client::tests::client_config::test_no_host_certificate_algorithm_is_offered

  @security
  @planned
  Scenario: FR-004 A presented certificate is rejected and not recorded
    Given the feature specification mandates "a presented certificate is rejected and not recorded"
    When  the implementation satisfies: the handler refuses the certificate variant, logs the host and the port, and writes no known_hosts record
    Then  the certificate is rejected and no record is written

  @security
  @planned
  Scenario: FR-005 The plain host-key decision is unchanged
    Given the feature specification mandates "the plain host-key decision keeps its three outcomes"
    When  the implementation satisfies: the handler passes a plain host key to verify_host_key with no new branch
    Then  the three existing host-key tests pass unchanged

  Scenario: FR-006 The channel-open retry classification reads the typed error, and a resource shortage is transient
    Given the feature specification mandates "the retry classification reads the typed error and a resource shortage is transient"
    When  the implementation satisfies: the classifier matches the three exhaustion reasons on russh::Error and every other value fails through
    Then  the observable contract holds: proved by crate::adapters::ssh::internal::client::tests::transient_open_errors::exhaustion_reasons_are_transient
    And   the observable contract holds: proved by crate::adapters::ssh::internal::client::tests::transient_open_errors::other_reasons_and_errors_are_not_transient

  @planned
  Scenario: FR-007 The project gate is green on the new library
    Given the moved library and the current source tree
    When  the four gate commands run
    Then  each exits 0 with no lint disabled and no test ignored or removed

  @planned
  Scenario: FR-008 The wire behaviour of the new library passes the Python integration suites
    Given the debug-profile binaries ssh-mcp and ssh-mcp-stdio and an SSH endpoint
    When  the HTTP, transfer-progress and rsync HTTP suites run under pytest
    Then  each suite exits 0

  @planned
  Scenario: FR-009 The published MCP surface does not change
    Given an MCP host of the current release
    When  it completes initialize and calls the tools
    Then  the catalogue, the arguments, the response lines and the error codes are unchanged
    And   every environment variable keeps its name and its default

  @planned
  Scenario: FR-010 The change is recorded
    Given the feature landed
    When  a reader opens CHANGELOG.md, docs/MIGRATION.md and entry TM-021
    Then  each names russh 0.65.x, russh-sftp 4.x and what stays open

  @planned
  Scenario: FR-011 The adapter calls no session-level write, the one SFTP contract that changed
    Given the SFTP adapter and the moved russh-sftp release
    When  every call it makes into the SFTP client is read
    Then  no call to the session-level write exists
    And   its session-level metadata, create, open, open_with_flags, read_dir, symlink_metadata, read_link, create_dir, remove_dir, remove_file, symlink, set_metadata and close calls are the ones it makes
    And   its file-level write_all, read, seek and shutdown calls on an opened file stay
    And   the library defaults apply with no override
