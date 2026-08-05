Feature: definition

  The model renders the declared infrastructure artifacts, and the committed
  tree is what the model says it is. R1 applied to `.network` files, firewall
  rules, Quadlets, kernel arguments, timer units, kickstarts and `ssh_config`
  rather than only to documentation (SPEC.md §7.2).

  @CD-01 @build
  Scenario: Ports are classified by link speed, and no MAC is rendered
    Given a model declaring an interface class per link speed
    When the tree is rendered
    Then no rendered artifact carries a MAC address
    And the rendered policy carries the speed threshold for each class
    And the mesh threshold is above the LAN one
    And the LAN class is addressed by DHCP

  @CD-02 @build
  Scenario: The routing policy is rendered rather than compiled in
    Given a model declaring route metrics and addressing bases
    When the tree is rendered
    Then the rendered policy carries both route metrics
    And it carries the loopback and link bases the addresses derive from
    And the direct metric is below the transit metric
    And forwarding is enabled, so a transit route has something to transit

  @CD-03 @build
  Scenario: The firewall drops by default and accepts only declared flows
    Given a firewall declaring a drop policy on input
    When the packet filter is rendered
    Then the input chain's policy is drop
    And every declared rule appears as an accept
    And the forward chain accepts only when both addresses are mesh addresses

  @CD-04 @build
  Scenario: Every name resolves from the ordinals, with no resolver
    Given a fleet of ordinals and a cluster domain
    When the hosts file is rendered
    Then every ordinal resolves at its fully-qualified and short name
    And the bare cluster name resolves to the ordinal holding storage
    And no management name appears, because those addresses come from DHCP
    And no name appears that the ordinals do not derive

  @CD-05 @build
  Scenario: Every Quadlet volume mount carries its relabel flag
    Given a variant declaring Quadlets with volume mounts
    When the Quadlet units are rendered
    Then every Volume line ends in the relabel flag its model row declares

  @CD-06 @build
  Scenario: Isolation is a role's kernel argument and never the image's
    Given one role declaring an isolated CPU set
    When the kernel arguments are rendered
    Then the base set carries no isolation argument
    And the isolating role's own set names the CPUs it declares
    And exactly one role declares isolation

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
    And the updater environment carries every ordinal's health endpoint
    And it carries no position, which the image cannot know
    And every declared drain budget appears with its class and its exceed action

  @CD-09 @build
  Scenario: The committed tree equals the render and is fully asserted about
    Given a committed generated tree
    When the model is rendered in memory
    Then the committed bytes equal the rendered bytes for every file
    And no file exists under the tree that the model does not render
    And every file whose format admits a comment names a registered claim in its header

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
    Then every JSON document parses and carries only the keys its schema defines
    And every interpreted script carries its interpreter on the first line
    And every other file carries the generated provenance as a comment

  @CD-17 @build
  Scenario: A node configures itself from a rendered policy, not from constants
    Given class thresholds, addressing bases, metrics and a role table in the model
    When the node policy is rendered
    Then it carries every value cluster-init needs to configure a machine
    And the binary that reads it declares none of them itself

  @CD-18 @build
  Scenario: Each role's firewall include is rendered, empty ones included
    Given a firewall rule restricted to one role
    When the packet filter is rendered
    Then the common ruleset includes exactly one role file
    And a file is rendered for every role, including those adding no rules
    And the restricted rule appears only in its own role's file

  @CD-19 @build
  Scenario: Each role's kernel arguments are rendered, empty ones included
    Given one role declaring an isolated CPU set and two declaring none
    When the kernel arguments are rendered
    Then a set is rendered for every role
    And the isolating role's set carries its arguments
    And the sets for the other roles are empty rather than absent

  @CD-20 @build
  Scenario: The enrolled secrets are declared by destination, never by value
    Given secrets the operator enters after the cluster boots
    When the enrolment policy is rendered
    Then every declared secret appears with its destination and mode
    And no destination is writable beyond its owner
    And no model file or rendered artifact carries a value
