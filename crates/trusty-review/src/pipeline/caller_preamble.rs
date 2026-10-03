//! A `# Context:` preamble on a diff is caller context, not diff noise (#8654).
//!
//! Why: `review_diff` (before #8654) and code-intelligence's subprocess adapter
//! prefixed PR context onto the diff as a `# Context: …` block. The diff parser
//! cannot attribute those lines to a file, so it reported them as an
//! `UnparsedSection` and the analyzer logged and dropped them: the PR
//! description never reached the reviewer, and the review JSON kept no trace.
//! What: [`consume_context_preamble`] splits off the lines before the first
//! file header when the first non-blank line starts with [`CONTEXT_MARKER`],
//! strips the marker, and appends the text to
//! [`CallerContext::pr_description`]. Any other unattributable preamble stays
//! in the diff, where the analyzer still warns about it (#4458).
//! Test: `caller_preamble_tests.rs`.

use tracing::info;

use crate::pipeline::runner::CallerContext;

/// The line prefix that marks a diff preamble as caller context.
pub const CONTEXT_MARKER: &str = "# Context:";

/// Split a `# Context:` preamble off `raw`, returning `(context, diff)`.
///
/// Why: kept pure so the boundary rules are testable without a pipeline.
/// What: `None` unless the first non-blank line starts with [`CONTEXT_MARKER`]
/// AND a file header follows — a `diff --git ` line, an `@@ ` hunk header, or a
/// `--- ` line directly followed by `+++ `. The context is every line before
/// that header, with the marker stripped from each line that carries it.
/// Test: `context_preamble_is_split_at_the_first_file_header`,
/// `a_preamble_with_no_diff_after_it_is_left_alone`,
/// `an_unmarked_preamble_is_left_in_the_diff`.
pub fn split_context_preamble(raw: &str) -> Option<(String, &str)> {
    let first = raw.lines().find(|l| !l.trim().is_empty())?;
    if !first.trim_start().starts_with(CONTEXT_MARKER) {
        return None;
    }
    let mut offset = 0usize;
    let mut lines = raw.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        let body = line.trim_end_matches(['\n', '\r']);
        let header_pair =
            body.starts_with("--- ") && lines.peek().is_some_and(|next| next.starts_with("+++ "));
        if body.starts_with("diff --git ") || body.starts_with("@@ ") || header_pair {
            return Some((preamble_text(&raw[..offset]), &raw[offset..]));
        }
        offset += line.len();
    }
    None
}

/// The preamble's prose: each line with a leading marker stripped, trimmed.
fn preamble_text(preamble: &str) -> String {
    let lines: Vec<&str> = preamble
        .lines()
        .map(|l| {
            l.trim_start()
                .strip_prefix(CONTEXT_MARKER)
                .map_or(l, str::trim_start)
        })
        .collect();
    lines.join("\n").trim().to_string()
}

/// Fold a `# Context:` preamble into `caller` and return the diff without it.
///
/// Why: the runner's single entry point for #8654 condition 4 — the preamble
/// is consumed, never silently discarded.
/// What: on a split, appends the context to `caller.pr_description` (after a
/// blank line when a description is already set; skipped when that
/// description already contains it) and logs at `info`. Without a split, the
/// diff is returned unchanged and `caller` is untouched.
/// Test: `consumed_preamble_reaches_the_pr_description`,
/// `preamble_is_appended_after_a_caller_supplied_description`.
pub fn consume_context_preamble(raw_diff: String, caller: &mut CallerContext) -> String {
    let Some((text, rest)) = split_context_preamble(&raw_diff) else {
        return raw_diff;
    };
    if !text.is_empty() {
        info!(
            chars = text.len(),
            "diff preamble marked {CONTEXT_MARKER:?} consumed as the PR description (#8654)"
        );
        caller.pr_description = Some(match caller.pr_description.take() {
            Some(existing) if existing.contains(&text) => existing,
            Some(existing) if !existing.trim().is_empty() => format!("{existing}\n\n{text}"),
            _ => text,
        });
    }
    rest.to_string()
}

#[cfg(test)]
#[path = "caller_preamble_tests.rs"]
mod tests;
