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
    #[error("bootstrap failed")]
    Bootstrap(#[from] BootstrapError),
    /// Indicates privilege management failed.
    #[error("privilege management failed")]
    Privilege(#[from] PrivilegeError),
    /// Indicates configuration parsing failed.
    #[error("configuration parsing failed")]
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
