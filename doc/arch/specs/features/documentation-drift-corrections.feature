Feature: Documentation Drift Corrections
  As the maintainer of the published documentation
  I want the six drifted statements corrected to the code they describe
  So that an operator or a host neither runs a needless step nor branches on a code that never arrives

  # One scenario per FR of spec.md, named with the FR id so the link is
  # machine-checkable. Tags are optional and closed: `@serial` runs the
  # scenario alone, `@security` counts it into the advisory security tally,
  # and `@planned` marks a scenario whose proof does not exist yet. This
  # feature edits prose, so no scenario cites a test and all stay `@planned`.
  # A step outside the published vocabulary (`speckit manual --topic
  # verify-steps`) is reported unbound.
  #
  # Decision provenance: every decision here was taken in this session on the
  # owner's instruction to carry the whole update plan forward, and it is
  # recorded for the owner's review.

  @planned
  Scenario: FR-001 the upgrade guide says a stale lock re-resolves by itself
    Given docs/MIGRATION.md line 882
    When  the stale-lock sentence is read
    Then  it says no manual step is needed and names no cargo update command
    And   grep -c 'needs no manual step' returns 2

  @planned
  Scenario: FR-002 INTERNAL_ERROR is reachable on resources/*
    Given docs/OPERATIONS.md line 409
    When  the INTERNAL_ERROR bullet is read
    Then  it names the per-URI and server-wide lane caps and the message they produce
    And   grep -c 'currently never raised' and grep -c '17th' return 0

  @planned
  Scenario: FR-003 the cap codes are the ones the code emits
    Given docs/CONFIGURATION.md lines 149 and 150
    When  the two cap rows are read
    Then  they name MAX_SUBS_PER_URI_EXCEEDED and MAX_SUBS_TOTAL_EXCEEDED
    And   grep -c 'LANE_LIMIT_PER_URI' and grep -c 'LANE_LIMIT_TOTAL' return 0

  @planned
  Scenario: FR-004 the filter compile failure is INVALID_ARGUMENT
    Given the three wrong FILTER_INVALID occurrences
    When  they are read after the change
    Then  grep -rn 'FILTER_INVALID' docs doc/arch/threat-model prints only docs/CONFIGURATION.md:153
    And   the two API lines and the threat-model line name INVALID_ARGUMENT

  @planned
  Scenario: FR-005 the sub_open error list names only emitted codes
    Given docs/API.md line 1106
    When  the sub_open error list is read
    Then  it names RESOURCE_GONE, MAX_SUBS_PER_URI_EXCEEDED, MAX_SUBS_TOTAL_EXCEEDED and INVALID_ARGUMENT
    And   it names no LANE_LIMIT code, FILTER_INVALID or INLINE_PUSH_BAD_PARAMS

  @planned
  Scenario: FR-006 the daemon subscribe op reports SUBSCRIBE_FAILED
    Given docs/DAEMON.md line 147
    When  the subscribe error list is read
    Then  it names SUBSCRIBE_FAILED as the event code
    And   the JSON-RPC causes are described as detail content

  @planned
  Scenario: FR-007 nothing else changes
    Given the feature diff
    When  git diff --name-only and make tools-export-check run
    Then  only the seven named documents and the feature artifacts appear
    And   both catalogue files are unchanged

  @planned
  Scenario: FR-008 speckit validate adds no finding to the baseline
    Given the feature's artifacts on disk
    When  speckit validate runs over the corpus
    Then  the error count stays at 70 with the same rule set
    And   no warning fires

  @planned
  Scenario: FR-009 the defects and the open item are recorded in session.md
    Given session.md after the change
    When  the two sections are read
    Then  the three defects sit under the defect heading with their file:line
    And   the catalogue open item sits under the owner heading

  @planned
  Scenario: FR-010 the decision is recorded with its provenance
    Given the specification, the decision record and this feature file
    When  the provenance sentence is searched
    Then  each carries it once
