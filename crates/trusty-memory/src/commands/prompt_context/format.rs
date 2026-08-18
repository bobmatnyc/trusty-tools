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
///
/// The count alone does not enforce the rule it is argued from: four 55-char
/// tags exceed 220 characters, and no cap on *how many* can prevent that.
/// [`MAX_RENDERED_TAG_CHARS`] enforces the byte side directly; this constant is
/// the common-case cap, that one is the guarantee (#5038 review).
/// Test: `injection_caps_rendered_tag_count`.
pub(super) const MAX_RENDERED_TAGS: usize = 4;

/// Hard character ceiling on one drawer's rendered tag suffix.
///
/// Why (#5038 review): [`MAX_RENDERED_TAGS`] is justified by "a label must never
/// outweigh the payload it labels" but enforces a count, which is only a proxy.
/// Tag length is not bounded anywhere — a palace using long hyphenated tags
/// (`slate-prioritization-in-flight`, `trusty-search-reinstall-in-flight`, both
/// real tags from the corpus) blows the budget at four. This makes the stated
/// rule the actual invariant rather than an argument for a different one.
/// What: [`DRAWER_PREVIEW_CHARS`] — the same budget the content preview gets, so
/// the label can at most equal its payload, never exceed it. Measured in
/// characters, matching how `DRAWER_PREVIEW_CHARS` measures the content. One
/// exception, deliberate: a *single* tag longer than the whole budget still
/// renders whole, because a suffix reading only `_(tags: +1 more)_` tells the
/// model strictly less than the tag would.
/// Test: `injection_caps_rendered_tag_bytes`.
pub(super) const MAX_RENDERED_TAG_CHARS: usize = DRAWER_PREVIEW_CHARS;

/// Byte budget for the rendered "Relevant KG facts" section.
///
/// Why (ADR-0028 D7): the KG section is the last of the four injection
/// sections and the only one with no length control of its own — `top_k`
/// bounded the triple *count* but a triple's rendered width is unbounded, so a
/// palace with long subject or object strings could spend the whole remaining
/// budget on the section ADR-0028 C10 measured as 93.5% plumbing estate-wide.
/// D7 allocates it 256 bytes. That is a real allocation, not a token gesture: a
/// hot triple is short by construction (`tga is_alias_for
/// trusty-git-analytics` renders to 44 bytes), so 256 bytes carries roughly
/// five of them.
/// What: `256`. Triples render until the next line would cross it, then the
/// section stops. There is no `+N more` marker — the surplus is by definition
/// lower-ranked than what fit, and announcing it would spend bytes the cap
/// exists to protect.
/// Test: `compose_injection_caps_kg_section_bytes`.
///
/// # Spec References
/// - [ADR-0028 D7](docs/adr/0028-memory-recall-tiers-standing-current-episodic.md)
pub(super) const KG_SECTION_BYTE_CAP: usize = 256;

/// Compose the final injection block.
///
/// Why: a single coherent Markdown block is easier for the model to read
/// than three loose strings, and the section headers tell the model
/// where each piece came from so it can weigh them appropriately.
/// What: appends sections in priority order (workspace facts → drawers →
/// KG triples), each separated by a blank line. Truncates at
/// [`INJECTION_BYTE_CAP`] bytes with a `…` marker. `withheld` is how many
/// drawers the relevance floor dropped (#5037); it is announced by
/// [`withheld_notice`] only as a suffix to a drawer section that has something
/// in it.
///
/// Returns an empty string when every section is empty, and the caller prints
/// nothing rather than a placeholder. Silence is the correct output for a
/// prompt with no relevant memory: it costs zero tokens, and a sentence saying
/// so costs real ones on every turn of every session (#5819).
/// Test: `compose_injection_truncates_at_cap`,
/// `compose_injection_announces_withheld_drawers`,
/// `compose_injection_is_silent_when_everything_was_withheld`,
/// `compose_injection_empty_inputs_yields_empty`,
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
    // #5819: `withheld > 0` used to open this section on its own, so a prompt
    // that matched nothing still rendered a paragraph announcing the fact.
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
            // #5038: the tag list used to render whole and unfiltered here.
            if let Some(tags) = render_tags(&d.tags) {
                section.push_str(&tags);
            }
            section.push('\n');
        }
        if withheld > 0 {
            section.push_str(&withheld_notice(withheld));
            section.push('\n');
        }
        push_section(&mut out, section.trim_end());
    }
    if !triples.is_empty() {
        let mut section = String::from("## Relevant KG facts\n");
        let mut rendered = 0usize;
        for t in triples {
            let line = format!("- {} **{}** {}\n", t.subject, t.predicate, t.object);
            // #5819 (ADR-0028 D7): the KG section gets its own byte budget, so
            // a palace with many hot triples cannot crowd out drawer recall.
            if section.len() + line.len() > KG_SECTION_BYTE_CAP {
                break;
            }
            section.push_str(&line);
            rendered += 1;
        }
        // A heading with no bullets under it is pure cost. Emit the section
        // only when at least one triple fit.
        if rendered > 0 {
            push_section(&mut out, section.trim_end());
        }
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

/// Render the "further results were withheld" line for a non-empty drawer
/// section.
///
/// Why (issue #5037, requirement 4, narrowed by #5819): when the injection
/// already shows the reader some memories, saying how many more exist below the
/// floor is cheap and tells them `memory_recall` would return more. #5037 also
/// emitted a longer variant when the floor kept *nothing*, on the reasoning that
/// "no good match" and "empty palace" call for opposite next actions. That
/// variant is removed: it rendered on every prompt that matched nothing, which
/// is most prompts, and it spent roughly 200 bytes per turn to report having
/// nothing to say. Silence carries the same information at zero cost — the
/// reader who wants to know either way calls `memory_recall`.
/// What: one italic Markdown line naming the count and pointing at
/// `memory_recall`. Only ever appended after at least one rendered drawer.
/// Test: `compose_injection_announces_withheld_drawers`,
/// `compose_injection_is_silent_when_everything_was_withheld`.
pub(super) fn withheld_notice(withheld: usize) -> String {
    let plural = if withheld == 1 { "memory" } else { "memories" };
    format!(
        "_({withheld} further {plural} withheld below the relevance floor — \
         call `memory_recall` for the full ranked set.)_"
    )
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
/// `injection_caps_rendered_tag_bytes`,
/// `prompt_context_injection_has_no_provenance_tags`.
pub(super) fn render_tags(tags: &[String]) -> Option<String> {
    let topical: Vec<&String> = tags
        .iter()
        .filter(|t| !crate::attribution::is_creator_tag(t))
        .collect();
    if topical.is_empty() {
        return None;
    }
    // Room the `+N more` marker and the `)_` close will need. Reserving it up
    // front is what keeps the *finished* suffix inside the budget, rather than
    // only the part written before the marker.
    const TRAILER_RESERVE: usize = ", +99 more)_".len();
    let mut out = String::from("  _(tags: ");
    let mut shown = 0usize;
    for tag in &topical {
        // `, ` separator (after the first) + two backticks + the tag itself.
        let width = usize::from(shown > 0) * 2 + 2 + tag.chars().count();
        let projected = out.chars().count() + width + TRAILER_RESERVE;
        if shown == MAX_RENDERED_TAGS || (shown > 0 && projected > MAX_RENDERED_TAG_CHARS) {
            break;
        }
        if shown > 0 {
            out.push_str(", ");
        }
        out.push('`');
        out.push_str(tag);
        out.push('`');
        shown += 1;
    }
    if let Some(hidden) = topical.len().checked_sub(shown).filter(|n| *n > 0) {
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
