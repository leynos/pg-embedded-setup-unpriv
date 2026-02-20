//! Shared panic-formatting helpers for cluster runtime and worker flows.

use std::any::Any;

use color_eyre::eyre::eyre;

use crate::error::BootstrapError;

/// Formats panic payloads from helper threads into a stable string.
pub(crate) fn panic_payload_to_string(panic_payload: Box<dyn Any + Send>) -> String {
    match panic_payload.downcast::<String>() {
        Ok(message) => *message,
        Err(other_payload) => other_payload.downcast::<&'static str>().map_or_else(
            |_| "unknown panic payload".to_owned(),
            |message| (*message).to_owned(),
        ),
    }
}

/// Creates a consistent `BootstrapError` for helper-thread panics.
pub(crate) fn nested_runtime_thread_panic(
    context: &'static str,
    operation: &'static str,
    panic_payload: Box<dyn Any + Send>,
) -> BootstrapError {
    let message = panic_payload_to_string(panic_payload);
    BootstrapError::from(eyre!(
        "{context}: helper thread panicked while running {operation}: {message}"
    ))
}
