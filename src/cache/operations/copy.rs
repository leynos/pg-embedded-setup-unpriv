//! File and directory copy operations for the binary cache.
//!
//! Provides recursive directory copying with permission preservation.

use camino::Utf8Path;
use color_eyre::eyre::Context;
use std::fs;
use std::io;
use std::path::Path;
use tracing::debug;

use crate::error::BootstrapResult;

/// Observability target for cache operations.
const LOG_TARGET: &str = "pg_embed::cache";

/// Copies cached binaries to the target installation directory.
///
/// Performs a recursive copy of the source directory contents to the target,
/// preserving directory structure and file permissions.
///
/// # Arguments
///
/// * `source` - Source directory containing cached binaries
/// * `target` - Target installation directory
///
/// # Errors
///
/// Returns an error if:
/// - The source directory does not exist or cannot be read
/// - The target directory cannot be created
/// - Any file copy operation fails
///
/// # Examples
///
/// ```no_run
/// use camino::Utf8Path;
/// use pg_embedded_setup_unpriv::cache::copy_from_cache;
///
/// let source = Utf8Path::new("/home/user/.cache/pg-embedded/binaries/17.4.0");
/// let target = Utf8Path::new("/tmp/sandbox/install");
/// copy_from_cache(source, target)?;
/// # Ok::<(), color_eyre::Report>(())
/// ```
pub fn copy_from_cache(source: &Utf8Path, target: &Utf8Path) -> BootstrapResult<()> {
    log_copy_start(source, target);

    fs::create_dir_all(target)
        .with_context(|| format!("failed to create target directory for cache copy: {target}"))?;

    copy_dir_recursive(source.as_std_path(), target.as_std_path())
        .with_context(|| format!("failed to copy cached binaries from {source} to {target}"))?;

    log_copy_complete(source, target);
    Ok(())
}

/// Logs the start of a cache copy operation.
fn log_copy_start(source: &Utf8Path, target: &Utf8Path) {
    debug!(
        target: LOG_TARGET,
        source = %source,
        target = %target,
        "copying binaries from cache"
    );
}

/// Logs the completion of a cache copy operation.
fn log_copy_complete(source: &Utf8Path, target: &Utf8Path) {
    debug!(
        target: LOG_TARGET,
        source = %source,
        target = %target,
        "cache copy completed"
    );
}

/// Recursively copies a directory and its contents.
///
/// Preserves directory structure and copies file metadata where possible.
pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }

    for dir_entry in fs::read_dir(src)? {
        let entry = dir_entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_symlink() {
            copy_symlink(&src_path, &dst_path)?;
        } else {
            copy_file_with_permissions(&src_path, &dst_path)?;
        }
    }

    copy_permissions(src, dst);

    Ok(())
}

/// Copies a file preserving its permissions.
fn copy_file_with_permissions(src: &Path, dst: &Path) -> io::Result<()> {
    fs::copy(src, dst)?;
    copy_permissions(src, dst);
    Ok(())
}

/// Best-effort permission copy from source to destination.
fn copy_permissions(src: &Path, dst: &Path) {
    let Ok(metadata) = fs::metadata(src) else {
        return;
    };
    if let Err(err) = fs::set_permissions(dst, metadata.permissions()) {
        log_permission_copy_failure(src, dst, &err);
    }
}

/// Logs a permission copy failure for debugging.
fn log_permission_copy_failure(src: &Path, dst: &Path, err: &io::Error) {
    debug!(
        target: LOG_TARGET,
        src = %src.display(),
        dst = %dst.display(),
        error = %err,
        "failed to copy permissions (best effort)"
    );
}

/// Copies a symbolic link.
#[cfg(unix)]
fn copy_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    let target = fs::read_link(src)?;
    std::os::unix::fs::symlink(&target, dst)?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    // On non-Unix, fall back to copying the target file
    if src.is_file() {
        fs::copy(src, dst)?;
    }
    Ok(())
}
