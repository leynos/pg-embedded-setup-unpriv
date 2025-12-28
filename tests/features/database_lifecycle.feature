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

  Scenario: Temporary database is cleaned up on drop
    Given a sandboxed TestCluster is running
    When a temporary database is created
    Then the temporary database exists
    When the temporary database guard is dropped
    Then the temporary database no longer exists

  Scenario: Ensure template exists creates template only once
    Given a sandboxed TestCluster is running
    When ensure_template_exists is called with a setup function
    Then the template database exists
    And the setup function was called exactly once
    When ensure_template_exists is called again for the same template
    Then the setup function was still called exactly once

  Scenario: Create database from template clones template
    Given a sandboxed TestCluster is running
    When a template database is created and populated
    And a database is created from the template
    Then the cloned database exists
