//! RAII wrapper that boots an embedded PostgreSQL instance for tests.
//!
//! The cluster starts during [`TestCluster::new`] and shuts down automatically when the
//! value drops out of scope.
//!
//! # Examples
//! ```no_run
//! use pg_embedded_setup_unpriv::TestCluster;
//!
//! # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
//! let cluster = TestCluster::new()?;
//! let url = cluster.settings().url("my_database");
//! // Perform test database work here.
//! drop(cluster); // PostgreSQL stops automatically.
//! # Ok(())
//! # }
//! ```

use crate::env::ScopedEnv;
use crate::error::{BootstrapError, BootstrapResult};
use crate::worker::WorkerPayload;
use crate::{TestBootstrapEnvironment, TestBootstrapSettings};
use crate::{bootstrap::setup_with_env, bootstrap_for_tests};
use color_eyre::eyre::{Context, eyre};
use postgresql_embedded::{PostgreSQL, Settings};
use serde_json::to_writer;
use std::future::Future;
use std::io::Write;
use std::process::Command;
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::runtime::{Builder, Runtime};
use tokio::time;

#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
use nix::unistd::User;
#[cfg(all(
    unix,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ),
))]
use std::os::unix::process::CommandExt;

/// Embedded PostgreSQL instance whose lifecycle follows Rust's drop semantics.
#[derive(Debug)]
pub struct TestCluster {
    runtime: Runtime,
    postgres: Option<PostgreSQL>,
    bootstrap: TestBootstrapSettings,
    managed_via_worker: bool,
    _env_guard: ScopedEnv,
}

impl TestCluster {
    /// Boots a PostgreSQL instance configured by [`bootstrap_for_tests`].
    ///
    /// The constructor blocks until the underlying server process is running and returns an
    /// error when startup fails.
    pub fn new() -> BootstrapResult<Self> {
        let bootstrap = bootstrap_for_tests()?;
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to create Tokio runtime for TestCluster")
            .map_err(BootstrapError::from)?;

        let env_vars = bootstrap.environment.to_env();
        let privileges = bootstrap.privileges;

        if matches!(privileges, crate::ExecutionPrivileges::Unprivileged) {
            setup_with_env(&runtime, &env_vars, &bootstrap.settings)
                .map_err(BootstrapError::from)?;
        }

        let env_guard = ScopedEnv::apply(&env_vars);
        let mut postgres = PostgreSQL::new(bootstrap.settings.clone());

        if matches!(privileges, crate::ExecutionPrivileges::Root) {
            Self::with_privileges(
                &runtime,
                privileges,
                &bootstrap,
                &env_vars,
                WorkerOperation::Setup,
                async { postgres.setup().await },
            )?;
        }

        Self::with_privileges(
            &runtime,
            privileges,
            &bootstrap,
            &env_vars,
            WorkerOperation::Start,
            async { postgres.start().await },
        )?;

        let managed_via_worker = matches!(privileges, crate::ExecutionPrivileges::Root);
        let postgres = if managed_via_worker {
            None
        } else {
            Some(postgres)
        };

        Ok(Self {
            runtime,
            postgres,
            bootstrap,
            managed_via_worker,
            _env_guard: env_guard,
        })
    }

