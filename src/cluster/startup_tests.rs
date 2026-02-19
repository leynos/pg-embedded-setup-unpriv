//! Tests for setup-only startup orchestration.

use std::ffi::OsString;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, ensure, eyre};
use postgresql_embedded::VersionReq;
use serial_test::serial;
use tempfile::tempdir;

use super::*;
use crate::test_support::{
    dummy_settings, install_run_root_operation_hook, scoped_env, test_runtime,
};

fn utf8_path(path: std::path::PathBuf, context: &str) -> Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(path).map_err(|_| eyre!("{context} is not valid UTF-8"))
}

fn configure_root_bootstrap(
    bootstrap: &mut TestBootstrapSettings,
    install_dir: &Utf8PathBuf,
    data_dir: &Utf8PathBuf,
    scoped_cache_home: &Utf8PathBuf,
) {
    let runtime_dir = install_dir.join("run");
    bootstrap.settings.installation_dir = install_dir.clone().into_std_path_buf();
    bootstrap.settings.data_dir = data_dir.clone().into_std_path_buf();
    bootstrap.settings.version = VersionReq::parse("^17").expect("valid version requirement");
    bootstrap.environment.home = install_dir.clone();
    bootstrap.environment.xdg_cache_home = scoped_cache_home.clone();
    bootstrap.environment.xdg_runtime_dir = runtime_dir;
    bootstrap.environment.pgpass_file = install_dir.join(".pgpass");
}

fn create_complete_cache_entry(cache_home: &Utf8PathBuf, version: &str) -> Result<()> {
    let version_dir = cache_home
        .join("pg-embedded")
        .join("binaries")
        .join(version);
    fs::create_dir_all(version_dir.join("bin"))?;
    fs::write(version_dir.join(".complete"), b"")?;
    Ok(())
}

#[test]
#[serial(startup_setup_only)]
fn setup_postgres_only_resolves_cache_before_scoped_env_and_runs_setup_only() -> Result<()> {
    let temp = tempdir()?;
    let base = utf8_path(temp.path().to_path_buf(), "tempdir")?;
    let install_dir = base.join("install");
    let data_dir = base.join("data");
    let scoped_cache_home = base.join("scoped-cache-home");
    let host_cache_home = base.join("host-cache-home");
    fs::create_dir_all(install_dir.as_std_path())?;
    fs::create_dir_all(data_dir.as_std_path())?;
    create_complete_cache_entry(&host_cache_home, "17.4.0")?;

    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    configure_root_bootstrap(&mut bootstrap, &install_dir, &data_dir, &scoped_cache_home);

    let operations = Arc::new(Mutex::new(Vec::new()));
    let recorded_operations = Arc::clone(&operations);
    let _hook_guard = install_run_root_operation_hook(move |_, _, operation| {
        recorded_operations
            .lock()
            .expect("operation mutex poisoned")
            .push(operation);
        Ok(())
    })
    .map_err(|err| eyre!(err))?;
    let _env_guard = scoped_env([
        (OsString::from("PG_BINARY_CACHE_DIR"), None),
        (
            OsString::from("XDG_CACHE_HOME"),
            Some(OsString::from(host_cache_home.as_str())),
        ),
    ]);

    let prepared = setup_postgres_only(bootstrap)?;

    let recorded_ops = operations.lock().expect("operation mutex poisoned");
    ensure!(
        matches!(recorded_ops.as_slice(), [operation] if operation.as_str() == "setup"),
        "setup-only lifecycle should invoke exactly one Setup operation",
    );
    ensure!(
        prepared.settings.trust_installation_dir,
        "cache hit should mark installation directory as trusted"
    );

    let observed_install_dir = utf8_path(
        prepared.settings.installation_dir.clone(),
        "installation directory",
    )?;
    ensure!(
        observed_install_dir == install_dir.join("17.4.0"),
        "expected setup to consume host cache before scoped env (observed: {observed_install_dir})"
    );
    ensure!(
        !prepared.settings.data_dir.join("postmaster.pid").exists(),
        "setup-only path must not start PostgreSQL"
    );
    Ok(())
}

