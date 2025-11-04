//! Captures tracing output for assertions in behavioural tests.
//!
//! The helper records `WARN`-level logs without timestamps so assertions can
//! match human-readable messages directly.

use std::io::{Result as IoResult, Write};
use std::sync::{Arc, Mutex};

use tracing::Level;
use tracing::subscriber::with_default;
use tracing_subscriber::fmt;

struct BufferWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

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
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer_buffer = Arc::clone(&buffer);
    let subscriber = fmt()
        .with_max_level(Level::WARN)
        .without_time()
        .with_writer(move || BufferWriter {
            buffer: Arc::clone(&writer_buffer),
        })
        .finish();

    let result = with_default(subscriber, action);

    let bytes = buffer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let content =
        String::from_utf8(bytes).unwrap_or_else(|err| panic!("logs should be valid UTF-8: {err}"));
    let logs = content.lines().map(str::to_owned).collect();
    (logs, result)
}
