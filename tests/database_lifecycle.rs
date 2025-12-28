#![cfg(unix)]
//! Behavioural coverage for database lifecycle methods on `TestClusterConnection`.

use std::cell::RefCell;
use std::sync::atomic::Ordering;

use color_eyre::eyre::{Context, Result, ensure};
use postgres::NoTls;
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/cluster_skip.rs"]
mod cluster_skip;
#[path = "support/database_lifecycle_helpers.rs"]
mod database_lifecycle_helpers;
#[path = "support/env.rs"]
mod env;
#[path = "support/sandbox.rs"]
mod sandbox;
#[path = "support/scenario.rs"]
mod scenario;
#[path = "support/serial.rs"]
mod serial;
#[path = "support/skip.rs"]
mod skip;

use database_lifecycle_helpers::{
    DatabaseWorld, DatabaseWorldFixture, SETUP_CALL_COUNT, borrow_world, check_db_exists,
    check_db_exists_via_delegation, execute_db_op, setup_sandboxed_cluster, verify_error,
};
use scenario::expect_fixture;
use serial::{ScenarioSerialGuard, serial_guard};

const TEST_DB_NAME: &str = "test_lifecycle_db";
const TEMP_DB_NAME: &str = "temp_lifecycle_db";
const TEMPLATE_NAME: &str = "test_template_db";
const CLONED_DB_NAME: &str = "cloned_from_template_db";

#[fixture]
fn world() -> DatabaseWorldFixture {
    Ok(RefCell::new(DatabaseWorld::new()?))
}

#[given("a sandboxed TestCluster is running")]
fn given_running_cluster(world: &DatabaseWorldFixture) -> Result<()> {
    setup_sandboxed_cluster(world)
}

#[when("a new database is created")]
fn when_database_created(world: &DatabaseWorldFixture) -> Result<()> {
    execute_db_op(
        world,
        |cluster| cluster.connection().create_database(TEST_DB_NAME),
        true,
    )
}

#[when("the same database is created again")]
fn when_duplicate_database_created(world: &DatabaseWorldFixture) -> Result<()> {
    execute_db_op(
        world,
        |cluster| cluster.connection().create_database(TEST_DB_NAME),
        true,
    )
}

#[when("the database is dropped")]
fn when_database_dropped(world: &DatabaseWorldFixture) -> Result<()> {
    execute_db_op(
        world,
        |cluster| cluster.connection().drop_database(TEST_DB_NAME),
        false,
    )
}

#[when("a non-existent database is dropped")]
fn when_nonexistent_database_dropped(world: &DatabaseWorldFixture) -> Result<()> {
    execute_db_op(
        world,
        |cluster| cluster.connection().drop_database("nonexistent_db_12345"),
        false,
    )
}

#[when("a database is created via TestCluster delegation")]
fn when_database_created_via_delegation(world: &DatabaseWorldFixture) -> Result<()> {
    execute_db_op(world, |cluster| cluster.create_database(TEST_DB_NAME), true)
}

#[when("the database is dropped via TestCluster delegation")]
fn when_database_dropped_via_delegation(world: &DatabaseWorldFixture) -> Result<()> {
    execute_db_op(world, |cluster| cluster.drop_database(TEST_DB_NAME), false)
}

#[then("the database exists in the cluster")]
fn then_database_exists(world: &DatabaseWorldFixture) -> Result<()> {
    check_db_exists(world, TEST_DB_NAME, true)
}

#[then("the database no longer exists")]
fn then_database_does_not_exist(world: &DatabaseWorldFixture) -> Result<()> {
    check_db_exists(world, TEST_DB_NAME, false)
}

#[then("the database exists via TestCluster delegation")]
fn then_database_exists_via_delegation(world: &DatabaseWorldFixture) -> Result<()> {
    check_db_exists_via_delegation(world, TEST_DB_NAME, true)
}

#[then("the database no longer exists via TestCluster delegation")]
fn then_database_does_not_exist_via_delegation(world: &DatabaseWorldFixture) -> Result<()> {
    check_db_exists_via_delegation(world, TEST_DB_NAME, false)
}

#[then("a duplicate database error is returned")]
fn then_duplicate_error(world: &DatabaseWorldFixture) -> Result<()> {
    verify_error(
        world,
        true,
        &["already exists", "duplicate"],
        "duplicate database",
    )
}

#[then("a missing database error is returned")]
fn then_missing_error(world: &DatabaseWorldFixture) -> Result<()> {
    verify_error(world, false, &["does not exist"], "missing database")
}

// --- Temporary database scenario steps ---

#[when("a temporary database is created")]
fn when_temp_database_created(world: &DatabaseWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    if world_cell.borrow().is_skipped() {
        return Ok(());
    }
    let temp_db = world_cell
        .borrow()
        .cluster()?
        .temporary_database(TEMP_DB_NAME)?;
    world_cell.borrow_mut().temp_database = Some(temp_db);
    Ok(())
}

#[then("the temporary database exists")]
fn then_temp_database_exists(world: &DatabaseWorldFixture) -> Result<()> {
    check_db_exists(world, TEMP_DB_NAME, true)
}

#[when("the temporary database guard is dropped")]
fn when_temp_database_dropped(world: &DatabaseWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    if world_cell.borrow().is_skipped() {
        return Ok(());
    }
    // Take the temp database out, dropping it
    let _ = world_cell.borrow_mut().temp_database.take();
    Ok(())
}

