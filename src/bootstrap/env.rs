//! Parses environment variables used by the bootstrapper and surfaces the
//! resulting configuration for the filesystem preparers.

use std::env::{self, VarError};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::Report;

use crate::error::{BootstrapError, BootstrapErrorKind, BootstrapResult};
use crate::fs::ambient_dir_and_path;

#[cfg(unix)]
use cap_std::fs::PermissionsExt;

pub(super) const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_SHUTDOWN_TIMEOUT_SECS: u64 = 600;
const SHUTDOWN_TIMEOUT_ENV: &str = "PG_SHUTDOWN_TIMEOUT_SECS";

/// Common Unix paths where time zone databases may be installed.
///
/// Used by `find_timezone_dir` to probe for the first existing candidate.
#[cfg(unix)]
pub const TZDIR_CANDIDATES: [&str; 4] = [
    "/usr/share/zoneinfo",
    "/usr/lib/zoneinfo",
    "/etc/zoneinfo",
    "/share/zoneinfo",
];

/// Probes common Unix paths for the time zone database directory.
///
/// Returns the first existing candidate from `TZDIR_CANDIDATES`, or `None` if
/// none exist or on non-Unix platforms. This helper enables test harnesses to
/// set `TZDIR` consistently with production bootstrap logic.
///
/// # Examples
///
/// ```
/// use pg_embedded_setup_unpriv::find_timezone_dir;
///
/// if let Some(tzdir) = find_timezone_dir() {
///     println!("Found timezone directory: {}", tzdir);
/// }
/// ```
#[must_use]
pub fn find_timezone_dir() -> Option<&'static Utf8Path> {
    #[cfg(unix)]
    {
        TZDIR_CANDIDATES
            .iter()
            .copied()
            .map(Utf8Path::new)
            .find(|path| path.exists())
    }

    #[cfg(not(unix))]
    {
        None
    }
}

pub(super) fn shutdown_timeout_from_env() -> BootstrapResult<Duration> {
    match env::var(SHUTDOWN_TIMEOUT_ENV) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                    "{SHUTDOWN_TIMEOUT_ENV} is present but empty"
                )));
            }

            let seconds: u64 = trimmed.parse().map_err(|err| {
                BootstrapError::from(color_eyre::eyre::eyre!(
                    "failed to parse {SHUTDOWN_TIMEOUT_ENV} from '{trimmed}': {err}"
                ))
            })?;

            if seconds == 0 {
                return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                    "{SHUTDOWN_TIMEOUT_ENV} must be at least 1 second (received {trimmed})"
                )));
            }

            if seconds > MAX_SHUTDOWN_TIMEOUT_SECS {
                return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                    "{SHUTDOWN_TIMEOUT_ENV} must be {MAX_SHUTDOWN_TIMEOUT_SECS} seconds or less (received {trimmed})"
                )));
            }

            Ok(Duration::from_secs(seconds))
        }
        Err(VarError::NotPresent) => Ok(DEFAULT_SHUTDOWN_TIMEOUT),
        Err(VarError::NotUnicode(value)) => Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "{SHUTDOWN_TIMEOUT_ENV} must contain a valid UTF-8 value (received {:?})",
            value
        ))),
    }
}

pub(super) fn worker_binary_from_env() -> BootstrapResult<Option<Utf8PathBuf>> {
    // First: check explicit PG_EMBEDDED_WORKER environment variable
    if let Some(raw) = env::var_os("PG_EMBEDDED_WORKER") {
        let path = parse_worker_path_from_env(&raw)?;
        validate_worker_binary(&path)?;
        return Ok(Some(path));
    }

    // Second: search PATH for pg_worker binary
    if let Some(path) = discover_worker_from_path() {
        validate_worker_binary(&path)?;
        return Ok(Some(path));
    }

    Ok(None)
}

