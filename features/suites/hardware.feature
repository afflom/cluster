Feature: hardware

  What only real nodes can establish. A simulated run cannot confirm that
  virtualization is enabled in firmware, that a MAC belongs to the card the
  model names, or that a device is physically mounted where the chassis was
  supposed to take it (SPEC.md §2.4, §21.2).

  @CH-01 @build
  Scenario: Every declared firmware setting holds
    Given the firmware settings the model declares and the probe named for each
    When each node is asked through that probe
    Then the setting is observed to hold
    And the reason the model gives for it is reported alongside any failure

  @CH-02 @build
  Scenario: Every declared interface is present in its role
    Given the interface addresses the model declares for each node
    When each node's interfaces are read
    Then every declared address is carried by an interface
    And that interface carries the address the model gives its role
    And a swapped cable or a replaced mainboard therefore fails here

  @CH-03 @build
  Scenario: The declared storage devices are present
    Given the devices and partitions the model declares
    When each node's block devices are read
    Then the node has exactly the declared number of devices
    And each declared device kind is present with its rotational nature
    And every declared partition is mounted

  @CH-04 @build
  Scenario: The real fleet is healthy on the promoted image
    Given the fleet after a promotion
    When each node is asked for its health report
    Then every node reports healthy
    And every node reports the same booted digest

  @CH-05 @build
  Scenario: Every node is the hardware its profile declares
    Given the hardware profile the model declares and references
    When each node is asked about itself
    Then its core count is the declared one
    And its vector extensions are as declared
    And its maximum clock is the declared one, so it has no boost algorithm
    And its installed memory and slot count are the declared ones
    And it has at least as many interfaces as the declared ports

  @CH-06 @build
  Scenario: The management controller answers out of band
    Given each node's controller address from the model
    When it is queried without going through the node
    Then it answers
    And it reports the declared restore-on-power-loss behaviour
    And only the storage node carries a power-on delay
