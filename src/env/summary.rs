//! Helpers for rendering bounded environment change summaries.

pub(super) const MAX_ENV_CHANGES_SUMMARY_LEN: usize = 512;

pub(super) fn truncate_env_changes_summary(
    changes: &str,
    max_len: usize,
    change_count: usize,
) -> String {
    if changes.len() <= max_len {
        return changes.to_owned();
    }

    let preferred_cut = find_preferred_cut_point(changes, max_len);

    let mut truncated: String = changes
        .char_indices()
        .take_while(|(idx, _)| *idx < preferred_cut)
        .map(|(_, ch)| ch)
        .collect();
    truncated = truncated.trim_end().to_owned();

    // Estimate how many entries were shown to annotate how many were omitted.
    let shown_changes = truncated.chars().filter(|&c| c == ',').count() + 1;
    append_truncation_suffix(&mut truncated, shown_changes, change_count);

    truncated
}

fn find_preferred_cut_point(changes: &str, max_len: usize) -> usize {
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

    // Only use the last comma if it's in the latter half of max_len to avoid
    // trimming away most of the summary.
    last_comma_before_cut
        .filter(|pos| pos.saturating_mul(2) > max_len)
        .unwrap_or(cut)
}

fn append_truncation_suffix(truncated: &mut String, shown_changes: usize, change_count: usize) {
    if change_count > shown_changes {
        let remaining = change_count - shown_changes;
        if !truncated.is_empty() && !truncated.ends_with(',') {
            truncated.push_str(", ");
        }
        write_truncation_suffix(truncated, remaining);
    } else if !truncated.ends_with("...") {
        truncated.push_str("...");
    }
}

fn write_truncation_suffix(truncated: &mut String, remaining: usize) {
    truncated.push_str("+ ");
    truncated.push_str(&remaining.to_string());
    truncated.push_str(" more");
}