fn parse_worker_path_from_env(raw: &std::ffi::OsStr) -> BootstrapResult<Utf8PathBuf> {
    let path = Utf8PathBuf::from_path_buf(PathBuf::from(raw)).map_err(|_| {
        let invalid_value = raw.to_string_lossy().to_string();
        BootstrapError::from(color_eyre::eyre::eyre!(
            "PG_EMBEDDED_WORKER contains a non-UTF-8 value: {invalid_value:?}. \
             Provide a UTF-8 encoded absolute path to the worker binary."
        ))
    })?;

    if path.as_str().is_empty() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PG_EMBEDDED_WORKER must not be empty"
        )));
    }
    if path.as_str() == "/" {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PG_EMBEDDED_WORKER must not point at the filesystem root"
        )));
    }

    Ok(path)
}

/// Searches the `PATH` environment variable for the `pg_worker` binary.
///
/// Returns the first valid UTF-8 path to an existing `pg_worker` file, or
/// `None` if no candidate is found. This enables zero-configuration usage when
/// the worker is installed via `cargo install`. Non-UTF-8 PATH segments and
/// candidates are skipped gracefully.
fn discover_worker_from_path() -> Option<Utf8PathBuf> {
    let path_var = env::var_os("PATH")?;
    env::split_paths(&path_var)
        .map(|dir| {
            let candidate = dir.join("pg_worker");
            #[cfg(windows)]
            let candidate = candidate.with_extension("exe");
            candidate
        })
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| Utf8PathBuf::from_path_buf(candidate).ok())
}

fn validate_worker_binary(path: &Utf8PathBuf) -> BootstrapResult<()> {
    let (dir, relative) =
        ambient_dir_and_path(path).map_err(|err| worker_binary_error(path, err))?;
    let metadata = dir
        .metadata(relative.as_std_path())
        .map_err(|err| worker_binary_error(path, Report::new(err)))?;

    if !metadata.is_file() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PG_EMBEDDED_WORKER must reference a regular file: {path}"
        )));
    }

    #[cfg(unix)]
    {
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                "PG_EMBEDDED_WORKER must be executable: {path}"
            )));
        }
    }

    Ok(())
}

fn worker_binary_error(path: &Utf8Path, err: Report) -> BootstrapError {
    let is_not_found = error_chain_has_not_found(&err);
    let context = format!("failed to access PG_EMBEDDED_WORKER at {path}: {err}");
    let report = err.wrap_err(context);

    if is_not_found {
        BootstrapError::new(BootstrapErrorKind::WorkerBinaryMissing, report)
    } else {
        BootstrapError::from(report)
    }
}

fn error_chain_has_not_found(err: &Report) -> bool {
    err.chain()
        .filter_map(|source| source.downcast_ref::<std::io::Error>())
        .any(|source| source.kind() == ErrorKind::NotFound)
}

#[derive(Debug, Clone)]
pub(super) struct TimezoneEnv {
    pub(super) dir: Option<Utf8PathBuf>,
    pub(super) zone: String,
}

/// Holds filesystem and time zone settings used by the bootstrap tests.
///
/// # Examples
/// ```
/// use camino::Utf8PathBuf;
/// use pg_embedded_setup_unpriv::TestBootstrapEnvironment;
///
/// let environment = TestBootstrapEnvironment {
///     home: Utf8PathBuf::from("/tmp/home"),
///     xdg_cache_home: Utf8PathBuf::from("/tmp/home/cache"),
///     xdg_runtime_dir: Utf8PathBuf::from("/tmp/home/run"),
///     pgpass_file: Utf8PathBuf::from("/tmp/home/.pgpass"),
///     tz_dir: None,
///     timezone: "UTC".into(),
/// };
/// assert_eq!(environment.to_env().len(), 6);
/// ```
#[derive(Debug, Clone)]
pub struct TestBootstrapEnvironment {
    /// Effective home directory for the `PostgreSQL` user during the tests.
    pub home: Utf8PathBuf,
    /// Directory used for cached `PostgreSQL` artefacts.
    pub xdg_cache_home: Utf8PathBuf,
    /// Directory used for `PostgreSQL` runtime state, such as sockets.
    pub xdg_runtime_dir: Utf8PathBuf,
    /// Location of the generated `.pgpass` file.
    pub pgpass_file: Utf8PathBuf,
    /// Resolved time zone database directory, if discovery succeeded.
    pub tz_dir: Option<Utf8PathBuf>,
    /// Time zone identifier exported via the `TZ` environment variable.
    pub timezone: String,
}

