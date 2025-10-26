//! Facilitates preparing an embedded `PostgreSQL` instance while dropping root
//! privileges.
//!
//! The library owns the lifecycle for configuring paths, permissions, and
//! process identity so the bundled `PostgreSQL` binaries can initialise safely
//! under an unprivileged account.

mod bootstrap;
mod cluster;
mod env;
mod error;
mod fs;
#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
mod privileges;
#[doc(hidden)]
pub mod test_support;
#[doc(hidden)]
pub mod worker;
pub(crate) mod worker_process;

/// Integration test shims for worker process orchestration.
#[doc(hidden)]
pub mod worker_process_test_api {
    use std::time::Duration;

    use camino::Utf8Path;
    use postgresql_embedded::Settings;

    use crate::WorkerOperation;
    use crate::worker_process;

    #[cfg(all(
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "dragonfly",
        ),
    ))]
    use crate::worker_process::PrivilegeDropGuard as InnerPrivilegeDropGuard;

    /// Test-visible wrapper around the internal worker request.
    ///
    /// Use this helper when integration tests need to exercise worker process
    /// orchestration without exposing the internals as part of the public API.
    pub struct WorkerRequest<'a>(worker_process::WorkerRequest<'a>);

    impl<'a> WorkerRequest<'a> {
        #[must_use]
        #[expect(
            clippy::too_many_arguments,
            reason = "test helper mirrors worker request constructor"
        )]
        /// Constructs a worker request for invoking an operation in tests.
        ///
        /// # Examples
        ///
        /// ```ignore
        /// # use std::time::Duration;
        /// # use camino::Utf8Path;
        /// # use postgresql_embedded::Settings;
        /// # use pg_embedded_setup_unpriv::{WorkerOperation, worker_process_test_api::WorkerRequest};
        /// # let worker = Utf8Path::new("/tmp/worker");
        /// # let settings = Settings::default();
        /// # let env_vars: Vec<(String, Option<String>)> = Vec::new();
        /// let request = WorkerRequest::new(
        ///     worker,
        ///     &settings,
        ///     &env_vars,
        ///     WorkerOperation::Setup,
        ///     Duration::from_secs(1),
        /// );
        /// # let _ = request;
        /// ```
        pub const fn new(
            worker: &'a Utf8Path,
            settings: &'a Settings,
            env_vars: &'a [(String, Option<String>)],
            operation: WorkerOperation,
            timeout: Duration,
        ) -> Self {
            Self(worker_process::WorkerRequest::new(
                worker, settings, env_vars, operation, timeout,
            ))
        }

        /// Returns a reference to the wrapped worker request.
        pub(crate) const fn inner(&self) -> &worker_process::WorkerRequest<'a> {
            &self.0
        }
    }

    /// Executes a worker request whilst returning crate-level errors.
    pub fn run(request: &WorkerRequest<'_>) -> crate::BootstrapResult<()> {
        worker_process::run(request.inner())
    }

    #[cfg(all(
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "dragonfly",
        ),
    ))]
    /// Guard that restores the privilege-drop toggle when tests finish.
    pub struct PrivilegeDropGuard {
        _inner: InnerPrivilegeDropGuard,
    }

    #[cfg(all(
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "dragonfly",
        ),
    ))]
    #[must_use]
    /// Temporarily disables privilege dropping so tests can run deterministic
    /// worker binaries without adjusting file ownership.
    pub fn disable_privilege_drop_for_tests() -> PrivilegeDropGuard {
        PrivilegeDropGuard {
            _inner: worker_process::disable_privilege_drop_for_tests(),
        }
    }

    #[must_use]
    /// Renders a worker failure for assertion-friendly error strings.
    pub fn render_failure_for_tests(
        context: &str,
        output: &std::process::Output,
    ) -> crate::BootstrapError {
        worker_process::render_failure_for_tests(context, output)
    }
}

#[doc(hidden)]
pub use crate::env::ScopedEnv;
pub use bootstrap::{
    ExecutionMode, ExecutionPrivileges, TestBootstrapEnvironment, TestBootstrapSettings,
    bootstrap_for_tests, detect_execution_privileges, run,
};
#[doc(hidden)]
pub use cluster::PrivilegedOperationContext;
pub use cluster::TestCluster;
#[doc(hidden)]
pub use cluster::WorkerOperation;
#[doc(hidden)]
pub use error::BootstrapResult;
pub use error::PgEmbeddedError as Error;
pub use error::{BootstrapError, BootstrapErrorKind, PgEmbeddedError, Result};
#[cfg(feature = "privileged-tests")]
#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
#[expect(
    deprecated,
    reason = "with_temp_euid() remains exported for backward compatibility whilst deprecated"
)]
pub use privileges::with_temp_euid;
#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
pub use privileges::{default_paths_for, make_data_dir_private, make_dir_accessible, nobody_uid};

