//! Markdown-aware rendering helpers for the chat scrollback: fenced
//! code-block detection and box-drawing table rendering.
//!
//! Why: DOC-50 §5 Slice 4 migrates tagent's `repl/tui/markdown.rs` (297
//! lines) into this shared crate verbatim — every function here is already
//! engine-agnostic (pure `&str`/`Vec<String>` in, `Line`/`Span` out; no
//! `ReplApp` dependency), so the migration is a straight move rather than a
//! generalization. [`crate::widgets::scrollback::build_chat_lines`] is the
//! sole caller, walking a message body line-by-line and dispatching to
//! [`code_fence_lang`]/[`is_executable_shell_lang`] (fence detection) or
//! [`is_md_table_row`]/[`is_md_table_separator`]/[`parse_md_table_cells`]/
//! [`render_markdown_table`] (table detection) as it goes.
//!
//! What: no state, no I/O — every function is a pure transform, which is why
//! this module lives under [`crate::render`] rather than [`crate::widgets`]
//! (see that module's doc comment for the distinction).
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4): migrate `markdown.rs`.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Parse a fenced code-block opener line and return the language tag.
///
/// Why: We need to distinguish opening fences (` ```bash `) from closing
/// fences (` ``` `) and from non-fence content. Returning `Some(lang)` for
/// openers (including `Some("")` for bare ``` ``` openers) and `None` for
/// anything that isn't a fence line at all lets the renderer drive its state
/// machine cleanly.
/// What: Trims the line; if it starts with three backticks, returns the
/// trailing tag (lowercased) — empty string for bare openers, `None` for
/// anything that isn't a fence line at all.
/// Test: [`tests::code_fence_lang_recognizes_openers_and_closers`].
pub fn code_fence_lang(line: &str) -> Option<String> {
    let t = line.trim();
    t.strip_prefix("```")
        .map(|rest| rest.trim().to_ascii_lowercase())
}

/// Whether a fenced-code-block language tag denotes an executable shell.
///
/// Why: Shell blocks get a distinct visual treatment from other languages so
/// a reader can spot "this is something you could paste into a terminal" at
/// a glance.
/// What: Matches `bash`, `sh`, `zsh`, `fish` (case-insensitive — caller
/// passes a lowercased tag).
/// Test: [`tests::is_executable_shell_lang_matches_shells`].
pub fn is_executable_shell_lang(lang: &str) -> bool {
    matches!(lang, "bash" | "sh" | "zsh" | "fish")
}

/// Extract the last executable-shell fenced code block body from a message.
///
/// Why: When a message contains multiple shell blocks, "grab the most
/// recently shown command" needs the *last* one, not the first.
/// What: Walks the lines, tracks fence state, and returns `Some(body)` of the
/// last completed bash/sh/zsh/fish block. Body lines are joined with `\n`.
/// Returns `None` if no executable shell block is found, or the block was
/// opened but never closed (an unclosed fence is discarded, not inferred to
/// end at EOF — a truncated response should not be pasted as a partial
/// command).
/// Test: [`tests::extract_last_shell_block_finds_bash`],
/// [`tests::extract_last_shell_block_returns_last_when_multiple`],
/// [`tests::extract_last_shell_block_ignores_non_shell`],
/// [`tests::extract_last_shell_block_none_without_block`],
/// [`tests::extract_last_shell_block_unclosed_fence_returns_none`],
/// [`tests::extract_last_shell_block_closed_then_unclosed_returns_closed`].
pub fn extract_last_shell_block(text: &str) -> Option<String> {
    let mut last: Option<String> = None;
    let mut current_body: Option<Vec<String>> = None;
    let mut in_shell = false;
    for line in text.lines() {
        if let Some(lang) = code_fence_lang(line) {
            if let Some(body) = current_body.take() {
                if in_shell {
                    last = Some(body.join("\n"));
                }
                in_shell = false;
            } else {
                in_shell = is_executable_shell_lang(&lang);
                current_body = Some(Vec::new());
            }
        } else if let Some(body) = current_body.as_mut() {
            body.push(line.to_string());
        }
    }
    last
}

