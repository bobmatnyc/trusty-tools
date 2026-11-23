//! The bounded excerpt every backend receives.
//!
//! Why: plan section 1, step 4. A model call is paid per token, so the `llm`
//! mode sends the opening text plus the most central sections, never the
//! whole of a document larger than `summary.max_input_chars`.
//!
//! What: [`bounded_excerpt`] returns the whole text when it fits. Otherwise
//! it keeps the opening (about a third of the budget, cut at a line end),
//! ranks the outline sections after it by vocabulary shared with the other
//! sections (no model), and adds whole sections in rank order while they fit, cutting
//! the first one that does not at a line end. Chosen parts are joined in
//! document order with a `[…]` line between non-adjacent parts.

use std::collections::HashMap;

use crate::extractive::content_words;
use crate::segment::Section;
use crate::text::LineIndex;

const GAP: &str = "\n[…]\n";
/// Sections shorter than this are not worth a gap marker.
const MIN_PART_CHARS: usize = 80;

/// The excerpt of `text` sent to a backend.
///
/// Postcondition: `chars(result) <= max_chars` (for `max_chars >= 1`); the
/// result equals `text` when `text` has at most `max_chars` characters;
/// otherwise it starts with a prefix of `text`.
/// Test: `excerpt_is_bounded_for_huge_documents`, `excerpt_keeps_short_text_whole`,
/// `excerpt_adds_central_sections_in_order`
pub fn bounded_excerpt(text: &str, sections: &[Section], max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let index = LineIndex::new(text);
    let head = cut_at_line(text, (max_chars / 3).max(1));
    let head_lines = head.matches('\n').count();
    let mut budget = max_chars.saturating_sub(head.chars().count());

    // Section bodies after the opening, with their first line number.
    let bodies: Vec<(usize, &str)> = sections
        .iter()
        .filter(|s| s.lines.end > head_lines)
        .filter_map(|s| {
            let start = s.lines.start.max(head_lines + 1);
            let span = crate::text::Span::new(start, s.lines.end);
            index.slice(span).ok().map(|body| (start, body))
        })
        .filter(|(_, b)| !b.trim().is_empty())
        .collect();

    // Centrality: a section scores by how many other sections share its
    // words (distinct words, so repetition inside one section adds nothing).
    let mut freq: HashMap<String, f64> = HashMap::new();
    let words: Vec<Vec<String>> = bodies
        .iter()
        .map(|(_, b)| {
            let mut ws = content_words(b);
            ws.sort_unstable();
            ws.dedup();
            ws
        })
        .collect();
    for w in words.iter().flatten() {
        *freq.entry(w.clone()).or_default() += 1.0;
    }
    let mut ranked: Vec<(usize, f64)> = words
        .iter()
        .enumerate()
        .map(|(i, ws)| {
            let score = if ws.is_empty() {
                0.0
            } else {
                ws.iter().map(|w| freq[w]).sum::<f64>() / (ws.len() as f64).sqrt()
            };
            (i, score)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

    let gap = GAP.chars().count();
    let mut chosen: Vec<(usize, String)> = Vec::new();
    for (i, _) in ranked {
        if budget <= gap + MIN_PART_CHARS {
            break;
        }
        let (start, body) = bodies[i];
        let room = budget - gap;
        let part = if body.chars().count() <= room {
            body.to_string()
        } else {
            cut_at_line(body, room).to_string()
        };
        if part.trim().chars().count() < MIN_PART_CHARS.min(body.trim().chars().count()) {
            continue;
        }
        budget -= gap + part.chars().count();
        chosen.push((start, part));
    }
    chosen.sort_by_key(|(start, _)| *start);

    let mut out = head.to_string();
    let mut next_line = head_lines + 1;
    for (start, part) in chosen {
        if start != next_line || !out.ends_with('\n') {
            out.push_str(GAP);
        }
        next_line = start + part.matches('\n').count();
        out.push_str(&part);
    }
    // Adjacent parts add no gap, so the sum can only be smaller; this is
    // the contract, checked in debug builds.
    debug_assert!(out.chars().count() <= max_chars);
    out
}

/// The first `max_chars` characters of `text`, cut back to a line end when
/// one exists.
fn cut_at_line(text: &str, max_chars: usize) -> &str {
    let Some((cut, _)) = text.char_indices().nth(max_chars) else {
        return text;
    };
    match text[..cut].rfind('\n') {
        Some(nl) => &text[..=nl],
        None => &text[..cut],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::{SegmentOptions, segment};

    fn sections(text: &str) -> Vec<Section> {
        segment(
            text,
            SegmentOptions {
                markdown: true,
                max_section_lines: 20,
                hard_bound: false,
            },
        )
    }

    #[test]
    fn excerpt_keeps_short_text_whole() {
        let t = "# A\n\nshort text\n";
        assert_eq!(bounded_excerpt(t, &sections(t), 1000), t);
        assert_eq!(cut_at_line("ab\ncd\nef", 7), "ab\ncd\n");
        assert_eq!(cut_at_line("abcdef", 3), "abc");
        assert_eq!(cut_at_line("ééé", 2), "éé");
    }

    #[test]
    fn excerpt_is_bounded_for_huge_documents() {
        let mut t = String::from("# Opening\n\nThe opening paragraph says what this is.\n\n");
        for i in 0..5000 {
            t.push_str(&format!(
                "## Part {i}\n\nPart {i} covers harbor lights and survey counts in detail.\n\n"
            ));
        }
        for max in [200, 1000, 5000] {
            let e = bounded_excerpt(&t, &sections(&t), max);
            assert!(e.chars().count() <= max, "{} > {max}", e.chars().count());
            assert!(e.starts_with("# Opening\n"), "{e}");
            assert!(t.chars().count() > 50 * max);
        }
    }

    #[test]
    fn excerpt_adds_central_sections_in_order() {
        let filler = "Weather notes about clouds and rain falling softly today.\n".repeat(8);
        let t = format!(
            "# Intro\n\n{}\n## Harbor\n\nHarbor lights survey harbor lights count harbor.\n\
             Harbor lights survey harbor lights count harbor.\n\n## Weather\n\n{filler}\n\
             ## Harbor again\n\nHarbor lights survey count lights harbor again.\n\
             Harbor lights survey count lights harbor again.\n",
            "Intro line for the document.\n".repeat(10)
        );
        let e = bounded_excerpt(&t, &sections(&t), 700);
        assert!(e.chars().count() <= 700);
        let first = e.find("## Harbor\n").expect("central section kept");
        let second = e.find("## Harbor again").expect("central section kept");
        assert!(first < second, "document order");
        assert!(e.contains("[…]"));
    }
}
