//! Validates translating environment settings into `PostgreSQL` configuration.

use camino::Utf8PathBuf;
use color_eyre::eyre::{ensure, eyre};
use std::path::Path;

use nix::unistd::geteuid;
#[cfg(feature = "privileged-tests")]
use pg_embedded_setup_unpriv::Error as PgEmbeddedError;
#[cfg(any(
    feature = "privileged-tests",
    all(unix, feature = "cluster-unit-tests")
))]
use pg_embedded_setup_unpriv::nobody_uid;
use pg_embedded_setup_unpriv::{ExecutionPrivileges, PgEnvCfg, detect_execution_privileges};
#[cfg(all(unix, feature = "cluster-unit-tests"))]
use pg_embedded_setup_unpriv::{make_data_dir_private, make_dir_accessible};
use postgresql_embedded::VersionReq;
use rstest::rstest;

#[cfg(feature = "privileged-tests")]
#[expect(
    deprecated,
    reason = "Tests assert the deprecated helper surfaces its failure path"
)]
fn invoke_deprecated_with_temp_euid() -> pg_embedded_setup_unpriv::Result<()> {
    pg_embedded_setup_unpriv::with_temp_euid(nobody_uid(), || Ok(()))
}

/// Tests that a `PgEnvCfg` with specific settings is correctly converted to a `settings` object,
/// and that all relevant fields and configuration values are preserved.
///
/// # Returns
/// A `color_eyre::Result` indicating success or failure of the round-trip conversion.
///
/// # Examples
/// ```no_run
/// to_settings_roundtrip()?;
/// ```
#[rstest]
fn to_settings_roundtrip() -> color_eyre::Result<()> {
    let cfg = PgEnvCfg {
        version_req: Some("=16.4.0".into()),
        port: Some(5433),
        superuser: Some("admin".into()),
        password: Some("secret".into()),
        data_dir: Some(Utf8PathBuf::from("/tmp/data")),
        runtime_dir: Some(Utf8PathBuf::from("/tmp/runtime")),
        locale: Some("en_US".into()),
        encoding: Some("UTF8".into()),
        binary_cache_dir: None,
    };
    let settings = cfg.to_settings()?;
    let expected_version = VersionReq::parse("=16.4.0").map_err(|err| eyre!(err))?;
    ensure!(
        settings.version == expected_version,
        "version requirement should match the configuration",
    );
    ensure!(settings.port == 5433, "port should match the configuration");
    ensure!(settings.username == "admin", "superuser should propagate");
    ensure!(settings.password == "secret", "password should propagate");
    ensure!(
        settings.data_dir == Path::new("/tmp/data"),
        "data directory should match the configuration",
    );
    ensure!(
        settings.installation_dir == Path::new("/tmp/runtime"),
        "installation directory should match the configuration",
    );
    ensure!(
        settings
            .configuration
            .get("locale")
            .is_some_and(|value| value == "en_US"),
        "locale should be recorded in the configuration map",
    );
    ensure!(
        settings
            .configuration
            .get("encoding")
            .is_some_and(|value| value == "UTF8"),
        "encoding should be recorded in the configuration map",
    );

    // Note: binary_cache_dir is not propagated through to_settings() as it is
    // specific to this crate's cache system, not postgresql_embedded::Settings.
    // See bootstrap module tests for binary_cache_dir propagation coverage.

    Ok(())
}

/// Tests that the default `PgEnvCfg` configuration can be converted to settings without error.
#[rstest]
fn to_settings_default_config() -> color_eyre::Result<()> {
    let cfg = PgEnvCfg::default();
    cfg.to_settings()?;
    Ok(())
}

#[rstest]
fn to_settings_applies_worker_limits() -> color_eyre::Result<()> {
    let cfg = PgEnvCfg::default();
    let settings = cfg.to_settings()?;

    let expected = [
        ("max_connections", "20"),
        ("max_worker_processes", "2"),
        ("max_parallel_workers", "0"),
        ("max_parallel_workers_per_gather", "0"),
        ("max_parallel_maintenance_workers", "0"),
        ("autovacuum", "off"),
        ("max_wal_senders", "0"),
        ("max_replication_slots", "0"),
    ];

    for (key, value) in expected {
        ensure!(
            settings
                .configuration
                .get(key)
                .is_some_and(|observed| observed == value),
            "{key} should be configured as {value}",
        );
    }

    Ok(())
}

