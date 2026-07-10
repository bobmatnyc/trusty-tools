//! Post-render output polishing: comment stripping + omit-empty (#2342 / wave 2).
//!
//! Why: owner feedback on the first generated reports was twofold — inline
//! template instruction comments leaked into every output, and walls of "not
//! stated in source data" made the document read as an empty skeleton.  This pass
//! runs AFTER the deterministic fill (and before the appended status notes) to
//! (a) strip every template HTML comment except the semantic `<!-- dataset: … -->`
//! markers downstream tooling lifts tables by, and (b) drop rows/bullets whose
//! only value is the honesty marker, collapse fully-empty sections to a single
//! line, and collect every dropped field into a compact "Data gaps" list in the
//! Gaps & Caveats section.  The honesty rule is unchanged — nothing is invented;
//! empty scaffolding is simply not rendered.
//! What: [`polish`] composes [`strip_template_comments`], the omit-empty line
//! pass, section collapse, and Gaps & Caveats regeneration.
//! Test: `polish_tests.rs` covers comment stripping (dataset markers survive),
//! marker-row drop, empty-section collapse, and the gaps list.

use super::fill::HONESTY_MARKER;

/// The line rendered in place of a section whose every data row was omitted.
const COLLAPSE_LINE: &str = "_No data available — see Gaps & Caveats._";

/// The heading text (without the `##` prefix / number) of the gaps section.
const GAPS_HEADING_NEEDLE: &str = "Gaps & Caveats";

/// Run the full post-render polish pass over filled markdown.
///
/// Why: the single entry point the reporter calls after filling the template and
/// before appending the synthesis/benchmark status notes (which must not be
/// subject to omit-empty).
/// What: strips non-dataset comments, drops honesty-marker rows/bullets while
/// collecting the dropped field labels, collapses emptied sections, and rewrites
/// the Gaps & Caveats section body with the compact gap list.
/// Test: `polish_tests.rs::polish_end_to_end`.
pub fn polish(rendered: &str) -> String {
    let stripped = strip_template_comments(rendered);
    let (body, gaps) = omit_empty(&stripped);
    let out = render_gaps_section(&body, &gaps);
    // The line-based passes join with `\n` and drop the trailing newline; restore
    // it when the input had one so exact-output expectations hold.
    if rendered.ends_with('\n') && !out.ends_with('\n') {
        format!("{out}\n")
    } else {
        out
    }
}

// ─── Comment stripping (#2342.1) ────────────────────────────────────────────

/// Strip every template HTML comment from output except `<!-- dataset: … -->`.
///
/// Why: inline instructional comments ("One paragraph, deal-relevant…", "extend
/// to 4-5 rows…") are authoring guidance, not report content, and must never
/// reach a generated report; the dataset markers, by contrast, are SEMANTIC —
/// downstream charting tools lift tables by them — so they are preserved.
/// What: scans the text, removing each `<!-- … -->` span whose inner content does
/// not begin with `dataset:` (after trimming).  Comments inside fenced code
/// blocks (```` ``` ````) are left untouched (out of scope).  A blank line left
/// dangling where a whole-line comment was removed is collapsed.
/// Test: `polish_tests.rs::{strips_instructional_comments, keeps_dataset_markers,
/// keeps_comments_in_code_fences}`.
pub fn strip_template_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let mut in_fence = false;
    let mut at_line_start = true;

    while i < text.len() {
        let rest = &text[i..];

        // Track fenced code blocks: a ``` at the start of a line toggles state.
        if at_line_start && rest.starts_with("```") {
            in_fence = !in_fence;
            out.push_str("```");
            i += 3;
            at_line_start = false;
            continue;
        }

        if !in_fence
            && rest.starts_with("<!--")
            && let Some(rel_end) = rest.find("-->")
        {
            let inner = &rest[4..rel_end];
            if inner.trim_start().starts_with("dataset:") {
                // Semantic marker — keep verbatim (dataset markers never nest).
                let end = rel_end + 3;
                out.push_str(&rest[..end]);
                i += end;
                at_line_start = false;
                continue;
            }
            // Instructional comment — drop it entirely.  Balance nested `<!--`
            // so an embedded `<!-- dataset: … -->` example (literal documentation
            // text) does not end the strip early and leave residue.
            if let Some(end) = balanced_comment_len(rest) {
                i += end;
                continue;
            }
        }

        // Advance by one whole char so multi-byte UTF-8 (e.g. provenance markers)
        // is never sliced mid-codepoint.
        let ch = rest.chars().next().expect("non-empty rest");
        out.push(ch);
        at_line_start = ch == '\n';
        i += ch.len_utf8();
    }

    collapse_blank_runs(&out)
}

