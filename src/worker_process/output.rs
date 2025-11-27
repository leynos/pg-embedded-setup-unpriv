//! Output truncation and error rendering helpers for worker processes.

use crate::error::BootstrapError;
use color_eyre::eyre::eyre;
use std::borrow::Cow;
use std::fmt::Write as FmtWrite;
use std::process::Output;

pub(super) const OUTPUT_CHAR_LIMIT: usize = 2_048;
pub(super) const TRUNCATION_SUFFIX: &str = "… [truncated]";

pub(super) fn render_failure(context: &str, output: &Output) -> BootstrapError {
    let stdout = truncate_output(String::from_utf8_lossy(&output.stdout));
    let stderr = truncate_output(String::from_utf8_lossy(&output.stderr));
    BootstrapError::from(eyre!("{context}\nstdout: {stdout}\nstderr: {stderr}"))
}

pub(super) fn combine_errors(primary: BootstrapError, cleanup: BootstrapError) -> BootstrapError {
    let primary_report = primary.into_report();
    let cleanup_report = cleanup.into_report();
    BootstrapError::from(eyre!(
        "{primary_report}\nSecondary failure whilst removing worker payload: {cleanup_report}"
    ))
}

pub(super) fn append_error_context(
    message: &mut String,
    detail: &str,
    error: &impl std::fmt::Display,
    fallback: &str,
) {
    if FmtWrite::write_fmt(message, format_args!("; {detail}: {error}")).is_err() {
        message.push_str(fallback);
    }
}

pub(super) fn truncate_output(text: Cow<'_, str>) -> String {
    let mut out = String::with_capacity(OUTPUT_CHAR_LIMIT + TRUNCATION_SUFFIX.len());
    let mut chars = text.chars();
    for _ in 0..OUTPUT_CHAR_LIMIT {
        match chars.next() {
            Some(ch) => out.push(ch),
            None => return text.into_owned(),
        }
    }

    if chars.next().is_none() {
        return text.into_owned();
    }

    out.push_str(TRUNCATION_SUFFIX);
    out
}

#[doc(hidden)]
#[must_use]
pub(crate) fn render_failure_for_tests(context: &str, output: &Output) -> BootstrapError {
    render_failure(context, output)
}
