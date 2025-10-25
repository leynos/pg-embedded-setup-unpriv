#![cfg(all(
    test,
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
#![doc = "Integration tests covering the `worker_process` module."]

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Context, eyre};
use pg_embedded_setup_unpriv::worker_process_test_api::{
    WorkerRequest, disable_privilege_drop_for_tests, render_failure_for_tests, run,
};
use pg_embedded_setup_unpriv::{BootstrapError, BootstrapResult, WorkerOperation};
use postgresql_embedded::Settings;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tempfile::tempdir;

const TRUNCATION_SUFFIX: &str = "… [truncated]";

fn test_mutex() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn with_privilege_drop_disabled<T>(f: impl FnOnce() -> T) -> T {
    let _guard = test_mutex()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let drop_guard = disable_privilege_drop_for_tests();
    let result = f();
    drop(drop_guard);
    result
}

fn sample_settings(root: &std::path::Path) -> Settings {
    Settings {
        installation_dir: root.join("install"),
        password_file: root.join("pgpass"),
        data_dir: root.join("data"),
        timeout: Some(Duration::from_secs(30)),
        ..Settings::default()
    }
}

fn write_script(root: &std::path::Path, name: &str, body: &str) -> BootstrapResult<Utf8PathBuf> {
    let path = root.join(name);
    fs::write(&path, body).context("write script")?;
    let mut perms = fs::metadata(&path)
        .context("script metadata")?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).context("set script permissions")?;
    let utf8 = Utf8PathBuf::from_path_buf(path).map_err(|_| eyre!("script path must be UTF-8"))?;
    Ok(utf8)
}

const fn request<'a>(
    worker: &'a Utf8Path,
    settings: &'a Settings,
    env: &'a [(String, Option<String>)],
    timeout: Duration,
) -> WorkerRequest<'a> {
    WorkerRequest::new(worker, settings, env, WorkerOperation::Setup, timeout)
}

fn require_contains(message: &str, needle: &str, description: &str) -> BootstrapResult<()> {
    if message.contains(needle) {
        Ok(())
    } else {
        Err(BootstrapError::from(eyre!("{description}: {message}")))
    }
}

#[test]
fn run_succeeds_when_worker_exits_successfully() -> BootstrapResult<()> {
    with_privilege_drop_disabled(|| -> BootstrapResult<()> {
        let sandbox = tempdir().context("create sandbox")?;
        fs::create_dir_all(sandbox.path().join("install")).context("install dir")?;
        fs::create_dir_all(sandbox.path().join("data")).context("data dir")?;
        fs::write(sandbox.path().join("pgpass"), b"").context("pgpass")?;
        let settings = sample_settings(sandbox.path());
        let env_vars = Vec::new();
        let worker_path = write_script(sandbox.path(), "ok.sh", "#!/bin/sh\nexit 0\n")?;
        let request = request(
            worker_path.as_path(),
            &settings,
            &env_vars,
            Duration::from_secs(1),
        );

        run(&request)
    })
}

#[test]
fn run_truncates_stdout_and_stderr_on_failure() -> BootstrapResult<()> {
    with_privilege_drop_disabled(|| -> BootstrapResult<()> {
        let sandbox = tempdir().context("create sandbox")?;
        fs::create_dir_all(sandbox.path().join("install")).context("install dir")?;
        fs::create_dir_all(sandbox.path().join("data")).context("data dir")?;
        fs::write(sandbox.path().join("pgpass"), b"").context("pgpass")?;
        let settings = sample_settings(sandbox.path());
        let env_vars = Vec::new();
        let long_output = "A".repeat(5_000);
        let script_body = format!(
            "#!/bin/sh\ncat <<'EOF'\n{long_output}\nEOF\ncat <<'EOF' >&2\n{long_output}\nEOF\nexit 1\n"
        );
        let worker_path = write_script(sandbox.path(), "fail.sh", &script_body)?;
        let request = request(
            worker_path.as_path(),
            &settings,
            &env_vars,
            Duration::from_secs(1),
        );

        match run(&request) {
            Ok(()) => Err(BootstrapError::from(eyre!("worker must fail"))),
            Err(err) => {
                let message = err.to_string();
                require_contains(&message, "stdout:", "missing stdout")?;
                require_contains(&message, "stderr:", "missing stderr")?;
                require_contains(
                    &message,
                    TRUNCATION_SUFFIX,
                    "error should mention truncation",
                )?;
                Ok(())
            }
        }
    })
}

#[test]
fn run_reports_timeout_errors() -> BootstrapResult<()> {
    with_privilege_drop_disabled(|| -> BootstrapResult<()> {
        let sandbox = tempdir().context("create sandbox")?;
        fs::create_dir_all(sandbox.path().join("install")).context("install dir")?;
        fs::create_dir_all(sandbox.path().join("data")).context("data dir")?;
        fs::write(sandbox.path().join("pgpass"), b"").context("pgpass")?;
        let settings = sample_settings(sandbox.path());
        let env_vars = Vec::new();
        let script_body = "#!/bin/sh\nsleep 5\n";
        let worker_path = write_script(sandbox.path(), "sleep.sh", script_body)?;
        let request = request(
            worker_path.as_path(),
            &settings,
            &env_vars,
            Duration::from_millis(50),
        );

        match run(&request) {
            Ok(()) => Err(BootstrapError::from(eyre!("worker should time out"))),
            Err(err) => {
                let message = err.to_string();
                require_contains(&message, "timed out", "timeout context missing")?;
                Ok(())
            }
        }
    })
}

#[test]
fn render_failure_truncates_outputs() -> BootstrapResult<()> {
    let long = "B".repeat(4_096);
    let output = std::process::Output {
        status: ExitStatusExt::from_raw(0),
        stdout: long.as_bytes().to_vec(),
        stderr: long.as_bytes().to_vec(),
    };

    let err = render_failure_for_tests("ctx", &output);
    let message = err.to_string();
    require_contains(
        &message,
        TRUNCATION_SUFFIX,
        "error should mention truncation",
    )?;
    Ok(())
}