/// Detect a markdown table row: trimmed line starts with `|`.
///
/// Why: Table detection is the gate for box-drawing rendering — non-table
/// lines fall through to plain rendering.
/// What: Returns true if the trimmed line starts with `|`.
/// Test: [`tests::is_md_table_row_basic`].
pub fn is_md_table_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// Detect a markdown table separator row: cells contain only `-`, `:`, spaces.
///
/// Why: The separator row distinguishes header from body and confirms a
/// block is actually a markdown table (not just a line that happens to start
/// with `|`).
/// What: Returns true if every non-empty cell after split-on-`|` is composed
/// solely of `-`, `:`, or whitespace, AND at least one `-` appears.
/// Test: [`tests::is_md_table_separator_basic`].
pub fn is_md_table_separator(line: &str) -> bool {
    if !is_md_table_row(line) {
        return false;
    }
    let cells = parse_md_table_cells(line);
    if cells.is_empty() {
        return false;
    }
    let mut saw_dash = false;
    for c in &cells {
        for ch in c.chars() {
            match ch {
                '-' => saw_dash = true,
                ':' | ' ' | '\t' => {}
                _ => return false,
            }
        }
    }
    saw_dash
}

/// Split a markdown table row on `|` and trim each cell.
///
/// Why: Markdown table rows have leading and trailing pipes that produce
/// empty cells when split naïvely; consumers want only the real cell content.
/// What: Returns the trimmed cell strings, dropping leading/trailing empties
/// produced by the bordering pipes.
/// Test: [`tests::parse_md_table_cells_basic`].
pub fn parse_md_table_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let mut parts: Vec<String> = trimmed.split('|').map(|s| s.trim().to_string()).collect();
    if parts.first().map(|s| s.is_empty()).unwrap_or(false) {
        parts.remove(0);
    }
    if parts.last().map(|s| s.is_empty()).unwrap_or(false) {
        parts.pop();
    }
    parts
}

