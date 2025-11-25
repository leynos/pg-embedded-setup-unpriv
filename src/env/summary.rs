//! Helpers for rendering bounded environment change summaries.

use std::fmt::Write as FmtWrite;

pub(super) const MAX_ENV_CHANGES_SUMMARY_LEN: usize = 512;

#[expect(
    clippy::cognitive_complexity,
    reason = "truncation heuristics favour explicit branching for readability"
)]
pub(super) fn truncate_env_changes_summary(
    changes: &str,
    max_len: usize,
    change_count: usize,
) -> String {
    if changes.len() <= max_len {
        return changes.to_owned();
    }

    // Prefer to cut at a comma boundary close to the limit to avoid mangling keys.
    let mut cut = 0usize;
    let mut last_comma_before_cut = None;
    for (idx, ch) in changes.char_indices() {
        if idx >= max_len {
            break;
        }
        cut = idx + ch.len_utf8();
        if ch == ',' {
            last_comma_before_cut = Some(cut);
        }
    }

    let preferred_cut = last_comma_before_cut
        .filter(|pos| pos.saturating_mul(2) > max_len)
        .unwrap_or(cut);

    let mut truncated: String = changes
        .char_indices()
        .take_while(|(idx, _)| *idx < preferred_cut)
        .map(|(_, ch)| ch)
        .collect();
    truncated = truncated.trim_end().to_owned();

    // Estimate how many entries were shown to annotate how many were omitted.
    let shown_changes = truncated.chars().filter(|&c| c == ',').count() + 1;
    if change_count > shown_changes {
        let remaining = change_count - shown_changes;
        if !truncated.is_empty() && !truncated.ends_with(',') {
            truncated.push_str(", ");
        }
        if let Err(err) = write!(truncated, "+ {remaining} more") {
            debug_assert!(false, "writing truncated summary failed: {err}");
        }
    } else if !truncated.ends_with("...") {
        truncated.push_str("...");
    }

    truncated
}
