use std::panic::AssertUnwindSafe;

use color_eyre::eyre::{ensure, Result};
use pg_embedded_setup_unpriv::test_support::test_cluster;
use rstest_bdd_macros::{given, scenario, then, when};

use super::{
    serial::ScenarioSerialGuard,
    world::{FixtureEnvProfile, FixtureWorldFixture, borrow_world, env_for_profile},
};
#[path = "../support/scenario.rs"]
mod scenario;

use scenario::expect_fixture;

#[given("the rstest fixture uses the default environment")]
fn given_default_fixture(world: &FixtureWorldFixture) -> Result<()> {
    set_profile(world, FixtureEnvProfile::Default)
}

#[given("the rstest fixture runs without time zone data")]
fn given_missing_timezone(world: &FixtureWorldFixture) -> Result<()> {
    set_profile(world, FixtureEnvProfile::MissingTimezone)
}

#[given("the rstest fixture runs without a worker binary")]
fn given_missing_worker(world: &FixtureWorldFixture) -> Result<()> {
    set_profile(world, FixtureEnvProfile::MissingWorkerBinary)
}

#[given("the rstest fixture uses a non-executable worker binary")]
fn given_non_executable_worker(world: &FixtureWorldFixture) -> Result<()> {
    set_profile(world, FixtureEnvProfile::NonExecutableWorkerBinary)
}

#[given("the rstest fixture encounters filesystem permission issues")]
fn given_permission_denied(world: &FixtureWorldFixture) -> Result<()> {
    set_profile(world, FixtureEnvProfile::PermissionDenied)
}

#[given("the rstest fixture encounters read-only filesystem permissions")]
fn given_read_only_permissions(world: &FixtureWorldFixture) -> Result<()> {
    set_profile(world, FixtureEnvProfile::ReadOnlyFilesystem)
}

#[given("the rstest fixture uses an invalid configuration override")]
fn given_invalid_configuration(world: &FixtureWorldFixture) -> Result<()> {
    set_profile(world, FixtureEnvProfile::InvalidConfiguration)
}

fn set_profile(world: &FixtureWorldFixture, profile: FixtureEnvProfile) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let mut world_mut = world_cell.borrow_mut();
    world_mut.sandbox.reset()?;
    world_mut.env_profile = profile;
    Ok(())
}

#[when("test_cluster is invoked via rstest")]
fn when_fixture_runs(world: &FixtureWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    let env_profile = { world_cell.borrow().env_profile };
    let vars = {
        let world_ref = world_cell.borrow();
        env_for_profile(&world_ref.sandbox, env_profile)?
    };
    let result = {
        let world_ref = world_cell.borrow();
        world_ref.sandbox.with_env(vars, || {
            std::panic::catch_unwind(AssertUnwindSafe(test_cluster))
        })
    };

    match result {
        Ok(cluster) => {
            world_cell.borrow_mut().record_cluster(cluster);
            Ok(())
        }
        Err(payload) => {
            world_cell.borrow_mut().record_failure(payload);
            Ok(())
        }
    }
}

#[then("the fixture yields a running TestCluster")]
fn then_fixture_yields_cluster(world: &FixtureWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let cluster = world_ref.cluster()?;
    let data_dir = &cluster.settings().data_dir;
    ensure!(
        data_dir.join("postmaster.pid").exists(),
        "postmaster.pid should be present whilst the fixture is active",
    );
    Ok(())
}

/// Ensures the recorded panic message contains at least one of the expected keywords.
fn assert_panic_message_contains(
    world: &FixtureWorldFixture,
    expected_keywords: &[&str],
    error_description: &str,
) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let message = world_ref.panic_message()?;
    ensure!(
        expected_keywords.iter().any(|keyword| message.contains(keyword)),
        "expected {error_description} but observed: {message}"
    );
    Ok(())
}

#[then("the fixture reports a missing timezone error")]
fn then_fixture_reports_timezone_error(world: &FixtureWorldFixture) -> Result<()> {
    assert_panic_message_contains(world, &["time zone"], "timezone error")
}

#[then("the fixture reports a missing worker binary error")]
fn then_fixture_reports_missing_worker_binary_error(world: &FixtureWorldFixture) -> Result<()> {
    assert_panic_message_contains(
        world,
        &["worker binary", "No such file"],
        "missing worker binary error",
    )
}

#[then("the fixture reports a non-executable worker binary error")]
fn then_fixture_reports_non_executable_worker_binary_error(
    world: &FixtureWorldFixture,
) -> Result<()> {
    assert_panic_message_contains(
        world,
        &["Permission denied", "not executable", "Operation not permitted"],
        "non-executable worker binary error",
    )
}

#[then("the fixture reports a permission error")]
fn then_fixture_reports_permission_error(world: &FixtureWorldFixture) -> Result<()> {
    assert_panic_message_contains(
        world,
        &["Permission", "permission"],
        "permission error",
    )
}

#[then("the fixture reports a read-only permission error")]
fn then_fixture_reports_read_only_permission_error(world: &FixtureWorldFixture) -> Result<()> {
    assert_panic_message_contains(
        world,
        &["read-only", "Read-only file system", "cannot write", "write permission"],
        "read-only permission error",
    )
}

#[then("the fixture reports an invalid configuration error")]
fn then_fixture_reports_invalid_configuration_error(world: &FixtureWorldFixture) -> Result<()> {
    assert_panic_message_contains(
        world,
        &["configuration", "invalid"],
        "configuration error",
    )
}

#[scenario(path = "tests/features/test_cluster_fixture.feature", index = 0)]
fn scenario_fixture_happy_path(
    serial_guard: ScenarioSerialGuard,
    world: FixtureWorldFixture,
) {
    expect_fixture(
        execute_scenario(serial_guard, world),
        "fixture happy path",
    );
}

#[scenario(path = "tests/features/test_cluster_fixture.feature", index = 1)]
fn scenario_fixture_missing_timezone(
    serial_guard: ScenarioSerialGuard,
    world: FixtureWorldFixture,
) {
    expect_fixture(
        execute_scenario(serial_guard, world),
        "fixture missing timezone",
    );
}

#[scenario(path = "tests/features/test_cluster_fixture.feature", index = 2)]
fn scenario_fixture_missing_worker(
    serial_guard: ScenarioSerialGuard,
    world: FixtureWorldFixture,
) {
    expect_fixture(
        execute_scenario(serial_guard, world),
        "fixture missing worker",
    );
}

#[scenario(path = "tests/features/test_cluster_fixture.feature", index = 3)]
fn scenario_fixture_non_exec_worker(
    serial_guard: ScenarioSerialGuard,
    world: FixtureWorldFixture,
) {
    expect_fixture(
        execute_scenario(serial_guard, world),
        "fixture non executable worker",
    );
}

#[scenario(path = "tests/features/test_cluster_fixture.feature", index = 4)]
fn scenario_fixture_read_only_permissions(
    serial_guard: ScenarioSerialGuard,
    world: FixtureWorldFixture,
) {
    expect_fixture(
        execute_scenario(serial_guard, world),
        "fixture read only permissions",
    );
}

fn execute_scenario(
    serial_guard: ScenarioSerialGuard,
    world: FixtureWorldFixture,
) -> Result<()> {
    let _guard = serial_guard;
    let _ = world?;
    Ok(())
}
