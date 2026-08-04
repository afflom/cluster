Feature: reclaim

  Devcontainer retention, and the dirty exemption that is the point of it.
  Reclaiming resources from an abandoned container is housekeeping; deleting
  someone's uncommitted work because a timer expired is a betrayal, and a system
  that does it once is never trusted again (SPEC.md §15.3).

  @CG-01 @build
  Scenario: An idle session is notified at its threshold
    Given a session whose last attachment is below the notify threshold
    When reclamation decides
    Then it is left alone
    When the session has been idle past the notify threshold
    Then its owner is notified and it is marked idle
    And the same holds whether or not its workspace is dirty

  @CG-02 @build
  Scenario: Archiving happens at its threshold and is reversible
    Given a clean session idle past the archive threshold
    When reclamation decides
    Then it is archived
    And archiving is reversible from the snapshot and its devcontainer.json
    When a clean session reaches the purge threshold without having been archived
    Then it is archived first so the reversible step is never skipped
    And a session that is already purged is left alone
    When a dirty session reaches the archive threshold
    Then it is archived too, because archiving is reversible and only purging is not

  @CG-03 @build
  Scenario: A dirty session is never purged
    Given a session whose workspace is dirty
    When it has been idle past the purge threshold in any state
    Then it is held indefinitely for acknowledgement
    And no decision about it is irreversible
    When an otherwise identical session is clean
    Then it is purged, so the exemption is not passing vacuously

  @CG-04 @build
  Scenario: Reclamation does not run during a rollout
    Given a session old enough to purge and a workspace that is not dirty
    When no rollout is in progress
    Then it is purged
    When a rollout is in progress
    Then reclamation takes no action at all
    And a session a drain is migrating is left alone in either case

  @CG-05 @build
  Scenario: The flag a destructive decision sees is the one just observed
    Given a stored record whose dirty flag says clean
    When the workspace is observed to be dirty immediately before deciding
    Then the decision uses the observed flag and not the stored one
    And the stored record is left unchanged
    When the workspace cannot be observed at all
    Then it is treated as dirty

  @CG-06 @build
  Scenario: Dirty is three independent conditions, computed now
    Given a workspace with uncommitted changes, unpushed commits, or untracked files
    When its state is computed
    Then each condition is reported independently
    And the workspace is dirty if any of them holds
    When the workspace cannot be inspected at all
    Then it is reported dirty, because a wrong purge costs more than a held archive

  @CG-07 @build
  Scenario: An SSH session records an attachment
    Given a session a developer reaches over SSH
    When any SSH session opens against it, interactive or not
    Then an attachment is recorded against that session
    And the session therefore does not appear idle to the retention thresholds
    And this holds whether or not its workspace is dirty

  @CG-08 @build
  Scenario: An attached session is never archived on a stale timestamp
    Given a session whose container is running a tunnel and no editor server
    When attachment is observed
    Then it is not attached
    When an editor server process is running
    Then it is attached
    When the process table cannot be read at all
    Then it is read as attached, because the two errors are not the same size
    And this holds whether or not the workspace is dirty
