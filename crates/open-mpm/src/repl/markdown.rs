//! Lightweight ANSI markdown renderer for REPL responses.
//!
//! Why: Claude Code surfaces assistant responses with light syntactic
//! highlighting (bold, inline code, code fences, headers, bullets). We want
//! the open-mpm REPL transcript to read the same way without pulling a full
//! markdown parser. A small line-by-line + inline scan handles ~95% of cases
//! and keeps the dependency footprint flat.
//! What: `render_markdown_ansi` walks the input one line at a time. Lines
//! inside ` ```fence ``` ` blocks are emitted with a dim block style (and the
//! optional language tag is rendered dim before the body). Outside fences, we
//! detect headers (`#`–`####`), bullets (`- `, `* `), and then run a single
//! inline pass that colors `**bold**`, `*italic*`/`_italic_`, and inline
//! `` `code` ``.
//! Test: `render_markdown_ansi_*` unit tests cover headers, bullets, fences,
//! inline code/bold, and pass-through for plain text.

// ANSI escape constants — kept short and centralized so the helpers below
// stay readable. Using nu_ansi_term/crossterm here would be heavier than
// raw escapes for this tiny rendering surface.
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const UNDERLINE: &str = "\x1b[4m";
const FG_BRIGHT_CYAN: &str = "\x1b[96m";
const FG_YELLOW: &str = "\x1b[33m";
const FG_BRIGHT_YELLOW: &str = "\x1b[93m";
const FG_GREEN: &str = "\x1b[32m";
const FG_MAGENTA: &str = "\x1b[35m";

/// Render `text` with light ANSI markdown highlighting.
///
/// Why: Centralized renderer so callers (REPL chat printer) can pipe
/// assistant responses through one transformation point.
/// What: Returns a new String with ANSI escapes injected. Empty input yields
/// empty output. The renderer never panics — malformed markdown is emitted
/// verbatim with a best-effort highlight pass.
/// Test: See module-level `tests` for header/bullet/fence/inline coverage.
pub fn render_markdown_ansi(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(text.len() + 64);
    let mut in_fence = false;

    for (idx, line) in text.split('\n').enumerate() {
        if idx > 0 {
            out.push('\n');
        }

        // Code fence delimiter handling. We toggle state and emit a dim
        // marker line so the user can still see fence boundaries. The
        // opening fence's language tag is rendered inline; it isn't
        // carried across iterations because each delimiter line ends with
        // `continue`, so we compute it locally.
        if let Some(rest) = line.trim_start().strip_prefix("```") {
            if in_fence {
                in_fence = false;
                out.push_str(DIM);
                out.push_str("```");
                out.push_str(RESET);
                continue;
            } else {
                in_fence = true;
                let lang = rest.trim();
                out.push_str(DIM);
                out.push_str("```");
                if !lang.is_empty() {
                    out.push_str(lang);
                }
                out.push_str(RESET);
                continue;
            }
        }

        if in_fence {
            // Inside a fenced block: emit body in green so it visually reads
            // as a code block. We do NOT run inline highlighting here — code
            // bodies should be left literal.
            out.push_str(FG_GREEN);
            out.push_str(line);
            out.push_str(RESET);
            continue;
        }

        // Header detection (only at line start, up to four `#`).
        let trimmed = line.trim_start();
        if let Some(stripped) = trimmed.strip_prefix("#### ") {
            out.push_str(BOLD);
            out.push_str(FG_MAGENTA);
            out.push_str(stripped);
            out.push_str(RESET);
            continue;
        } else if let Some(stripped) = trimmed.strip_prefix("### ") {
            out.push_str(BOLD);
            out.push_str(FG_BRIGHT_CYAN);
            out.push_str(stripped);
            out.push_str(RESET);
            continue;
        } else if let Some(stripped) = trimmed.strip_prefix("## ") {
            out.push_str(BOLD);
            out.push_str(UNDERLINE);
            out.push_str(FG_BRIGHT_CYAN);
            out.push_str(stripped);
            out.push_str(RESET);
            continue;
        } else if let Some(stripped) = trimmed.strip_prefix("# ") {
            out.push_str(BOLD);
            out.push_str(UNDERLINE);
            out.push_str(FG_BRIGHT_YELLOW);
            out.push_str(stripped);
            out.push_str(RESET);
            continue;
        }

        // Bullet detection (`- ` or `* ` at line start, preserving indent).
        let indent_len = line.len() - trimmed.len();
        let indent = &line[..indent_len];
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            out.push_str(indent);
            out.push_str(FG_YELLOW);
            out.push('•');
            out.push_str(RESET);
            out.push(' ');
            out.push_str(&render_inline(rest));
            continue;
        }

        // Plain prose — run inline highlighting.
        out.push_str(&render_inline(line));
    }

    out
}