#[then("the temporary database no longer exists")]
fn then_temp_database_does_not_exist(world: &DatabaseWorldFixture) -> Result<()> {
    check_db_exists(world, TEMP_DB_NAME, false)
}

// --- Ensure template exists scenario steps ---

#[when("ensure_template_exists is called with a setup function")]
fn when_ensure_template_called(world: &DatabaseWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    if world_cell.borrow().is_skipped() {
        return Ok(());
    }
    world_cell
        .borrow()
        .cluster()?
        .ensure_template_exists(TEMPLATE_NAME, |_db_name| {
            SETUP_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })?;
    Ok(())
}

#[then("the template database exists")]
fn then_template_exists(world: &DatabaseWorldFixture) -> Result<()> {
    check_db_exists(world, TEMPLATE_NAME, true)
}

#[then("the setup function was called exactly once")]
fn then_setup_called_once(world: &DatabaseWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    if world_cell.borrow().is_skipped() {
        return Ok(());
    }
    let start = world_cell.borrow().setup_call_count_at_start;
    let current = SETUP_CALL_COUNT.load(Ordering::SeqCst);
    let calls = current - start;
    ensure!(
        calls == 1,
        "expected setup function to be called exactly once, was called {calls} times"
    );
    Ok(())
}

#[when("ensure_template_exists is called again for the same template")]
fn when_ensure_template_called_again(world: &DatabaseWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    if world_cell.borrow().is_skipped() {
        return Ok(());
    }
    world_cell
        .borrow()
        .cluster()?
        .ensure_template_exists(TEMPLATE_NAME, |_db_name| {
            SETUP_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })?;
    Ok(())
}

#[then("the setup function was still called exactly once")]
fn then_setup_still_called_once(world: &DatabaseWorldFixture) -> Result<()> {
    then_setup_called_once(world)
}

// --- Create from template scenario steps ---

#[when("a template database is created and populated")]
fn when_template_created_and_populated(world: &DatabaseWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    if world_cell.borrow().is_skipped() {
        return Ok(());
    }
    let world_ref = world_cell.borrow();
    let cluster = world_ref.cluster()?;
    cluster.connection().create_database(TEMPLATE_NAME)?;

    // Populate the template with test data
    let url = cluster.connection().database_url(TEMPLATE_NAME);
    let mut client =
        postgres::Client::connect(&url, NoTls).context("connect to template database")?;
    client
        .batch_execute(
            "CREATE TABLE test_table (id SERIAL PRIMARY KEY, value TEXT); \
             INSERT INTO test_table (value) VALUES ('template_data');",
        )
        .context("create test table and insert data")?;
    Ok(())
}

#[when("a database is created from the template")]
fn when_database_created_from_template(world: &DatabaseWorldFixture) -> Result<()> {
    execute_db_op(
        world,
        |cluster| {
            cluster
                .connection()
                .create_database_from_template(CLONED_DB_NAME, TEMPLATE_NAME)
        },
        true,
    )
}

#[then("the cloned database exists")]
fn then_cloned_database_exists(world: &DatabaseWorldFixture) -> Result<()> {
    check_db_exists(world, CLONED_DB_NAME, true)
}

#[then("the cloned database contains the template data")]
fn then_cloned_database_contains_template_data(world: &DatabaseWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    if world_cell.borrow().is_skipped() {
        return Ok(());
    }
    let world_ref = world_cell.borrow();
    let cluster = world_ref.cluster()?;
    let url = cluster.connection().database_url(CLONED_DB_NAME);
    let mut client =
        postgres::Client::connect(&url, NoTls).context("connect to cloned database")?;
    let row = client
        .query_one("SELECT value FROM test_table WHERE id = 1", &[])
        .context("query test_table")?;
    let value: String = row.get("value");
    ensure!(
        value == "template_data",
        "expected 'template_data' but got '{value}'"
    );
    Ok(())
}

// --- Scenario declarations ---

#[scenario(path = "tests/features/database_lifecycle.feature", index = 0)]
fn scenario_create_and_drop(serial_guard: ScenarioSerialGuard, world: DatabaseWorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "database lifecycle create/drop world");
}

#[scenario(path = "tests/features/database_lifecycle.feature", index = 1)]
fn scenario_duplicate_database(serial_guard: ScenarioSerialGuard, world: DatabaseWorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "database lifecycle duplicate world");
}

#[scenario(path = "tests/features/database_lifecycle.feature", index = 2)]
fn scenario_drop_nonexistent(serial_guard: ScenarioSerialGuard, world: DatabaseWorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "database lifecycle drop nonexistent world");
}

#[scenario(path = "tests/features/database_lifecycle.feature", index = 3)]
fn scenario_delegation_methods(serial_guard: ScenarioSerialGuard, world: DatabaseWorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "database lifecycle delegation world");
}

#[scenario(path = "tests/features/database_lifecycle.feature", index = 4)]
fn scenario_temp_database_cleanup(serial_guard: ScenarioSerialGuard, world: DatabaseWorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "database lifecycle temp database world");
}

#[scenario(path = "tests/features/database_lifecycle.feature", index = 5)]
fn scenario_ensure_template_exists(serial_guard: ScenarioSerialGuard, world: DatabaseWorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "database lifecycle ensure template world");
}

#[scenario(path = "tests/features/database_lifecycle.feature", index = 6)]
fn scenario_create_from_template(serial_guard: ScenarioSerialGuard, world: DatabaseWorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "database lifecycle create from template world");
}
