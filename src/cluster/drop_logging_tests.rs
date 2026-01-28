//! Tests for drop-time warning logs.

use crate::test_support::capture_warn_logs;

use super::shutdown;

#[test]
fn warn_stop_timeout_emits_warning() {
    let (logs, ()) = capture_warn_logs(|| shutdown::warn_stop_timeout(5, "ctx"));
    assert!(
        logs.iter()
            .any(|line| line.contains("stop() timed out after 5s (ctx)")),
        "expected timeout warning, got {logs:?}"
    );
}

#[test]
fn warn_stop_failure_emits_warning() {
    let (logs, ()) = capture_warn_logs(|| shutdown::warn_stop_failure("ctx", &"boom"));
    assert!(
        logs.iter()
            .any(|line| line.contains("failed to stop embedded postgres instance")),
        "expected failure warning, got {logs:?}"
    );
}
