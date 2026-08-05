Feature: control

  The control plane: session registry, rollout state, and the API the web
  interface speaks to. Authentication is the transport's; authorization is a
  model fact (SPEC.md §16).

  @CC-01 @build
  Scenario: Only a permitted login may drive the cluster
    Given a list of authorized logins from the model
    When a request carries a permitted login
    Then it is authorized
    When a request carries a login that is not on the list
    Then it is refused as not authorized
    When a request carries no identity at all
    Then it is refused as unauthenticated
    And an empty permit list refuses everyone rather than admitting everyone

  @CC-02 @build
  Scenario: Every reportable condition is one the model sanctions
    Given the conditions the control plane can report
    When each is turned into a response
    Then each carries a status that says what the caller should do
    And none of them is an internal server error
    And an identifier that names nothing is reported as not found

  @CC-03 @build
  Scenario: A rollback quarantines the digest that caused it
    Given a rollout state with no quarantined digests
    When a node reports that it rolled back from a digest
    Then that digest is quarantined
    And the record survives a restart of the control plane
    When the same node reports the same digest again
    Then the record is not duplicated
    And nodes on differing digests are reported as a split version

  @CC-04 @build
  Scenario: A connect response survives a migration
    Given a session recorded as hosted on a node
    When its connection details are requested
    Then the response names the node currently hosting it
    And the SSH alias it returns does not encode that node

  @CC-05 @build
  Scenario: An unreachable control plane is not an empty list
    Given a browser rendering the web interface
    When the control plane cannot be reached
    Then the page renders an explicit disconnected state
    And the message names both the tailnet and a rebooting node as possible causes
    And it says that running devcontainers and the SSH alias are unaffected
    When the control plane answers with no sessions
    Then that is rendered as an empty list rather than as a failure
    And a held dirty archive is labelled distinctly from an ordinary archive

  @CC-06 @build
  Scenario: The control plane is reachable where the operator is
    Given a control plane bound to the mesh loopback
    When the publish unit is rendered
    Then it serves that same bind address over TLS on the tailnet
    And it requires the control plane, so it cannot publish nothing
    And stopping it withdraws the publication

  @CC-07 @build
  Scenario: The token cache bounds revocation lag
    Given a validated bearer token and the cache interval the model declares
    When the same token is presented inside that interval
    Then the login is served from cache
    When it is presented at the interval
    Then the identity provider is asked again
    And the cache is keyed on the token, so two tokens for one person expire independently

  @CC-08 @build
  Scenario: A browser can learn how to authenticate
    Given a browser with no token
    When it asks the control plane for the device flow's parameters
    Then they are served without authentication, because every one of them is public
    And the authorization states are distinct, so no code is shown that is not held
    And cross-origin access names one exact origin rather than a wildcard

  @CC-09 @build
  Scenario: A cluster says which secrets it is waiting for, and hands none back
    Given a cluster that has been given none of its secrets
    When the control plane is asked about enrolment
    Then it reports each declared secret as not yet given
    And it returns no value for any of them
    And the page names what it is waiting for rather than reading as broken
    And an identifier the cluster does not declare is refused, naming what it does
    And a value carrying a newline is refused without quoting it

  @CC-10 @build
  Scenario: A session identifier is one every consumer of it can carry
    Given an identifier that becomes a directory, a URL segment, a container name and an alias
    When a session is created with it
    Then an identifier carrying a path separator or a traversal is refused
    And one carrying a space, an uppercase letter or a quote is refused
    And one longer than a hostname label is refused
    And the agent refuses it again at its own boundary rather than trusting the control plane
    And the refusal is a sanctioned condition rather than a rename or a panic