#[test]
#[serial(startup_setup_only)]
fn setup_lifecycle_invokes_setup_operation_only() -> Result<()> {
    let temp = tempdir()?;
    let base = utf8_path(temp.path().to_path_buf(), "tempdir")?;
    let install_dir = base.join("install");
    let data_dir = base.join("data");
    let scoped_cache_home = base.join("scoped-cache-home");
    let cache_dir = base.join("cache");
    fs::create_dir_all(install_dir.as_std_path())?;
    fs::create_dir_all(data_dir.as_std_path())?;
    fs::create_dir_all(cache_dir.as_std_path())?;

    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    configure_root_bootstrap(&mut bootstrap, &install_dir, &data_dir, &scoped_cache_home);
    let env_vars = bootstrap.environment.to_env();
    let cache_config = BinaryCacheConfig::with_dir(cache_dir);
    let runtime = test_runtime()?;

    let operations = Arc::new(Mutex::new(Vec::new()));
    let recorded_operations = Arc::clone(&operations);
    let _hook_guard = install_run_root_operation_hook(move |_, _, operation| {
        recorded_operations
            .lock()
            .expect("operation mutex poisoned")
            .push(operation);
        Ok(())
    })
    .map_err(|err| eyre!(err))?;

    let prepared = setup_lifecycle(&runtime, bootstrap, &env_vars, &cache_config)?;

    let recorded_ops = operations.lock().expect("operation mutex poisoned");
    ensure!(
        matches!(recorded_ops.as_slice(), [operation] if operation.as_str() == "setup"),
        "setup_lifecycle should only dispatch Setup",
    );
    ensure!(
        !prepared.settings.data_dir.join("postmaster.pid").exists(),
        "setup_lifecycle must not start PostgreSQL",
    );
    Ok(())
}

#[test]
#[serial(startup_setup_only)]
fn setup_with_privileges_root_dispatches_setup_only() -> Result<()> {
    let temp = tempdir()?;
    let base = utf8_path(temp.path().to_path_buf(), "tempdir")?;
    let install_dir = base.join("install");
    let data_dir = base.join("data");
    let scoped_cache_home = base.join("scoped-cache-home");
    fs::create_dir_all(install_dir.as_std_path())?;
    fs::create_dir_all(data_dir.as_std_path())?;

    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    configure_root_bootstrap(&mut bootstrap, &install_dir, &data_dir, &scoped_cache_home);
    let env_vars = bootstrap.environment.to_env();
    let runtime = test_runtime()?;

    let operations = Arc::new(Mutex::new(Vec::new()));
    let recorded_operations = Arc::clone(&operations);
    let _hook_guard = install_run_root_operation_hook(move |_, _, operation| {
        recorded_operations
            .lock()
            .expect("operation mutex poisoned")
            .push(operation);
        Ok(())
    })
    .map_err(|err| eyre!(err))?;

    setup_with_privileges(
        ExecutionPrivileges::Root,
        &runtime,
        &mut bootstrap,
        &env_vars,
    )?;

    let recorded_ops = operations.lock().expect("operation mutex poisoned");
    ensure!(
        matches!(recorded_ops.as_slice(), [operation] if operation.as_str() == "setup"),
        "setup_with_privileges should dispatch Setup only",
    );
    Ok(())
}

#[test]
fn populate_cache_on_miss_only_populates_on_cache_miss() -> Result<()> {
    let temp = tempdir()?;
    let base = utf8_path(temp.path().to_path_buf(), "tempdir")?;
    let install_dir = base.join("install").join("17.4.0");
    let data_dir = base.join("data");
    let cache_dir = base.join("cache");
    fs::create_dir_all(install_dir.join("bin").as_std_path())?;
    fs::create_dir_all(data_dir.as_std_path())?;
    fs::create_dir_all(cache_dir.as_std_path())?;

    let mut bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    bootstrap.settings.installation_dir = install_dir.clone().into_std_path_buf();
    bootstrap.settings.data_dir = data_dir.into_std_path_buf();
    let cache_config = BinaryCacheConfig::with_dir(cache_dir.clone());
    let marker_path = cache_dir.join("17.4.0").join(".complete");

    populate_cache_on_miss(true, &cache_config, &bootstrap);
    ensure!(
        !marker_path.exists(),
        "cache marker should remain absent on cache hit"
    );

    populate_cache_on_miss(false, &cache_config, &bootstrap);
    ensure!(
        marker_path.exists(),
        "cache marker should be written on cache miss"
    );
    Ok(())
}

#[test]
fn setup_postgres_only_inside_runtime_returns_recoverable_error() -> Result<()> {
    let temp = tempdir()?;
    let base = utf8_path(temp.path().to_path_buf(), "tempdir")?;
    let install_file = base.join("install-file");
    let data_dir = base.join("data");
    fs::write(install_file.as_std_path(), b"not-a-directory")?;
    fs::create_dir_all(data_dir.as_std_path())?;

    let mut bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    bootstrap.settings.installation_dir = install_file.into_std_path_buf();
    bootstrap.settings.data_dir = data_dir.into_std_path_buf();
    bootstrap.setup_timeout = Duration::from_secs(1);

    let runtime = test_runtime()?;
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(async { setup_postgres_only(bootstrap) })
    }));

    ensure!(
        outcome.is_ok(),
        "setup_postgres_only should not panic inside an existing runtime"
    );
    let setup_result = outcome.expect("nested-runtime outcome should be captured");
    ensure!(
        setup_result.is_err(),
        "expected a recoverable setup error for invalid installation path"
    );
    Ok(())
}
