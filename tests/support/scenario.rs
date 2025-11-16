//! Helpers for `rstest-bdd` scenarios that need to unwrap fixtures in tests.

use color_eyre::eyre::Result;

/// Turns a `Result<T>` fixture into `T`, panicking with a contextual message on
/// failure so individual scenario functions stay terse.
///
/// # Examples
///
/// ```rust,ignore
/// let world = Ok(42);
/// let value = expect_fixture(world, "demo fixture");
/// assert_eq!(value, 42);
/// ```
pub fn expect_fixture<T>(fixture: Result<T>, label: &str) -> T {
    fixture.unwrap_or_else(|err| panic!("{label} fixture failed: {err:?}"))
}
