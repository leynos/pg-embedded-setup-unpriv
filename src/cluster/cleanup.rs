//! Cleanup helpers for `TestCluster` shutdown.

use crate::cleanup_helpers::{RemovalOutcome, try_remove_dir_all};
use crate::observability::LOG_TARGET;
use crate::{CleanupMode, TestBootstrapSettings};
use postgresql_embedded::Settings;
use std::error::Error;
use std::path::Path;

use super::worker_invoker::WorkerInvoker as ClusterWorkerInvoker;
use super::worker_operation;

#[derive(Debug, Clone, Copy)]
enum DirectoryLabel {
    Data,
    Installation,
    InstallationRoot,
}

impl DirectoryLabel {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Installation => "installation",
            Self::InstallationRoot => "installation-root",
        }
    }
}

pub(super) fn cleanup_worker_managed_with_runtime(
    runtime: &tokio::runtime::Runtime,
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    context: &str,
) {
    let Some(operation) = cleanup_operation(bootstrap.cleanup_mode) else {
        return;
    };
    tracing::info!(
        target: LOG_TARGET,
        context = %context,
        operation = operation.as_str(),
        "cleaning up postgres directories via worker"
    );
    let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
    if let Err(err) = invoker.invoke_as_root(operation) {
        warn_cleanup_failure(context, operation, &err);
    }
}

pub(super) fn cleanup_in_process(cleanup_mode: CleanupMode, settings: &Settings, context: &str) {
    if cleanup_mode == CleanupMode::None {
        return;
    }
    log_cleanup_start(cleanup_mode, context);
    cleanup_data_dir(cleanup_mode, settings, context);
    cleanup_install_dir(cleanup_mode, settings, context);
}

fn log_cleanup_start(cleanup_mode: CleanupMode, context: &str) {
    tracing::info!(
        target: LOG_TARGET,
        context = %context,
        cleanup_mode = ?cleanup_mode,
        "cleaning up postgres directories"
    );
}

fn cleanup_data_dir(cleanup_mode: CleanupMode, settings: &Settings, context: &str) {
    if should_remove_data(cleanup_mode) {
        remove_dir_all_if_exists(&settings.data_dir, DirectoryLabel::Data, context);
    }
}

fn cleanup_install_dir(cleanup_mode: CleanupMode, settings: &Settings, context: &str) {
    if should_remove_install(cleanup_mode) {
        remove_dir_all_if_exists(
            &settings.installation_dir,
            DirectoryLabel::Installation,
            context,
        );
        if let Some(parent) = settings.password_file.parent() {
            if parent != settings.installation_dir.as_path()
                && parent.starts_with(&settings.installation_dir)
            {
                remove_dir_all_if_exists(parent, DirectoryLabel::InstallationRoot, context);
            }
        }
    }
}

const fn should_remove_data(cleanup_mode: CleanupMode) -> bool {
    matches!(cleanup_mode, CleanupMode::DataOnly | CleanupMode::Full)
}

const fn should_remove_install(cleanup_mode: CleanupMode) -> bool {
    matches!(cleanup_mode, CleanupMode::Full)
}

const fn cleanup_operation(cleanup_mode: CleanupMode) -> Option<worker_operation::WorkerOperation> {
    match cleanup_mode {
        CleanupMode::DataOnly => Some(worker_operation::WorkerOperation::Cleanup),
        CleanupMode::Full => Some(worker_operation::WorkerOperation::CleanupFull),
        CleanupMode::None => None,
    }
}

fn remove_dir_all_if_exists(path: &Path, label: DirectoryLabel, context: &str) {
    match try_remove_dir_all(path) {
        Ok(outcome) => log_removal_outcome(outcome, path, label, context),
        Err(err) => warn_cleanup_removal_failure(context, label, path, &err),
    }
}

fn log_removal_outcome(outcome: RemovalOutcome, path: &Path, label: DirectoryLabel, context: &str) {
    match outcome {
        RemovalOutcome::Removed => log_dir_removed(path, label, context),
        RemovalOutcome::Missing => log_dir_missing(path, label, context),
    }
}

fn log_dir_removed(path: &Path, label: DirectoryLabel, context: &str) {
    tracing::info!(
        target: LOG_TARGET,
        context = %context,
        path = %path.display(),
        label = label.as_str(),
        "removed postgres directory"
    );
}

fn log_dir_missing(path: &Path, label: DirectoryLabel, context: &str) {
    tracing::debug!(
        target: LOG_TARGET,
        context = %context,
        path = %path.display(),
        label = label.as_str(),
        "postgres directory already removed"
    );
}

fn warn_cleanup_failure(
    context: &str,
    operation: worker_operation::WorkerOperation,
    err: &dyn Error,
) {
    tracing::warn!(
        "SKIP-TEST-CLUSTER: failed to clean up postgres directories ({} via {}): {}",
        context,
        operation.as_str(),
        err
    );
}

fn warn_cleanup_removal_failure(
    context: &str,
    label: DirectoryLabel,
    path: &Path,
    err: &dyn Error,
) {
    tracing::warn!(
        "SKIP-TEST-CLUSTER: failed to remove {} directory {} ({context}): {err}",
        label.as_str(),
        path.display()
    );
}

#[cfg(test)]
mod tests {
    use super::cleanup_in_process;
    use crate::CleanupMode;
    use postgresql_embedded::Settings;
    use rstest::rstest;
    use std::fs;
    use tempfile::tempdir;

    #[rstest]
    #[case::data_only(CleanupMode::DataOnly, false, true)]
    #[case::full(CleanupMode::Full, false, false)]
    #[case::none(CleanupMode::None, true, true)]
    fn cleanup_in_process_respects_mode(
        #[case] mode: CleanupMode,
        #[case] expect_data_exists: bool,
        #[case] expect_install_exists: bool,
    ) {
        let sandbox = tempdir().expect("tempdir");
        let data_dir = sandbox.path().join("data");
        let install_dir = sandbox.path().join("install");
        fs::create_dir_all(&data_dir).expect("create data dir");
        fs::create_dir_all(&install_dir).expect("create install dir");
        fs::write(data_dir.join("marker"), b"data").expect("write data marker");
        fs::write(install_dir.join("marker"), b"install").expect("write install marker");

        let settings = Settings {
            data_dir,
            installation_dir: install_dir,
            ..Settings::default()
        };

        cleanup_in_process(mode, &settings, "cleanup-test");
        cleanup_in_process(mode, &settings, "cleanup-test");

        assert_eq!(
            settings.data_dir.exists(),
            expect_data_exists,
            "data directory presence should match cleanup mode",
        );
        assert_eq!(
            settings.installation_dir.exists(),
            expect_install_exists,
            "installation directory presence should match cleanup mode",
        );
    }
}
