//! Test utilities and unit checks for cluster fixtures.

use color_eyre::eyre::{Context, Result, ensure};
use pg_embedded_setup_unpriv::{TestCluster, test_support::test_cluster};
use rstest::rstest;

use super::{
    TestSandbox,
    env_isolation::{EnvIsolationGuard, set_env_var},
    process_utils::{
        read_postmaster_pid, sandbox_root_path, wait_for_pid_file_removal, wait_for_process_exit,
    },
    serial::{ScenarioSerialGuard, serial_guard},
};

pub(super) fn run_unit_fixture_test<F>(name: &str, test: F) -> Result<()>
where
    F: FnOnce(TestCluster) -> Result<()>,
{
    let _env_reset = EnvIsolationGuard::capture();
    let sandbox =
        TestSandbox::new(name).context("create rstest fixture sandbox for unit coverage")?;
    sandbox
        .reset()
        .context("reset rstest fixture sandbox before applying env")?;
    let result = sandbox.with_env(sandbox.env_without_timezone(), || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(test_cluster))
    });
    result.map_or_else(super::world::handle_fixture_panic, test)
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds fixtures even when ignored in the body"
)]
#[rstest]
pub(super) fn fixture_exposes_connection_metadata(
    _serial_guard: ScenarioSerialGuard,
) -> Result<()> {
    run_unit_fixture_test("rstest-fixture-metadata", |test_cluster| {
        let metadata = test_cluster.connection().metadata();
        ensure!(
            metadata.port() > 0,
            "fixture should yield a running PostgreSQL instance",
        );
        ensure!(
            metadata.superuser() == "postgres",
            "tests rely on deterministic sandbox credentials",
        );
        Ok(())
    })
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds fixtures even when ignored in the body"
)]
#[rstest]
pub(super) fn fixture_reuses_cluster_environment(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    run_unit_fixture_test("rstest-fixture-env", |test_cluster| {
        let env = test_cluster.environment();
        ensure!(
            env.pgpass_file.starts_with(env.home.as_str()),
            "pgpass file should live under HOME for the fixture",
        );
        ensure!(
            test_cluster.settings().data_dir.exists(),
            "data directory should exist whilst the cluster runs",
        );
        Ok(())
    })
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds fixtures even when ignored in the body"
)]
#[rstest]
pub(super) fn fixture_environment_variable_isolation(
    _serial_guard: ScenarioSerialGuard,
) -> Result<()> {
    const LEAK_VAR: &str = "RSTEST_CLUSTER_ENV_POLLUTION";
    run_unit_fixture_test("rstest-fixture-env-pollution", |_test_cluster| {
        unsafe { set_env_var(LEAK_VAR, "polluted_value") };
        Ok(())
    })?;
    ensure!(
        std::env::var(LEAK_VAR).is_err(),
        "{leak} should not leak across fixture runs",
        leak = LEAK_VAR,
    );
    run_unit_fixture_test("rstest-fixture-env-clean", |_test_cluster| {
        ensure!(
            std::env::var(LEAK_VAR).is_err(),
            "{leak} should be absent when the fixture starts",
            leak = LEAK_VAR,
        );
        Ok(())
    })
}

#[expect(
    clippy::used_underscore_binding,
    reason = "rstest binds fixtures even when ignored in the body"
)]
#[rstest]
pub(super) fn fixture_teardown_resource_cleanup(_serial_guard: ScenarioSerialGuard) -> Result<()> {
    let sandbox = TestSandbox::new("rstest-fixture-teardown")
        .context("create sandbox for teardown coverage")?;
    sandbox
        .reset()
        .context("reset sandbox before teardown coverage")?;
    let sandbox_root = sandbox_root_path(&sandbox)?;
    let result = sandbox.with_env(sandbox.env_without_timezone(), || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(test_cluster))
    });
    let cluster = match result {
        Ok(cluster) => cluster,
        Err(payload) => {
            super::world::handle_fixture_panic(payload)?;
            return Ok(());
        }
    };
    let data_dir = cluster.settings().data_dir.clone();
    let pid = read_postmaster_pid(data_dir.as_path())?;
    drop(cluster);
    wait_for_process_exit(pid)?;
    wait_for_pid_file_removal(data_dir.as_path())?;
    drop(sandbox);
    ensure!(
        !sandbox_root.exists(),
        "sandbox root should be removed once the guard drops",
    );
    Ok(())
}
