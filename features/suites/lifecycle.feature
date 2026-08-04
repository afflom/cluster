Feature: lifecycle

  Signature policy, promotion, and provenance: what a node will stage, and how
  the thing it stages came to be published (SPEC.md §9, §12.3).

  @CL-01 @build
  Scenario: The signature policy binds this repository's promote workflow
    Given a signing identity in the model
    When the container signature policy is rendered
    Then it rejects by default
    And it admits only a sigstore signature from the declared issuer
    And the certificate identity it admits names the promote workflow
    And an image signed by another workflow in the same repository is therefore not admitted

  @CL-02 @build
  Scenario: Every pulled prefix is mirrored locally
    Given the registry order the model declares
    When the registry configuration is rendered
    Then every prefix a node pulls carries a mirror on the local registry
    And every declared fallback is present
    And a pull therefore continues over the wide-area network when the local registry is down

  @CL-03 @build
  Scenario: The promoted digest is the validated one
    Given the build and promotion workflows
    When they are read
    Then the build captures each image digest as an output
    And promotion resolves its tag to a commit and copies that digest
    And promotion never builds an image
    And signing happens before the copy to the stable tag
    And promotions are serialised against each other

  @CL-04 @build
  Scenario: An image the policy does not admit fails to stage
    Given a booted node and an image in this repository's namespace that the policy does not admit
    When the node is asked to stage it
    Then staging fails
    And the node is still running the image it booted

  @CL-05 @build
  Scenario: The release publishes the installer and its checksum
    Given the promotion workflow
    When it is read
    Then it builds an installer from the digest it promoted
    And it builds it with this repository's bootstrap configuration
    And it publishes the installer and its SHA-256 as release artifacts
    And that checksum is the anchor a first install has, the policy shipping inside the image

  @CL-06 @build
  Scenario: Nothing is promoted on a tier that did not run
    Given the promotion workflow
    When it is read
    Then it resolves its tag to a commit and finds that commit's build run
    And it refuses a commit with no build run at all
    And it refuses a tier whose outcome is failure
    And it refuses a tier that was skipped, rather than reading absence as consent

  @CL-07 @build
  Scenario: A fork's code never runs on a node
    Given the workflows that schedule jobs on self-hosted runners
    When each is read
    Then every such job refuses a pull request from another repository
    And the guard is present whether or not the fleet is currently registered
