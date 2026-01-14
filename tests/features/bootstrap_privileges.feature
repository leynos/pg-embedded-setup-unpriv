Feature: Runtime privilege detection
  Postgres bootstrapping adapts to the caller's privileges so CI jobs and
  local workflows can call the same entry point regardless of root access.

  Scenario: Bootstrapping as an unprivileged user
    Given a fresh bootstrap sandbox
    When the bootstrap runs as an unprivileged user
    Then the sandbox directories are owned by the target uid
    And the detected privileges were unprivileged

  Scenario: Bootstrapping twice as root
    Given a fresh bootstrap sandbox
    When the bootstrap runs twice as root
    Then the sandbox directories are owned by the target uid
    And the detected privileges were root

  Scenario: Root bootstrap requires a worker binary
    Given a fresh bootstrap sandbox
    When the bootstrap runs as root without a worker
    Then the bootstrap reports the missing worker