/// Byte length of an HTML comment starting at the head of `rest`, through its
/// matching `-->`, balancing nested `<!--`/`-->` pairs (like the leading-comment
/// stripper).  `None` when the comment is never closed.
///
/// Why: an instructional comment may embed a literal `<!-- dataset: … -->`
/// example as documentation text; a naive first-`-->` scan would cut inside it
/// and leak the remainder into the output.
/// What: `rest` must start with `<!--`; depth-balances to the outer close.
/// Test: `polish_tests.rs::strips_comment_with_embedded_close`.
fn balanced_comment_len(rest: &str) -> Option<usize> {
    let mut depth = 1i32;
    let mut cur = 4usize; // past the opening "<!--"
    loop {
        let next_open = rest[cur..].find("<!--").map(|x| x + cur);
        let next_close = rest[cur..].find("-->").map(|x| x + cur)?;
        match next_open {
            Some(o) if o < next_close => {
                depth += 1;
                cur = o + 4;
            }
            _ => {
                depth -= 1;
                cur = next_close + 3;
                if depth == 0 {
                    return Some(cur);
                }
            }
        }
    }
}

/// Collapse runs of 3+ consecutive newlines (left by removed whole-line comments)
/// into a single blank line, preserving the rest of the layout.
fn collapse_blank_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut newlines = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            newlines += 1;
            if newlines <= 2 {
                out.push(ch);
            }
        } else {
            newlines = 0;
            out.push(ch);
        }
    }
    out
}

// ─── Omit-empty (#2342.4) ───────────────────────────────────────────────────

/// Drop honesty-marker rows/bullets and collapse emptied sections.
///
/// Why: a table row whose value is only the honesty marker, or a whole section
/// with no data, is empty scaffolding — dropping it (and listing the field under
/// Gaps) is what turns the report from a template into a document.
/// What: buffers each markdown table, drops body rows whose value cells are all
/// the honesty marker, and — when a table loses every body row — replaces it with
/// a single collapse line; drops marker-only bullets; records every dropped
/// field/section label as a gap.  The Gaps & Caveats section is left untouched
/// here (it is regenerated wholesale afterwards).  Returns the rewritten body and
/// the ordered, de-duplicated gap labels.
/// Test: `polish_tests.rs::{drops_marker_rows, collapses_empty_section}`.
fn omit_empty(text: &str) -> (String, Vec<String>) {
    let mut out: Vec<String> = Vec::new();
    let mut gaps: Vec<String> = Vec::new();
    let mut section = String::new();
    let mut in_gaps_section = false;

    let lines: Vec<&str> = text.lines().collect();
    let mut idx = 0usize;
    while idx < lines.len() {
        let line = lines[idx];
        let trimmed = line.trim();

        if let Some(heading) = heading_text(trimmed) {
            section = heading.to_string();
            in_gaps_section = heading.contains(GAPS_HEADING_NEEDLE);
            out.push(line.to_string());
            idx += 1;
            continue;
        }

        // Inside the Gaps section: leave every line as-is (regenerated later).
        if in_gaps_section {
            out.push(line.to_string());
            idx += 1;
            continue;
        }

        // A markdown table: buffer the contiguous run of `|`-lines and process.
        if is_table_line(trimmed) {
            let start = idx;
            while idx < lines.len() && is_table_line(lines[idx].trim()) {
                idx += 1;
            }
            let table = &lines[start..idx];
            process_table(table, &section, &mut out, &mut gaps);
            continue;
        }

        // A marker-only bullet (e.g. an unfilled GREEN topic): drop + record.
        if is_marker_bullet(trimmed) {
            push_gap(&mut gaps, &section);
            idx += 1;
            continue;
        }

        // A standalone honesty-marker paragraph (e.g. an un-synthesised executive
        // summary): drop it and record the section as a gap.
        if trimmed == HONESTY_MARKER {
            push_gap(&mut gaps, &section);
            idx += 1;
            continue;
        }

        out.push(line.to_string());
        idx += 1;
    }

    let collapsed = collapse_empty_sections(&out, &mut gaps);
    (collapse_blank_runs(&collapsed), dedup(gaps))
}

