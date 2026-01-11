//! Parses environment variables used by the bootstrapper and surfaces the
//! resulting configuration for the filesystem preparers.

use std::env::{self, VarError};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::{BootstrapError, BootstrapErrorKind, BootstrapResult};
use crate::fs::ambient_dir_and_path;

#[cfg(unix)]
use cap_std::fs::PermissionsExt;

pub(super) const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_SHUTDOWN_TIMEOUT_SECS: u64 = 600;
const SHUTDOWN_TIMEOUT_ENV: &str = "PG_SHUTDOWN_TIMEOUT_SECS";

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
    let Some(raw) = env::var_os("PG_EMBEDDED_WORKER") else {
        return Ok(None);
    };

    let path = Utf8PathBuf::from_path_buf(PathBuf::from(&raw)).map_err(|_| {
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

    validate_worker_binary(&path)?;
    Ok(Some(path))
}

fn validate_worker_binary(path: &Utf8PathBuf) -> BootstrapResult<()> {
    let (dir, relative) =
        ambient_dir_and_path(path).map_err(|err| match fs::metadata(path.as_std_path()) {
            Err(meta_err) if meta_err.kind() == ErrorKind::NotFound => BootstrapError::new(
                BootstrapErrorKind::WorkerBinaryMissing,
                color_eyre::eyre::eyre!("failed to access PG_EMBEDDED_WORKER at {path}: {err}"),
            ),
            _ => BootstrapError::from(err),
        })?;
    let metadata = if relative.as_str().is_empty() {
        dir.dir_metadata()
    } else {
        dir.metadata(relative.as_std_path())
    }
    .map_err(|err| {
        if err.kind() == ErrorKind::NotFound {
            BootstrapError::new(
                BootstrapErrorKind::WorkerBinaryMissing,
                color_eyre::eyre::eyre!("failed to access PG_EMBEDDED_WORKER at {path}: {err}"),
            )
        } else {
            BootstrapError::from(color_eyre::eyre::eyre!(
                "failed to access PG_EMBEDDED_WORKER at {path}: {err}"
            ))
        }
    })?;

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
        static CANDIDATES: [&str; 4] = [
            "/usr/share/zoneinfo",
            "/usr/lib/zoneinfo",
            "/etc/zoneinfo",
            "/share/zoneinfo",
        ];

        let candidate = CANDIDATES
            .iter()
            .map(Utf8Path::new)
            .find(|path| path.exists())
            .ok_or_else(|| -> crate::error::BootstrapError {
                color_eyre::eyre::eyre!(
                    "time zone database not found. Set TZDIR or install tzdata."
                )
                .into()
            })?;

        Ok(Some(candidate.to_owned()))
    }

    #[cfg(not(unix))]
    {
        Ok(None)
    }
}
