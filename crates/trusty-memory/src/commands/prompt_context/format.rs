//! Formatting and composition helpers for `prompt-context`.
//!
//! Why: separating the Markdown rendering logic from the fetch and filter
//! layers keeps each piece independently readable and testable.
//! What: exports `compose_injection`, `push_section`, `drawer_preview`,
//! `count_facts`.
//! Test: `compose_injection_truncates_at_cap`,
//! `compose_injection_empty_inputs_yields_empty`.

use super::filter::{RawTriple, RecalledDrawer};
use super::{DRAWER_PREVIEW_CHARS, INJECTION_BYTE_CAP};

/// Compose the final injection block.
///
/// Why: a single coherent Markdown block is easier for the model to read
/// than three loose strings, and the section headers tell the model
/// where each piece came from so it can weigh them appropriately.
/// What: appends sections in priority order (workspace facts → drawers →
/// KG triples), each separated by a blank line. Truncates at
/// [`INJECTION_BYTE_CAP`] bytes with a `…` marker.
/// Test: `compose_injection_truncates_at_cap`,
/// `prompt_context_recalls_palace_drawers`.
pub(super) fn compose_injection(
    global_facts: Option<&str>,
    drawers: &[RecalledDrawer],
    triples: &[RawTriple],
    palace_slug: Option<&str>,
) -> String {
    let mut out = String::new();
    if let Some(facts) = global_facts {
        push_section(&mut out, facts.trim_end());
    }
    if !drawers.is_empty() {
        let mut section = String::new();
        if let Some(slug) = palace_slug {
            section.push_str(&format!("## Relevant memories from palace `{slug}`\n"));
        } else {
            section.push_str("## Relevant memories\n");
        }
        for d in drawers {
            section.push_str("- ");
            section.push_str(&drawer_preview(&d.content));
            if !d.tags.is_empty() {
                section.push_str("  _(tags: ");
                let tags = d
                    .tags
                    .iter()
                    .map(|t| format!("`{t}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                section.push_str(&tags);
                section.push(')');
                section.push('_');
            }
            section.push('\n');
        }
        push_section(&mut out, section.trim_end());
    }
    if !triples.is_empty() {
        let mut section = String::new();
        section.push_str("## Relevant KG facts\n");
        for t in triples {
            section.push_str(&format!(
                "- {} **{}** {}\n",
                t.subject, t.predicate, t.object
            ));
        }
        push_section(&mut out, section.trim_end());
    }
    if out.len() > INJECTION_BYTE_CAP {
        // Reserve 3 bytes for the `…` marker (UTF-8). Walk back to a char
        // boundary so the truncated string stays valid UTF-8.
        const ELLIPSIS: char = '…';
        let ellipsis_len = ELLIPSIS.len_utf8();
        let mut cut = INJECTION_BYTE_CAP.saturating_sub(ellipsis_len);
        while cut > 0 && !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push(ELLIPSIS);
    }
    out
}

/// Append `section` to `out` separated by a blank line when `out`
/// already has content.
///
/// Why: avoids leading or double blank lines in the composed injection by
/// centralising the separator logic.
/// What: pushes a newline separator before `section` when `out` is non-empty,
/// then appends the section text. No-ops on empty `section`.
/// Test: indirectly via `compose_injection_*` tests.
pub(super) fn push_section(out: &mut String, section: &str) {
    if section.is_empty() {
        return;
    }
    if !out.is_empty() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(section);
}

/// Collapse a drawer's content to a single-line preview capped at
/// [`DRAWER_PREVIEW_CHARS`].
///
/// Why: dumping the full drawer body would burn the byte budget on a
/// single entry; a short single-line preview is enough to remind the model
/// what's available and lets it pull more via MCP recall if needed.
/// What: whitespace-collapses and truncates to [`DRAWER_PREVIEW_CHARS`]
/// chars with a trailing `…` when cut.
/// Test: indirectly via `prompt_context_recalls_palace_drawers`.
pub(super) fn drawer_preview(content: &str) -> String {
    let normalised: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalised.chars().count() <= DRAWER_PREVIEW_CHARS {
        normalised
    } else {
        let kept: String = normalised
            .chars()
            .take(DRAWER_PREVIEW_CHARS.saturating_sub(1))
            .collect();
        format!("{kept}…")
    }
}

/// Approximate the number of facts in the rendered prompt-context body.
///
/// Why: the daemon's response is plain Markdown; counting bullet lines
/// (`- ` prefix) gives a quick proxy for "how many facts were injected" that
/// is useful for log analysis without an additional round trip.
/// What: counts non-empty lines whose first non-whitespace characters are
/// `- `. Returns 0 for an empty / placeholder body.
/// Test: covered indirectly by `single_event_roundtrip` in the integration
/// tests; the heuristic is intentionally cheap and approximate.
pub(super) fn count_facts(body: &str) -> usize {
    body.lines()
        .filter(|l| l.trim_start().starts_with("- "))
        .count()
}
