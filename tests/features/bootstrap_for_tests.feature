Feature: bootstrap_for_tests helper
  The helper prepares PostgreSQL settings and environment defaults so tests
  can initialise clusters without manual configuration.

  Scenario: Bootstrapping without time zone overrides
    Given a bootstrap sandbox for tests
    When bootstrap_for_tests runs without time zone overrides
    Then the helper returns sandbox-aligned settings
    And the helper prepares default environment variables

  Scenario: Failing when the time zone database is missing
    Given a bootstrap sandbox for tests
    When bootstrap_for_tests runs with a missing time zone database
    Then the helper reports a time zone error