    fn with_privileges<Fut>(
        runtime: &Runtime,
        mode: crate::ExecutionPrivileges,
        bootstrap: &TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        operation: WorkerOperation,
        in_process_op: Fut,
    ) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        match mode {
            crate::ExecutionPrivileges::Unprivileged => {
                Self::block_in_process(runtime, in_process_op, operation.error_context())
            }
            crate::ExecutionPrivileges::Root => {
                Self::run_root_operation(bootstrap, env_vars, operation)
            }
        }
    }

    fn block_in_process<Fut>(
        runtime: &Runtime,
        future: Fut,
        ctx: &'static str,
    ) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        runtime
            .block_on(future)
            .context(ctx)
            .map_err(BootstrapError::from)
    }

    fn run_root_operation(
        bootstrap: &TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        operation: WorkerOperation,
    ) -> BootstrapResult<()> {
        #[cfg(test)]
        {
            if let Some(hook) = run_root_operation_hook()
                .lock()
                .expect("run_root_operation_hook lock poisoned")
                .clone()
            {
                return hook(bootstrap, env_vars, operation);
            }
        }

        match bootstrap.execution_mode {
            crate::ExecutionMode::InProcess => Err(BootstrapError::from(eyre!(
                "ExecutionMode::InProcess cannot be used when running as root"
            ))),
            crate::ExecutionMode::Subprocess => Self::spawn_worker(bootstrap, env_vars, operation),
        }
    }

    fn spawn_worker(
        bootstrap: &TestBootstrapSettings,
        env_vars: &[(String, Option<String>)],
        operation: WorkerOperation,
    ) -> BootstrapResult<()> {
        let worker = bootstrap.worker_binary.as_ref().ok_or_else(|| {
            BootstrapError::from(eyre!(
                "PG_EMBEDDED_WORKER must be set when using ExecutionMode::Subprocess"
            ))
        })?;

        let payload = WorkerPayload::new(&bootstrap.settings, env_vars.to_vec())?;
        let mut file = NamedTempFile::new()
            .context("failed to create worker payload file")
            .map_err(BootstrapError::from)?;
        to_writer(&mut file, &payload)
            .context("failed to serialise worker payload")
            .map_err(BootstrapError::from)?;
        file.flush()
            .context("failed to flush worker payload")
            .map_err(BootstrapError::from)?;
        let temp_path = file.into_temp_path();
        let path_buf = temp_path.to_path_buf();

        let mut command = Command::new(worker.as_std_path());
        command.arg(operation.as_str());
        command.arg(&path_buf);

        #[cfg(all(
            unix,
            any(
                target_os = "linux",
                target_os = "android",
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "dragonfly",
            ),
        ))]
        {
            let user = User::from_name("nobody")
                .context("failed to resolve user 'nobody'")
                .map_err(BootstrapError::from)?
                .ok_or_else(|| BootstrapError::from(eyre!("user 'nobody' not found")))?;
            command.uid(user.uid.as_raw());
            command.gid(user.gid.as_raw());
        }

        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());

        let output = command
            .output()
            .context("failed to execute pg_worker")
            .map_err(BootstrapError::from)?;

        temp_path
            .close()
            .context("failed to clean up worker payload file")
            .map_err(BootstrapError::from)?;

        if output.status.success() {
            Ok(())
        } else {
            Err(BootstrapError::from(eyre!(
                "{}\nstdout: {}\nstderr: {}",
                operation.error_context(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }

    /// Returns the prepared PostgreSQL settings for the running cluster.
    pub fn settings(&self) -> &Settings {
        &self.bootstrap.settings
    }

    /// Returns the environment required for clients to interact with the cluster.
    pub fn environment(&self) -> &TestBootstrapEnvironment {
        &self.bootstrap.environment
    }

    /// Returns the bootstrap metadata captured when the cluster was started.
    pub fn bootstrap(&self) -> &TestBootstrapSettings {
        &self.bootstrap
    }
}

#[derive(Clone, Copy)]
enum WorkerOperation {
    Setup,
    Start,
    Stop,
}

impl WorkerOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Start => "start",
            Self::Stop => "stop",
        }
    }

    fn error_context(self) -> &'static str {
        match self {
            Self::Setup => "postgresql_embedded::setup() failed",
            Self::Start => "postgresql_embedded::start() failed",
            Self::Stop => "postgresql_embedded::stop() failed",
        }
    }
}

impl Drop for TestCluster {
    fn drop(&mut self) {
        if self.managed_via_worker {
            let env_vars = self.bootstrap.environment.to_env();
            if let Err(err) =
                Self::run_root_operation(&self.bootstrap, &env_vars, WorkerOperation::Stop)
            {
                eprintln!("SKIP-TEST-CLUSTER: failed to stop embedded postgres instance: {err}");
            }
            return;
        }

        if let Some(postgres) = self.postgres.take() {
            let outcome = self
                .runtime
                .block_on(async { time::timeout(Duration::from_secs(15), postgres.stop()).await });
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    eprintln!(
                        "SKIP-TEST-CLUSTER: failed to stop embedded postgres instance: {err}"
                    );
                }
                Err(_) => {
                    eprintln!(
                        "SKIP-TEST-CLUSTER: stop() timed out after 15s; proceeding with drop"
                    );
                }
            }
        }
        // `env_guard` drops after this block, restoring the environment.
    }
}