/// Render inline markdown (bold, italic, inline code) in a single line.
///
/// Why: Splitting inline rendering into its own helper keeps the line-level
/// dispatch in `render_markdown_ansi` readable. Called per-line so the cost
/// stays bounded.
/// What: Walks bytes left-to-right, recognizing in priority order:
///   1. `` `code` `` — inline code
///   2. `**bold**` — bold (must be matched pair on same line)
///   3. `_italic_` / `*italic*` — italic (matched pair)
/// Unmatched delimiters are emitted literally.
/// Test: `inline_code`, `inline_bold`, `inline_italic`, `unmatched_delim`.
fn render_inline(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len() + 16);
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        // Inline code: `…`
        if b == b'`'
            && let Some(rel) = find_byte(&bytes[i + 1..], b'`')
        {
            let end = i + 1 + rel;
            out.push_str(FG_BRIGHT_CYAN);
            out.push_str(&line[i..=end]);
            out.push_str(RESET);
            i = end + 1;
            continue;
        }

        // Bold: **…**
        if b == b'*'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'*'
            && let Some(rel) = find_double_star(&bytes[i + 2..])
        {
            let inner_start = i + 2;
            let inner_end = i + 2 + rel;
            out.push_str(BOLD);
            out.push_str(&line[inner_start..inner_end]);
            out.push_str(RESET);
            i = inner_end + 2;
            continue;
        }

        // Italic: *…*  (single-star, ensure not part of **…**)
        if b == b'*'
            && (i + 1 >= bytes.len() || bytes[i + 1] != b'*')
            && let Some(rel) = find_byte(&bytes[i + 1..], b'*')
        {
            let inner_start = i + 1;
            let inner_end = i + 1 + rel;
            // Reject if the closing star is itself a `**` pair (avoids
            // mis-eating bold).
            if inner_end + 1 < bytes.len() && bytes[inner_end + 1] == b'*' {
                out.push(b as char);
                i += 1;
                continue;
            }
            out.push_str(BOLD);
            out.push_str(&line[inner_start..inner_end]);
            out.push_str(RESET);
            i = inner_end + 1;
            continue;
        }

        // Italic: _…_ (matched pair, no spaces immediately inside)
        if b == b'_'
            && let Some(rel) = find_byte(&bytes[i + 1..], b'_')
            && rel > 0
        {
            let inner_start = i + 1;
            let inner_end = i + 1 + rel;
            out.push_str(BOLD);
            out.push_str(&line[inner_start..inner_end]);
            out.push_str(RESET);
            i = inner_end + 1;
            continue;
        }

        out.push(b as char);
        i += 1;
    }

    out
}

/// Locate the first occurrence of `target` in `slice`, returning its index.
fn find_byte(slice: &[u8], target: u8) -> Option<usize> {
    slice.iter().position(|&x| x == target)
}

/// Locate the first `**` sequence in `slice`, returning the index of the
/// first `*` of the pair.
fn find_double_star(slice: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < slice.len() {
        if slice[i] == b'*' && slice[i + 1] == b'*' {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_markdown_ansi_empty() {
        assert_eq!(render_markdown_ansi(""), "");
    }

    #[test]
    fn render_markdown_ansi_plain_text_unchanged_semantically() {
        let out = render_markdown_ansi("just plain text");
        assert!(out.contains("just plain text"));
    }

    #[test]
    fn render_markdown_ansi_h1_bold_underline() {
        let out = render_markdown_ansi("# Title");
        assert!(out.contains(BOLD));
        assert!(out.contains(UNDERLINE));
        assert!(out.contains("Title"));
        assert!(!out.contains("# Title"), "leading # should be stripped");
    }

    #[test]
    fn render_markdown_ansi_h2_bold_underline() {
        let out = render_markdown_ansi("## Heading");
        assert!(out.contains(BOLD));
        assert!(out.contains(UNDERLINE));
        assert!(out.contains("Heading"));
    }

    #[test]
    fn render_markdown_ansi_bullet_renders_dot() {
        let out = render_markdown_ansi("- item");
        assert!(out.contains("•"));
        assert!(out.contains("item"));
    }

    #[test]
    fn render_markdown_ansi_inline_code() {
        let out = render_markdown_ansi("call `foo()` here");
        assert!(out.contains(FG_BRIGHT_CYAN));
        assert!(out.contains("`foo()`"));
    }

    #[test]
    fn render_markdown_ansi_inline_bold() {
        let out = render_markdown_ansi("hello **world**");
        assert!(out.contains(BOLD));
        assert!(out.contains("world"));
        assert!(
            !out.contains("**world**"),
            "bold markers should be consumed"
        );
    }

    #[test]
    fn render_markdown_ansi_code_fence() {
        let input = "before\n```python\nprint('hi')\n```\nafter";
        let out = render_markdown_ansi(input);
        assert!(out.contains("python"));
        assert!(out.contains("print('hi')"));
        assert!(out.contains(FG_GREEN));
        assert!(out.contains(DIM));
    }

    #[test]
    fn render_markdown_ansi_unmatched_backtick_passthrough() {
        // Unmatched ` should not crash and should leave the line readable.
        let out = render_markdown_ansi("a ` b c");
        assert!(out.contains("b c"));
    }

    #[test]
    fn render_markdown_ansi_preserves_newlines() {
        let out = render_markdown_ansi("line one\nline two");
        assert_eq!(out.matches('\n').count(), 1);
    }
}
