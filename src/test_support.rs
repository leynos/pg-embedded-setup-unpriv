//! Internal helpers re-exported for integration tests.
use camino::{Utf8Path, Utf8PathBuf};
use cap_std::fs::Dir;
use color_eyre::eyre::{Report, Result};

use crate::error::{BootstrapError, Error, PrivilegeError};
use crate::fs;

pub fn ambient_dir_and_path(path: &Utf8Path) -> Result<(Dir, Utf8PathBuf)> {
    fs::ambient_dir_and_path(path)
}

pub fn ensure_dir_exists(path: &Utf8Path) -> Result<()> {
    fs::ensure_dir_exists(path)
}

pub fn set_permissions(path: &Utf8Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, mode)
}

pub fn bootstrap_error(err: Report) -> Error {
    Error::Bootstrap(BootstrapError::from(err))
}

pub fn privilege_error(err: Report) -> Error {
    Error::Privilege(PrivilegeError::from(err))
}