/// Process one buffered markdown table, emitting survivors or a collapse line.
///
/// Why: dropping individual marker rows and, when none survive, collapsing the
/// whole table keeps the omit-empty logic in one place with correct
/// header/separator handling.
/// What: identifies the header + separator (if any), filters body rows whose
/// value cells are all the honesty marker (recording a gap per dropped row), and
/// pushes the header + separator + surviving rows — or nothing, leaving the empty
/// section for [`collapse_empty_sections`] to note.
/// Test: `polish_tests.rs::{drops_marker_rows, collapses_empty_section}`.
fn process_table(table: &[&str], section: &str, out: &mut Vec<String>, gaps: &mut Vec<String>) {
    // Find the separator row index (if the table has a header).
    let sep_idx = table.iter().position(|l| is_separator_row(l.trim()));
    let (head_end, body_start) = match sep_idx {
        Some(s) => (s + 1, s + 1),
        None => (0, 0),
    };

    let mut body: Vec<&str> = Vec::new();
    for line in &table[body_start..] {
        let cells = cells_of(line.trim());
        if is_all_marker_value(&cells) {
            push_gap(gaps, &row_gap_label(&cells, section));
        } else {
            body.push(line);
        }
    }

    if body.is_empty() {
        // Whole table collapsed — the section-collapse pass adds the note if the
        // section is now entirely empty; here we simply omit the skeleton.
        return;
    }
    for line in &table[..head_end] {
        out.push(line.to_string());
    }
    for line in body {
        out.push(line.to_string());
    }
}

/// The nesting level assigned to a bold-only pseudo-heading line (e.g.
/// `**Health-Factor Scores**`, `**Benchmark Position**`) — deeper than any real
/// `#`-heading (max level 6) so it never terminates an enclosing `#` section's
/// span, but terminates its OWN span at the next real heading or the next
/// sibling pseudo-heading (live-QA wave-2 defect #4).
const PSEUDO_HEADING_LEVEL: usize = 100;

/// A structural boundary line for the collapse pass: either a `#`-heading
/// (`level` = hash count) or a bold-only pseudo-heading (`level` =
/// [`PSEUDO_HEADING_LEVEL`]).  Returns the boundary's level and display text.
///
/// Why: the bundled templates use bold-wrapped lines (`**Profile**`,
/// `**Health-Factor Scores**`) as sub-headings within a `###` per-application
/// block; without recognising them, an orphaned bold label whose table was
/// entirely dropped rendered with nothing beneath it and no collapse note.
/// What: `#`-headings via [`heading_text`]; a bold pseudo-heading via
/// [`bold_heading_text`]. `None` for any other line.
/// Test: `polish_tests.rs::{collapses_orphaned_bold_pseudo_heading,
/// parent_with_populated_child_not_collapsed}`.
fn boundary(trimmed: &str) -> Option<(usize, &str)> {
    if let Some(text) = heading_text(trimmed) {
        let level = trimmed.len() - trimmed.trim_start_matches('#').len();
        return Some((level, text));
    }
    bold_heading_text(trimmed).map(|text| (PSEUDO_HEADING_LEVEL, text))
}

/// The inner text of a bold-only pseudo-heading line (`**Text**` with nothing
/// else on the line), or `None`.
fn bold_heading_text(trimmed: &str) -> Option<&str> {
    let inner = trimmed.strip_prefix("**")?.strip_suffix("**")?;
    if inner.is_empty() || inner.contains("**") {
        None
    } else {
        Some(inner)
    }
}

