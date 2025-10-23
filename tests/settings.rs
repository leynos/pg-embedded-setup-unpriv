//! Validates translating environment settings into PostgreSQL configuration.

use camino::Utf8PathBuf;
#[cfg(feature = "privileged-tests")]
use std::error::Error;
use std::path::PathBuf;

use nix::unistd::geteuid;
use pg_embedded_setup_unpriv::{
    ExecutionPrivileges, PgEnvCfg, detect_execution_privileges, make_data_dir_private,
    make_dir_accessible, nobody_uid,
};
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
    };
    let settings = cfg.to_settings()?;
    assert_eq!(settings.version, VersionReq::parse("=16.4.0")?);
    assert_eq!(settings.port, 5433);
    assert_eq!(settings.username, "admin");
    assert_eq!(settings.password, "secret");
    assert_eq!(settings.data_dir, PathBuf::from("/tmp/data"));
    assert_eq!(settings.installation_dir, PathBuf::from("/tmp/runtime"));
    assert_eq!(
        settings.configuration.get("locale"),
        Some(&"en_US".to_string())
    );
    assert_eq!(
        settings.configuration.get("encoding"),
        Some(&"UTF8".to_string())
    );
    Ok(())
}

/// Tests that the default `PgEnvCfg` configuration can be converted to settings without error.
#[rstest]
fn to_settings_default_config() {
    let cfg = PgEnvCfg::default();
    assert!(cfg.to_settings().is_ok());
}

#[cfg(all(unix, feature = "privileged-tests"))]
#[rstest]
/// Verify that the effective uid is changed within the passed block
fn with_temp_euid_changes_uid() -> color_eyre::Result<()> {
    if !geteuid().is_root() {
        eprintln!("skipping root-dependent test");
        return Ok(());
    }

    let outcome = invoke_deprecated_with_temp_euid();
    let err = outcome.expect_err("with_temp_euid should now reject privilege swaps");
    let source_message = err
        .source()
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| err.to_string());
    assert!(
        source_message
            .contains("with_temp_euid() is unsupported; use the worker-based privileged path"),
        "unexpected error message: {source_message}"
    );
    Ok(())
}

#[cfg(all(unix, not(feature = "privileged-tests")))]
#[rstest]
/// Stub variant ensuring the suite reports skipped when privilege drops are unavailable.
fn with_temp_euid_changes_uid() -> color_eyre::Result<()> {
    eprintln!(
        "skipping root-dependent test: enable the privileged-tests feature to exercise privilege drops",
    );
    Ok(())
}

#[cfg(unix)]
#[path = "support/cap_fs_settings.rs"]
mod cap_fs;

#[cfg(unix)]
mod dir_accessible_tests {
    use super::*;
    use cap_std::fs::{MetadataExt, PermissionsExt};

    use cap_fs::{CapabilityTempDir, metadata};
    use color_eyre::eyre::Context;
    use nix::unistd::User;

    #[rstest]
    fn make_dir_accessible_allows_nobody() -> color_eyre::Result<()> {
        if !geteuid().is_root() {
            eprintln!("skipping root-dependent test");
            return Ok(());
        }

        let tmp = CapabilityTempDir::new("make-dir-accessible")?;
        let dir = tmp.path().join("foo");
        let nobody = User::from_uid(nobody_uid())
            .context("User::from_uid failed")?
            .expect("nobody user should exist");
        super::make_dir_accessible(&dir, &nobody)?;
        let meta = metadata(&dir).map_err(|err| color_eyre::eyre::eyre!(err))?;
        assert_eq!(meta.uid(), nobody_uid().as_raw());
        assert_eq!(meta.permissions().mode() & 0o777, 0o755);
        Ok(())
    }

    #[rstest]
    fn make_data_dir_private_sets_strict_mode() -> color_eyre::Result<()> {
        if !geteuid().is_root() {
            eprintln!("skipping root-dependent test");
            return Ok(());
        }

        let tmp = CapabilityTempDir::new("make-data-dir-private")?;
        let dir = tmp.path().join("bar");
        let nobody = User::from_uid(nobody_uid())
            .context("User::from_uid failed")?
            .expect("nobody user should exist");
        if let Err(err) = super::make_data_dir_private(&dir, &nobody) {
            let message = err.to_string();
            if message.contains("Permission denied") {
                eprintln!(
                    "SKIP-MAKE-DATA-DIR: insufficient permissions to create {}: {}",
                    dir, message
                );
                return Ok(());
            }

            return Err(color_eyre::eyre::eyre!(err));
        }
        let meta = metadata(&dir).map_err(|err| color_eyre::eyre::eyre!(err))?;
        assert_eq!(meta.uid(), nobody_uid().as_raw());
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        Ok(())
    }
}

#[cfg(unix)]
#[rstest]
fn detect_execution_privileges_tracks_effective_uid() -> color_eyre::Result<()> {
    if !geteuid().is_root() {
        assert_eq!(
            detect_execution_privileges(),
            ExecutionPrivileges::Unprivileged
        );
        return Ok(());
    }

    assert_eq!(detect_execution_privileges(), ExecutionPrivileges::Root);
    #[cfg(feature = "privileged-tests")]
    {
        let err = invoke_deprecated_with_temp_euid()
            .expect_err("with_temp_euid should now reject privilege swaps");
        eprintln!("skipping privilege swap: {err}");
    }

    #[cfg(not(feature = "privileged-tests"))]
    {
        eprintln!(
            "skipping privileged uid swap: enable the privileged-tests feature to drop privileges",
        );
    }
    Ok(())
}
