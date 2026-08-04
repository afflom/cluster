Feature: workload

  Devcontainers and runners: the things the cluster exists to run
  (SPEC.md §9.5, §11).

  @CW-01 @build
  Scenario: A devcontainer starts and can be entered
    Given a workspace with a devcontainer definition on the compute node
    When the devcontainer is brought up against the declared runtime
    Then it starts
    And a command executes inside the running container

  @CW-02 @build
  Scenario: Runners are ephemeral and re-register
    Given the runners the model declares for each node
    When each node's runner units are read
    Then every runner is declared ephemeral
    And its unit is active
    And its unit restarts it after each job, so a drain can stop it by not restarting it

  @CW-03 @build
  Scenario: A migration preserves the durable state, in order
    Given a devcontainer to be moved to the migration target
    When the migration is planned
    Then the six declared steps run in the order the specification gives them
    And everything up to the workspace copy leaves the container running
    And everything after the container stops is past the point of no return
    And the container is recreated from the same image digest
    And it is recreated against the copied workspace, not the one being drained

  @CW-04 @build
  Scenario: A session's URL carries no host
    Given a session identifier and the URL template the model declares
    When the session URL is derived
    Then it contains the identifier the SSH alias uses
    And it contains no node name
    And no migration can therefore change it

  @CW-05 @build
  Scenario: The tunnel Feature installs what will actually run
    Given the published tunnel Feature
    When its install script is read
    Then it installs the command-line client into the image layer
    And it does not bake the server payload, which is pinned by commit upstream
    And it namespaces the authentication directory by the container user's identifier
    And it installs a supervisor whose backoff bounds come from the model
    And it ships an unregister step for the archive path