/// Replace any section (real heading or bold pseudo-heading) whose body has no
/// content with a collapse line — recursively, so a parent is never falsely
/// collapsed just because its first child happens to be a deeper boundary.
///
/// Why: after row/table/bullet dropping, a boundary can be left immediately
/// followed by the next boundary (or the trailing `---`) with nothing between —
/// the "sections with zero data collapse to one line" rule. The PREVIOUS
/// single-pass scan terminated a heading's body at the next heading of ANY
/// level, so a `##` parent immediately followed by a populated `###` child was
/// scanned as empty (zero lines examined before hitting the child heading) and
/// incorrectly collapsed right above the child's real content (live-QA wave-2
/// defect #2). Recursion fixes this: a level-L boundary's span runs until the
/// next boundary of level <= L (or `---`/EOF); nested deeper boundaries are
/// processed first, and their survival (or collapse) is decided before the
/// parent's own has-content verdict is computed.
/// What: delegates to [`collapse_recursive`], excluding the Gaps & Caveats
/// section (left untouched; regenerated wholesale afterwards).
/// Test: `polish_tests.rs::{collapses_empty_section,
/// parent_with_populated_child_not_collapsed,
/// collapses_orphaned_bold_pseudo_heading}`.
fn collapse_empty_sections(lines: &[String], gaps: &mut Vec<String>) -> String {
    let (rendered, _has_content) = collapse_recursive(lines, gaps);
    rendered.join("\n")
}

/// Recursively collapse empty boundary spans within `lines`, returning the
/// rendered lines and whether this span (as a whole) has any content.
///
/// Why: a boundary's own emptiness verdict depends on whether ITS descendants
/// (recursively) have content — a `##` parent has content whenever a nested
/// `###`/bold-pseudo child survives non-collapsed, even if the parent's own
/// direct text is empty.
/// What: walks `lines` once; a boundary line recurses into its own span first,
/// then either re-emits the child span (child had content — this also marks the
/// parent as having content) or replaces it with [`COLLAPSE_LINE`] + a recorded
/// gap (child was empty — does NOT count toward the parent's own content). The
/// Gaps & Caveats section is copied through untouched (never collapsed here).
/// Test: `polish_tests.rs::{parent_with_populated_child_not_collapsed,
/// collapses_orphaned_bold_pseudo_heading}`.
fn collapse_recursive(lines: &[String], gaps: &mut Vec<String>) -> (Vec<String>, bool) {
    let mut out: Vec<String> = Vec::new();
    let mut has_content = false;
    let mut idx = 0usize;

    while idx < lines.len() {
        let line = &lines[idx];
        let trimmed = line.trim();

        if trimmed == "---" {
            out.push(line.clone());
            idx += 1;
            continue;
        }

        let Some((level, heading)) = boundary(trimmed) else {
            if !trimmed.is_empty() {
                has_content = true;
            }
            out.push(line.clone());
            idx += 1;
            continue;
        };

        out.push(line.clone());
        idx += 1;

        let body_start = idx;
        while idx < lines.len() {
            let t = lines[idx].trim();
            if t == "---" {
                break;
            }
            if let Some((child_level, _)) = boundary(t)
                && child_level <= level
            {
                break;
            }
            idx += 1;
        }
        let child_slice = &lines[body_start..idx];

        if heading.contains(GAPS_HEADING_NEEDLE) {
            // Left untouched here — regenerated wholesale after this pass.
            out.extend(child_slice.iter().cloned());
            continue;
        }

        let (rendered_child, child_has_content) = collapse_recursive(child_slice, gaps);
        if child_has_content {
            out.extend(rendered_child);
            has_content = true;
        } else {
            push_gap(gaps, heading);
            out.push(String::new());
            out.push(COLLAPSE_LINE.to_string());
            out.push(String::new());
        }
    }

    (out, has_content)
}

// ─── Gaps & Caveats regeneration ────────────────────────────────────────────

