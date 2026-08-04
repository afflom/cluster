Feature: network

  Mesh behaviour and failover, on booted nodes. The rendered routes and rules
  are asserted by the definition suite; this is whether they do what they were
  rendered to do (SPEC.md §4).

  @CN-01 @build
  Scenario: Every peer answers at the full mesh payload size
    Given three booted nodes on the declared mesh
    When each node probes every peer loopback with the do-not-fragment flag
    Then every probe of the full declared payload size is answered
    And a path that will not carry jumbo frames fails rather than answering small

  @CN-02 @build
  Scenario: A failed link fails over to the transit route
    Given three booted nodes joined in a triangle
    When the route to a peer is resolved before any link is cut
    Then it resolves over the direct link
    When that link loses carrier
    Then the peer loopback is still reachable at the full payload size
    And the path taken has changed to the route through the remaining peer

  @CN-03 @build
  Scenario: The loaded filter accepts only declared flows
    Given the packet filter loaded on each booted node
    When its ruleset is read
    Then the input chain's policy is drop
    And every flow the model declares for that node is present

  @CN-04 @build
  Scenario: Both ends of a cable agree without being told
    Given three nodes wired as a direct triangle
    When each node discovers the peer on each of its mesh ports
    Then each port is addressed from the ordinal pair the cable joins
    And the two ends of every cable hold the two addresses of one /31
    And neither end was told which of the two to take

  @CN-05 @build
  Scenario: Ordinals are handed out in arrival order and reused after release
    Given a registrar and two machines holding no bulk disk
    When each registers in turn
    Then the first is assigned compute and the second testbed
    And a machine that registers twice receives the same assignment
    And a released ordinal is handed to the next machine rather than a fresh one
