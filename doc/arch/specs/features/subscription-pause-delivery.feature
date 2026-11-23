Feature: Subscription Pause Delivery
  As the maintainer of the subscription lane surface
  I want sub_pause to gate delivery on the lane it names
  So that a paused lane stops receiving and a resume loses no byte

  # One scenario per FR of spec.md, named with the FR id so the link is
  # machine-checkable. Tags are optional and closed: `@serial` runs the
  # scenario alone, `@security` counts it into the advisory security tally,
  # and `@planned` marks a scenario whose proof does not exist yet. A step
  # outside the published vocabulary (`speckit manual --topic verify-steps`)
  # is reported unbound. A scenario cites a test only when the test proves its
  # Then; a `#[tokio::test]` cannot be cited, so the delivery scenarios stay
  # `@planned` and only the synchronous render tests bind.
  #
  # Decision provenance: every decision here was taken in this session on the
  # owner's instruction to carry the whole update plan forward, and it is
  # recorded for the owner's review.

  @planned
  Scenario: FR-001 a pause stops both delivery methods for that lane
    Given a lane with inline push on and its peer
    When  the lane is paused and a write reaches its URI
    Then  no resource event and no inline event are recorded for it
    And   the write after resume records one of each

  @planned
  Scenario: FR-002 a resume emits exactly one catch-up notification
    Given a lane paused with no production
    When  sub_resume runs
    Then  exactly one notifications/resources/updated is recorded for that lane
    And   a resume of a running lane records none

  @planned
  Scenario: FR-003 inline seq and cursor stay absolute across the pause
    Given an inline lane that produced 5 bytes before the pause and 5 during it
    When  4 more bytes are produced after resume
    Then  the shipped fragments carry seq 0 / cursor_after 5 and seq 2 / cursor_after 14

  @planned
  Scenario: FR-004 another lane and another peer keep delivering
    Given two lanes from two peers on one URI
    When  one is paused and the URI is fanned out
    Then  only the unpaused peer is recorded

  @planned
  Scenario: FR-005 the edge cases keep today's outcome
    Given an unknown sub_id, a lane paused twice and a lane closed while paused
    When  the pause and resume tools run
    Then  the unknown and closed cases carry SUB_NOT_FOUND
    And   the repeated pause and the running resume carry OK

  Scenario: FR-006 sub_list renders the paused flag
    Given the feature specification mandates "sub_list renders the paused flag"
    When  the implementation satisfies: a paused summary and a running summary are rendered
    Then  the observable contract holds: proved by crate::infra::mcp::render::subscription::tests::list_render_emits_one_line_per_summary

  @security
  Scenario: FR-007 no wire change to the pause and resume responses
    Given the feature specification mandates "no wire change to the pause and resume responses"
    When  the implementation satisfies: both toggle responses render their state label and structured twin
    Then  the observable contract holds: proved by crate::infra::mcp::render::subscription::tests::pause_resume_renders_distinct_state_labels

  @planned
  Scenario: FR-008 the gate is green on the repaired tree
    Given the repaired tree
    When  the gate commands run
    Then  each exits 0 with at least 2141 lib tests and 2315 tests with the integration suites
    And   no lint is disabled, no allow attribute is added and no test is ignored or removed
    And   both catalogue files are unchanged

  @planned
  Scenario: FR-009 the change is recorded and the five sentences are corrected
    Given the change landed
    When  a reader opens CHANGELOG.md, docs/MIGRATION.md or session.md
    Then  each carries its record
    And   the six listed sentences read as required
    And   docs/API.md names sub_open as the gated path and tells an inline host to read the gap
    And   session.md carries the residual defect entry and the open-item extension
    And   grep finds "drain loop" in docs/API.md and docs/OPERATIONS.md nothing
    And   docs/adr/0004-channel-mux-fairness.md:136-137 and docs/MIGRATION.md:631-632 stay as written

  @planned
  Scenario: FR-010 speckit validate adds no finding to the baseline
    Given the feature's artifacts on disk
    When  speckit validate runs over the corpus
    Then  the error count stays at 68 with the same rule set
    And   no warning fires

  @planned
  Scenario: FR-011 the resources/subscribe lane is a pinned residual
    Given a peer subscribed through resources/subscribe and a peerless lane on the same URI
    When  the lane is paused and a byte-threshold flush fires
    Then  the peer event arrives and the lane counters stay at zero
