Feature: Debouncer Consumer Lifetime
  As the maintainer of the subscription surface
  I want the per-URI debouncer to follow both of its consumer kinds
  So that a lane is never orphaned by a peer and a closed lane releases its task

  # One scenario per FR of spec.md, named with the FR id so the link is
  # machine-checkable. Tags are optional and closed: `@serial` runs the
  # scenario alone, `@security` counts it into the advisory security tally,
  # and `@planned` marks a scenario whose proof does not exist yet. A step
  # outside the published vocabulary (`speckit manual --topic verify-steps`)
  # is reported unbound. Every proving test is a `#[tokio::test]` or a
  # `#[test]`, so every scenario stays `@planned`.
  #
  # Decision provenance: the split and the boundary were decided by the
  # orchestrator under the owner's instruction and are registered for the
  # owner's review; the mechanism is the plan's decision (ADR 0008, Option A;
  # plan.md D-1).

  @planned
  Scenario: FR-001 a peer-list emptiness alone does not stop the debouncer
    Given a peer subscription and a lane on one URI
    When  the peer unsubscribes
    Then  the lane is still listed and keeps its debouncer
    And   a threshold write plus a poke deliver one notification within the bound

  @planned
  Scenario: FR-002 the last lane close with no peer stops the debouncer
    Given a lane on a URI with no peer subscription
    When  the lane is closed
    Then  the debouncer task is stopped
    And   the four per-resource map entries the stop owns are removed

  @planned
  Scenario: FR-003 the last peer unsubscribe with no lane still stops the debouncer
    Given a peer subscription on a URI with no lane
    When  the peer unsubscribes
    Then  the debouncer task is stopped
    And   the task count is zero

  @planned
  Scenario: FR-004 the three open/close races keep one debouncer per live consumer
    Given a sub_open, a resources/subscribe and a peer unsubscribe each racing a close of the last lane
    When  the race resolves
    Then  exactly one debouncer remains for the live consumer
    And   concurrent ensures spawn exactly one task
    And   one write per round reaches the peer of the lane that remains

  @planned
  Scenario: FR-005 the registry is released after the last consumer leaves
    Given the last consumer of a registry is gone and every external handle is dropped
    When  the close path has run
    Then  no task keeps the registry alive
    And   the adapter and the bridge are still released after drop

  @planned
  Scenario: FR-006 the production composition installs the link
    Given the use case container built by build_use_cases
    When  the last lane of a URI closes with no peer
    Then  the debouncer observed through that composition is stopped
    And   the proof fails without the link

  @security
  @planned
  Scenario: FR-007 no wire change
    Given the repaired tree
    When  make tools-export-check runs
    Then  both catalogue files are unchanged

  @planned
  Scenario: FR-008 the gate is green on the repaired tree
    Given the repaired tree
    When  the gate commands run
    Then  each exits 0 with at least 2174 lib tests and 2349 tests with the integration suites
    And   the adversarial and flush-bytes scripts report 63 passed and 1 passed
    And   no lint is disabled, no allow attribute is added and no test is ignored or removed

  @planned
  Scenario: FR-009 speckit validate has no feature finding
    Given the feature's artifacts on disk
    When  speckit validate runs over the corpus
    Then  the error count is 63 with the reduced rule set
    And   no warning fires
    And   no error or warning names a file of this feature or session.md

  @planned
  Scenario: FR-010 session.md records the measured defects
    Given the change landed
    When  a reader opens session.md
    Then  every defect of the measurement carries its file:line
    And   every patch test says it is not in the tree
    And   no existing entry is duplicated

  @planned
  Scenario: FR-012 a registry without a presence query keeps the peer-list semantics
    Given a registry without a presence query
    When  ensure_for_uri runs
    Then  the task survives
    And   a peer unsubscribe stops it

  @planned
  Scenario: FR-013 the two non-debouncer per-resource maps survive a stop
    Given eight open and release cycles
    When  the tasks are stopped
    Then  the four debouncer maps are empty
    And   the sequence and tail maps keep eight entries

  @planned
  Scenario: FR-014 bytes written while no task exists are delivered on the next wakeup
    Given bytes written below and above the threshold with no task
    When  a consumer opens and pokes
    Then  exactly one notification arrives within the bound
    And   the forward tail is readable

  @planned
  Scenario: FR-011 the stop decision exists once and the code rules hold
    Given the stop decision in the source
    When  it is searched
    Then  it exists once with a single stop implementation
    And   no new Mutex, super import or else-after-return is added
    And   no comment names a feature, a requirement id or a work item
