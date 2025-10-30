use color_eyre::eyre::Report;

use crate::Error;
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
#[must_use]
pub fn privilege_error(err: Report) -> Error {
    Error::Privilege(PrivilegeError::from(err))
}
