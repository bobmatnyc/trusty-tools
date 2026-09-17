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
//! - [`SPEC-TTUI-05~draft`](docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4).

/// Shorten `text` to `width` columns, preferring `/` boundaries (#8164).
///
/// Why: a splash or status line that names an absolute path routinely
/// overflows its column, and clipping the tail removes the leaf — the one
/// component a reader is checking. Eliding whole path components instead
/// keeps the deepest directories intact, which is where a repository name
/// and a worktree name live.
///
/// What it GUARANTEES, and nothing more: when `text` contains `/` and `…/`
/// plus its last two components fit in `width`, the result keeps the final
/// components WHOLE — as many as fit, deepest first — behind that leading
/// `…/`. The two columns the prefix costs are part of the budget, so a
/// last-two that fits `width` but not `width - 2` still falls back.
/// Components nearer the filesystem root are dropped first, so a root-side
/// name can still be lost; only the tail is promised. Anything else (a path
/// whose own last two components overflow, or a string with no `/`) falls
/// back to character elision, which keeps both ends and replaces the middle
/// with `…`. `text` shorter than `width` is returned unchanged, and a
/// `width` of 0 still yields the one-character `…` — the ellipsis is never
/// dropped.
/// Test: `tests::elide_middle_keeps_whole_trailing_path_components`,
/// `tests::elide_middle_keeps_a_repo_name_that_is_the_leaf`,
/// `tests::elide_middle_falls_back_to_character_elision`,
/// `tests::elide_middle_handles_degenerate_widths`.
pub fn elide_middle(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if text.contains('/')
        && let Some(elided) = elide_path_components(text, width)
    {
        return elided;
    }
    elide_chars(text, width)
}

/// [`elide_middle`]'s path arm: `…/` plus the deepest components that fit.
///
/// Why kept separate: it can legitimately fail (a single component wider than
/// `width`), and `None` is what routes the caller to character elision rather
/// than to a path rendering that does not fit.
/// What: walks the components from the leaf backwards, accepting each while
/// `…/<kept>` stays within `width`. Returns `None` unless at least the last
/// TWO components were accepted — one lone directory name behind an ellipsis
/// says less than the head-and-tail character elision would.
/// Test: `tests::elide_middle_keeps_whole_trailing_path_components`,
/// `tests::elide_middle_falls_back_to_character_elision`.
fn elide_path_components(text: &str, width: usize) -> Option<String> {
    let components: Vec<&str> = text.split('/').filter(|c| !c.is_empty()).collect();
    let mut kept = 0usize;
    let mut rendered = String::new();
    for count in 1..=components.len() {
        let tail = components[components.len() - count..].join("/");
        let candidate = format!("…/{tail}");
        if candidate.chars().count() > width {
            break;
        }
        kept = count;
        rendered = candidate;
    }
    (kept >= 2).then_some(rendered)
}

/// [`elide_middle`]'s fallback: keep both ends, replace the middle with `…`.
///
/// What: splits the `width - 1` remaining budget between the two ends, the
/// odd character going to the head. Counts chars, not bytes. A `width` of 0
/// or 1 returns the bare `…`, so a zero budget is overshot by one column
/// rather than answered with an empty string.
/// Test: `tests::elide_middle_falls_back_to_character_elision`,
/// `tests::elide_middle_handles_degenerate_widths`.
fn elide_chars(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let keep = width - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// Strip leading and trailing whitespace-only lines from a multi-line string.
///
/// Why: Assistant responses regularly include extra `\n\n` at the head or
/// tail. Rendering them verbatim leaves visible blank rows in the chat
/// scrollback, which reads as a UI bug. Trimming once at the boundary keeps
/// interior blank lines (which carry paragraph-break meaning) intact.
/// What: Trims trailing whitespace first, then drops leading whitespace-only
/// lines one at a time. Interior blank lines are untouched.
/// Test: `tests::trim_surrounding_blank_lines_strips_leading_and_trailing`,
/// `tests::trim_surrounding_blank_lines_preserves_when_no_blanks`,
/// `tests::trim_surrounding_blank_lines_empty_input`.
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
/// Test: `tests::strip_interior_blank_lines_drops_all_blanks`,
/// `tests::strip_interior_blank_lines_drops_single_blank`,
/// `tests::strip_interior_blank_lines_treats_whitespace_only_as_blank`.
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

    /// The motivating case (#8164): a worktree path on an 80-column banner
    /// gets 54 columns. The deepest components must arrive WHOLE — a clipped
    /// tail would name neither the worktree nor the directory holding it.
    #[test]
    fn elide_middle_keeps_whole_trailing_path_components() {
        let path = "/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools/.claude/worktrees/agent-af14d84dd34577cee";
        let out = elide_middle(path, 54);
        assert!(out.chars().count() <= 54, "{out}");
        assert!(out.starts_with("…/"), "{out}");
        assert!(
            out.ends_with("/worktrees/agent-af14d84dd34577cee"),
            "the last two components must survive whole: {out}"
        );
        // Nothing is clipped mid-component: every retained segment is one of
        // the original path's own components.
        for segment in out.trim_start_matches("…/").split('/') {
            assert!(
                path.split('/').any(|c| c == segment),
                "{segment:?} in {out}"
            );
        }
    }

    /// A plain repository path is short enough that the repo name — its leaf
    /// — survives, which is the everyday case the guarantee covers.
    #[test]
    fn elide_middle_keeps_a_repo_name_that_is_the_leaf() {
        let path = "/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools";
        let out = elide_middle(path, 30);
        assert!(out.chars().count() <= 30, "{out}");
        assert!(out.ends_with("/trusty-tools"), "{out}");
    }

    /// A string with no `/`, and a path whose last two components alone
    /// overflow, both fall back to character elision — head and tail kept.
    #[test]
    fn elide_middle_falls_back_to_character_elision() {
        let out = elide_middle("abcdefghijklmnopqrstuvwxyz", 11);
        assert_eq!(out.chars().count(), 11, "{out}");
        assert_eq!(out, "abcde…vwxyz", "head and tail both kept");
        assert!(out.contains('…'), "{out}");

        let deep = "/a/averyveryverylongdirectoryname/anotherverylongleafname";
        let out = elide_middle(deep, 20);
        assert_eq!(out.chars().count(), 20, "{out}");
        assert!(out.starts_with("/a/"), "{out}");
        assert!(out.ends_with("leafname"), "{out}");
    }

    /// Degenerate widths must not panic and must not exceed the budget.
    #[test]
    fn elide_middle_handles_degenerate_widths() {
        assert_eq!(elide_middle("/a/b/c", 0), "…");
        assert_eq!(elide_middle("/a/b/c", 1), "…");
        assert_eq!(elide_middle("short", 99), "short");
    }

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
