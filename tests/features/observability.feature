Feature: Observability instrumentation
  Scenario: Cluster emits observability logs
    Given a fresh observability sandbox
    And observability log capture is installed
    When a TestCluster boots successfully
    Then lifecycle, environment, and filesystem events are logged

  Scenario: Lifecycle failures emit logs
    Given a fresh observability sandbox
    And observability log capture is installed
    When a lifecycle operation fails
    Then the failure is reported in the logs
