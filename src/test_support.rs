//! Internal helpers re-exported for integration tests.
//!
//! Besides filesystem convenience wrappers, this module exposes the
//! `RUN_ROOT_OPERATION_HOOK` plumbing so behavioural tests can intercept and
//! inspect privileged worker operations. [`install_run_root_operation_hook`]
//! registers a closure for the duration of a [`HookGuard`], ensuring
//! `TestCluster` calls are observable without leaking state across suites.
use camino::{Utf8Path, Utf8PathBuf};
use cap_std::{
    ambient_authority,
    fs::{Dir, Metadata},
};
use color_eyre::eyre::{Context, Report, Result};
#[cfg(any(test, feature = "cluster-unit-tests"))]
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(any(test, feature = "cluster-unit-tests"))]
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(any(test, feature = "cluster-unit-tests"))]
use crate::cluster::{TestCluster, WorkerOperation};
#[cfg(any(test, feature = "cluster-unit-tests"))]
use crate::error::BootstrapResult;
use crate::error::{BootstrapError, Error, PrivilegeError};
use crate::fs;
#[cfg(any(test, feature = "cluster-unit-tests"))]
use crate::{ExecutionPrivileges, TestBootstrapSettings};

/// Opens the ambient directory containing `path` and returns its relative component.
///
/// # Examples
/// ```no_run
/// # use camino::Utf8Path;
/// # use color_eyre::eyre::Result;
/// # use pg_embedded_setup_unpriv::test_support::ambient_dir_and_path;
/// # fn main() -> Result<()> {
/// let (_dir, relative) = ambient_dir_and_path(Utf8Path::new("."))?;
/// assert_eq!(relative.as_str(), ".");
///
/// let (_root, root_rel) = ambient_dir_and_path(Utf8Path::new("/"))?;
/// assert!(root_rel.as_str().is_empty());
/// # Ok(())
/// # }
/// ```
pub fn ambient_dir_and_path(path: &Utf8Path) -> Result<(Dir, Utf8PathBuf)> {
    fs::ambient_dir_and_path(path)
}

/// Ensures the provided directory exists, creating intermediate components when missing.
///
/// # Examples
/// ```no_run
/// # use camino::Utf8Path;
/// # use color_eyre::eyre::Result;
/// # use pg_embedded_setup_unpriv::test_support::ensure_dir_exists;
/// # fn main() -> Result<()> {
/// ensure_dir_exists(Utf8Path::new("./target/tmp/cache"))?;
/// # Ok(())
/// # }
/// ```
pub fn ensure_dir_exists(path: &Utf8Path) -> Result<()> {
    fs::ensure_dir_exists(path)
}

/// Applies POSIX permissions to the provided path when it already exists.
///
/// # Examples
/// ```no_run
/// # use camino::Utf8Path;
/// # use color_eyre::eyre::Result;
/// # use pg_embedded_setup_unpriv::test_support::set_permissions;
/// # fn main() -> Result<()> {
/// set_permissions(Utf8Path::new("./target/tmp/cache"), 0o755)?;
/// # Ok(())
/// # }
/// ```
pub fn set_permissions(path: &Utf8Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, mode)
}

#[cfg(any(test, feature = "cluster-unit-tests"))]
#[doc(hidden)]
/// Signature for intercepting privileged worker operations triggered by `TestCluster`.
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::test_support::RunRootOperationHook;
///
/// fn installs_hook(hook: RunRootOperationHook) {
///     let _ = hook;
/// }
/// ```
pub type RunRootOperationHook = Arc<
    dyn Fn(
            &TestBootstrapSettings,
            &[(String, Option<String>)],
            WorkerOperation,
        ) -> BootstrapResult<()>
        + Send
        + Sync,
>;

#[cfg(any(test, feature = "cluster-unit-tests"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[doc(hidden)]
pub enum RunRootOperationHookInstallError {
    #[error("run_root_operation_hook already installed")]
    AlreadyInstalled,
}

#[cfg(any(test, feature = "cluster-unit-tests"))]
static RUN_ROOT_OPERATION_HOOK: OnceLock<Mutex<Option<RunRootOperationHook>>> = OnceLock::new();

#[cfg(any(test, feature = "cluster-unit-tests"))]
#[doc(hidden)]
/// Retrieves the optional run-root-operation hook for inspection or mutation.
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::test_support::{
///     install_run_root_operation_hook,
///     run_root_operation_hook,
/// };
///
/// let guard = install_run_root_operation_hook(|_, _, _| Ok(()))
///     .expect("hook should install");
/// assert!(
///     run_root_operation_hook()
///         .lock()
///         .expect("hook mutex poisoned")
///         .is_some()
/// );
/// drop(guard);
/// ```
pub fn run_root_operation_hook() -> &'static Mutex<Option<RunRootOperationHook>> {
    RUN_ROOT_OPERATION_HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(any(test, feature = "cluster-unit-tests"))]
