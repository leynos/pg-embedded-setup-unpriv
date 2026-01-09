//! Scoped environment guard that temporarily overrides variables for test
//! cases, restoring the original process state when dropped.

use std::ffi::OsString;

use crate::env::ScopedEnv;

/// Applies environment overrides for tests using the library's shared guard.
///
/// # Examples
/// ```rust,no_run
/// use std::ffi::OsString;
///
/// use pg_embedded_setup_unpriv::test_support;
///
/// let guard = test_support::scoped_env(vec![
///     (OsString::from("PGUSER"), Some(OsString::from("postgres"))),
/// ]);
/// drop(guard);
/// ```
#[doc(hidden)]
pub fn scoped_env<I>(vars: I) -> ScopedEnv
where
    I: IntoIterator<Item = (OsString, Option<OsString>)>,
{
    ScopedEnv::apply_os(vars)
}
