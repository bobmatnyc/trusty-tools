//! Formatting and composition helpers for `prompt-context`.
//!
//! Why: separating the Markdown rendering logic from the fetch and filter
//! layers keeps each piece independently readable and testable.
//! What: exports `compose_injection`, `push_section`, `drawer_preview`,
//! `render_tags`, `count_facts`.
//! Test: `compose_injection_truncates_at_cap`,
//! `compose_injection_empty_inputs_yields_empty`.

use super::filter::{RawTriple, RecalledDrawer};
use super::{DRAWER_PREVIEW_CHARS, INJECTION_BYTE_CAP};

/// Most tags rendered on one drawer's injected preview.
///
/// Why (issue #5038): tags were rendered in full, uncapped. Over the same
/// 17,176-firing corpus the `_(tags: …)_` suffix is **53.0% of every byte the
/// hook injects** (24.9 MB of 47.0 MB) — the label outweighs the content it
/// labels on 82.3% of drawers, at a mean 333 bytes of tags against 220 bytes of
/// preview. [`INJECTION_BYTE_CAP`] does not fix that; it just means the noise
/// evicts real drawers instead of growing the block.
/// What: `4`. The rule is that a label must never outweigh its payload, so the
/// cap is the largest one whose *worst observed* tag block stays clear of
/// [`DRAWER_PREVIEW_CHARS`] (220). Per-drawer tag-block bytes after provenance
/// filtering, over the corpus:
///
/// | cap | p50 | p95 | max |
/// |---|---|---|---|
/// | none | 208 | 382 | 903 |
/// | 6 | 112 | 150 | 244 — over the content budget |
/// | 5 | 98 | 127 | 204 — 93% of it, effectively equal weight |
/// | **4** | **82** | **104** | **174** |
/// | 3 | 67 | 84 | 159 |
///
/// Combined with the provenance filter this returns 40.1% of the total
/// injection to content. Tags are not dropped entirely — they orient a model
/// that has only a 220-character preview to go on — but the surplus is
/// announced (`+N more`) rather than rendered, matching the withheld-signal
/// rule the #5037 ruling sets for drawers.
/// Test: `injection_caps_rendered_tag_count`.
pub(super) const MAX_RENDERED_TAGS: usize = 4;

