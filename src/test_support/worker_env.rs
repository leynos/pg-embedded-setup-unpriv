//! Resolves and stages worker binaries for privileged test runs.

use std::ffi::OsString;
use std::sync::OnceLock;

#[cfg(unix)]
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::{fs, io};

/// Returns the worker binary path staged for privileged test execution.
///
/// The path is resolved once per process and, on Unix, staged into a
/// world-executable location so privilege-dropped subprocesses can launch it.
///
/// # Examples
///
/// ```no_run
/// use pg_embedded_setup_unpriv::test_support::worker_binary_for_tests;
///
/// let worker = worker_binary_for_tests();
/// # let _ = worker;
/// ```
#[must_use]
pub fn worker_binary_for_tests() -> Option<OsString> {
    worker_binary()
}

pub(super) fn worker_binary() -> Option<OsString> {
    static WORKER_PATH: OnceLock<Option<OsString>> = OnceLock::new();
    WORKER_PATH
        .get_or_init(|| {
            let original =
                std::env::var_os("CARGO_BIN_EXE_pg_worker").or_else(locate_worker_binary)?;
            Some(stage_worker_binary(original))
        })
        .clone()
}

fn stage_worker_binary(original: OsString) -> OsString {
    #[cfg(unix)]
    if let Ok(path) = try_stage_worker_binary(&original) {
        return path;
    }

    original
}

/// Stages the worker binary to the system temporary directory for accessibility by
/// privilege-dropped processes.
///
/// The binary is staged to `{temp_dir}/pg-worker-{profile}-{hash}/pg_worker` where the hash
/// ensures uniqueness per source path. A pointer file is written to `target/{profile}/`
/// for discoverability and cleanup.
#[cfg(unix)]
fn try_stage_worker_binary(original: &OsString) -> io::Result<OsString> {
    let source = PathBuf::from(original);
    let filename = source.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "worker path missing filename")
    })?;

    // Compute staging directory in the system temporary directory and find target
    // directory for the pointer file.
    let (staged_dir, target_profile_dir) = find_staging_directory(&source);

    // Security: Create staging directory with validation against symlink attacks
    create_staging_directory_secure(&staged_dir)?;

    let staged = staged_dir.join(filename);

    // Only copy if source is newer than staged (or staged doesn't exist).
    if should_restage(&source, &staged)? {
        fs::copy(&source, &staged)?;
        let mut perms = fs::metadata(&staged)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&staged, perms)?;
    }

    // Write pointer file to target directory for discoverability and cleanup.
    // Errors are intentionally ignored - the pointer is optional for cleanup convenience.
    if let Some(target_dir) = target_profile_dir {
        drop(write_pointer_file(&target_dir, &staged));
    }

    Ok(staged.into_os_string())
}

/// Creates the staging directory securely, protecting against symlink attacks.
///
/// Validates that:
/// 1. If the path already exists, it is not a symlink
/// 2. If the path already exists, it is owned by the current user
/// 3. After creation, the directory is not a symlink and is owned by current user
#[cfg(unix)]
fn create_staging_directory_secure(staged_dir: &PathBuf) -> io::Result<()> {
    use nix::unistd::geteuid;
    use std::os::unix::fs::MetadataExt;

    let current_uid = geteuid().as_raw();

    // Check if path already exists
    if let Ok(meta) = fs::symlink_metadata(staged_dir) {
        // Reject symlinks or non-directories - could be attacker-controlled
        if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "staging path is not a real directory: {}",
                    staged_dir.display()
                ),
            ));
        }

        // Reject directories not owned by current user
        if meta.uid() != current_uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "staging directory owned by uid {} (expected {}): {}",
                    meta.uid(),
                    current_uid,
                    staged_dir.display()
                ),
            ));
        }
    } else {
        // Create directory with restrictive permissions first
        fs::create_dir_all(staged_dir)?;
    }

    // Re-validate after creation to protect against TOCTOU
    let meta = fs::symlink_metadata(staged_dir)?;
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "staging path became non-directory after creation: {}",
                staged_dir.display()
            ),
        ));
    }
    if meta.uid() != current_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "staging directory ownership changed after creation: {}",
                staged_dir.display()
            ),
        ));
    }

    // Set directory to world-executable so privilege-dropped subprocesses can access it.
    let mut dir_perms = meta.permissions();
    dir_perms.set_mode(0o755);
    fs::set_permissions(staged_dir, dir_perms)?;

    Ok(())
}

