Feature: image

  Properties of the built container, before anything boots it. A Containerfile
  has to agree with the model about the base digest, the runtime, the packages
  and whose rendered tree it carries, and every one of those agreements can be
  read rather than booted (SPEC.md §8, §10.2).

  @CI-01 @build
  Scenario: The upstream base is pinned, never floated
    Given the Containerfiles and the base digest the model declares
    When each FROM instruction is read
    Then exactly one names the upstream base
    And it names it by the digest the model declares
    And no instruction names the upstream base by a floating tag
    And a variant layering on this build's own base is not a floating reference

  @CI-02 @build
  Scenario: Each variant installs the runtime it declares and not the other
    Given a variant declaring one of the two legal container runtimes
    When its Containerfile is read
    Then every package of the declared runtime is installed
    And no package distinctive of the other runtime is installed
    And the Docker host variable points at the declared runtime's socket
    And every package the model declares for the variant is installed

  @CI-03 @build
  Scenario: A variant carries only its own rendered tree
    Given the rendered tree for each node
    When each variant's Containerfile is read
    Then it copies in the tree of the node it is built for
    And it copies in no other node's tree
    And the base takes its tree from a build argument, being built once per node

  @CI-04 @build
  Scenario: Every image is linted as a bootc host
    Given the Containerfiles
    When each is read
    Then each runs the bootc container lint before it is finished

  @CI-05 @build
  Scenario: The pin records the platform and something moves it
    Given the base pin in the model
    When it is read
    Then it records the index digest and a distinct manifest digest for the declared architecture
    And it records the date it was resolved
    And the build targets that platform explicitly
    When the workflows are read
    Then a scheduled workflow moves the pin
    And it measures staleness from the recorded date
    And it passes the full gate before proposing the change
    And it opens a pull request rather than committing

  @CI-06 @build
  Scenario: An upstream binary is pinned and verified
    Given a binary the model declares as fetched from upstream
    When the variant's build is read
    Then it fetches the declared version rather than a floating one
    And it checks the declared digest rather than merely recording it
    And it unpacks the declared compression
