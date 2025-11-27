Feature: Observability instrumentation
  Tracing spans and logs surface lifecycle and setup events.

  Scenario: Capturing observability logs during a successful bootstrap
    Given an observability sandbox
    When a cluster boots successfully with observability enabled
    Then logs include lifecycle, directory, and environment events

  Scenario: Capturing observability logs when directory preparation fails
    Given an observability sandbox
    When cluster bootstrap fails due to an invalid runtime path
    Then logs capture the directory failure context