impl TestBootstrapEnvironment {
    pub(super) fn from_components(
        xdg: XdgDirs,
        pgpass_file: Utf8PathBuf,
        timezone: TimezoneEnv,
    ) -> Self {
        Self {
            home: xdg.home,
            xdg_cache_home: xdg.cache,
            xdg_runtime_dir: xdg.runtime,
            pgpass_file,
            tz_dir: timezone.dir,
            timezone: timezone.zone,
        }
    }

    /// Returns the prepared environment variables as key/value pairs.
    ///
    /// # Examples
    /// ```
    /// use pg_embedded_setup_unpriv::TestBootstrapEnvironment;
    /// use camino::Utf8PathBuf;
    ///
    /// let env = TestBootstrapEnvironment {
    ///     home: Utf8PathBuf::from("/tmp/home"),
    ///     xdg_cache_home: Utf8PathBuf::from("/tmp/home/cache"),
    ///     xdg_runtime_dir: Utf8PathBuf::from("/tmp/home/run"),
    ///     pgpass_file: Utf8PathBuf::from("/tmp/home/.pgpass"),
    ///     tz_dir: None,
    ///     timezone: "UTC".into(),
    /// };
    /// assert_eq!(env.to_env().len(), 6);
    /// ```
    #[must_use]
    pub fn to_env(&self) -> Vec<(String, Option<String>)> {
        let mut env = vec![
            ("HOME".into(), Some(self.home.as_str().into())),
            (
                "XDG_CACHE_HOME".into(),
                Some(self.xdg_cache_home.as_str().into()),
            ),
            (
                "XDG_RUNTIME_DIR".into(),
                Some(self.xdg_runtime_dir.as_str().into()),
            ),
            ("PGPASSFILE".into(), Some(self.pgpass_file.as_str().into())),
        ];

        env.push((
            "TZDIR".into(),
            self.tz_dir.as_ref().map(|dir| dir.as_str().into()),
        ));

        env.push(("TZ".into(), Some(self.timezone.clone())));

        env
    }
}

#[derive(Debug, Clone)]
pub(super) struct XdgDirs {
    pub(super) home: Utf8PathBuf,
    pub(super) cache: Utf8PathBuf,
    pub(super) runtime: Utf8PathBuf,
}

pub(super) fn prepare_timezone_env() -> BootstrapResult<TimezoneEnv> {
    const DEFAULT_TIMEZONE: &str = "UTC";

    let tz_dir = if let Some(dir) = env::var_os("TZDIR") {
        let path = Utf8PathBuf::from_path_buf(PathBuf::from(dir)).map_err(
            |_| -> crate::error::BootstrapError {
                color_eyre::eyre::eyre!("TZDIR must be valid UTF-8").into()
            },
        )?;
        if !path.exists() {
            return Err(color_eyre::eyre::eyre!(
                "time zone database not found at {}. Set TZDIR or install tzdata.",
                path
            )
            .into());
        }
        Some(path)
    } else {
        discover_timezone_dir()?
    };

    let timezone = match env::var("TZ") {
        Ok(value) if !value.trim().is_empty() => value,
        Ok(_) | Err(std::env::VarError::NotPresent) => DEFAULT_TIMEZONE.to_owned(),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(color_eyre::eyre::eyre!("TZ must be valid UTF-8").into());
        }
    };

    Ok(TimezoneEnv {
        dir: tz_dir,
        zone: timezone,
    })
}

