//! Behavioural coverage for the connection helpers exposed by `TestCluster`.
#![cfg(unix)]

use std::cell::RefCell;

use color_eyre::eyre::{Context, Result, ensure, eyre};
use diesel::prelude::*;
use diesel::sql_types::Integer;
use pg_embedded_setup_unpriv::{ConnectionMetadata, TestCluster};
use rstest::fixture;
use rstest_bdd_macros::{given, scenario, then, when};
use std::ffi::{OsStr, OsString};

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

#[derive(QueryableByName, Debug, PartialEq, Eq)]
struct ValueRow {
    #[diesel(sql_type = Integer)]
    value: i32,
}

struct ConnectionWorld {
    sandbox: TestSandbox,
    cluster: Option<TestCluster>,
    metadata: Option<ConnectionMetadata>,
    selected_value: Option<i32>,
    query_error: Option<String>,
    skip_reason: Option<String>,
    bootstrap_error: Option<String>,
}

impl ConnectionWorld {
    fn new() -> Result<Self> {
        Ok(Self {
            sandbox: TestSandbox::new("test-cluster-connection")
                .context("create cluster sandbox")?,
            cluster: None,
            metadata: None,
            selected_value: None,
            query_error: None,
            skip_reason: None,
            bootstrap_error: None,
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
        self.metadata = None;
        self.selected_value = None;
        self.query_error = None;
        self.bootstrap_error = None;
        self.skip_reason = None;
    }

    fn record_bootstrap_error(&mut self, err: impl std::fmt::Display + std::fmt::Debug) {
        let message = err.to_string();
        let debug = format!("{err:?}");
        self.bootstrap_error = Some(message.clone());
        self.cluster = None;
        self.metadata = None;
        self.selected_value = None;
        self.query_error = None;
        if let Some(reason) = cluster_skip_message(&message, Some(&debug)) {
            self.mark_skip(reason);
        }
    }

    fn record_metadata(&mut self, metadata: ConnectionMetadata) {
        self.metadata = Some(metadata);
    }

    const fn record_selected_value(&mut self, value: i32) {
        self.selected_value = Some(value);
    }

    fn record_query_error(&mut self, err: impl Into<String>) {
        self.query_error = Some(err.into());
    }

    fn cluster(&self) -> Result<&TestCluster> {
        self.ensure_not_skipped()?;
        self.cluster
            .as_ref()
            .ok_or_else(|| eyre!("TestCluster was not created"))
    }

    fn metadata(&self) -> Result<&ConnectionMetadata> {
        self.ensure_not_skipped()?;
        self.metadata
            .as_ref()
            .ok_or_else(|| eyre!("connection metadata not captured"))
    }

    fn selected_value(&self) -> Result<i32> {
        self.ensure_not_skipped()?;
        self.selected_value
            .ok_or_else(|| eyre!("no query result recorded"))
    }

    fn query_error(&self) -> Result<&str> {
        self.ensure_not_skipped()?;
        self.query_error
            .as_deref()
            .ok_or_else(|| eyre!("no query error was recorded"))
    }
}

impl Drop for ConnectionWorld {
    fn drop(&mut self) {
        drop(self.cluster.take());
    }
}

type ConnectionWorldFixture = Result<RefCell<ConnectionWorld>>;

fn borrow_world(world: &ConnectionWorldFixture) -> Result<&RefCell<ConnectionWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!(format!("connection world failed to initialise: {err}")))
}

#[fixture]
fn world() -> ConnectionWorldFixture {
    Ok(RefCell::new(ConnectionWorld::new()?))
}

#[given("a sandboxed TestCluster is running")]
fn given_running_cluster(world: &ConnectionWorldFixture) -> Result<()> {
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

#[when("connection metadata is requested")]
fn when_metadata_requested(world: &ConnectionWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    if world_cell.borrow().is_skipped() {
        return Ok(());
    }
    let metadata = world_cell.borrow().cluster()?.connection().metadata();
    world_cell.borrow_mut().record_metadata(metadata);
    Ok(())
}

#[when("a Diesel client executes a simple SELECT")]
fn when_diesel_runs_select(world: &ConnectionWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    if world_cell.borrow().is_skipped() {
        return Ok(());
    }
    let mut connection = world_cell
        .borrow()
        .cluster()?
        .connection()
        .diesel_connection("postgres")
        .map_err(|err| eyre!(err))?;
    let rows: Vec<ValueRow> = diesel::sql_query("SELECT 42 AS value")
        .load(&mut connection)
        .map_err(|err| eyre!(err))?;
    let Some(row) = rows.first() else {
        return Err(eyre!("SELECT returned no rows"));
    };
    world_cell.borrow_mut().record_selected_value(row.value);
    Ok(())
}

#[when("a Diesel client executes a malformed query")]
fn when_diesel_runs_malformed_sql(world: &ConnectionWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;
    if world_cell.borrow().is_skipped() {
        return Ok(());
    }
    let mut connection = world_cell
        .borrow()
        .cluster()?
        .connection()
        .diesel_connection("postgres")
        .map_err(|err| eyre!(err))?;
    // Intentionally misspell SELECT so PostgreSQL raises a syntax error before planning.
    match diesel::sql_query("SELEC 1").execute(&mut connection) {
        Ok(_) => Err(eyre!("malformed query unexpectedly succeeded")),
        Err(err) => {
            world_cell.borrow_mut().record_query_error(err.to_string());
            Ok(())
        }
    }
}

#[then("the metadata matches the sandbox layout")]
fn then_metadata_matches_sandbox(world: &ConnectionWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let metadata = world_ref.metadata()?;
    let settings = world_ref.cluster()?.settings();
    ensure!(
        metadata.host() == settings.host.as_str(),
        "host should match cluster settings"
    );
    ensure!(
        metadata.port() == settings.port,
        "port should match cluster settings"
    );
    ensure!(
        metadata.superuser() == settings.username.as_str(),
        "superuser should match cluster settings"
    );
    let expected_pgpass = world_ref.sandbox.install_dir().join(".pgpass");
    ensure!(
        metadata.pgpass_file() == expected_pgpass.as_path(),
        "pgpass path should live under the sandbox install directory",
    );
    Ok(())
}

#[then("the Diesel helper returns the selected value")]
fn then_select_returns_value(world: &ConnectionWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    ensure!(
        world_ref.selected_value()? == 42,
        "expected SELECT result to be 42"
    );
    Ok(())
}

#[then("the Diesel helper reports the malformed query error")]
fn then_query_error_reported(world: &ConnectionWorldFixture) -> Result<()> {
    let world_ref = borrow_world(world)?.borrow();
    if world_ref.is_skipped() {
        return Ok(());
    }
    let error = world_ref.query_error()?;
    ensure!(
        error.to_ascii_lowercase().contains("syntax"),
        "expected syntax error in Diesel failure, got {error}"
    );
    Ok(())
}

#[scenario(path = "tests/features/test_cluster_connection.feature", index = 0)]
fn scenario_connection_metadata(serial_guard: ScenarioSerialGuard, world: ConnectionWorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "connection metadata world");
}

#[scenario(path = "tests/features/test_cluster_connection.feature", index = 1)]
fn scenario_diesel_query(serial_guard: ScenarioSerialGuard, world: ConnectionWorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "connection Diesel world");
}

#[scenario(path = "tests/features/test_cluster_connection.feature", index = 2)]
fn scenario_diesel_error(serial_guard: ScenarioSerialGuard, world: ConnectionWorldFixture) {
    let _guard = serial_guard;
    let _ = expect_fixture(world, "connection Diesel error world");
}
