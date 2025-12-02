Feature: Settings logging
  Debug logs surface prepared PostgreSQL settings without leaking secrets.

  Scenario: Logging prepared settings during a successful bootstrap
    Given a settings logging sandbox
    When bootstrap prepares settings with debug logging enabled
    Then the debug logs include sanitized settings

  Scenario: Logging prepared settings when preparation fails
    Given a settings logging sandbox
    When bootstrap fails while preparing settings with debug logging enabled
    Then the debug logs still redact sensitive values
