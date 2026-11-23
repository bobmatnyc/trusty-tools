//! Segments text into paragraphs and topic sections, with no model.
//!
//! Why: the outline's line and paragraph ranges let a reader fetch only the
//! part of a document it needs (plan section 5).
//!
//! What: a paragraph is a maximal run of non-blank lines. In Markdown, an ATX
//! heading is a paragraph of its own and a fenced code block stays one
//! paragraph across blank lines. A section starts at every Markdown heading
//! and whenever the current section has reached `max_section_lines`; it
//! always starts at a paragraph start, so the sections' line ranges tile the
//! document: no gap, no overlap, every line covered.

use serde::{Deserialize, Serialize};

use crate::text::{LineIndex, Span};

/// One topic in the outline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    /// Heading text, or the section's first line, at most 80 characters.
    pub title: String,
    /// The lines this section covers.
    pub lines: Span,
    /// The paragraphs that start in this section; `None` only for a
    /// document made entirely of blank lines.
    pub paragraphs: Option<Span>,
}

/// How to segment a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentOptions {
    /// Treat `#` headings and code fences as Markdown structure.
    pub markdown: bool,
    /// A section is split at the next paragraph once it spans this many
    /// lines. Must be at least 1.
    pub max_section_lines: usize,
    /// Also split inside a paragraph, so that no section spans more than
    /// `max_section_lines` lines (step 7: long PDF paragraphs and fenced
    /// blocks made single entries of many pages).
    pub hard_bound: bool,
}

#[derive(Debug)]
struct Paragraph {
    start: usize,
    heading: Option<String>,
}

const TITLE_CHARS: usize = 80;

/// Splits `text` into sections whose line ranges tile the document.
///
/// Why: the outline is the reference map the rest of the output points into.
/// What: returns an empty vector for an empty text; otherwise the first
/// section starts at line 1, each next section starts one line after the
/// previous one ends, and the last ends at the last line.
/// Test: `sections_tile_every_document`, `markdown_headings_start_sections`
pub fn segment(text: &str, opts: SegmentOptions) -> Vec<Section> {
    let index = LineIndex::new(text);
    let total = index.line_count();
    if total == 0 {
        return Vec::new();
    }
    let paragraphs = paragraphs(&index, opts.markdown);
    if paragraphs.is_empty() {
        return vec![Section {
            title: String::new(),
            lines: Span::new(1, total),
            paragraphs: None,
        }];
    }

    // Indices into `paragraphs` where each section begins.
    let max_lines = opts.max_section_lines.max(1);
    let mut starts = vec![0usize];
    for (i, p) in paragraphs.iter().enumerate().skip(1) {
        let section_start = paragraphs[*starts.last().expect("non-empty")].start;
        // With the hard bound, a section also ends before a paragraph that
        // would carry it past `max_lines`, so few sections need a cut.
        let end = paragraphs.get(i + 1).map_or(total, |n| n.start - 1);
        let full = if opts.hard_bound {
            end - section_start + 1 > max_lines
        } else {
            p.start - section_start >= max_lines
        };
        if p.heading.is_some() || full {
            starts.push(i);
        }
    }

    let mut sections = Vec::with_capacity(starts.len());
    for (k, &first) in starts.iter().enumerate() {
        let next = starts.get(k + 1).copied();
        let line_start = if k == 0 { 1 } else { paragraphs[first].start };
        let line_end = next.map_or(total, |n| paragraphs[n].start - 1);
        let last_para = next.unwrap_or(paragraphs.len()) - 1;
        let title = match &paragraphs[first].heading {
            Some(h) => h.clone(),
            None => index.line(paragraphs[first].start).trim().to_string(),
        };
        sections.push(Section {
            title: truncate_chars(&title, TITLE_CHARS),
            lines: Span::new(line_start, line_end),
            paragraphs: Some(Span::new(first + 1, last_para + 1)),
        });
    }
    if opts.hard_bound {
        sections = bound_sections(&index, &paragraphs, sections, max_lines);
    }
    sections
}

