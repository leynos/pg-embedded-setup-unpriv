//! Resolves worker installation directories after setup.

use std::path::{Path, PathBuf};

use postgresql_embedded::Settings;
use semver::Version;

use crate::{ExecutionPrivileges, TestBootstrapSettings};

pub(crate) fn refresh_worker_installation_dir(bootstrap: &mut TestBootstrapSettings) {
    if bootstrap.privileges != ExecutionPrivileges::Root {
        return;
    }

    if let Some(installed_dir) = resolve_installed_dir(&bootstrap.settings) {
        bootstrap.settings.installation_dir = installed_dir;
    }
}

pub(crate) fn resolve_installed_dir(settings: &Settings) -> Option<PathBuf> {
    let install_dir = &settings.installation_dir;

    if install_dir.join("bin").is_dir() {
        return Some(install_dir.clone());
    }

    if settings.trust_installation_dir {
        return Some(install_dir.clone());
    }

    let candidates = std::fs::read_dir(install_dir)
        .ok()?
        .filter_map(|dir_entry| {
            let entry = dir_entry.ok()?;
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let path = entry.path();
            path.join("bin").is_dir().then_some(path)
        })
        .collect::<Vec<_>>();

    let mut versioned_candidates = candidates
        .into_iter()
        .filter_map(|path| {
            let version = parse_installation_version(&path)?;
            Some((version, path))
        })
        .collect::<Vec<_>>();
    versioned_candidates.sort_by(|(left, _), (right, _)| right.cmp(left));
    versioned_candidates
        .into_iter()
        .map(|(_, path)| path)
        .next()
}

fn parse_installation_version(path: &Path) -> Option<Version> {
    let name = path.file_name()?.to_str()?;
    let raw_version = extract_version_prefix(name)?;
    let normalised = normalise_version_string(&raw_version)?;
    Version::parse(&normalised).ok()
}

fn extract_version_prefix(name: &str) -> Option<String> {
    let mut buffer = String::new();
    let mut in_version = false;

    for ch in name.chars() {
        if !in_version {
            if ch.is_ascii_digit() {
                in_version = true;
                buffer.push(ch);
            }
            continue;
        }

        if ch.is_ascii_digit() || ch == '.' {
            buffer.push(ch);
        } else {
            break;
        }
    }

    if buffer.is_empty() {
        None
    } else {
        Some(buffer)
    }
}

fn normalise_version_string(raw: &str) -> Option<String> {
    let mut components = raw
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;

    if components.is_empty() || components.len() > 3 {
        return None;
    }

    while components.len() < 3 {
        components.push(0);
    }

    let [major, minor, patch] = components.as_slice() else {
        return None;
    };

    Some(format!("{major}.{minor}.{patch}"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn resolve_installed_dir_prefers_latest_version() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let base_dir = temp_dir.path();
        let older = base_dir.join("15.9");
        let newer = base_dir.join("15.10");
        fs::create_dir_all(older.join("bin")).expect("older bin");
        fs::create_dir_all(newer.join("bin")).expect("newer bin");

        let settings = Settings {
            installation_dir: base_dir.to_path_buf(),
            trust_installation_dir: false,
            ..Settings::default()
        };

        let resolved = resolve_installed_dir(&settings).expect("resolve install dir");
        assert_eq!(resolved, newer);
    }
}
