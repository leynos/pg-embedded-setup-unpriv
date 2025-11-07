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