use color_eyre::eyre::{Context, eyre};
use ortho_config::OrthoConfig;
use postgresql_embedded::{Settings, VersionReq};
use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, ConfigResult};
use camino::Utf8PathBuf;
use std::ffi::OsString;

/// Captures `PostgreSQL` settings supplied via environment variables.
#[derive(Debug, Clone, Serialize, Deserialize, OrthoConfig, Default)]
#[ortho_config(prefix = "PG")]
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::PgEnvCfg;
///
/// let cfg = PgEnvCfg::default();
/// assert!(cfg.port.is_none());
/// ```
pub struct PgEnvCfg {
    /// Optional semver requirement that constrains the `PostgreSQL` version.
    pub version_req: Option<String>,
    /// Port assigned to the embedded `PostgreSQL` server.
    pub port: Option<u16>,
    /// Name of the administrative user created for the cluster.
    pub superuser: Option<String>,
    /// Password provisioned for the administrative user.
    pub password: Option<String>,
    /// Directory used for `PostgreSQL` data files when provided.
    pub data_dir: Option<Utf8PathBuf>,
    /// Directory containing the `PostgreSQL` binaries when provided.
    pub runtime_dir: Option<Utf8PathBuf>,
    /// Locale applied to `initdb` when specified.
    pub locale: Option<String>,
    /// Encoding applied to `initdb` when specified.
    pub encoding: Option<String>,
}

impl PgEnvCfg {
    /// Loads configuration from environment variables without parsing CLI arguments.
    ///
    /// # Errors
    /// Returns an error when environment parsing fails or derived configuration
    /// cannot be represented using UTF-8 paths.
    pub fn load() -> ConfigResult<Self> {
        let args = [OsString::from("pg-embedded-setup-unpriv")];
        Self::load_from_iter(args).map_err(|err| ConfigError::from(eyre!(err)))
    }

    /// Converts the configuration into a complete `postgresql_embedded::Settings` object.
    ///
    /// Applies version, connection, path, and locale settings from the current configuration.
    /// Returns an error if the version requirement is invalid.
    ///
    /// # Returns
    /// A fully configured `Settings` instance on success, or an error if configuration fails.
    ///
    /// # Errors
    /// Returns an error when the semantic version requirement cannot be parsed.
    pub fn to_settings(&self) -> Result<Settings> {
        let mut s = Settings::default();

        self.apply_version(&mut s)?;
        self.apply_connection(&mut s);
        self.apply_paths(&mut s);
        self.apply_locale(&mut s);

        Ok(s)
    }

    fn apply_version(&self, settings: &mut Settings) -> ConfigResult<()> {
        if let Some(ref vr) = self.version_req {
            settings.version =
                VersionReq::parse(vr).context("PG_VERSION_REQ invalid semver spec")?;
        }
        Ok(())
    }

    fn apply_connection(&self, settings: &mut Settings) {
        if let Some(p) = self.port {
            settings.port = p;
        }
        if let Some(ref u) = self.superuser {
            settings.username.clone_from(u);
        }
        if let Some(ref pw) = self.password {
            settings.password.clone_from(pw);
        }
    }

    fn apply_paths(&self, settings: &mut Settings) {
        if let Some(ref dir) = self.data_dir {
            settings.data_dir = dir.clone().into_std_path_buf();
        }
        if let Some(ref dir) = self.runtime_dir {
            settings.installation_dir = dir.clone().into_std_path_buf();
        }
    }

    /// Applies locale and encoding settings to the `PostgreSQL` configuration if specified
    /// in the environment.
    ///
    /// Inserts the `locale` and `encoding` values into the settings configuration map when
    /// present in the environment configuration.
    fn apply_locale(&self, settings: &mut Settings) {
        if let Some(ref loc) = self.locale {
            settings.configuration.insert("locale".into(), loc.clone());
        }
        if let Some(ref enc) = self.encoding {
            settings
                .configuration
                .insert("encoding".into(), enc.clone());
        }
    }
}
