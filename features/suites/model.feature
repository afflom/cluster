Feature: model

  The register is internally consistent, and the gates that say so can fail.
  These are the template's own claims, plus the one this repository adds about
  which tier may discharge what (SPEC.md §19.2).

  @CM-01 @build
  Scenario: The model is self-consistent and its documentation is generated
    Given the files under model/
    When the model is loaded and cross-checked
    Then every file carries the schema tag this build understands
    And no ID is registered twice and none is untagged
    And CONFORMANCE.md equals what the register renders

  @CM-02 @build
  Scenario: Every claim has a scenario and a test, in both directions
    Given the register, the feature suites, and the workspace test names
    When the meta-gate runs
    Then every registered ID has a scenario
    And every registered ID has a test whose name ends in it
    And every scenario names a registered ID
    And every test that names an ID names a registered one
    And no scenario leaves a step unbuilt
    When the test list is emptied
    Then the meta-gate fails, so it is not a gate that cannot

  @CM-03 @build
  Scenario: Every reproduced fact cites an authority that exists
    Given the ledger and the authorities
    When each some-true claim is checked
    Then it names an authority with a row
    And that authority carries a citation
    And it carries a checksum or a stated reason there is none

  @CM-04 @build
  Scenario: A hardware claim is never discharged by a guest
    Given the register and the four validation tiers
    When each tier's claims are collected
    Then every registered claim is collected by exactly one tier
    And no simulated tier collects a hardware claim
    And the QEMU socket pairs reproduce the declared triangle
    And an absent accelerator is reported as an explicit skip rather than emulated
