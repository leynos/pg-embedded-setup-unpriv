//! Directory removal helpers for the `pg_worker` binary.

use camino::Utf8Path;
use tracing::info;

use super::cleanup_helpers::{RemovalOutcome, try_remove_dir_all};

pub(super) fn remove_dir_all_if_exists(path: &Utf8Path, label: &str) -> Result<(), String> {
    match try_remove_dir_all(path.as_std_path()) {
        Ok(outcome) => {
            log_removal_outcome(outcome, path, label);
            Ok(())
        }
        Err(err) => Err(format!(
            "failed to remove {label} directory {}: {err}",
            path.as_str()
        )),
    }
}

fn log_removal_outcome(outcome: RemovalOutcome, path: &Utf8Path, label: &str) {
    match outcome {
        RemovalOutcome::Removed => log_dir_removed(path, label),
        RemovalOutcome::Missing => log_dir_missing(path, label),
    }
}

fn log_dir_removed(path: &Utf8Path, label: &str) {
    info!(path = %path, label, "removed postgres directory");
}

fn log_dir_missing(path: &Utf8Path, label: &str) {
    info!(path = %path, label, "postgres directory already removed");
}
