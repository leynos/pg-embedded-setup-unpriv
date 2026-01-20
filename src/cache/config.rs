//! Configuration for the shared binary cache.
//!
//! Resolves the cache directory from environment variables with XDG-compliant
//! fallback paths.

use camino::Utf8PathBuf;
use std::path::PathBuf;

/// Subdirectory path within the XDG cache home.
const CACHE_SUBDIR: &str = "pg-embedded/binaries";

/// Configuration for the shared binary cache.
#[derive(Debug, Clone)]
pub struct BinaryCacheConfig {
    /// Root directory for cached `PostgreSQL` binaries.
    pub cache_dir: Utf8PathBuf,
}

impl BinaryCacheConfig {
    /// Creates a new cache configuration using the resolved cache directory.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache_dir: resolve_cache_dir(),
        }
    }

    /// Creates a cache configuration with a custom directory.
    #[must_use]
    pub const fn with_dir(cache_dir: Utf8PathBuf) -> Self {
        Self { cache_dir }
    }
}

impl Default for BinaryCacheConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolves the binary cache directory from environment and XDG conventions.
///
/// The resolution order is:
///
/// 1. `PG_BINARY_CACHE_DIR` environment variable if set and valid UTF-8
/// 2. `$XDG_CACHE_HOME/pg-embedded/binaries` if `XDG_CACHE_HOME` is set
/// 3. `~/.cache/pg-embedded/binaries` as fallback
/// 4. `/tmp/pg-embedded/binaries` as last resort
///
/// # Examples
///
/// ```
/// use pg_embedded_setup_unpriv::cache::resolve_cache_dir;
///
/// let cache_dir = resolve_cache_dir();
/// // Returns a path like ~/.cache/pg-embedded/binaries
/// assert!(cache_dir.as_str().contains("pg-embedded"));
/// ```
#[must_use]
pub fn resolve_cache_dir() -> Utf8PathBuf {
    // Check explicit environment variable first
    if let Some(dir) = resolve_from_env() {
        return dir;
    }

    // Try XDG cache home
    if let Some(dir) = resolve_from_xdg_cache() {
        return dir;
    }

    // Fall back to home directory
    if let Some(dir) = resolve_from_home() {
        return dir;
    }

    // Last resort: temp directory
    Utf8PathBuf::from("/tmp/pg-embedded/binaries")
}

/// Attempts to resolve cache directory from `PG_BINARY_CACHE_DIR` environment variable.
fn resolve_from_env() -> Option<Utf8PathBuf> {
    let raw = std::env::var("PG_BINARY_CACHE_DIR").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Utf8PathBuf::from_path_buf(PathBuf::from(trimmed)).ok()
}

/// Attempts to resolve cache directory from `XDG_CACHE_HOME`.
fn resolve_from_xdg_cache() -> Option<Utf8PathBuf> {
    let raw = std::env::var("XDG_CACHE_HOME").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = Utf8PathBuf::from_path_buf(PathBuf::from(trimmed)).ok()?;
    Some(path.join(CACHE_SUBDIR))
}

/// Attempts to resolve cache directory from home directory.
fn resolve_from_home() -> Option<Utf8PathBuf> {
    let home = dirs::home_dir()?;
    let path = Utf8PathBuf::from_path_buf(home).ok()?;
    Some(path.join(".cache").join(CACHE_SUBDIR))
}

#[cfg(test)]
mod tests {
    use super::*;
    use temp_env::with_vars;

    #[test]
    fn resolve_cache_dir_respects_explicit_env_var() {
        let expected = "/custom/cache/path";
        let result = with_vars(
            [
                ("PG_BINARY_CACHE_DIR", Some(expected)),
                ("XDG_CACHE_HOME", None::<&str>),
            ],
            resolve_cache_dir,
        );
        assert_eq!(result.as_str(), expected);
    }

    #[test]
    fn resolve_cache_dir_uses_xdg_cache_home_when_env_var_unset() {
        let xdg_cache = "/home/testuser/.cache";
        let result = with_vars(
            [
                ("PG_BINARY_CACHE_DIR", None::<&str>),
                ("XDG_CACHE_HOME", Some(xdg_cache)),
            ],
            resolve_cache_dir,
        );
        assert_eq!(
            result.as_str(),
            format!("{xdg_cache}/{CACHE_SUBDIR}").as_str()
        );
    }

    #[test]
    fn resolve_cache_dir_ignores_empty_env_var() {
        let xdg_cache = "/home/testuser/.cache";
        let result = with_vars(
            [
                ("PG_BINARY_CACHE_DIR", Some("")),
                ("XDG_CACHE_HOME", Some(xdg_cache)),
            ],
            resolve_cache_dir,
        );
        assert_eq!(
            result.as_str(),
            format!("{xdg_cache}/{CACHE_SUBDIR}").as_str()
        );
    }

    #[test]
    fn resolve_cache_dir_ignores_whitespace_only_env_var() {
        let xdg_cache = "/home/testuser/.cache";
        let result = with_vars(
            [
                ("PG_BINARY_CACHE_DIR", Some("   ")),
                ("XDG_CACHE_HOME", Some(xdg_cache)),
            ],
            resolve_cache_dir,
        );
        assert_eq!(
            result.as_str(),
            format!("{xdg_cache}/{CACHE_SUBDIR}").as_str()
        );
    }

    #[test]
    fn binary_cache_config_default_uses_resolved_dir() {
        let config = BinaryCacheConfig::default();
        assert!(config.cache_dir.as_str().contains("pg-embedded"));
    }

    #[test]
    fn binary_cache_config_with_dir_uses_provided_path() {
        let custom_path = Utf8PathBuf::from("/custom/path");
        let config = BinaryCacheConfig::with_dir(custom_path.clone());
        assert_eq!(config.cache_dir, custom_path);
    }
}
