//! Domain error types for the embedded PostgreSQL bootstrapper.

use color_eyre::Report;
use thiserror::Error;

/// Result alias for operations that may return a [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Result alias for bootstrap-specific fallible operations.
pub type BootstrapResult<T> = std::result::Result<T, BootstrapError>;

/// Result alias for privilege-management fallible operations.
pub type PrivilegeResult<T> = std::result::Result<T, PrivilegeError>;

/// Result alias for configuration fallible operations.
pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

/// Top-level error exposed by the crate.
#[derive(Debug, Error)]
pub enum Error {
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

/// Captures bootstrap-specific failures.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct BootstrapError(#[from] Report);

impl From<PrivilegeError> for BootstrapError {
    fn from(err: PrivilegeError) -> Self {
        let PrivilegeError(report) = err;
        Self(report)
    }
}

impl From<ConfigError> for BootstrapError {
    fn from(err: ConfigError) -> Self {
        let ConfigError(report) = err;
        Self(report)
    }
}

impl From<Error> for BootstrapError {
    fn from(err: Error) -> Self {
        match err {
            Error::Bootstrap(inner) => inner,
            Error::Privilege(inner) => inner.into(),
            Error::Config(inner) => inner.into(),
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
