Feature: boot

  Properties of a single booted node: the health predicate that greenboot,
  the rollout ordering, and three test tiers all consume (SPEC.md §10.1).

  @CB-01 @build
  Scenario: The predicate holds exactly when every declared check holds
    Given a node observed to satisfy all eight checks in SPEC.md section 10.1
    When the predicate is evaluated
    Then it reports healthy
    And it reports one outcome per declared check
    When any single check is made to fail
    Then the predicate reports unhealthy
    And it names that check and no other
    When a declared probe cannot be executed at all
    Then the result is an unknown rather than a failed check

  @CB-02 @build
  Scenario: A booted node passes the health predicate
    Given a node booted from a candidate image
    When the health predicate is run on it
    Then it reports healthy
    And every declared check was evaluated rather than merely declared

  @CB-03 @build
  Scenario: SELinux is enforcing with no denials
    Given a booted node
    When its enforcement mode is read
    Then it is enforcing
    When the boot has settled and the audit log is read
    Then it holds no access-vector denial

  @CB-04 @build
  Scenario: The filesystem contract holds
    Given a booted node
    When a write to the system tree is attempted
    Then it is refused
    When a write to the state tree is attempted
    Then it succeeds

  @CB-05 @build
  Scenario: The declared runtime is present and answering
    Given a booted node and the runtime its variant declares
    When the runtime's socket unit is queried
    Then it is active
    When a version request is made over that socket
    Then the runtime answers it

  @CB-06 @build
  Scenario: The kernel reflects the declared isolation
    Given the node whose variant declares an isolated CPU set
    When the kernel's isolated set is read
    Then it is the set the model declares
    And simultaneous multithreading is off
    And the scaling governor is the declared one

  @CB-07 @build
  Scenario: A node works out its own ports, ordinal and addresses
    Given a machine booted from an image that names no node
    When cluster-init has run
    Then it sorted its ports into the classes their supported speeds imply
    And it took the ordinal its own hardware entitles it to
    And the addresses on its interfaces are the ones that ordinal derives
    And none of those facts was present in the image it booted

  @CB-08 @build
  Scenario: One role marker, and no unit failed for belonging to another role
    Given a machine that has discovered its role
    When the units are examined
    Then exactly one role marker exists under /run/cluster
    And every unit belonging to another role is inactive rather than failed
    And no unit belonging to this node's role was skipped
