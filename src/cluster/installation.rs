//! Resolves installation directories for worker-managed PostgreSQL setups.

use crate::{ExecutionPrivileges, TestBootstrapSettings};
use postgresql_embedded::Settings;
use semver::Version;
use std::path::{Path, PathBuf};

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

    select_latest_version(candidates)
}

fn select_latest_version(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    let mut versioned = candidates
        .into_iter()
        .filter_map(|path| parse_installation_version(&path).map(|version| (version, path)))
        .collect::<Vec<_>>();

    versioned.sort_by(|(left, _), (right, _)| right.cmp(left));
    versioned.into_iter().next().map(|(_, path)| path)
}

fn parse_installation_version(path: &Path) -> Option<Version> {
    let name = path.file_name()?.to_str()?;
    let raw_version = extract_version_string(name)?;
    let normalised = normalise_version(&raw_version)?;
    Version::parse(&normalised).ok()
}

fn extract_version_string(name: &str) -> Option<String> {
    let mut start = None;
    let mut end = None;

    for (idx, ch) in name.char_indices() {
        if ch.is_ascii_digit() {
            if start.is_none() {
                start = Some(idx);
            }
            end = Some(idx + ch.len_utf8());
            continue;
        }

        if ch == '.' && start.is_some() {
            end = Some(idx + 1);
            continue;
        }

        if start.is_some() {
            break;
        }
    }

    let raw = name.get(start?..end?)?.trim_end_matches('.');
    if raw.is_empty() {
        return None;
    }

    Some(raw.to_string())
}

fn normalise_version(raw: &str) -> Option<String> {
    let parts = raw
        .split('.')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    if parts.is_empty() {
        return None;
    }

    if parts
        .iter()
        .any(|part| !part.chars().all(|ch| ch.is_ascii_digit()))
    {
        return None;
    }

    let normalised = match parts.len() {
        1 => format!("{}.0.0", parts[0]),
        2 => format!("{}.{}.0", parts[0], parts[1]),
        3 => parts.join("."),
        _ => return None,
    };

    Some(normalised)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_installed_dir_prefers_higher_semver() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let install_dir = temp_dir.path();
        let older = install_dir.join("15.9").join("bin");
        let newer = install_dir.join("15.10").join("bin");

        std::fs::create_dir_all(&older).expect("create older bin");
        std::fs::create_dir_all(&newer).expect("create newer bin");

        let mut settings = Settings::default();
        settings.installation_dir = install_dir.to_path_buf();
        settings.trust_installation_dir = false;

        let selected = resolve_installed_dir(&settings).expect("resolve installed dir");
        assert_eq!(
            selected.file_name().and_then(|name| name.to_str()),
            Some("15.10"),
            "expected semver ordering to select the highest version"
        );
    }
}