/// Compose the final injection block.
///
/// Why: a single coherent Markdown block is easier for the model to read
/// than three loose strings, and the section headers tell the model
/// where each piece came from so it can weigh them appropriately.
/// What: appends sections in priority order (workspace facts → drawers →
/// KG triples), each separated by a blank line. Truncates at
/// [`INJECTION_BYTE_CAP`] bytes with a `…` marker. `withheld` is how many
/// drawers the relevance floor dropped (#5037); when it is non-zero the drawer
/// section carries the [`withheld_notice`] line, including when every drawer was
/// dropped and there is otherwise no section at all.
/// Test: `compose_injection_truncates_at_cap`,
/// `compose_injection_announces_withheld_drawers`,
/// `compose_injection_announces_total_silence`,
/// `prompt_context_recalls_palace_drawers`.
pub(super) fn compose_injection(
    global_facts: Option<&str>,
    drawers: &[RecalledDrawer],
    withheld: usize,
    triples: &[RawTriple],
    palace_slug: Option<&str>,
) -> String {
    let mut out = String::new();
    if let Some(facts) = global_facts {
        push_section(&mut out, facts.trim_end());
    }
    if !drawers.is_empty() || withheld > 0 {
        let mut section = String::new();
        if let Some(slug) = palace_slug {
            section.push_str(&format!("## Relevant memories from palace `{slug}`\n"));
        } else {
            section.push_str("## Relevant memories\n");
        }
        for d in drawers {
            section.push_str("- ");
            section.push_str(&drawer_preview(&d.content));
            // #5038: the tag list used to render whole and unfiltered here.
            if let Some(tags) = render_tags(&d.tags) {
                section.push_str(&tags);
            }
            section.push('\n');
        }
        if withheld > 0 {
            section.push_str(&withheld_notice(withheld, drawers.is_empty()));
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

/// Render the "results were withheld" line for the drawer section.
///
/// Why (issue #5037, requirement 4): the relevance floor is allowed to return
/// zero drawers where five noisy ones used to appear — that is the correct
/// outcome for a prompt with no good match. It is only correct if the reader can
/// tell it apart from "this palace holds nothing", because those two call for
/// opposite next actions: one means search harder, the other means store
/// something first. Without this line the floor would have swapped a visible
/// wrong answer for an invisible one.
/// What: one italic Markdown line naming the count, worded differently for a
/// partial drop and a total one, and pointing at `memory_recall` as the way to
/// see past the floor.
/// Test: `compose_injection_announces_withheld_drawers`,
/// `compose_injection_announces_total_silence`.
pub(super) fn withheld_notice(withheld: usize, nothing_kept: bool) -> String {
    let plural = if withheld == 1 { "memory" } else { "memories" };
    if nothing_kept {
        format!(
            "_(No stored {plural} cleared the relevance floor for this prompt — \
             {withheld} withheld as too weak a match. Nothing is missing from the \
             palace; call `memory_recall` to search it directly.)_"
        )
    } else {
        format!(
            "_({withheld} further {plural} withheld below the relevance floor — \
             call `memory_recall` for the full ranked set.)_"
        )
    }
}

/// Render a drawer's tag suffix, filtered and capped.
///
/// Why (issue #5038): the previous inline render walked `d.tags` in full with
/// no filter and no count cap, so storage provenance and a 20-tag topical list
/// took more of the injection than the drawer content did. `memory_remember`
/// stamps every drawer with `creator:client`, `creator:version`,
/// `creator:source` and `creator:cwd` — the last an absolute path, ~90
/// characters, repeated on every drawer of every firing. Over 17,176 real
/// firings in the enriched-prompt log those are 24.2% of rendered tags but
/// **33.4% of rendered tag bytes**, and on 2.8% of drawers they are the entire
/// list, producing a `_(tags: …)_` suffix made of nothing but paths and version
/// numbers. See [`MAX_RENDERED_TAGS`] for the count-cap half.
/// What: drops the `creator:*` namespace via
/// [`crate::attribution::is_creator_tag`] — the same predicate the TUI and
/// dashboard renderers already hide those tags with — keeps at most
/// [`MAX_RENDERED_TAGS`] of the rest in stored order, and appends `+N more`
/// when any were held back so the surplus is announced rather than hidden.
/// Returns `None` (no suffix at all) when nothing survives, the common case for
/// a drawer whose only tags were provenance. Filtering is render-time only; the
/// tags stay in storage and stay queryable through `memory_list` /
/// `memory_recall`.
/// Test: `injection_drops_provenance_tags`, `injection_caps_rendered_tag_count`,
/// `prompt_context_injection_has_no_provenance_tags`.
pub(super) fn render_tags(tags: &[String]) -> Option<String> {
    let topical: Vec<&String> = tags
        .iter()
        .filter(|t| !crate::attribution::is_creator_tag(t))
        .collect();
    if topical.is_empty() {
        return None;
    }
    let mut out = String::from("  _(tags: ");
    for (i, tag) in topical.iter().take(MAX_RENDERED_TAGS).enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push('`');
        out.push_str(tag);
        out.push('`');
    }
    if let Some(hidden) = topical
        .len()
        .checked_sub(MAX_RENDERED_TAGS)
        .filter(|n| *n > 0)
    {
        out.push_str(&format!(", +{hidden} more"));
    }
    out.push_str(")_");
    Some(out)
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
/// Test: indirectly via `prompt_context_recalls_palace_drawers`; the exactness
/// the backfill report depends on is pinned by `preview_matches_injection_bullet`.
// #4891: `pub(crate)` so `commands::backfill_report` reuses this exact
// truncation when joining drawers back to the hook logs.
pub(crate) fn drawer_preview(content: &str) -> String {
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
