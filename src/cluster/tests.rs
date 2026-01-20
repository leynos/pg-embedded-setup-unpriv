//! Test coverage for the cluster module helpers.

use std::ffi::OsString;
use std::fs;

use super::*;
use crate::ExecutionPrivileges;
use crate::observability::LOG_TARGET;
use crate::test_support::{dummy_settings, scoped_env};

#[test]
fn with_worker_guard_restores_environment() {
    const KEY: &str = "PG_EMBEDDED_WORKER_GUARD_TEST";
    let baseline = std::env::var(KEY).ok();
    let guard = scoped_env(vec![(OsString::from(KEY), Some(OsString::from("guarded")))]);
    let cluster = dummy_cluster().with_worker_guard(Some(guard));
    assert_eq!(
        std::env::var(KEY).as_deref(),
        Ok("guarded"),
        "worker guard should remain active whilst the cluster runs",
    );
    drop(cluster);
    match baseline {
        Some(value) => assert_eq!(
            std::env::var(KEY).as_deref(),
            Ok(value.as_str()),
            "worker guard should restore the previous value"
        ),
        None => assert!(
            std::env::var(KEY).is_err(),
            "worker guard should unset the variable once the cluster drops"
        ),
    }
}

#[test]
fn refresh_worker_port_reads_postmaster_pid() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let pid_path = temp_dir.path().join("postmaster.pid");
    let contents = format!("12345\n{}\n1700000000\n54321\n", temp_dir.path().display());
    fs::write(&pid_path, contents).expect("write postmaster.pid");

    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    bootstrap.settings.data_dir = temp_dir.path().to_path_buf();
    bootstrap.settings.port = 0;

    super::port_refresh::refresh_worker_port(&mut bootstrap).expect("refresh worker port");
    assert_eq!(bootstrap.settings.port, 54321);
}

fn dummy_cluster() -> TestCluster {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let span = tracing::info_span!(target: LOG_TARGET, "test_cluster");
    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    let env_vars = bootstrap.environment.to_env();
    let env_guard = ScopedEnv::apply(&env_vars);
    TestCluster {
        runtime: ClusterRuntime::Sync(runtime),
        postgres: None,
        bootstrap,
        is_managed_via_worker: false,
        env_vars,
        worker_guard: None,
        _env_guard: env_guard,
        _cluster_span: span,
    }
}

#[cfg(feature = "cluster-unit-tests")]
mod drop_logging_tests {
    use super::drop_handling::DropContext;
    use super::*;
    use crate::test_support::capture_warn_logs;

    #[test]
    fn warn_stop_timeout_emits_warning() {
        let context = DropContext::new("ctx");
        let (logs, ()) = capture_warn_logs(|| TestCluster::warn_stop_timeout(5, &context));
        assert!(
            logs.iter()
                .any(|line| line.contains("stop() timed out after 5s (ctx)")),
            "expected timeout warning, got {logs:?}"
        );
    }

    #[test]
    fn warn_stop_failure_emits_warning() {
        let context = DropContext::new("ctx");
        let (logs, ()) = capture_warn_logs(|| TestCluster::warn_stop_failure(&context, &"boom"));
        assert!(
            logs.iter()
                .any(|line| line.contains("failed to stop embedded postgres instance")),
            "expected failure warning, got {logs:?}"
        );
    }
}

#[cfg(not(feature = "cluster-unit-tests"))]
#[path = "../../tests/test_cluster.rs"]
mod test_cluster_tests;
