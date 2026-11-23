Feature: Resources Subscribe Lane Removal
  As the maintainer of the subscription surface
  I want resources/subscribe to stop opening a lane
  So that a legacy subscription costs one entry and no lane, and the lifecycle balances

  # One scenario per FR of spec.md, named with the FR id so the link is
  # machine-checkable. Tags are optional and closed: `@serial` runs the
  # scenario alone, `@security` counts it into the advisory security tally,
  # and `@planned` marks a scenario whose proof does not exist yet. A step
  # outside the published vocabulary (`speckit manual --topic verify-steps`)
  # is reported unbound. A scenario cites a test only when the test proves its
  # Then; every proving test here is a `#[tokio::test]`, so every scenario
  # stays `@planned`.
  #
  # Decision provenance: design 2 was chosen by the orchestrator under the
  # owner's instruction to proceed and is recorded in ADR 0006 for the owner's
  # review.

  @planned
  Scenario: FR-001 resources/subscribe opens no lane
    Given a seeded shell and a peer
    When  resources/subscribe runs
    Then  no lane exists for the URI
    And   one flush records exactly one peer event

  @planned
  Scenario: FR-002 resources/unsubscribe removes the registry entry
    Given a subscribed peer
    When  resources/unsubscribe runs
    Then  the registry entry is gone
    And   a second call succeeds

  @planned
  Scenario: FR-002 resources/unsubscribe leaves a sub_open lane open
    Given a sub_open lane on a URI
    When  resources/unsubscribe runs for the same URI
    Then  the lane still exists

  @planned
  Scenario: FR-003 a repeated subscribe counts once in the lifecycle
    Given one peer and URI subscribed twice
    When  resources/unsubscribe runs once
    Then  the lifecycle refcount is 0
    And   the snapshot is empty

  @planned
  Scenario: FR-004 a failed subscribe leaves nothing behind
    Given a tracked shell already Closed
    When  resources/subscribe runs
    Then  it returns DomainError::ResourceGone (McpError::resource_not_found, message resource gone: …)
    And   the snapshot stays empty
    And   no notification arrives
    And   a refresh that replaced the record meanwhile is removed with it

  @planned
  Scenario: FR-005 the feature-005 residual ends
    Given a resources/subscribe subscription
    When  sub_list runs
    Then  no lane row is returned

  @planned
  Scenario: FR-006 the false documents state the truth
    Given the corrected documents
    When  a reader opens them
    Then  no passage gives the legacy method a lane or a sub_id
    And   the two historical passages stay as written

  @security
  @planned
  Scenario: FR-007 no wire change
    Given the repaired tree
    When  make tools-export-check runs
    Then  both catalogue files are unchanged
    And   resources/subscribe and resources/unsubscribe still answer the empty result

  @planned
  Scenario: FR-008 the gate is green on the repaired tree
    Given the repaired tree
    When  the gate commands run
    Then  each exits 0 with at least 2142 lib tests and 2316 tests with the integration suites
    And   no lint is disabled, no allow attribute is added and no test is ignored or removed

  @planned
  Scenario: FR-009 speckit validate adds no finding to the baseline
    Given the feature's artifacts on disk
    When  speckit validate runs over the corpus
    Then  the error count stays at 68 with the same rule set
    And   no warning fires

  @planned
  Scenario: FR-010 the two out-of-scope defects are recorded
    Given the change landed
    When  a reader opens session.md
    Then  the never-invoked peer GC use case and the unsubscribe_sync race each carry their file:line
    And   each names its reproduction test and says it is not in the tree

  @planned
  Scenario: FR-011 more than 16 subscribe/unsubscribe cycles on one URI succeed
    Given one URI and the default lane cap of 16
    When  17 subscribe/unsubscribe cycles run
    Then  every round succeeds
