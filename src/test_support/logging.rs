//! Captures tracing output for assertions in behavioural tests.
//!
//! The helper records `WARN`-level logs without timestamps so assertions can
//! match human-readable messages directly.

use std::io::{Result as IoResult, Write};
use std::sync::{Arc, Mutex};

#[cfg(any(doc, feature = "cluster-unit-tests", feature = "dev-worker"))]
use tracing::subscriber::with_default;
#[cfg(any(doc, feature = "cluster-unit-tests", feature = "dev-worker"))]
use tracing::Level;
#[cfg(any(doc, feature = "cluster-unit-tests", feature = "dev-worker"))]
use tracing_subscriber::fmt::format::FmtSpan;

use crate::observability::LOG_TARGET;

#[cfg(any(doc, feature = "cluster-unit-tests", feature = "dev-worker"))]
struct BufferWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

#[cfg(any(doc, feature = "cluster-unit-tests", feature = "dev-worker"))]
impl Write for BufferWriter {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        let mut guard = self
            .buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> IoResult<()> {
        Ok(())
    }
}

/// Runs `action`, capturing warning logs and returning them alongside the
/// closure result.
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::test_support::capture_warn_logs;
///
/// let (logs, value) = capture_warn_logs(|| {
///     tracing::warn!("something happened");
///     41 + 1
/// });
/// assert!(logs.iter().any(|line| line.contains("something happened")));
/// assert_eq!(value, 42);
/// ```
#[must_use]
pub fn capture_warn_logs<F, R>(action: F) -> (Vec<String>, R)
where
    F: FnOnce() -> R,
{
    capture_logs(Level::WARN, false, action)
}

/// Runs `action`, capturing info-level logs and returning them with the result.
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::test_support::capture_info_logs;
///
/// let (logs, value) = capture_info_logs(|| {
///     tracing::info!("observability in action");
///     1 + 1
/// });
/// assert!(
///     logs.iter()
///         .any(|line| line.contains("observability in action"))
/// );
/// assert_eq!(value, 2);
/// ```
#[must_use]
pub fn capture_info_logs<F, R>(action: F) -> (Vec<String>, R)
where
    F: FnOnce() -> R,
{
    capture_logs(Level::INFO, false, action)
}

/// Runs `action`, capturing info-level logs plus span entry and exit events.
#[must_use]
pub fn capture_info_logs_with_spans<F, R>(action: F) -> (Vec<String>, R)
where
    F: FnOnce() -> R,
{
    capture_logs(Level::INFO, true, action)
}

/// Runs `action`, capturing debug-level logs and returning them with the result.
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::test_support::capture_debug_logs;
///
/// let (logs, value) = capture_debug_logs(|| {
///     tracing::debug!("debugging enabled");
///     21 * 2
/// });
/// assert!(
///     logs.iter().any(|line| line.contains("debugging enabled")),
///     "expected debug log to be captured"
/// );
/// assert_eq!(value, 42);
/// ```
#[must_use]
pub fn capture_debug_logs<F, R>(action: F) -> (Vec<String>, R)
where
    F: FnOnce() -> R,
{
    capture_logs(Level::DEBUG, false, action)
}

fn capture_logs<F, R>(max_level: Level, span_events: bool, action: F) -> (Vec<String>, R)
where
    F: FnOnce() -> R,
{
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer_buffer = Arc::clone(&buffer);
    let mut builder = fmt()
        .with_max_level(max_level)
        .without_time()
        .with_ansi(false)
        .with_writer(move || BufferWriter {
            buffer: Arc::clone(&writer_buffer),
        });
    if span_events {
        builder = builder.with_span_events(FmtSpan::ENTER | FmtSpan::CLOSE);
    }
    let subscriber = builder.finish();

    let result = with_default(subscriber, action);

    let bytes = buffer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let logs = decode_logs(bytes);
    (logs, result)
}

/// Decodes captured log output, warning and falling back to lossy UTF-8 when
/// invalid byte sequences are encountered.
fn decode_logs(bytes: Vec<u8>) -> Vec<String> {
    let content = match String::from_utf8(bytes) {
        Ok(content) => content,
        Err(err) => {
            let utf8_error = err.utf8_error();
            let err_bytes = err.into_bytes();
            tracing::warn!(
                target: LOG_TARGET,
                error = %utf8_error,
                "captured logs contained invalid UTF-8; decoding lossily"
            );
            String::from_utf8_lossy(&err_bytes).into_owned()
        }
    };
    content.lines().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::{capture_debug_logs, capture_info_logs_with_spans, capture_warn_logs, decode_logs};
    use tracing::info_span;

    #[test]
    fn captures_span_enter_and_close_events() {
        let (logs, ()) = capture_info_logs_with_spans(|| {
            let span = info_span!("test_span");
            let _entered = span.enter();
        });

        assert!(
            logs.iter()
                .any(|line| line.contains("test_span") && line.contains("enter")),
            "expected span enter event in logs, got {logs:?}"
        );
        assert!(
            logs.iter()
                .any(|line| line.contains("test_span") && line.contains("close")),
            "expected span close event in logs, got {logs:?}"
        );
    }

    #[test]
    fn captures_debug_logs() {
        let (logs, ()) = capture_debug_logs(|| {
            tracing::debug!("debug message for capture");
        });

        assert!(
            logs.iter()
                .any(|line| line.contains("debug message for capture")),
            "expected debug log to be captured, got {logs:?}"
        );
    }

    #[test]
    fn decode_logs_uses_lossy_utf8_for_invalid_bytes() {
        let bytes = vec![b'a', b'b', b'\n', 0xF0, 0x28, 0x8C, 0x28];
        let (warn_logs, logs) = capture_warn_logs(|| decode_logs(bytes));

        assert_eq!(
            logs,
            vec![String::from("ab"), String::from("\u{FFFD}(\u{FFFD}(")]
        );
        assert!(
            warn_logs.iter().any(
                |line| line.contains("captured logs contained invalid UTF-8; decoding lossily")
            ),
            "expected invalid UTF-8 warning, got {warn_logs:?}"
        );
    }
}
