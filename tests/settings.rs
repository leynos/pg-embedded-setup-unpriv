use std::path::PathBuf;

use color_eyre::eyre::Context;
use nix::unistd::{User, geteuid, getgid, getuid};
use pg_embedded_setup_unpriv::{
    ExecutionPrivileges, PgEnvCfg, detect_execution_privileges, make_data_dir_private,
    make_dir_accessible, nobody_uid, with_temp_euid,
};
use postgresql_embedded::VersionReq;
use rstest::rstest;

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
        data_dir: Some(PathBuf::from("/tmp/data")),
        runtime_dir: Some(PathBuf::from("/tmp/runtime")),
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

#[cfg(unix)]
#[rstest]
/// Verify that the effective uid is changed within the passed block
fn with_temp_euid_changes_uid() -> color_eyre::Result<()> {
    if !geteuid().is_root() {
        eprintln!("skipping root-dependent test");
        return Ok(());
    }

    let original_euid = geteuid();
    let original_uid = getuid();

    with_temp_euid(nobody_uid(), || {
        assert_eq!(geteuid(), nobody_uid());
        assert_eq!(getuid(), nobody_uid());
        let nobody = User::from_uid(nobody_uid())
            .context("User::from_uid failed")?
            .expect("nobody user should exist");
        assert_eq!(getgid(), nobody.gid);
        Ok(())
    })?;

    assert_eq!(geteuid(), original_euid);
    assert_eq!(getuid(), original_uid);
    Ok(())
}

#[cfg(unix)]
#[path = "support/mod.rs"]
mod support;

#[cfg(unix)]
mod dir_accessible_tests {
    use super::*;
    use cap_std::fs::{MetadataExt, PermissionsExt};

    use crate::support::cap_fs::{CapabilityTempDir, metadata};

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
        super::make_data_dir_private(&dir, &nobody)?;
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
    with_temp_euid(nobody_uid(), || {
        assert_eq!(
            detect_execution_privileges(),
            ExecutionPrivileges::Unprivileged
        );
        Ok(())
    })?;
    Ok(())
}