/// Guard that removes the installed run-root-operation hook when dropped.
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::test_support::install_run_root_operation_hook;
///
/// let guard = install_run_root_operation_hook(|_, _, _| Ok(()))
///     .expect("hook should install");
/// drop(guard); // hook removed automatically
/// ```
pub struct HookGuard;

#[cfg(any(test, feature = "cluster-unit-tests"))]
/// Installs a hook that observes privileged worker operations triggered by `TestCluster`.
///
/// The hook remains active until the returned [`HookGuard`] is dropped.
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::test_support::install_run_root_operation_hook;
///
/// let guard = install_run_root_operation_hook(|_, _, _| Ok(()))
///     .expect("hook should install");
/// drop(guard);
/// ```
pub fn install_run_root_operation_hook<F>(
    hook: F,
) -> Result<HookGuard, RunRootOperationHookInstallError>
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
        if guard.is_some() {
            return Err(RunRootOperationHookInstallError::AlreadyInstalled);
        }
        *guard = Some(Arc::new(hook));
    }
    Ok(HookGuard)
}

#[cfg(any(test, feature = "cluster-unit-tests"))]
impl Drop for HookGuard {
    fn drop(&mut self) {
        let slot = run_root_operation_hook();
        let mut guard = slot.lock().expect("run_root_operation_hook lock poisoned");
        guard.take();
    }
}

#[cfg(any(test, feature = "cluster-unit-tests"))]
#[doc(hidden)]
pub fn invoke_with_privileges<Fut>(
    runtime: &tokio::runtime::Runtime,
    privileges: ExecutionPrivileges,
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    operation: WorkerOperation,
    in_process_op: Fut,
) -> BootstrapResult<()>
where
    Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
{
    TestCluster::with_privileges(
        runtime,
        privileges,
        bootstrap,
        env_vars,
        operation,
        in_process_op,
    )
}

/// Retrieves metadata for the provided path using capability APIs.
pub fn metadata(path: &Utf8Path) -> std::io::Result<Metadata> {
    let (dir, relative) =
        ambient_dir_and_path(path).map_err(|err| std::io::Error::other(err.to_string()))?;
    if relative.as_str().is_empty() {
        dir.dir_metadata()
    } else {
        dir.metadata(relative.as_std_path())
    }
}

/// Converts a bootstrap error report into the library's public [`Error`] type.
///
/// # Examples
/// ```
/// # use color_eyre::Report;
/// # use pg_embedded_setup_unpriv::Error;
/// use pg_embedded_setup_unpriv::test_support::bootstrap_error;
///
/// let err = bootstrap_error(Report::msg("bootstrap failed"));
/// assert!(matches!(err, Error::Bootstrap(_)));
/// ```
pub fn bootstrap_error(err: Report) -> Error {
    Error::Bootstrap(BootstrapError::from(err))
}

/// Converts a privilege-related report into the library's public [`Error`] type.
///
/// # Examples
/// ```
/// # use color_eyre::Report;
/// # use pg_embedded_setup_unpriv::Error;
/// use pg_embedded_setup_unpriv::test_support::privilege_error;
///
/// let err = privilege_error(Report::msg("missing capability"));
/// assert!(matches!(err, Error::Privilege(_)));
/// ```
pub fn privilege_error(err: Report) -> Error {
    Error::Privilege(PrivilegeError::from(err))
}

/// Capability-aware temporary directory that exposes both a [`Dir`] handle and the UTF-8 path.
#[derive(Debug)]
pub struct CapabilityTempDir {
    dir: Option<Dir>,
    path: Utf8PathBuf,
}

impl CapabilityTempDir {
    /// Creates a new temporary directory rooted under the system temporary location.
    pub fn new(prefix: &str) -> Result<Self> {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        let system_tmp = std::env::temp_dir();
        let system_tmp = Utf8PathBuf::try_from(system_tmp)
            .map_err(|_| color_eyre::eyre::eyre!("system temp dir is not valid UTF-8"))?;
        let ambient = Dir::open_ambient_dir(system_tmp.as_std_path(), ambient_authority())
            .context("open ambient temp directory")?;

        let pid = std::process::id();
        let epoch_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();

        for attempt in 0..32 {
            let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
            let name = format!("{}-{}-{}-{}", prefix, pid, epoch_ns, counter + attempt);
            match ambient.create_dir(&name) {
                Ok(()) => {
                    let dir = ambient.open_dir(&name).context("open capability tempdir")?;
                    let path = system_tmp.join(&name);
                    return Ok(Self {
                        dir: Some(dir),
                        path,
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(err).with_context(|| format!("create capability tempdir {name}"));
                }
            }
        }

        Err(color_eyre::eyre::eyre!(
            "exhausted attempts creating capability tempdir"
        ))
    }

    /// Returns the UTF-8 path to the temporary directory.
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }
}

impl Drop for CapabilityTempDir {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            match dir.remove_open_dir_all() {
                Ok(()) => {}
                Err(err) => {
                    eprintln!("SKIP-CAP-TEMPDIR: failed to remove {}: {err}", self.path);
                }
            }
        }
    }
}
