//! Helper functions for database lifecycle behavioural tests.

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use color_eyre::eyre::{Context, Result, ensure, eyre};
use pg_embedded_setup_unpriv::{TemporaryDatabase, TestCluster, find_timezone_dir};

use super::cluster_skip::cluster_skip_message;
use super::env::ScopedEnvVars;
use super::sandbox::TestSandbox;

const BOOTSTRAP_RETRY_ATTEMPTS: usize = 3;
const BOOTSTRAP_RETRY_DELAY: Duration = Duration::from_millis(250);

struct BootstrapOutcome {
    cluster: Option<TestCluster>,
    error: Option<pg_embedded_setup_unpriv::BootstrapError>,
}

impl BootstrapOutcome {
    const fn success(cluster: TestCluster) -> Self {
        Self {
            cluster: Some(cluster),
            error: None,
        }
    }

    const fn failure(error: Option<pg_embedded_setup_unpriv::BootstrapError>) -> Self {
        Self {
            cluster: None,
            error,
        }
    }
}

/// Global counter for tracking setup function invocations across scenarios.
///
/// # Safety
///
/// Correctness requires that all scenarios hold the `serial_guard` fixture,
/// ensuring serial execution and preventing concurrent increments.
pub static SETUP_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

/// World state for database lifecycle scenarios.
pub struct DatabaseWorld {
    pub sandbox: TestSandbox,
    pub cluster: Option<TestCluster>,
    pub create_error: Option<String>,
    pub drop_error: Option<String>,
    pub skip_reason: Option<String>,
    pub bootstrap_error: Option<String>,
    pub temp_database: Option<TemporaryDatabase>,
    pub setup_call_count_at_start: usize,
}

impl DatabaseWorld {
    /// Creates a new `DatabaseWorld` with default state.
    ///
    /// # Errors
    ///
    /// Returns an error if sandbox creation fails.
    pub fn new() -> Result<Self> {
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

    /// Marks the scenario as skipped with the given reason.
    pub fn mark_skip(&mut self, reason: impl Into<String>) {
        let message = reason.into();
        tracing::warn!("{message}");
        self.skip_reason = Some(message);
    }

    /// Returns whether the scenario is skipped.
    #[must_use]
    pub const fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
    }

    /// Returns an error if the scenario is skipped.
    ///
    /// # Errors
    ///
    /// Returns an error if the scenario is marked as skipped.
    pub fn ensure_not_skipped(&self) -> Result<()> {
        if self.is_skipped() {
            Err(eyre!("scenario skipped"))
        } else {
            Ok(())
        }
    }

    /// Records a successful cluster creation.
    pub fn record_cluster(&mut self, cluster: TestCluster) {
        self.cluster = Some(cluster);
        self.create_error = None;
        self.drop_error = None;
        self.bootstrap_error = None;
        self.skip_reason = None;
        self.temp_database = None;
        self.setup_call_count_at_start = SETUP_CALL_COUNT.load(Ordering::SeqCst);
    }

    /// Records a bootstrap error, optionally marking the scenario as skipped.
    pub fn record_bootstrap_error(&mut self, err: impl std::fmt::Display + std::fmt::Debug) {
        let message = err.to_string();
        let debug = format!("{err:?}");
        self.bootstrap_error = Some(message.clone());
        self.cluster = None;
        if let Some(reason) = cluster_skip_message(&message, Some(&debug)) {
            self.mark_skip(reason);
        }
    }

    /// Returns a reference to the cluster, or an error if not created.
    ///
    /// # Errors
    ///
    /// Returns an error if the scenario is skipped or no cluster was created.
    pub fn cluster(&self) -> Result<&TestCluster> {
        self.ensure_not_skipped()?;
        self.cluster.as_ref().map_or_else(
            || {
                let message = self.bootstrap_error.as_deref().map_or_else(
                    || "TestCluster was not created".to_owned(),
                    |err| format!("TestCluster was not created: {err}"),
                );
                Err(eyre!(message))
            },
            Ok,
        )
    }
}

/// Type alias for the world fixture result.
pub type DatabaseWorldFixture = Result<RefCell<DatabaseWorld>>;

/// Borrows the world from the fixture result.
///
/// # Errors
///
/// Returns an error if the world failed to initialize.
pub fn borrow_world(world: &DatabaseWorldFixture) -> Result<&RefCell<DatabaseWorld>> {
    world
        .as_ref()
        .map_err(|err| eyre!(format!("database world failed to initialise: {err}")))
}