/// Rewrite the Gaps & Caveats section body with the compact gap list.
///
/// Why: instead of a wall of `not stated` bullets, one compact line names every
/// omitted field so a reader sees the gaps at a glance while the body stays lean.
/// What: locates the Gaps & Caveats heading, replaces every line from after it up
/// to the next `---` / heading / EOF with a one-line "Data gaps: …" summary (or a
/// "no material gaps" note when the list is empty).  When the section is absent
/// (a custom template), the body is returned unchanged.
/// Test: `polish_tests.rs::gaps_section_lists_dropped_fields`.
fn render_gaps_section(text: &str, gaps: &[String]) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some(head_idx) = lines
        .iter()
        .position(|l| heading_text(l.trim()).is_some_and(|h| h.contains(GAPS_HEADING_NEEDLE)))
    else {
        return text.to_string();
    };

    // Find the end of the section body (next `---` or heading, else EOF).
    let mut end = head_idx + 1;
    while end < lines.len() {
        let t = lines[end].trim();
        if t == "---" || heading_text(t).is_some() {
            break;
        }
        end += 1;
    }

    let mut out: Vec<String> = lines[..=head_idx].iter().map(|s| s.to_string()).collect();
    out.push(String::new());
    if gaps.is_empty() {
        out.push("No material data gaps: every templated field was populated from measured, declared, or inferred data.".to_string());
    } else {
        out.push(format!("Data gaps: {}.", gaps.join(", ")));
    }
    out.push(String::new());
    out.extend(lines[end..].iter().map(|s| s.to_string()));
    out.join("\n")
}

// ─── Line/cell helpers ──────────────────────────────────────────────────────

/// Return the heading text (after the `#`s and any leading section number) for a
/// markdown heading line, or `None` when the line is not a heading.
fn heading_text(trimmed: &str) -> Option<&str> {
    let rest = trimmed.trim_start_matches('#');
    if rest.len() == trimmed.len() {
        return None; // no leading '#'
    }
    Some(rest.trim())
}

/// True when a line belongs to a markdown pipe table.
fn is_table_line(trimmed: &str) -> bool {
    trimmed.starts_with('|')
}

/// True when a table line is the `|---|---|` separator row.
fn is_separator_row(trimmed: &str) -> bool {
    let inner = trimmed.trim_matches('|');
    !inner.is_empty()
        && inner
            .chars()
            .all(|c| matches!(c, '-' | ':' | '|' | ' ' | '\t'))
}

/// Split a table row into trimmed cell strings (outer pipes dropped).
fn cells_of(trimmed: &str) -> Vec<String> {
    let inner = trimmed.trim_matches('|');
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

/// True when every value cell (all cells after the first) is the honesty marker.
///
/// Why: a metadata row `| Label | marker |` and an unfilled repeatable row
/// `| marker | marker | … |` are both empty scaffolding; keying on the value
/// cells (not the label) drops both while preserving a row with any real value.
fn is_all_marker_value(cells: &[String]) -> bool {
    match cells.len() {
        0 => false,
        1 => cells[0] == HONESTY_MARKER,
        _ => cells[1..].iter().all(|c| c == HONESTY_MARKER),
    }
}

/// The gap label for a dropped row: its first cell when meaningful, else the
/// enclosing section heading.
///
/// Why: a two-column metadata row's label ("Client") is the useful gap name, but
/// a numeric index column (the Top Risks `#` column: "1", "2", "3") is noise —
/// those fall back to the section so the gaps list stays readable.
fn row_gap_label(cells: &[String], section: &str) -> String {
    match cells.first() {
        Some(first)
            if first != HONESTY_MARKER
                && !first.is_empty()
                && !first.chars().all(|c| c.is_ascii_digit()) =>
        {
            first.clone()
        }
        _ => section.to_string(),
    }
}

/// True when a line is a bullet whose only content is the honesty marker.
fn is_marker_bullet(trimmed: &str) -> bool {
    trimmed
        .strip_prefix("- ")
        .map(str::trim)
        .is_some_and(|c| c == HONESTY_MARKER)
}

/// Push a non-empty gap label.
fn push_gap(gaps: &mut Vec<String>, label: &str) {
    let label = label.trim();
    if !label.is_empty() {
        gaps.push(label.to_string());
    }
}

/// De-duplicate gap labels, preserving first-seen order.
fn dedup(gaps: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    gaps.into_iter()
        .filter(|g| seen.insert(g.clone()))
        .collect()
}

#[cfg(test)]
#[path = "polish_tests.rs"]
mod tests;
