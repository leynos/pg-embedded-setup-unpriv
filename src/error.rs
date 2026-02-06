//! Domain error types for the embedded `PostgreSQL` bootstrapper.

use color_eyre::Report;
use thiserror::Error;

/// Result alias for operations that may return a [`PgEmbeddedError`].
pub type Result<T> = std::result::Result<T, PgEmbeddedError>;

/// Result alias for bootstrap-specific fallible operations.
pub type BootstrapResult<T> = std::result::Result<T, BootstrapError>;

/// Result alias for privilege-management fallible operations.
pub type PrivilegeResult<T> = std::result::Result<T, PrivilegeError>;

/// Result alias for configuration fallible operations.
pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

/// Top-level error exposed by the crate.
#[derive(Debug, Error)]
pub enum PgEmbeddedError {
    /// Indicates bootstrap initialisation failed.
    #[error("bootstrap failed: {0}")]
    Bootstrap(#[from] BootstrapError),
    /// Indicates privilege management failed.
    #[error("privilege management failed: {0}")]
    Privilege(#[from] PrivilegeError),
    /// Indicates configuration parsing failed.
    #[error("configuration parsing failed: {0}")]
    Config(#[from] ConfigError),
}

/// Categorises bootstrap failures so callers can branch on structured errors.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum BootstrapErrorKind {
    /// Represents errors without a more specific semantic meaning.
    #[default]
    Other,
    /// Indicates the configured worker binary is missing from disk.
    WorkerBinaryMissing,
    /// Indicates a PATH entry used for worker discovery is not valid UTF-8.
    WorkerBinaryPathNonUtf8,
}

/// Captures bootstrap-specific failures.
#[derive(Debug, Error)]
#[error("{report}")]
pub struct BootstrapError {
    kind: BootstrapErrorKind,
    #[source]
    report: Report,
}

impl BootstrapError {
    /// Constructs a new bootstrap error with the provided kind and diagnostic
    /// report.
    #[must_use]
    pub const fn new(kind: BootstrapErrorKind, report: Report) -> Self {
        Self { kind, report }
    }

    /// Returns the semantic category for this bootstrap failure.
    #[must_use]
    pub const fn kind(&self) -> BootstrapErrorKind {
        self.kind
    }

    /// Extracts the underlying diagnostic report.
    pub fn into_report(self) -> Report {
        self.report
    }
}

impl From<Report> for BootstrapError {
    fn from(report: Report) -> Self {
        Self::new(BootstrapErrorKind::Other, report)
    }
}

impl From<PrivilegeError> for BootstrapError {
    fn from(err: PrivilegeError) -> Self {
        let PrivilegeError(report) = err;
        Self::new(BootstrapErrorKind::Other, report)
    }
}

impl From<ConfigError> for BootstrapError {
    fn from(err: ConfigError) -> Self {
        let ConfigError(report) = err;
        Self::new(BootstrapErrorKind::Other, report)
    }
}

impl From<PgEmbeddedError> for BootstrapError {
    fn from(err: PgEmbeddedError) -> Self {
        match err {
            PgEmbeddedError::Bootstrap(inner) => inner,
            PgEmbeddedError::Privilege(inner) => inner.into(),
            PgEmbeddedError::Config(inner) => inner.into(),
        }
    }
}

/// Captures privilege-management failures.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct PrivilegeError(#[from] Report);

/// Captures configuration failures.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct ConfigError(#[from] Report);

#[cfg(test)]
mod tests {
    //! Unit tests for error display formats.

    use super::*;
    use color_eyre::eyre::eyre;
    use rstest::rstest;

    #[rstest]
    #[case::bootstrap(
        "PG_EMBEDDED_WORKER must be set",
        "bootstrap failed:",
        |msg: &str| PgEmbeddedError::Bootstrap(BootstrapError::from(eyre!("{}", msg)))
    )]
    #[case::privilege(
        "failed to drop privileges",
        "privilege management failed:",
        |msg: &str| PgEmbeddedError::Privilege(PrivilegeError::from(eyre!("{}", msg)))
    )]
    #[case::config(
        "invalid port number",
        "configuration parsing failed:",
        |msg: &str| PgEmbeddedError::Config(ConfigError::from(eyre!("{}", msg)))
    )]
    fn pg_embedded_error_includes_inner_message(
        #[case] inner_message: &str,
        #[case] expected_prefix: &str,
        #[case] constructor: fn(&str) -> PgEmbeddedError,
    ) {
        let pg_err = constructor(inner_message);

        let display = pg_err.to_string();

        assert!(
            display.contains(expected_prefix),
            "expected '{expected_prefix}' prefix, got: {display}"
        );
        assert!(
            display.contains(inner_message),
            "expected inner message '{inner_message}' in display, got: {display}"
        );
    }

    #[test]
    fn bootstrap_error_displays_report_message() {
        let inner_message = "database connection failed";
        let err = BootstrapError::from(eyre!(inner_message));

        let display = err.to_string();

        assert!(
            display.contains(inner_message),
            "expected '{inner_message}' in display, got: {display}"
        );
    }
}