/// Execute a database operation, capturing any error in the specified field.
///
/// If `is_create` is true, errors are stored in `create_error`; otherwise in `drop_error`.
///
/// # Errors
///
/// Returns an error if the world failed to initialize.
pub fn execute_db_op<F>(world: &DatabaseWorldFixture, op: F, is_create: bool) -> Result<()>
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
///
/// # Errors
///
/// Returns an error if the check fails or state doesn't match expectations.
pub fn check_db_exists(world: &DatabaseWorldFixture, db_name: &str, expected: bool) -> Result<()> {
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
///
/// # Errors
///
/// Returns an error if the check fails or state doesn't match expectations.
pub fn check_db_exists_via_delegation(
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
///
/// # Errors
///
/// Returns an error if no error was recorded or patterns don't match.
pub fn verify_error(
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

/// Sets up environment variables for a sandboxed cluster and creates it.
///
/// # Errors
///
/// Returns an error if environment setup or cluster creation fails.
pub fn setup_sandboxed_cluster(world: &DatabaseWorldFixture) -> Result<()> {
    let world_cell = borrow_world(world)?;

    let vars = build_sandbox_env(world_cell);

    let outcome = bootstrap_cluster_with_retry(world_cell, &vars)?;
    if let Some(cluster) = outcome.cluster {
        world_cell.borrow_mut().record_cluster(cluster);
    }
    if let Some(err) = outcome.error {
        world_cell.borrow_mut().record_bootstrap_error(err);
    }
    Ok(())
}

fn build_sandbox_env(world_cell: &RefCell<DatabaseWorld>) -> ScopedEnvVars {
    let world_ref = world_cell.borrow();
    let mut vars = world_ref.sandbox.env_without_timezone();
    if let Some(tzdir) = find_timezone_dir() {
        override_env_value(&mut vars, "TZDIR", tzdir.as_str());
    }
    override_env_value(&mut vars, "TZ", "UTC");
    vars
}

fn bootstrap_cluster_with_retry(
    world_cell: &RefCell<DatabaseWorld>,
    vars: &ScopedEnvVars,
) -> Result<BootstrapOutcome> {
    let mut last_error = None;
    for attempt in 0..BOOTSTRAP_RETRY_ATTEMPTS {
        if let Some(cluster) = try_bootstrap_attempt(world_cell, vars, attempt, &mut last_error)? {
            return Ok(BootstrapOutcome::success(cluster));
        }
    }

    Ok(BootstrapOutcome::failure(last_error))
}

fn reset_sandbox(world_cell: &RefCell<DatabaseWorld>) -> Result<()> {
    let world_ref = world_cell.borrow();
    world_ref.sandbox.reset()
}

fn try_bootstrap_attempt(
    world_cell: &RefCell<DatabaseWorld>,
    vars: &ScopedEnvVars,
    attempt: usize,
    last_error: &mut Option<pg_embedded_setup_unpriv::BootstrapError>,
) -> Result<Option<TestCluster>> {
    reset_sandbox(world_cell)?;
    let result = {
        let world_ref = world_cell.borrow();
        world_ref.sandbox.with_env(vars.clone(), TestCluster::new)
    };

    match result {
        Ok(cluster) => Ok(Some(cluster)),
        Err(err) => {
            if attempt + 1 < BOOTSTRAP_RETRY_ATTEMPTS {
                log_bootstrap_retry(attempt, &err);
                std::thread::sleep(BOOTSTRAP_RETRY_DELAY);
            }
            *last_error = Some(err);
            Ok(None)
        }
    }
}

fn log_bootstrap_retry(attempt: usize, err: &pg_embedded_setup_unpriv::BootstrapError) {
    tracing::trace!(
        attempt = attempt + 1,
        total_attempts = BOOTSTRAP_RETRY_ATTEMPTS,
        error = %err,
        delay_ms = BOOTSTRAP_RETRY_DELAY.as_millis(),
        "bootstrap failed; sleeping before retry"
    );
}

fn override_env_value(vars: &mut ScopedEnvVars, key: &str, value: &str) {
    let key_ref = OsStr::new(key);
    let value_os = Some(OsString::from(value));
    if let Some((_, existing_value)) = vars
        .iter_mut()
        .find(|(candidate, _)| candidate.as_os_str() == key_ref)
    {
        *existing_value = value_os;
    } else {
        vars.push((key_ref.to_os_string(), value_os));
    }
}
