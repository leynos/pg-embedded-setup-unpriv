#![cfg(unix)]
//! Behavioural coverage for database lifecycle methods on `TestClusterConnection`.

use std::cell::RefCell;

use color_eyre::eyre::{Context, Result, ensure, eyre};
use pg_embedded_setup_unpriv::{TemporaryDatabase, TestCluster};
use postgres::NoTls;
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};
use std::ffi::{OsStr, OsString};
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/cluster_skip.rs"]
mod cluster_skip;
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

use cluster_skip::cluster_skip_message;
use sandbox::TestSandbox;
use scenario::expect_fixture;
use serial::{ScenarioSerialGuard, serial_guard};

const TEST_DB_NAME: &str = "test_lifecycle_db";
const TEMP_DB_NAME: &str = "temp_lifecycle_db";
const TEMPLATE_NAME: &str = "test_template_db";
const CLONED_DB_NAME: &str = "cloned_from_template_db";

/// Global counter for tracking setup function invocations across scenarios.
///
/// # Safety
///
/// Correctness requires that all scenarios hold the `serial_guard` fixture,
/// ensuring serial execution and preventing concurrent increments.
static SETUP_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

struct DatabaseWorld {
    sandbox: TestSandbox,
    cluster: Option<TestCluster>,
    create_error: Option<String>,
    drop_error: Option<String>,
    skip_reason: Option<String>,
    bootstrap_error: Option<String>,
    temp_database: Option<TemporaryDatabase>,
    setup_call_count_at_start: usize,
}

impl DatabaseWorld {
    fn new() -> Result<Self> {
        Ok(Self {
            sandbox: TestSandbox::new("database-lifecycle").context("create sandbox")?,
            cluster: None,
            create_error: None,
            drop_error: None,
            skip_reason: None,
            bootstrap_error: None,
            temp_database: None,
            setup_call_count_at_start: SETUP_CALL_COUNT.load(Ordering::SeqCst),
        })
    }

    fn mark_skip(&mut self, reason: impl Into<String>) {
        let message = reason.into();
        tracing::warn!("{message}");
        self.skip_reason = Some(message);
    }

    const fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
    }

    fn ensure_not_skipped(&self) -> Result<()> {
        if self.is_skipped() {
            Err(eyre!("scenario skipped"))
        } else {
            Ok(())
        }
    }

    fn record_cluster(&mut self, cluster: TestCluster) {
        self.cluster = Some(cluster);
        self.create_error = None;
        self.drop_error = None;
        self.bootstrap_error = None;
        self.skip_reason = None;
        self.temp_database = None;
        self.setup_call_count_at_start = SETUP_CALL_COUNT.load(Ordering::SeqCst);
    }

    fn record_bootstrap_error(&mut self, err: impl std::fmt::Display + std::fmt::Debug) {
        let message = err.to_string();
        let debug = format!("{err:?}");
        self.bootstrap_error = Some(message.clone());
        self.cluster = None;
        if let Some(reason) = cluster_skip_message(&message, Some(&debug)) {
            self.mark_skip(reason);
        }
    }

    fn cluster(&self) -> Result<&TestCluster> {
        self.ensure_not_skipped()?;
        self.cluster
            .as_ref()
            .ok_or_else(|| eyre!("TestCluster was not created"))
    }
}

type DatabaseWorldFixture = Result<RefCell<DatabaseWorld>>;

fn borrow_world(world: &DatabaseWorldFixture) -> Result<&RefCell<DatabaseWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!(format!("database world failed to initialise: {err}")))
}

/// Execute a database operation, capturing any error in the specified field.
///
/// If `is_create` is true, errors are stored in `create_error`; otherwise in `drop_error`.
fn execute_db_op<F>(world: &DatabaseWorldFixture, op: F, is_create: bool) -> Result<()>
where
    F: FnOnce(&TestCluster) -> pg_embedded_setup_unpriv::BootstrapResult<()>,
{
    let world_cell = borrow_world(world)?;
    if world_cell.borrow().is_skipped() {
        return Ok(());
    }
    let result = op(world_cell.borrow().cluster()?);
    if let Err(err) = result {
        let mut world_mut = world_cell.borrow_mut();
        let error_chain = format!("{err:?}");
        if is_create {
            world_mut.create_error = Some(error_chain.clone());
        } else {
            world_mut.drop_error = Some(error_chain);
        }
    }
    Ok(())
}

/// Check database existence with expected state using `TestClusterConnection`.
fn check_db_exists(world: &DatabaseWorldFixture, db_name: &str, expected: bool) -> Result<()> {
    let world_cell = borrow_world(world)?;
    if world_cell.borrow().is_skipped() {
        return Ok(());
    }
    let exists = world_cell
        .borrow()
        .cluster()?
        .connection()
        .database_exists(db_name)?;
    if expected {
        ensure!(exists, "expected database '{db_name}' to exist");
    } else {
        ensure!(!exists, "expected database '{db_name}' to not exist");
    }
    Ok(())
}

/// Check database existence with expected state using `TestCluster` delegation.
fn check_db_exists_via_delegation(
    world: &DatabaseWorldFixture,
    db_name: &str,
    expected: bool,
) -> Result<()> {
    let world_cell = borrow_world(world)?;
    if world_cell.borrow().is_skipped() {
        return Ok(());
    }
    let exists = world_cell.borrow().cluster()?.database_exists(db_name)?;
    if expected {
        ensure!(
            exists,
            "expected database '{db_name}' to exist via delegation"
        );
    } else {
        ensure!(
            !exists,
            "expected database '{db_name}' to not exist via delegation"
        );
    }
    Ok(())
}

/// Verify captured error contains expected text.
///
/// If `is_create` is true, checks `create_error`; otherwise checks `drop_error`.
fn verify_error(
    world: &DatabaseWorldFixture,
    is_create: bool,
    expected_patterns: &[&str],
    error_type: &str,
) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let error = if is_create {
        world_ref.create_error.as_ref()
    } else {
        world_ref.drop_error.as_ref()
    }
    .ok_or_else(|| eyre!("expected {error_type} error but none recorded"))?;
    let matches = expected_patterns.iter().any(|p| error.contains(p));
    ensure!(matches, "expected {error_type} error, got: {error}");
    Ok(())
}

#[fixture]
fn world() -> DatabaseWorldFixture {
    Ok(RefCell::new(DatabaseWorld::new()?))
}

#[given("a sandboxed TestCluster is running")]
fn given_running_cluster(world: &DatabaseWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    {
        let world_ref = world_cell.borrow();
        world_ref.sandbox.reset()?;
    }

    let vars = {
        let world_ref = world_cell.borrow();
        let mut vars = world_ref.sandbox.env_without_timezone();
        for (key, value) in &mut vars {
            if key.as_os_str() == OsStr::new("TZDIR") {
                *value = Some(OsString::from("/usr/share/zoneinfo"));
            }
            if key.as_os_str() == OsStr::new("TZ") {
                *value = Some(OsString::from("UTC"));
            }
        }
        vars
    };

    let result = {
        let world_ref = world_cell.borrow();
        world_ref.sandbox.with_env(vars, TestCluster::new)
    };

    match result {
        Ok(cluster) => {
            world_cell.borrow_mut().record_cluster(cluster);
            Ok(())
        }
        Err(err) => {
            world_cell.borrow_mut().record_bootstrap_error(err);
            Ok(())
        }
    }
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
