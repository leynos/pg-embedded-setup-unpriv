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

pub(crate) fn parse_worker_path_from_env(raw: &std::ffi::OsStr) -> BootstrapResult<Utf8PathBuf> {
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
/// Returns the first valid UTF-8 path to an existing executable `pg_worker`
/// file, or `None` if no candidate is found. This enables zero-configuration
/// usage when the worker is installed via `cargo install`.
///
/// Security: Skips relative PATH entries and world-writable directories to
/// prevent privilege escalation when running as root. Non-executable
/// candidates are also skipped so a valid worker later in PATH can be found.
pub(crate) fn discover_worker_from_path() -> Option<Utf8PathBuf> {
    let path_var = env::var_os("PATH")?;
    env::split_paths(&path_var)
        .filter(|dir| is_trusted_path_directory(dir))
        .map(|dir| {
            let candidate = dir.join("pg_worker");
            #[cfg(windows)]
            let candidate = candidate.with_extension("exe");
            candidate
        })
        .find(|candidate| is_executable(candidate))
        .and_then(|candidate| Utf8PathBuf::from_path_buf(candidate).ok())
}

/// Checks whether the candidate path is an executable file.
#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && (m.permissions().mode() & 0o111) != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file())
        .unwrap_or(false)
}

/// Checks whether a PATH directory is safe to search for executables.
///
/// Rejects relative paths and world-writable directories to prevent privilege
/// escalation when running as root.
#[cfg(unix)]
pub(crate) fn is_trusted_path_directory(dir: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if !dir.is_absolute() {
        return false;
    }
    std::fs::metadata(dir)
        .map(|m| m.is_dir() && (m.permissions().mode() & 0o002) == 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub(crate) fn is_trusted_path_directory(dir: &std::path::Path) -> bool {
    dir.is_absolute()
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