fn discover_timezone_dir() -> BootstrapResult<Option<Utf8PathBuf>> {
    #[cfg(unix)]
    {
        let candidate = find_timezone_dir().ok_or_else(|| -> crate::error::BootstrapError {
            color_eyre::eyre::eyre!("time zone database not found. Set TZDIR or install tzdata.")
                .into()
        })?;

        Ok(Some(candidate.to_owned()))
    }

    #[cfg(not(unix))]
    {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::fs;
    use tempfile::tempdir;

    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    // Tests for parse_worker_path_from_env

    #[test]
    fn parse_worker_path_rejects_empty_string() {
        let result = parse_worker_path_from_env(OsStr::new(""));
        let err = result.expect_err("empty path should be rejected");
        assert!(
            err.to_string().contains("must not be empty"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_worker_path_rejects_root_path() {
        let result = parse_worker_path_from_env(OsStr::new("/"));
        let err = result.expect_err("root path should be rejected");
        assert!(
            err.to_string()
                .contains("must not point at the filesystem root"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_worker_path_accepts_valid_path() {
        let result = parse_worker_path_from_env(OsStr::new("/usr/local/bin/pg_worker"));
        let path = result.expect("valid path should be accepted");
        assert_eq!(path.as_str(), "/usr/local/bin/pg_worker");
    }

    #[cfg(unix)]
    #[test]
    fn parse_worker_path_rejects_non_utf8() {
        let non_utf8 = OsStr::from_bytes(b"/path/with/invalid/\xff/bytes");
        let result = parse_worker_path_from_env(non_utf8);
        let err = result.expect_err("non-UTF-8 path should be rejected");
        assert!(
            err.to_string().contains("non-UTF-8 value"),
            "unexpected error: {err}"
        );
    }

    // Tests for discover_worker_from_path

    #[cfg(unix)]
    #[test]
    fn discover_worker_finds_binary_in_path() {
        let temp = tempdir().expect("create tempdir");
        let worker_path = temp.path().join("pg_worker");
        fs::write(&worker_path, b"#!/bin/sh\nexit 0\n").expect("write worker");
        let mut perms = fs::metadata(&worker_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&worker_path, perms).expect("set permissions");

        let original_path = std::env::var_os("PATH");
        let new_path = format!(
            "{}:{}",
            temp.path().display(),
            original_path
                .as_ref()
                .map(|p| p.to_string_lossy())
                .unwrap_or_default()
        );
        unsafe {
            // SAFETY: test is single-threaded for this env modification
            std::env::set_var("PATH", &new_path);
        }

        let result = discover_worker_from_path();

        // Restore PATH
        match original_path {
            Some(p) => unsafe { std::env::set_var("PATH", p) },
            None => unsafe { std::env::remove_var("PATH") },
        }

        let found = result.expect("should find worker in PATH");
        assert!(
            found.as_str().contains("pg_worker"),
            "found path should contain pg_worker: {found}"
        );
    }

    #[test]
    fn discover_worker_returns_none_for_empty_path() {
        let original_path = std::env::var_os("PATH");
        unsafe {
            // SAFETY: test is single-threaded for this env modification
            std::env::set_var("PATH", "");
        }

        let result = discover_worker_from_path();

        // Restore PATH
        match original_path {
            Some(p) => unsafe { std::env::set_var("PATH", p) },
            None => unsafe { std::env::remove_var("PATH") },
        }

        assert!(result.is_none(), "empty PATH should return None");
    }

    #[cfg(unix)]
    #[test]
    fn discover_worker_skips_directories() {
        let temp = tempdir().expect("create tempdir");
        let worker_dir = temp.path().join("pg_worker");
        fs::create_dir(&worker_dir).expect("create directory");

        let original_path = std::env::var_os("PATH");
        let new_path = temp.path().to_string_lossy().to_string();
        unsafe {
            // SAFETY: test is single-threaded for this env modification
            std::env::set_var("PATH", &new_path);
        }

        let result = discover_worker_from_path();

        // Restore PATH
        match original_path {
            Some(p) => unsafe { std::env::set_var("PATH", p) },
            None => unsafe { std::env::remove_var("PATH") },
        }

        assert!(
            result.is_none(),
            "should not find pg_worker when it is a directory"
        );
    }

    #[test]
    fn discover_worker_returns_none_when_not_found() {
        let temp = tempdir().expect("create tempdir");

        let original_path = std::env::var_os("PATH");
        // Use only the empty temp directory as PATH
        let new_path = temp.path().to_string_lossy().to_string();
        unsafe {
            // SAFETY: test is single-threaded for this env modification
            std::env::set_var("PATH", &new_path);
        }

        let result = discover_worker_from_path();

        // Restore PATH
        match original_path {
            Some(p) => unsafe { std::env::set_var("PATH", p) },
            None => unsafe { std::env::remove_var("PATH") },
        }

        assert!(result.is_none(), "should return None when worker not found");
    }
}