/// Truncate a string to `max` display chars, appending `…` if shortened.
///
/// Why: Table cells must fit within a column width budget; oversized content
/// gets a visual ellipsis so the reader knows truncation happened.
/// What: Returns the input unchanged if it fits, otherwise the first `max-1`
/// chars + `…`. If `max == 0`, returns an empty string.
/// Test: [`tests::truncate_cell_basic`].
pub fn truncate_cell(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Render a parsed markdown table as box-drawing styled `Line`s.
///
/// Why: Inline rendering keeps the table flush with surrounding chat text,
/// avoiding a layout split for a ratatui `Table` widget. Box characters give
/// a clean visual frame that scans well in monospaced fonts.
/// What: Given header + body rows, computes per-column widths (max of header
/// and any body cell), clamps total table width to `available_width`, and
/// emits top border, header row, separator, body rows, bottom border. Border
/// glyphs use `Color::DarkGray`; cell content uses `body_color` if provided.
/// Test: [`tests::render_markdown_table_emits_expected_lines`],
/// [`tests::render_markdown_table_truncates_when_too_wide`].
pub fn render_markdown_table(
    header: &[String],
    body: &[Vec<String>],
    available_width: usize,
    indent: &str,
    body_color: Option<Color>,
) -> Vec<Line<'static>> {
    let ncols = header
        .len()
        .max(body.iter().map(|r| r.len()).max().unwrap_or(0));
    if ncols == 0 {
        return Vec::new();
    }

    let header_n: Vec<String> = (0..ncols)
        .map(|i| header.get(i).cloned().unwrap_or_default())
        .collect();
    let body_n: Vec<Vec<String>> = body
        .iter()
        .map(|r| {
            (0..ncols)
                .map(|i| r.get(i).cloned().unwrap_or_default())
                .collect()
        })
        .collect();

    let mut widths: Vec<usize> = (0..ncols)
        .map(|i| {
            let mut w = header_n[i].chars().count();
            for r in &body_n {
                w = w.max(r[i].chars().count());
            }
            w
        })
        .collect();

    let indent_w = indent.chars().count();
    let frame_overhead = 1 + ncols;
    let padding_per_col = 2;
    let mut total: usize =
        indent_w + frame_overhead + widths.iter().map(|w| w + padding_per_col).sum::<usize>();

    let limit = available_width.max(indent_w + frame_overhead + ncols * (padding_per_col + 1));
    while total > available_width && available_width > 0 {
        let widest = widths
            .iter()
            .enumerate()
            .filter(|(_, w)| **w > 1)
            .max_by_key(|(_, w)| **w)
            .map(|(i, _)| i);
        match widest {
            Some(i) => {
                widths[i] -= 1;
                total -= 1;
            }
            None => break,
        }
    }
    let _ = limit;

    let border_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    let cell_style = match body_color {
        Some(c) => Style::default().fg(c),
        None => Style::default(),
    };

    let make_border = |left: char, mid: char, right: char| -> String {
        let mut s = String::new();
        s.push_str(indent);
        s.push(left);
        for (i, w) in widths.iter().enumerate() {
            for _ in 0..(w + padding_per_col) {
                s.push('─');
            }
            if i + 1 < widths.len() {
                s.push(mid);
            }
        }
        s.push(right);
        s
    };

    let make_data_row = |cells: &[String]| -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(ncols * 2 + 2);
        spans.push(Span::raw(indent.to_string()));
        spans.push(Span::styled("│".to_string(), border_style));
        for (i, w) in widths.iter().enumerate() {
            let cell = truncate_cell(&cells[i], *w);
            let pad_right = w.saturating_sub(cell.chars().count());
            let mut content = String::with_capacity(2 + w);
            content.push(' ');
            content.push_str(&cell);
            for _ in 0..pad_right {
                content.push(' ');
            }
            content.push(' ');
            spans.push(Span::styled(content, cell_style));
            let _ = i;
            spans.push(Span::styled("│".to_string(), border_style));
        }
        Line::from(spans)
    };

    let mut out: Vec<Line<'static>> = Vec::with_capacity(body_n.len() + 4);
    out.push(Line::from(Span::styled(
        make_border('┌', '┬', '┐'),
        border_style,
    )));
    out.push(make_data_row(&header_n));
    out.push(Line::from(Span::styled(
        make_border('├', '┼', '┤'),
        border_style,
    )));
    for row in &body_n {
        out.push(make_data_row(row));
    }
    out.push(Line::from(Span::styled(
        make_border('└', '┴', '┘'),
        border_style,
    )));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_fence_lang_recognizes_openers_and_closers() {
        assert_eq!(code_fence_lang("```bash"), Some("bash".into()));
        assert_eq!(code_fence_lang("```sh"), Some("sh".into()));
        assert_eq!(code_fence_lang("```Rust"), Some("rust".into()));
        assert_eq!(code_fence_lang("```"), Some("".into()));
        assert_eq!(code_fence_lang("  ```bash  "), Some("bash".into()));
        assert_eq!(code_fence_lang("hello"), None);
        assert_eq!(code_fence_lang("``"), None);
    }

    #[test]
    fn is_executable_shell_lang_matches_shells() {
        assert!(is_executable_shell_lang("bash"));
        assert!(is_executable_shell_lang("sh"));
        assert!(is_executable_shell_lang("zsh"));
        assert!(is_executable_shell_lang("fish"));
        assert!(!is_executable_shell_lang("rust"));
        assert!(!is_executable_shell_lang("python"));
        assert!(!is_executable_shell_lang(""));
    }

    #[test]
    fn extract_last_shell_block_finds_bash() {
        let text = "Run this:\n```bash\necho hello\nls -la\n```\nDone.";
        assert_eq!(
            extract_last_shell_block(text),
            Some("echo hello\nls -la".into())
        );
    }

    #[test]
    fn extract_last_shell_block_returns_last_when_multiple() {
        let text = "```bash\nfirst\n```\nstuff\n```sh\nsecond\nthird\n```\n";
        assert_eq!(extract_last_shell_block(text), Some("second\nthird".into()));
    }

    #[test]
    fn extract_last_shell_block_ignores_non_shell() {
        let text = "```rust\nfn main() {}\n```";
        assert_eq!(extract_last_shell_block(text), None);
    }

    #[test]
    fn extract_last_shell_block_none_without_block() {
        assert_eq!(extract_last_shell_block("plain prose only"), None);
    }

    #[test]
    fn extract_last_shell_block_unclosed_fence_returns_none() {
        let text = "Here's a script:\n```bash\necho hello\nls -la";
        assert_eq!(extract_last_shell_block(text), None);
    }

    #[test]
    fn extract_last_shell_block_closed_then_unclosed_returns_closed() {
        let text = "```bash\necho first\n```\n```sh\nunclosed";
        assert_eq!(
            extract_last_shell_block(text),
            Some("echo first".to_string())
        );
    }

    #[test]
    fn is_md_table_row_basic() {
        assert!(is_md_table_row("| a | b |"));
        assert!(is_md_table_row("  |x|"));
        assert!(!is_md_table_row("hello"));
        assert!(!is_md_table_row(""));
    }

    #[test]
    fn is_md_table_separator_basic() {
        assert!(is_md_table_separator("|---|---|"));
        assert!(is_md_table_separator("| :--- | ---: |"));
        assert!(is_md_table_separator("|:-:|:-:|"));
        assert!(!is_md_table_separator("| a | b |"));
        assert!(!is_md_table_separator("hello"));
        assert!(!is_md_table_separator("|   |   |"));
    }

    #[test]
    fn parse_md_table_cells_basic() {
        assert_eq!(
            parse_md_table_cells("| a | b |"),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(
            parse_md_table_cells("|x|y|z|"),
            vec!["x".to_string(), "y".to_string(), "z".to_string()]
        );
        assert_eq!(
            parse_md_table_cells("| Technique | Impact | Effort |"),
            vec![
                "Technique".to_string(),
                "Impact".to_string(),
                "Effort".to_string()
            ]
        );
    }

    #[test]
    fn truncate_cell_basic() {
        assert_eq!(truncate_cell("abc", 5), "abc");
        assert_eq!(truncate_cell("abcdef", 4), "abc…");
        assert_eq!(truncate_cell("hello", 0), "");
        assert_eq!(truncate_cell("hello", 5), "hello");
    }

    #[test]
    fn render_markdown_table_emits_expected_lines() {
        let header = vec![
            "Technique".to_string(),
            "Impact".to_string(),
            "Effort".to_string(),
        ];
        let body = vec![
            vec![
                "Prompt constraints".to_string(),
                "10–20%".to_string(),
                "Low".to_string(),
            ],
            vec![
                "max_tokens caps".to_string(),
                "Prevents runaway".to_string(),
                "Low".to_string(),
            ],
        ];
        let out = render_markdown_table(&header, &body, 200, "   ", None);
        assert_eq!(out.len(), 6);
        let first: String = out[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            first.contains('┌'),
            "expected ┌ in top border, got: {first}"
        );
        assert!(first.contains('┬'));
        assert!(first.contains('┐'));
        let header_line: String = out[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(header_line.contains("Technique"));
        assert!(header_line.contains("Impact"));
        assert!(header_line.contains("Effort"));
        let sep_line: String = out[2]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(sep_line.contains('├'));
        assert!(sep_line.contains('┼'));
        assert!(sep_line.contains('┤'));
        let last: String = out[5]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(last.contains('└'));
        assert!(last.contains('┴'));
        assert!(last.contains('┘'));
        for l in &out {
            let s: String = l
                .spans
                .iter()
                .map(|sp| sp.content.as_ref())
                .collect::<Vec<_>>()
                .join("");
            assert!(s.starts_with("   "), "line missing indent: {s:?}");
        }
    }

    #[test]
    fn render_markdown_table_truncates_when_too_wide() {
        let header = vec!["AVeryLongHeaderName".to_string(), "B".to_string()];
        let body = vec![vec!["X".to_string(), "Y".to_string()]];
        let out = render_markdown_table(&header, &body, 18, "", None);
        assert_eq!(out.len(), 5);
        let header_line: String = out[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            header_line.chars().count() <= 18,
            "row too wide: {header_line:?}"
        );
    }
}
