//! Tests for drop-time warning logs.

use crate::test_support::capture_warn_logs;
use rstest::rstest;

use super::shutdown;

#[rstest]
#[case::timeout(
    || shutdown::warn_stop_timeout(5, "ctx"),
    "stop() timed out after 5s (ctx)"
)]
#[case::failure(
    || shutdown::warn_stop_failure("ctx", &"boom"),
    "failed to stop embedded postgres instance"
)]
fn warn_stop_emits_warning(#[case] action: fn(), #[case] expected_substring: &str) {
    let (logs, ()) = capture_warn_logs(action);
    assert!(
        logs.iter().any(|line| line.contains(expected_substring)),
        "expected warning, got {logs:?}"
    );
}
