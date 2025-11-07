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

  Scenario: Missing worker binary surfaces an error
    Given the rstest fixture runs without a worker binary
    When test_cluster is invoked via rstest
    Then the fixture reports a missing worker binary error

  Scenario: Non-executable worker binary surfaces an error
    Given the rstest fixture uses a non-executable worker binary
    When test_cluster is invoked via rstest
    Then the fixture reports a non-executable worker binary error

  Scenario: Read-only filesystem prevents bootstrap
    Given the rstest fixture encounters read-only filesystem permissions
    When test_cluster is invoked via rstest
    Then the fixture reports a read-only permission error