#[cfg(all(unix, feature = "privileged-tests"))]
#[rstest]
/// Verify that the effective uid is changed within the passed block
fn with_temp_euid_changes_uid() -> color_eyre::Result<()> {
    if !geteuid().is_root() {
        tracing::warn!("skipping root-dependent test");
        return Ok(());
    }

    let outcome = invoke_deprecated_with_temp_euid();
    let Err(err) = outcome else {
        return Err(eyre!("with_temp_euid should now reject privilege swaps"));
    };
    let privilege_err = match err {
        PgEmbeddedError::Privilege(inner) => inner,
        other => {
            return Err(eyre!(
                "expected privilege error variant, received {other:?}"
            ));
        }
    };
    let source_message = privilege_err.to_string();
    ensure!(
        source_message
            .contains("with_temp_euid() is unsupported; use the worker-based privileged path"),
        "unexpected error message: {source_message}",
    );
    Ok(())
}

#[cfg(all(unix, not(feature = "privileged-tests")))]
#[rstest]
/// Stub variant ensuring the suite reports skipped when privilege drops are unavailable.
fn with_temp_euid_changes_uid() -> color_eyre::Result<()> {
    tracing::warn!(
        "skipping root-dependent test: enable the privileged-tests feature to exercise privilege drops",
    );
    Ok(())
}

#[cfg(all(unix, feature = "cluster-unit-tests"))]
#[path = "support/cap_fs_settings.rs"]
mod cap_fs;

#[cfg(all(unix, feature = "cluster-unit-tests"))]
mod dir_accessible_tests {
    use super::*;
    use cap_std::fs::{MetadataExt, PermissionsExt};

    use cap_fs::{CapabilityTempDir, metadata};
    use color_eyre::eyre::{Context, ensure};
    use nix::unistd::User;

    #[rstest]
    fn make_dir_accessible_allows_nobody() -> color_eyre::Result<()> {
        if !geteuid().is_root() {
            tracing::warn!("skipping root-dependent test");
            return Ok(());
        }

        let tmp = CapabilityTempDir::new("make-dir-accessible")?;
        let dir = tmp.path().join("foo");
        let maybe_user = User::from_uid(nobody_uid()).context("User::from_uid failed")?;
        let Some(nobody) = maybe_user else {
            tracing::warn!("skipping test: 'nobody' user not found");
            return Ok(());
        };
        super::make_dir_accessible(&dir, &nobody)?;
        let meta = metadata(&dir).map_err(|err| eyre!(err))?;
        ensure!(
            meta.uid() == nobody_uid().as_raw(),
            "directory should be owned by the nobody user",
        );
        ensure!(
            meta.permissions().mode() & 0o777 == 0o755,
            "directory should be world-readable",
        );
        Ok(())
    }

    #[rstest]
    fn make_data_dir_private_sets_strict_mode() -> color_eyre::Result<()> {
        if !geteuid().is_root() {
            tracing::warn!("skipping root-dependent test");
            return Ok(());
        }

        let tmp = CapabilityTempDir::new("make-data-dir-private")?;
        let dir = tmp.path().join("bar");
        let maybe_user = User::from_uid(nobody_uid()).context("User::from_uid failed")?;
        let Some(nobody) = maybe_user else {
            tracing::warn!("skipping test: 'nobody' user not found");
            return Ok(());
        };
        if let Err(err) = super::make_data_dir_private(&dir, &nobody) {
            let message = err.to_string();
            if message.contains("Permission denied") {
                tracing::warn!(
                    "SKIP-MAKE-DATA-DIR: insufficient permissions to create {}: {}",
                    dir,
                    message
                );
                return Ok(());
            }

            return Err(color_eyre::eyre::eyre!(err));
        }
        let meta = metadata(&dir).map_err(|err| eyre!(err))?;
        ensure!(
            meta.uid() == nobody_uid().as_raw(),
            "data directory should be owned by the nobody user",
        );
        ensure!(
            meta.permissions().mode() & 0o777 == 0o700,
            "data directory should restrict permissions to 0700",
        );
        Ok(())
    }
}

#[cfg(unix)]
#[rstest]
fn detect_execution_privileges_tracks_effective_uid() -> color_eyre::Result<()> {
    if !geteuid().is_root() {
        ensure!(
            detect_execution_privileges() == ExecutionPrivileges::Unprivileged,
            "non-root execution should be detected as unprivileged",
        );
        return Ok(());
    }

    ensure!(
        detect_execution_privileges() == ExecutionPrivileges::Root,
        "root execution should be detected as privileged",
    );
    #[cfg(feature = "privileged-tests")]
    {
        let Err(err) = invoke_deprecated_with_temp_euid() else {
            return Err(eyre!("with_temp_euid should now reject privilege swaps"));
        };
        tracing::warn!("skipping privilege swap: {err}");
    }

    #[cfg(not(feature = "privileged-tests"))]
    {
        tracing::warn!(
            "skipping privileged uid swap: enable the privileged-tests feature to drop privileges",
        );
    }
    Ok(())
}
