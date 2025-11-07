Feature: rstest TestCluster fixture
  Publishing the fixture keeps integration tests declarative whilst
  maintaining the RAII guarantees of TestCluster.

  Scenario: Injecting a ready cluster via the fixture
    Given an rstest fixture sandbox
    When the rstest fixture runs with the default environment
    Then the fixture starts a cluster bound to the sandbox paths
    And dropping the fixture restores the process environment

  Scenario: Propagating bootstrap errors through the fixture
    Given an rstest fixture sandbox
    When the rstest fixture runs without time zone data
    Then the fixture surfaces a time zone bootstrap error
