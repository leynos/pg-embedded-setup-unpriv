use std::env::{self, VarError};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::{BootstrapError, BootstrapErrorKind, BootstrapResult};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub(super) const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_SHUTDOWN_TIMEOUT_SECS: u64 = 600;
const SHUTDOWN_TIMEOUT_ENV: &str = "PG_SHUTDOWN_TIMEOUT_SECS";

pub(super) fn shutdown_timeout_from_env() -> BootstrapResult<Duration> {
    match env::var(SHUTDOWN_TIMEOUT_ENV) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                    "{SHUTDOWN_TIMEOUT_ENV} must not be empty"
                )));
            }

            let seconds: u64 = trimmed.parse().map_err(|err| {
                BootstrapError::from(color_eyre::eyre::eyre!(
                    "failed to parse {SHUTDOWN_TIMEOUT_ENV}: {err}"
                ))
            })?;

            if seconds == 0 {
                return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                    "{SHUTDOWN_TIMEOUT_ENV} must be at least 1 second"
                )));
            }

            if seconds > MAX_SHUTDOWN_TIMEOUT_SECS {
                return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                    "{SHUTDOWN_TIMEOUT_ENV} must be {MAX_SHUTDOWN_TIMEOUT_SECS} seconds or less"
                )));
            }

            Ok(Duration::from_secs(seconds))
        }
        Err(VarError::NotPresent) => Ok(DEFAULT_SHUTDOWN_TIMEOUT),
        Err(VarError::NotUnicode(_)) => Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "{SHUTDOWN_TIMEOUT_ENV} must contain a valid UTF-8 value"
        ))),
    }
}

pub(super) fn worker_binary_from_env() -> BootstrapResult<Option<Utf8PathBuf>> {
    let Some(raw) = env::var_os("PG_EMBEDDED_WORKER") else {
        return Ok(None);
    };

    let path = Utf8PathBuf::from_path_buf(PathBuf::from(raw)).map_err(|_| {
        BootstrapError::from(color_eyre::eyre::eyre!(
            "PG_EMBEDDED_WORKER must contain a valid UTF-8 path"
        ))
    })?;

    validate_worker_binary(&path)?;
    Ok(Some(path))
}

fn validate_worker_binary(path: &Utf8PathBuf) -> BootstrapResult<()> {
    let metadata = fs::metadata(path.as_std_path()).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            return BootstrapError::new(
                BootstrapErrorKind::WorkerBinaryMissing,
                color_eyre::eyre::eyre!("failed to access PG_EMBEDDED_WORKER at {path}: {err}"),
            );
        }

        BootstrapError::from(color_eyre::eyre::eyre!(
            "failed to access PG_EMBEDDED_WORKER at {path}: {err}"
        ))
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
