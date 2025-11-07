Feature: TestCluster connection helpers
  Scenario: Metadata surfaces sandbox configuration
    Given a sandboxed TestCluster is running
    When connection metadata is requested
    Then the metadata matches the sandbox layout

  Scenario: Diesel helper executes SQL statements
    Given a sandboxed TestCluster is running
    When a Diesel client executes a simple SELECT
    Then the Diesel helper returns the selected value

  Scenario: Diesel helper reports descriptive errors
    Given a sandboxed TestCluster is running
    When a Diesel client executes a malformed query
    Then the Diesel helper reports the malformed query error
