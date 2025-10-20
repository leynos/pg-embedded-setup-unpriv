Feature: TestCluster lifecycle
  The RAII guard starts PostgreSQL automatically and stops it when dropped,
  keeping callers free from manual orchestration.

  Scenario: Managing a cluster lifecycle
    Given a cluster sandbox for tests
    When a TestCluster is created
    Then the cluster reports sandbox-aligned settings
    And the environment remains applied whilst the cluster runs
    And the cluster stops automatically on drop

  Scenario: Failing without a time zone database
    Given a cluster sandbox for tests
    When a TestCluster is created without a time zone database
    Then the cluster creation reports a time zone error
