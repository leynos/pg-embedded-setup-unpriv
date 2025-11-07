Feature: rstest test_cluster fixture
  The built-in rstest fixture should let integration tests consume TestCluster
  without invoking TestCluster::new manually.

  Scenario: Injecting the fixture without extra setup
    Given the rstest fixture uses the default environment
    When test_cluster is invoked via rstest
    Then the fixture yields a running TestCluster

  Scenario: Surfacing bootstrap errors
    Given the rstest fixture runs without time zone data
    When test_cluster is invoked via rstest
    Then the fixture reports a missing timezone error

  Scenario: Surfacing missing worker binary errors
    Given the rstest fixture runs without a worker binary
    When test_cluster is invoked via rstest
    Then the fixture reports a missing worker binary error

  Scenario: Surfacing sandbox permission errors
    Given the rstest fixture cannot write to its sandbox
    When test_cluster is invoked via rstest
    Then the fixture reports a permission error

  Scenario: Surfacing invalid configuration errors
    Given the rstest fixture uses an invalid configuration
    When test_cluster is invoked via rstest
    Then the fixture reports an invalid configuration error
