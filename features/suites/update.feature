Feature: update

  Unattended rolling update: ordering without a lock, draining without force,
  and rollback without an operator (SPEC.md §13, §14).

  @CU-01 @build
  Scenario: At most one node updates at a time, in the declared order
    Given three nodes with the update positions the model declares
    When every assignment of booted digests across the fleet is enumerated
    Then no state admits more than one node
    And at least one state admits a node
    When the admitted node is advanced to the target repeatedly
    Then the nodes update in the order the model declares
    And the rollout terminates with every node on the target
    When a peer reports that it is updating
    Then this node waits rather than halting

  @CU-02 @build
  Scenario: A quarantined digest is never applied
    Given a node otherwise admitted to apply a target
    When that target is recorded as quarantined
    Then the predicate halts
    And the reason names the quarantine

  @CU-03 @build
  Scenario: An unreadable peer halts rather than being assumed
    Given a node otherwise admitted to apply a target
    When a peer's health cannot be read
    Then the predicate halts and names that peer as unknown
    When the peer instead reports itself unhealthy
    Then the predicate halts and names it as unhealthy
    And a halt is a decision rather than an error

  @CU-04 @build
  Scenario: An exceeded budget halts and never kills the work
    Given a drain budget whose declared action on exceeding is halt
    When the elapsed time is within the budget
    Then the outcome is within budget
    When the elapsed time exceeds the budget
    Then the rollout halts
    And a bench job in flight is still waited for rather than terminated

  @CU-05 @build
  Scenario: The migration cap is enforced rather than exceeded
    Given devcontainers whose declared memory exceeds the model's cap
    When the drain is planned
    Then the migrated memory does not exceed the cap
    And the most recently attached sessions are the ones that move
    And the excess is stopped with notice so the session survives
    And planning the same input twice chooses the same workloads

  @CU-06 @build
  Scenario: Nothing migrates to a reserved node
    Given a migration target the model declares as never receiving work
    When the drain is planned
    Then nothing is migrated
    And every devcontainer is stopped with notice instead

  @CU-07 @build
  Scenario: Exactly one node updates at a time across the booted fleet
    Given three booted nodes and a candidate digest
    When each node evaluates the predicate from what its peers actually report
    Then exactly one node is admitted
    And no node halts
    When the admitted node applies the update and returns
    Then it reports healthy before another node is admitted
    And the fleet updates in the order the model declares

  @CU-08 @build
  Scenario: A failed boot rolls back and quarantines
    Given a booted node and a change that will fail the health predicate
    When the node reboots into it
    Then the previous deployment is restored automatically
    And the node reports the digest it booted before
    And the digest that failed is recorded as quarantined

  @CU-09 @build
  Scenario: A drain preserves the worktree it moves
    Given a devcontainer worktree on the compute node
    When that node is drained
    Then the worktree is present on the migration target and unchanged
    And nothing has reached a node the model declares as never receiving work

  @CU-10 @build
  Scenario: The rollout survives one version boundary
    Given the fleet on the previously promoted release
    When the first node in the sequence is moved to the candidate
    Then every node still reports healthy
    And every node can read and parse every peer's health report
    And the simulation ran from the previous release rather than from the candidate to itself
