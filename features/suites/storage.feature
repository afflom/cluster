Feature: storage

  The registry, the object store, the network filesystem, and the cache that
  makes a spinning disk usable (SPEC.md §5).

  @CS-01 @build
  Scenario: A node pulls its image across the mesh
    Given a registry on the storage node and two nodes that are not it
    When each of those nodes pulls its own image from the registry
    Then the pull succeeds over the mesh

  @CS-02 @build
  Scenario: The network filesystem is exported to one address
    Given the storage node's export table
    When it is read
    Then it names the loopback of the one node that mounts it
    And it names no wildcard
    And it names no other node's loopback

  @CS-03 @build
  Scenario: The data volume is writethrough
    Given the cached data volume on the storage node
    When its device-mapper status is read
    Then the cache mode is writethrough
    And losing the cache device is therefore not losing origin data

  @CS-04 @build
  Scenario: The registry mirrors and caches what the model declares
    Given the registry configuration in the model
    When it is rendered
    Then it binds the storage node's mesh loopback
    And it mirrors this repository's namespace from the upstream on the declared interval
    And it pull-through caches each declared fallback on demand
