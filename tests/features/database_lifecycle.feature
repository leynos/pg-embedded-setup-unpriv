Feature: Database lifecycle management
  Scenario: Create and drop a database
    Given a sandboxed TestCluster is running
    When a new database is created
    Then the database exists in the cluster
    When the database is dropped
    Then the database no longer exists

  Scenario: Create duplicate database fails
    Given a sandboxed TestCluster is running
    When a new database is created
    And the same database is created again
    Then a duplicate database error is returned

  Scenario: Drop non-existent database fails
    Given a sandboxed TestCluster is running
    When a non-existent database is dropped
    Then a missing database error is returned

  Scenario: Delegation methods on TestCluster work correctly
    Given a sandboxed TestCluster is running
    When a database is created via TestCluster delegation
    Then the database exists via TestCluster delegation
    When the database is dropped via TestCluster delegation
    Then the database no longer exists via TestCluster delegation
