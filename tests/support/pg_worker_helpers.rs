//! Test helpers and fixtures for `pg_worker` tests.

use postgresql_embedded::{Settings, VersionReq};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;

use pg_embedded_setup_unpriv::worker::{PlainSecret, WorkerPayload};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub const PG_CTL_STUB: &str = include_str!("fixtures/pg_ctl_stub.sh");

/// Trait for environment variable operations, allowing mock implementations in tests.
pub trait EnvironmentOperations {
    fn set_var(&self, key: &str, value: &str);
    fn remove_var(&self, key: &str);
}

/// Mock environment implementation for tests that tracks environment operations
/// without actually mutating the process environment.
#[derive(Debug, Default)]
pub struct MockEnvironment {
    vars: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
}

impl MockEnvironment {
    pub fn get(&self, key: &str) -> Option<String> {
        self.vars
            .lock()
            .expect("MockEnvironment.vars mutex poisoned")
            .get(key)
            .cloned()
    }
}

impl EnvironmentOperations for MockEnvironment {
    fn set_var(&self, key: &str, value: &str) {
        let mut vars = self.vars.lock().expect("mock env mutex poisoned");
        vars.insert(key.to_owned(), value.to_owned());
    }

    fn remove_var(&self, key: &str) {
        let mut vars = self.vars.lock().expect("mock env mutex poisoned");
        vars.remove(key);
    }
}

/// Applies environment overrides using the provided operations implementation.
pub fn apply_worker_environment_with<E>(env_ops: &E, environment: &[(String, Option<PlainSecret>)])
where
    E: EnvironmentOperations,
{
    for (key, value) in environment {
        match value {
            Some(env_value) => env_ops.set_var(key, env_value.expose()),
            None => env_ops.remove_var(key),
        }
    }
}

/// Writes a `pg_ctl` stub script to the specified binary directory.
pub fn write_pg_ctl_stub(bin_dir: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(bin_dir)?;
    let pg_ctl_path = bin_dir.join("pg_ctl");
    fs::write(&pg_ctl_path, PG_CTL_STUB)?;
    let mut permissions = fs::metadata(&pg_ctl_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&pg_ctl_path, permissions)?;
    Ok(())
}

/// Builds test Settings with valid defaults for testing.
pub fn build_settings(
    temp_root: &TempDir,
    install_dir: PathBuf,
    data_dir: PathBuf,
) -> Result<Settings, BoxError> {
    Ok(Settings {
        releases_url: "https://example.invalid/releases".into(),
        version: VersionReq::parse("=16.4.0")?,
        installation_dir: install_dir,
        password_file: temp_root.path().join("pgpass"),
        data_dir,
        host: "127.0.0.1".into(),
        port: 54_321,
        username: "postgres".into(),
        password: "postgres".into(),
        temporary: false,
        timeout: Some(Duration::from_secs(5)),
        configuration: HashMap::new(),
        trust_installation_dir: true,
    })
}

/// Writes a worker config JSON file for testing.
pub fn write_worker_config(temp_root: &TempDir, settings: &Settings) -> Result<PathBuf, BoxError> {
    let payload = WorkerPayload::new(settings, Vec::new())?;
    let config_path = temp_root.path().join("config.json");
    let config_bytes = serde_json::to_vec(&payload)?;
    fs::write(&config_path, config_bytes)?;
    Ok(config_path)
}
