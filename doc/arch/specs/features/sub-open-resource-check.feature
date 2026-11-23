Feature: Sub Open Resource Check
  As the maintainer of the subscription surface
  I want sub_open to refuse a missing repository-backed resource
  So that a typo opens no lane and both subscribe surfaces answer alike

  # One scenario per FR of spec.md, named with the FR id so the link is
  # machine-checkable. Tags are optional and closed: `@serial` runs the
  # scenario alone, `@security` counts it into the advisory security tally,
  # and `@planned` marks a scenario whose proof does not exist yet. A step
  # outside the published vocabulary (`speckit manual --topic verify-steps`)
  # is reported unbound. A scenario cites a test only when the test proves its
  # Then; every proving test here is a `#[tokio::test]` or a `#[test]`, so
  # every scenario stays `@planned`.
  #
  # Decision provenance: the per-kind answer code was chosen by the
  # orchestrator under the owner's instruction to proceed and is recorded in
  # ADR 0007 for the owner's review.

  @planned
  Scenario: FR-001 a missing repository-backed resource is refused
    Given a shell, command, transfer and session id no repository holds
    When  sub_open runs for each kind
    Then  it returns ShellNotFound, CommandNotFound, TransferNotFound and SessionNotFound
    And   each variant carries the parsed id

  @planned
  Scenario: FR-002 a refusal costs nothing
    Given a missing shell and a recording debouncer activator
    When  sub_open runs
    Then  it returns an error
    And   sub_list is empty
    And   the activator recorded no URI
    And   the per-URI cap is still fully available

  @planned
  Scenario: FR-003 forward, serial and rsync stay permissive
    Given a forward, a serial and an rsync URI with no producer
    When  sub_open runs for each
    Then  it answers OK with a SubId

  @planned
  Scenario: FR-004 the kind policy exists once
    Given the repaired tree
    When  the shared policy is searched
    Then  the kind table has one implementation in src
    And   no adapter imports crate::application
    And   the five resources/subscribe ensure tests pass

  @planned
  Scenario: FR-005 the tool answer carries the per-kind code
    Given a missing shell
    When  the sub_open helper runs the guarded use case and renders the refusal
    Then  the body carries SUB_OPEN: ERROR and REASON: [SHELL_NOT_FOUND] with the shell cure
    And   the structured twin carries code SHELL_NOT_FOUND
    And   the frozen description still lists RESOURCE_NOT_FOUND

  @planned
  Scenario: FR-006 the false documents state the truth
    Given the corrected documents
    When  a reader opens them
    Then  a missing id is documented as the per-kind code
    And   the three permissive kinds keep their reasons
    And   the two historical passages stay as written

  @planned
  Scenario: FR-007 session.md records the out-of-scope defects
    Given the change landed
    When  a reader opens session.md
    Then  every out-of-scope defect of the measurement carries its file:line
    And   every patch test says it is not in the tree
    And   the trace layer line of item 6 is the line `grep -n 'TraceLayer::new_for_http' src/composition/prod.rs` prints

  @security
  @planned
  Scenario: FR-008 no other wire change
    Given the repaired tree
    When  make tools-export-check runs
    Then  both catalogue files are unchanged

  @planned
  Scenario: FR-009 the gate is green on the repaired tree
    Given the repaired tree
    When  the gate commands run
    Then  each exits 0 with at least 2154 lib tests and 2329 tests with the integration suites
    And   the adapted adversarial and flush-bytes scripts report 63 passed and 1 passed
    And   no lint is disabled, no allow attribute is added and no test is ignored or removed

  @planned
  Scenario: FR-010 speckit validate adds no finding to the baseline
    Given the feature's artifacts on disk
    When  speckit validate runs over the corpus
    Then  the error count stays at 68 with the same rule set
    And   no warning fires

  @planned
  Scenario: FR-012 a terminal repository row is accepted
    Given a command and a transfer row in a terminal status
    When  sub_open runs for each
    Then  both open

  @planned
  Scenario: FR-013 a refusal is not cached under its idempotency key
    Given a refused sub_open and its _meta.idempotency_key
    When  the same key is retried after the resource exists
    Then  the retry re-runs the guard and opens

  @planned
  Scenario: FR-011 the production composition installs the guard
    Given the use case container built by build_use_cases
    When  UseCases::sub_subscribe runs for a missing shell, command, transfer and session and for a forward URI
    Then  the four are refused with their per-kind variant
    And   the forward opens with a SubId
