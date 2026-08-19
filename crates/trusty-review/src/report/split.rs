//! Cutting one rendered document into the code-review report and a standalone
//! authorship report (#6046).
//!
//! Why: owner request 2026-08-19 — "I'd like to see the outputs of the code
//! review separate from authorship." Authorship is derived by tga from git
//! history and never reads source, so a diligence reader wants it as its own
//! deliverable rather than a section two thirds of the way down the
//! code-review document.
//! What: [`split_authorship`] removes the Authorship & Key-Person Risk section
//! from a rendered, polished document and hands it back separately;
//! [`authorship_document`] wraps that section as a document with its own title
//! and provenance legend. Both run inside `Reporter::render_documents`, after
//! `polish` (so the section carries the same dropped-marker and no-data
//! treatment it had inside the one document) and BEFORE
//! `contents_links::inject` (so the code-review report's jump list never links
//! a heading it no longer carries).
//! Test: `split_tests.rs`.

use super::provenance;

/// Text that identifies the authorship section's level-2 heading.
///
/// Why: the bundled template heads it `## Authorship & Key-Person Risk`, but an
/// operator template may number or reword it. Matching one distinctive word
/// keeps a renamed heading splitting instead of silently landing back in the
/// code-review document.
const AUTHORSHIP_HEADING_NEEDLE: &str = "Authorship";

/// Filename suffix the authorship document is written under.
pub const AUTHORSHIP_STEM_SUFFIX: &str = "-authorship";

/// Remove the authorship section from a rendered document.
///
/// Why: the split is a post-render cut rather than a second template, so both
/// documents are filled from one scope and cannot drift in wording, provenance
/// tagging, or honesty-marker handling.
/// What: scans level-2 (`## `) headings in document order, tracking fenced code
/// blocks with the same toggle guard [`super::contents_links::inject`] uses so
/// a `## `-prefixed line inside quoted source bytes is never mistaken for a
/// heading. Everything from the authorship heading up to the next level-2
/// heading (or end of document) is returned as the second element; everything
/// else is the first. A document with no authorship heading — a custom template
/// that omits the section — returns `(rendered, None)` unchanged. A run with no
/// authorship data still splits: `polish` keeps the heading and replaces the
/// body with its no-data line, which is what the second document then carries.
/// Test: `split_tests::{splits_the_authorship_section,
/// no_authorship_heading_leaves_the_document_whole,
/// a_fenced_heading_line_is_not_split_on,
/// a_trailing_authorship_section_splits_to_the_end}`.
pub fn split_authorship(rendered: &str) -> (String, Option<String>) {
    let mut kept: Vec<&str> = Vec::new();
    let mut section: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut in_section = false;

    for line in rendered.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start().starts_with("```") {
            in_fence = !in_fence;
        } else if !in_fence && let Some(text) = trimmed.strip_prefix("## ") {
            in_section = text.contains(AUTHORSHIP_HEADING_NEEDLE);
        }
        if in_section {
            section.push(line);
        } else {
            kept.push(line);
        }
    }

    if section.is_empty() {
        return (rendered.to_string(), None);
    }
    (rejoin(&kept, rendered), Some(rejoin(&section, rendered)))
}

/// Rejoin split lines, restoring the source's trailing newline.
///
/// Why: `str::lines` drops the final newline, and a markdown file that stops
/// mid-line reads as truncated to every downstream tool.
fn rejoin(lines: &[&str], source: &str) -> String {
    let mut out = lines.join("\n");
    if source.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Wrap an extracted authorship section as a standalone document.
///
/// Why: the section alone has no title, no date, and no key to the provenance
/// markers its own table cells carry — everything a reader needs travelled in
/// the code-review document's header. This adds exactly that much back and
/// nothing else.
/// What: promotes the section's `## ` heading to the document's `# ` title,
/// suffixed with the report codename, then the provenance legend, a line naming
/// the companion report and the generation date, and the section body verbatim.
/// Test: `split_tests::{authorship_document_carries_title_and_legend,
/// authorship_document_keeps_the_section_body}`.
pub fn authorship_document(title: &str, generated_date: &str, section: &str) -> String {
    let mut lines = section.lines();
    let heading = lines
        .next()
        .and_then(|l| l.trim_end().strip_prefix("## "))
        .map(str::trim)
        .unwrap_or("Authorship & Key-Person Risk")
        .to_string();
    let body = lines.collect::<Vec<_>>().join("\n");

    format!(
        "# {heading}: {title}\n\n{legend}\n\nCompanion to the technical \
         due-diligence report for {title}; generated {generated_date}.\n\n{body}\n",
        legend = provenance::LEGEND,
        body = body.trim(),
    )
}

#[cfg(test)]
#[path = "split_tests.rs"]
mod tests;