#[cfg(test)]
type RunRootOperationHook = std::sync::Arc<
    dyn Fn(
            &TestBootstrapSettings,
            &[(String, Option<String>)],
            WorkerOperation,
        ) -> BootstrapResult<()>
        + Send
        + Sync,
>;

#[cfg(test)]
static RUN_ROOT_OPERATION_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<RunRootOperationHook>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn run_root_operation_hook() -> &'static std::sync::Mutex<Option<RunRootOperationHook>> {
    RUN_ROOT_OPERATION_HOOK.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
struct HookGuard;

#[cfg(test)]
fn install_run_root_operation_hook<F>(hook: F) -> HookGuard
where
    F: Fn(
            &TestBootstrapSettings,
            &[(String, Option<String>)],
            WorkerOperation,
        ) -> BootstrapResult<()>
        + Send
        + Sync
        + 'static,
{
    let slot = run_root_operation_hook();
    {
        let mut guard = slot.lock().expect("run_root_operation_hook lock poisoned");
        assert!(guard.is_none(), "run_root_operation_hook already installed");
        *guard = Some(std::sync::Arc::new(hook));
    }
    HookGuard
}

#[cfg(test)]
impl Drop for HookGuard {
    fn drop(&mut self) {
        let slot = run_root_operation_hook();
        let mut guard = slot.lock().expect("run_root_operation_hook lock poisoned");
        guard.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    fn test_runtime() -> Runtime {
        Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("create test runtime")
    }

    fn dummy_environment() -> TestBootstrapEnvironment {
        TestBootstrapEnvironment {
            home: Utf8PathBuf::from("/tmp/pg-home"),
            xdg_cache_home: Utf8PathBuf::from("/tmp/pg-cache"),
            xdg_runtime_dir: Utf8PathBuf::from("/tmp/pg-run"),
            pgpass_file: Utf8PathBuf::from("/tmp/.pgpass"),
            tz_dir: Some(Utf8PathBuf::from("/usr/share/zoneinfo")),
            timezone: "UTC".into(),
        }
    }

    fn dummy_settings(privileges: crate::ExecutionPrivileges) -> TestBootstrapSettings {
        TestBootstrapSettings {
            privileges,
            execution_mode: crate::ExecutionMode::Subprocess,
            settings: Settings::default(),
            environment: dummy_environment(),
            worker_binary: None,
        }
    }

    #[test]
    fn unprivileged_operations_run_in_process() {
        let runtime = test_runtime();
        let bootstrap = dummy_settings(crate::ExecutionPrivileges::Unprivileged);
        let env_vars = bootstrap.environment.to_env();
        let setup_calls = AtomicUsize::new(0);

        for operation in [WorkerOperation::Setup, WorkerOperation::Start] {
            let call_counter = &setup_calls;
            TestCluster::with_privileges(
                &runtime,
                crate::ExecutionPrivileges::Unprivileged,
                &bootstrap,
                &env_vars,
                operation,
                async move {
                    call_counter.fetch_add(1, Ordering::SeqCst);
                    Ok::<(), postgresql_embedded::Error>(())
                },
            )
            .expect("in-process operation should succeed");
        }

        assert_eq!(setup_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn root_operations_delegate_to_worker() {
        let runtime = test_runtime();
        let bootstrap = dummy_settings(crate::ExecutionPrivileges::Root);
        let env_vars = bootstrap.environment.to_env();
        let worker_calls = Arc::new(AtomicUsize::new(0));
        let in_process_invoked = Arc::new(AtomicBool::new(false));

        let hook_calls = Arc::clone(&worker_calls);
        let _guard = install_run_root_operation_hook(move |_, _, _| {
            hook_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });

        for operation in [WorkerOperation::Setup, WorkerOperation::Start] {
            let flag = Arc::clone(&in_process_invoked);
            TestCluster::with_privileges(
                &runtime,
                crate::ExecutionPrivileges::Root,
                &bootstrap,
                &env_vars,
                operation,
                async move {
                    flag.store(true, Ordering::SeqCst);
                    Ok::<(), postgresql_embedded::Error>(())
                },
            )
            .expect("worker operation should succeed");
        }

        assert_eq!(worker_calls.load(Ordering::SeqCst), 2);
        assert!(!in_process_invoked.load(Ordering::SeqCst));
    }
}
