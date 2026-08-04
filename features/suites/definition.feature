Feature: definition

  The model renders the declared infrastructure artifacts, and the committed
  tree is what the model says it is. R1 applied to `.network` files, firewall
  rules, Quadlets, kernel arguments, timer units, kickstarts and `ssh_config`
  rather than only to documentation (SPEC.md §7.2).

  @CD-01 @build
  Scenario: Interfaces are matched by MAC, never by kernel name
    Given a model declaring four MAC addresses per node
    When the network units are rendered
    Then every unit for a physical interface matches on MACAddress
    And every declared MAC appears in exactly one unit
    And no unit for a physical interface matches on Name

  @CD-02 @build
  Scenario: Each node holds a direct and a transit route to every peer
    Given a direct triangle with one link per pair of nodes
    When the network units are rendered
    Then each node has a route to every peer loopback at the direct metric
    And each node has a route to every peer loopback at the transit metric
    And the transit route's gateway is the remaining peer's link address

  @CD-03 @build
  Scenario: The firewall drops by default and accepts only declared flows
    Given a firewall declaring a drop policy on input
    When the packet filter is rendered
    Then the input chain's policy is drop
    And every declared rule appears as an accept
    And the forward chain accepts only when both addresses are mesh addresses

  @CD-04 @build
  Scenario: Names resolve from the node table with no resolver
    Given nodes with declared loopbacks and management addresses
    When the hosts file is rendered
    Then every node's mesh name resolves to its loopback
    And every node's management name resolves to its management address
    And no name appears that the model does not declare

  @CD-05 @build
  Scenario: Every Quadlet volume mount carries its relabel flag
    Given a variant declaring Quadlets with volume mounts
    When the Quadlet units are rendered
    Then every Volume line ends in the relabel flag its model row declares

  @CD-06 @build
  Scenario: Isolation kernel arguments render on the measurement node alone
    Given one variant declaring an isolated CPU set
    When the kernel arguments are rendered
    Then every node carries the base kernel arguments
    And only the measurement node carries the isolation set
    And its isolcpus argument names the CPUs the variant declares

  @CD-07 @build
  Scenario: The kickstart carries the declared layout and no secret
    Given a partition table and three secret placeholders
    When the kickstart is rendered
    Then every declared partition appears with its declared filesystem
    And each secret appears only as its named placeholder
    And no line carries a value that looks like a key or a token

  @CD-08 @build
  Scenario: Unattended behaviour is carried by rendered units
    Given a policy declaring poll intervals, deadlines and drain budgets
    When the systemd units are rendered
    Then the updater timer carries the declared interval and jitter
    And the greenboot check carries the declared deadline
    And the updater environment carries this node's position and its peers' endpoints
    And every declared drain budget appears with its class and its exceed action

  @CD-09 @build
  Scenario: The committed tree equals the render and is fully asserted about
    Given a committed generated tree
    When the model is rendered in memory
    Then the committed bytes equal the rendered bytes for every file
    And no file exists under the tree that the model does not render
    And every file names a registered definition claim in its header

  @CD-10 @build
  Scenario: A devcontainer alias survives a migration
    Given a control plane address and a devcontainer alias pattern
    When the client SSH configuration is rendered
    Then the alias resolves the session's current host from the control plane
    And it falls back to the last known host when the control plane is unreachable

  @CD-11 @build
  Scenario: Trust and pull order are rendered from the model
    Given a signing identity and a registry order in the model
    When the container configuration is rendered
    Then the signature policy defaults to reject
    And it admits only the declared issuer and certificate identity
    And the registry configuration lists the local mirror before its fallbacks
    And every address in the rendered tree is substituted from the node table

  @CD-12 @build
  Scenario: Nothing dangles and nothing rendered is inert
    Given the rendered tree, the image builds, and the control plane's routes
    When the joins between them are read
    Then every rendered artifact is copied into an image by some build
    And every executable a rendered unit invokes is produced by a build or a declared package
    And every image the model names in this repository's namespace has a Containerfile
    And every configuration a unit mounts read-only is rendered
    And every control-plane endpoint any component calls is a route it serves

  @CD-13 @build
  Scenario: Every declared alert renders as a rule
    Given the alerts the model declares
    When the alert rules are rendered
    Then each appears with its condition, its duration and its severity
    And no declared alert renders to nothing

  @CD-14 @build
  Scenario: Host policy is rendered, not declared twice
    Given the SSH, SELinux and greenboot settings the model declares
    When the host configuration is rendered
    Then the SSH daemon policy carries the declared values
    And the SELinux mode and type carry the declared values
    And greenboot carries both the declared deadline and the declared attempt count
    And no image build declares any of them a second time

  @CD-15 @build
  Scenario: The control plane is published and the tailnet policy is rendered
    Given the tailnet and the authorized logins in the model
    When the tailnet artifacts are rendered
    Then a unit publishes the control plane over TLS on the tailnet
    And the access policy admits only the authorized logins
    And it advertises the management prefix and no mesh address

  @CD-16 @build
  Scenario: Every rendered artifact is valid in its own syntax
    Given rendered artifacts in several syntaxes
    When each is read as the kind of file it is
    Then every JSON document parses
    And every interpreted script carries its interpreter on the first line
    And the generated provenance is present in a form that syntax admits
