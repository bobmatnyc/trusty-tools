//! Pure text-shaping helpers shared by [`crate::app`]'s chat mutators and
//! [`crate::widgets::scrollback`]'s renderer.
//!
//! Why: LLM/agent responses routinely arrive with extra leading/trailing
//! blank lines (a model's closing paragraph break) and interior `\n\n`
//! paragraph gaps that read as wasted vertical space once rendered into a
//! fixed-height terminal pane. Both problems are pure string transforms with
//! no dependency on `ReplApp` or ratatui, so they live in their own module
//! rather than being duplicated between the state mutator that trims stored
//! text ([`crate::app::ReplApp::push_assistant`]) and any future consumer
//! that needs the same shaping. Extracted verbatim (behavior-preserving) from
//! `crates/trusty-agents/src/repl/tui/helpers.rs`'s `trim_surrounding_blank_lines`/
//! `strip_interior_blank_lines` (DOC-50 §3.1/§5 Slice 4 migration).
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4).

/// Strip leading and trailing whitespace-only lines from a multi-line string.
///
/// Why: Assistant responses regularly include extra `\n\n` at the head or
/// tail. Rendering them verbatim leaves visible blank rows in the chat
/// scrollback, which reads as a UI bug. Trimming once at the boundary keeps
/// interior blank lines (which carry paragraph-break meaning) intact.
/// What: Trims trailing whitespace first, then drops leading whitespace-only
/// lines one at a time. Interior blank lines are untouched.
/// Test: [`tests::trim_surrounding_blank_lines_strips_leading_and_trailing`],
/// [`tests::trim_surrounding_blank_lines_preserves_when_no_blanks`],
/// [`tests::trim_surrounding_blank_lines_empty_input`].
pub fn trim_surrounding_blank_lines(s: &str) -> String {
    let trimmed_end = s.trim_end();
    let mut start = 0usize;
    let bytes = trimmed_end.as_bytes();
    while start < bytes.len() {
        let nl = bytes[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| start + p);
        let line_end = nl.unwrap_or(bytes.len());
        let line = &trimmed_end[start..line_end];
        if line.trim().is_empty() {
            start = match nl {
                Some(p) => p + 1,
                None => bytes.len(),
            };
        } else {
            break;
        }
    }
    trimmed_end[start..].to_string()
}

/// Drop every whitespace-only line from within a response.
///
/// Why: Markdown-style double-newline paragraph breaks accumulate into
/// wasted vertical space in a terminal chat panel — the user already reads
/// consecutive paragraphs as one flowing thought, so the gap just pushes
/// later content off-screen sooner. Removing every interior blank produces a
/// tight, compact response block.
/// What: Returns a string where every whitespace-only line is dropped;
/// non-blank lines are preserved verbatim and rejoined with single `\n`s.
/// Test: [`tests::strip_interior_blank_lines_drops_all_blanks`],
/// [`tests::strip_interior_blank_lines_drops_single_blank`],
/// [`tests::strip_interior_blank_lines_treats_whitespace_only_as_blank`].
pub fn strip_interior_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut first = true;
    for line in s.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if !first {
            out.push('\n');
        }
        out.push_str(line);
        first = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_surrounding_blank_lines_strips_leading_and_trailing() {
        let input = "\n\n  \nhello\n\nworld\n\n  \n";
        let out = trim_surrounding_blank_lines(input);
        assert_eq!(out, "hello\n\nworld");
    }

    #[test]
    fn trim_surrounding_blank_lines_preserves_when_no_blanks() {
        assert_eq!(trim_surrounding_blank_lines("hi"), "hi");
        assert_eq!(trim_surrounding_blank_lines("a\nb"), "a\nb");
    }

    #[test]
    fn trim_surrounding_blank_lines_empty_input() {
        assert_eq!(trim_surrounding_blank_lines(""), "");
        assert_eq!(trim_surrounding_blank_lines("\n\n  \n"), "");
    }

    #[test]
    fn strip_interior_blank_lines_drops_all_blanks() {
        let input = "para1\n\n\npara2\n\n\n\npara3";
        let out = strip_interior_blank_lines(input);
        assert_eq!(out, "para1\npara2\npara3");
    }

    #[test]
    fn strip_interior_blank_lines_drops_single_blank() {
        let input = "para1\n\npara2";
        assert_eq!(strip_interior_blank_lines(input), "para1\npara2");
    }

    #[test]
    fn strip_interior_blank_lines_treats_whitespace_only_as_blank() {
        let input = "a\n   \n\t\nb";
        assert_eq!(strip_interior_blank_lines(input), "a\nb");
    }
}