/// Finds the staging directory for the worker binary.
///
/// Returns a tuple of:
/// - The staging directory in `{temp_dir}/pg-worker-{profile}-{hash}/`
/// - The target profile directory (if found) for writing the pointer file
#[cfg(unix)]
fn find_staging_directory(source: &std::path::Path) -> (PathBuf, Option<PathBuf>) {
    let path_hash = compute_path_hash(source);
    let (profile_name, target_profile_dir) = find_profile_directory(source);
    let staged_dir = std::env::temp_dir().join(format!("pg-worker-{profile_name}-{path_hash}"));
    (staged_dir, target_profile_dir)
}

/// Walks up from source to find the Cargo profile directory (debug/release).
///
/// Returns the profile name and the profile directory path if found.
#[cfg(unix)]
fn find_profile_directory(source: &std::path::Path) -> (&'static str, Option<PathBuf>) {
    let mut current = source.parent();

    while let Some(dir) = current {
        if let Some(result) = check_directory_for_profile(dir) {
            return result;
        }
        current = dir.parent();
    }

    ("unknown", None)
}

/// Checks if a directory is a profile directory or contains profile information.
///
/// Returns `Some((profile_name, profile_dir))` if the directory is a profile dir
/// (debug/release) or a deps directory whose parent is a profile dir.
#[cfg(unix)]
fn check_directory_for_profile(dir: &std::path::Path) -> Option<(&'static str, Option<PathBuf>)> {
    let dir_name = dir.file_name().and_then(|n| n.to_str())?;

    match dir_name {
        "debug" => Some(("debug", Some(dir.to_path_buf()))),
        "release" => Some(("release", Some(dir.to_path_buf()))),
        "deps" => check_deps_parent_for_profile(dir),
        _ => None,
    }
}

/// Checks if the parent of a deps directory is a profile directory.
#[cfg(unix)]
fn check_deps_parent_for_profile(
    deps_dir: &std::path::Path,
) -> Option<(&'static str, Option<PathBuf>)> {
    let profile_dir = deps_dir.parent()?;
    let profile_name = profile_dir.file_name().and_then(|n| n.to_str())?;

    match profile_name {
        "debug" => Some(("debug", Some(profile_dir.to_path_buf()))),
        "release" => Some(("release", Some(profile_dir.to_path_buf()))),
        _ => Some((
            profile_name_to_static(profile_name),
            Some(profile_dir.to_path_buf()),
        )),
    }
}

/// Returns "unknown" for non-standard profile names.
///
/// This function is only called from `check_deps_parent_for_profile` after
/// "debug" and "release" have already been handled, so it always returns "unknown".
#[cfg(unix)]
const fn profile_name_to_static(_name: &str) -> &'static str {
    "unknown"
}

/// Computes a short hash of the source path for staging directory uniqueness.
#[cfg(unix)]
fn compute_path_hash(source: &std::path::Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.as_os_str().as_bytes());
    let result = hasher.finalize();
    // Use first 8 hex chars for brevity. SHA-256 always produces 32 bytes.
    let bytes: [u8; 32] = result.into();
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}

/// Writes a pointer file to the target directory for discoverability and cleanup.
///
/// The pointer file contains the full path to the staged binary in the system
/// temporary directory.
#[cfg(unix)]
fn write_pointer_file(
    target_dir: &std::path::Path,
    staged_path: &std::path::Path,
) -> io::Result<()> {
    let pointer_path = target_dir.join("pg_worker_staged.path");
    let temp_path = target_dir.join("pg_worker_staged.path.tmp");

    // Write atomically: write to temp file, then rename
    fs::write(&temp_path, staged_path.as_os_str().as_bytes())?;
    fs::rename(&temp_path, &pointer_path)?;
    Ok(())
}

/// Returns true if the staged binary needs to be updated.
#[cfg(unix)]
fn should_restage(source: &std::path::Path, staged: &std::path::Path) -> io::Result<bool> {
    let staged_meta = match fs::metadata(staged) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e),
    };

    let source_meta = fs::metadata(source)?;
    let source_mtime = source_meta.modified()?;
    let staged_mtime = staged_meta.modified()?;

    Ok(source_mtime > staged_mtime)
}

fn locate_worker_binary() -> Option<OsString> {
    let exe = std::env::current_exe().ok()?;
    let deps_dir = exe.parent()?;
    let target_dir = deps_dir.parent()?;
    let worker_path = target_dir.join("pg_worker");
    if worker_path.is_file() {
        return Some(worker_path.into_os_string());
    }

    let entries = std::fs::read_dir(deps_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if is_worker_binary(&path) {
            return Some(path.into_os_string());
        }
    }

    None
}

fn is_worker_binary(path: &std::path::Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    if !name.starts_with("pg_worker") {
        return false;
    }

    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("d"))
    {
        return false;
    }

    path.is_file()
}

#[cfg(test)]
#[path = "worker_env_tests.rs"]
pub mod tests;
