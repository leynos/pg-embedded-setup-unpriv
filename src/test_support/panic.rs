//! Panic payload helpers shared by integration test support modules.

use std::any::Any;

/// Converts a panic payload into a stable, human-readable string.
#[must_use]
pub fn panic_payload_to_string(payload: Box<dyn Any + Send>) -> String {
    crate::cluster::panic_utils::panic_payload_to_string(payload)
}
