//! Error conversion helpers that adapt `color-eyre` reports into the public
//! [`crate::Error`] type for test scaffolding.

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
use color_eyre::eyre::Report;

#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
use crate::Error;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
use crate::error::{BootstrapError, PrivilegeError};

/// Converts a bootstrap error report into the library's public [`Error`] type.
/// This helper exists for test scaffolding and should not be used in published
/// APIs.
///
/// # Examples
/// ```
/// # use color_eyre::Report;
/// # use pg_embedded_setup_unpriv::Error;
/// use pg_embedded_setup_unpriv::test_support::bootstrap_error;
///
/// let err = bootstrap_error(Report::msg("bootstrap failed"));
/// assert!(matches!(err, Error::Bootstrap(_)));
/// ```
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
#[must_use]
pub fn bootstrap_error(err: Report) -> Error {
    Error::Bootstrap(BootstrapError::from(err))
}

/// Converts a privilege-related report into the library's public [`Error`] type.
/// This helper exists for test scaffolding and should not be used in published
/// APIs.
///
/// # Examples
/// ```
/// # use color_eyre::Report;
/// # use pg_embedded_setup_unpriv::Error;
/// use pg_embedded_setup_unpriv::test_support::privilege_error;
///
/// let err = privilege_error(Report::msg("missing capability"));
/// assert!(matches!(err, Error::Privilege(_)));
/// ```
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
#[must_use]
pub fn privilege_error(err: Report) -> Error {
    Error::Privilege(PrivilegeError::from(err))
}
