//! Unit tests for `TestCluster`.

use std::ffi::OsString;

use super::TestCluster;
use super::guard::ClusterGuard;
use super::handle::ClusterHandle;
use super::runtime_mode::ClusterRuntime;
use crate::ExecutionPrivileges;
use crate::env::ScopedEnv;
use crate::observability::LOG_TARGET;
use crate::test_support::{dummy_settings, scoped_env};
use tracing::info_span;

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

fn dummy_cluster() -> TestCluster {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let span = info_span!(target: LOG_TARGET, "test_cluster");
    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    let env_vars = bootstrap.environment.to_env();
    let env_guard = ScopedEnv::apply(&env_vars);

    let handle = ClusterHandle::new(bootstrap.clone());
    let guard = ClusterGuard {
        runtime: ClusterRuntime::Sync(runtime),
        postgres: None,
        bootstrap,
        is_managed_via_worker: false,
        env_vars,
        worker_guard: None,
        _env_guard: env_guard,
        _cluster_span: span,
    };

    TestCluster { handle, guard }
}