/// Cuts every section longer than `max_lines` into the fewest equal pieces
/// of at most `max_lines` lines. A later piece is titled by its first non-blank line
/// and lists the paragraphs that start in it (`None` when none does).
fn bound_sections(
    index: &LineIndex<'_>,
    paragraphs: &[Paragraph],
    sections: Vec<Section>,
    max_lines: usize,
) -> Vec<Section> {
    let mut out = Vec::with_capacity(sections.len());
    for s in sections {
        if s.lines.len() <= max_lines {
            out.push(s);
            continue;
        }
        // Equal pieces, so no piece is a short remainder.
        let pieces = s.lines.len().div_ceil(max_lines);
        let size = s.lines.len().div_ceil(pieces);
        let mut start = s.lines.start;
        while start <= s.lines.end {
            let end = (start + size - 1).min(s.lines.end);
            let title = if start == s.lines.start {
                s.title.clone()
            } else {
                let first = (start..=end)
                    .map(|n| index.line(n).trim())
                    .find(|l| !l.is_empty())
                    .unwrap_or("");
                truncate_chars(first, TITLE_CHARS)
            };
            // Paragraph numbers are 1-based indices into `paragraphs`.
            let first_p = paragraphs.partition_point(|p| p.start < start);
            let last_p = paragraphs.partition_point(|p| p.start <= end);
            let paras = (first_p < last_p).then(|| Span::new(first_p + 1, last_p));
            out.push(Section {
                title,
                lines: Span::new(start, end),
                paragraphs: paras,
            });
            start = end + 1;
        }
    }
    out
}

fn paragraphs(index: &LineIndex<'_>, markdown: bool) -> Vec<Paragraph> {
    let mut out = Vec::new();
    let mut in_paragraph = false;
    let mut in_fence = false;
    for n in 1..=index.line_count() {
        let line = index.line(n);
        let trimmed = line.trim();
        if markdown && is_fence(trimmed) {
            // A fence interrupts a paragraph and is one paragraph until it
            // closes, blank lines included.
            if !in_fence {
                out.push(Paragraph {
                    start: n,
                    heading: None,
                });
            }
            in_fence = !in_fence;
            in_paragraph = false;
            continue;
        }
        if in_fence {
            continue;
        }
        if trimmed.is_empty() {
            in_paragraph = false;
            continue;
        }
        if markdown && let Some(heading) = heading_text(line) {
            out.push(Paragraph {
                start: n,
                heading: Some(heading),
            });
            in_paragraph = false;
            continue;
        }
        if !in_paragraph {
            out.push(Paragraph {
                start: n,
                heading: None,
            });
            in_paragraph = true;
        }
    }
    out
}

fn is_fence(trimmed: &str) -> bool {
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

/// The text of an ATX heading (`# Title`), or `None`.
pub(crate) fn heading_text(line: &str) -> Option<String> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let hashes = rest.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after = &rest[hashes..];
    if !(after.is_empty() || after.starts_with([' ', '\t'])) {
        return None;
    }
    Some(after.trim().trim_end_matches('#').trim().to_string())
}

/// Whether a converted-document line is a footer or boilerplate that
/// makes a poor outline title: a copyright or rights line, a page-number
/// line, or a line with no letters (a bare price, number or date).
///
/// Test: `boilerplate_lines_are_not_titles`
pub(crate) fn is_boilerplate(line: &str) -> bool {
    let l = line.trim().to_lowercase();
    let words = || l.split_whitespace();
    l.is_empty()
        || l.starts_with('©')
        || l.starts_with("(c)")
        || l.starts_with("copyright")
        || l.contains("all rights reserved")
        || l.contains("confidential and proprietary")
        || (l.starts_with("page ") && words().count() <= 4 && l.chars().any(|c| c.is_ascii_digit()))
        || !l.chars().any(char::is_alphabetic)
        // A short line whose only letters are a currency or unit code
        // (`USD 2.58`, `$105,000 ACV`) is a value, not a heading.
        || (l.chars().any(|c| c.is_ascii_digit())
            && words().count() <= 2
            && words().filter(|w| w.chars().any(char::is_alphabetic)).all(|w| w.len() <= 3))
}

/// B4: the title of `lines` in a converted document: its first line that
/// is not [`is_boilerplate`], at most 80 characters, or empty when every
/// line is.
///
/// Test: `boilerplate_lines_are_not_titles`
pub fn meaningful_title(index: &LineIndex<'_>, lines: Span) -> String {
    let first = (lines.start..=lines.end)
        .map(|n| index.line(n).trim())
        .find(|l| !is_boilerplate(l))
        .unwrap_or("");
    truncate_chars(first, TITLE_CHARS)
}

