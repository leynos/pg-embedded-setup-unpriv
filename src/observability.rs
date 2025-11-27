//! Shared tracing configuration for observability instrumentation.
//!
//! Centralises the log target used by the crate so subscribers can filter
//! observability events without pulling in unrelated application logs.

/// Target used by observability spans and logs.
pub(crate) const LOG_TARGET: &str = "pg_embed::observability";
