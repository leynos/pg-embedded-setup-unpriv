//! Shared skip heuristics for integration tests.
//!
//! These helpers centralise the set of failure signatures that should result in
//! a soft skip rather than a hard assertion failure. Keeping the substrings and
//! formatting logic here ensures new tests can inherit the same defensive
//! behaviour without duplicating arrays of magic strings.

/// Message substrings that signal an external failure which should skip tests
/// gracefully.
pub(crate) const DEFAULT_SKIP_CONDITIONS: &[(&str, &str)] = &[
    (
        "rate limit exceeded",
        "rate limit exceeded whilst downloading PostgreSQL",
    ),
    (
        "No such file or directory",
        "PostgreSQL binaries were unavailable for the requested operation",
    ),
    (
        "failed to read worker config",
        "worker helper cannot access its configuration",
    ),
    (
        "Permission denied",
        "worker helper lacks filesystem permissions",
    ),
    (
        "setgroups failed",
        "kernel refused to adjust supplementary groups",
    ),
    (
        "must start as root to drop privileges temporarily",
        "root privileges are unavailable for the privileged bootstrap path",
    ),
];

/// Formats a skip message using `prefix` and the shared reason when any known
/// condition appears in `message` or `debug`.
pub(crate) fn skip_message(prefix: &str, message: &str, debug: Option<&str>) -> Option<String> {
    let message_lc = message.to_ascii_lowercase();
    let debug_lc = debug.map_or_else(String::new, str::to_ascii_lowercase);
    DEFAULT_SKIP_CONDITIONS
        .iter()
        .find(|(needle, _)| {
            // Case-insensitive comparison so skip detection is resilient to
            // platform-specific capitalisation in error messages.
            let needle_lc = needle.to_ascii_lowercase();
            message_lc.contains(&needle_lc) || debug_lc.contains(&needle_lc)
        })
        .map(|(_, reason)| format!("{prefix}: {reason}: {message}"))
}