/// Cuts `s` to at most `max` characters, marking a cut with `…`.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boilerplate_lines_are_not_titles() {
        for line in [
            "$2.58",
            "14%",
            "$105,000",
            "2025-11-14",
            "(c) 2025 Snowflake",
            "© 2025 Snowflake Inc. All rights reserved.",
            "Copyright 2024 Acme",
            "Page 3 of 12",
            "USD 2.58",
            "",
        ] {
            assert!(is_boilerplate(line), "{line:?}");
        }
        for line in ["Credit Pricing", "Section 2: 2025 plan", "Annex 17 budget"] {
            assert!(!is_boilerplate(line), "{line:?}");
        }
        let text = "$2.58\n(c) 2025 Snowflake\nCredit Pricing\n14%\n";
        let index = LineIndex::new(text);
        assert_eq!(meaningful_title(&index, Span::new(1, 4)), "Credit Pricing");
        assert_eq!(meaningful_title(&index, Span::new(1, 2)), "");
    }

    fn assert_tiles(text: &str, sections: &[Section]) {
        let total = LineIndex::new(text).line_count();
        if total == 0 {
            assert!(sections.is_empty());
            return;
        }
        let mut expected_start = 1;
        let mut expected_para = 1;
        for s in sections {
            assert_eq!(
                s.lines.start, expected_start,
                "gap or overlap: {sections:?}"
            );
            assert!(s.lines.end >= s.lines.start);
            expected_start = s.lines.end + 1;
            if let Some(p) = s.paragraphs {
                assert_eq!(p.start, expected_para, "paragraph gap: {sections:?}");
                expected_para = p.end + 1;
            }
        }
        assert_eq!(expected_start, total + 1, "lines not covered: {sections:?}");
    }

    /// Deterministic xorshift, so the test needs no dependency.
    fn next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    #[test]
    fn sections_tile_every_document() {
        let pieces = [
            "",
            "text line",
            "# Heading",
            "## Sub",
            "```",
            "    indented",
            "\r",
            "- item",
        ];
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..500 {
            let n = (next(&mut state) % 40) as usize;
            let mut text = String::new();
            for _ in 0..n {
                text.push_str(pieces[(next(&mut state) % pieces.len() as u64) as usize]);
                text.push('\n');
            }
            if next(&mut state).is_multiple_of(2) {
                text.pop();
            }
            for markdown in [true, false] {
                for max in [1, 3, 60] {
                    for hard_bound in [false, true] {
                        let opts = SegmentOptions {
                            markdown,
                            max_section_lines: max,
                            hard_bound,
                        };
                        let got = segment(&text, opts);
                        assert_tiles(&text, &got);
                        if hard_bound {
                            assert!(got.iter().all(|s| s.lines.len() <= max), "{got:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn markdown_headings_start_sections() {
        let text = "intro\n\n# One\nbody\n\n## Two\nmore\n```\n# not a heading\n\n```\n";
        let s = segment(
            text,
            SegmentOptions {
                markdown: true,
                max_section_lines: 60,
                hard_bound: true,
            },
        );
        let titles: Vec<_> = s.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["intro", "One", "Two"]);
        assert_eq!(s[1].lines, Span::new(3, 5));
        assert_eq!(s[2].lines, Span::new(6, 11));
        assert_eq!(s[2].paragraphs, Some(Span::new(4, 6)));
    }

    #[test]
    fn blank_only_document_is_one_section() {
        let s = segment(
            "\n\n",
            SegmentOptions {
                markdown: false,
                max_section_lines: 60,
                hard_bound: true,
            },
        );
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].lines, Span::new(1, 2));
        assert_eq!(s[0].paragraphs, None);
    }

    #[test]
    fn hard_bound_splits_a_long_paragraph() {
        // One 250-line paragraph (a PDF page run with no blank line).
        let text: String = (1..=250).map(|n| format!("line {n}\n")).collect();
        let opts = |hard_bound| SegmentOptions {
            markdown: false,
            max_section_lines: 60,
            hard_bound,
        };
        assert_eq!(segment(&text, opts(false)).len(), 1);
        let s = segment(&text, opts(true));
        assert_eq!(s.len(), 5);
        assert!(s.iter().all(|s| s.lines.len() <= 60));
        assert_eq!(s[1].title, "line 51");
        assert_eq!(s[0].paragraphs, Some(Span::new(1, 1)));
        assert_eq!(s[1].paragraphs, None);
        assert_tiles(&text, &s);
    }

    #[test]
    fn heading_text_parses_atx_only() {
        assert_eq!(heading_text("## Two ##").as_deref(), Some("Two"));
        assert_eq!(heading_text("#hashtag"), None);
        assert_eq!(heading_text("####### seven"), None);
    }
}
