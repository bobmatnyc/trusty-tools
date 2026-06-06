//! Part of the `tui` module (split from the original monolithic `tui.rs`
//! to satisfy the 500-line file cap — see #357). Cross-submodule items and
//! external imports resolve through the flat re-exports in `mod.rs`.

use super::*;

/// Parse a fenced code-block opener line and return the language tag.
///
/// Why: We need to distinguish opening fences (` ```bash `) from closing
/// fences (` ``` `) and from non-fence content. Returning `Some(lang)` for
/// openers (including `Some("")` for bare ``` ``` openers) and `None` for
/// non-fence lines lets the renderer drive its state machine cleanly.
/// What: Trims the line; if it starts with three backticks, returns the
/// trailing tag (lowercased) — empty string for bare openers, `None` for
/// anything that isn't a fence line at all.
/// Test: `code_fence_lang_*` asserts bash/sh/empty/non-fence cases.
pub(crate) fn code_fence_lang(line: &str) -> Option<String> {
    let t = line.trim();
    t.strip_prefix("```")
        .map(|rest| rest.trim().to_ascii_lowercase())
}

/// Whether a fenced-code-block language tag denotes an executable shell.
///
/// Why: Shell blocks get the bright-green `▶` indicator and feed the
/// Ctrl+E paste buffer; non-shell blocks (rust, python, json, …) use the
/// neutral `⬡` indicator and are NOT pasted into the input.
/// What: Matches `bash`, `sh`, `zsh`, `fish` (case-insensitive — caller
/// passes a lowercased tag).
/// Test: `is_executable_shell_lang_*` asserts each tag.
pub(crate) fn is_executable_shell_lang(lang: &str) -> bool {
    matches!(lang, "bash" | "sh" | "zsh" | "fish")
}

/// Extract the last executable-shell fenced code block body from a message.
///
/// Why: When an assistant message offers multiple shell blocks, Ctrl+E should
/// paste the *most recently shown* one — i.e. the last block in the message.
/// What: Walks the lines, tracks fence state, and returns `Some(body)` of
/// the last completed bash/sh/zsh/fish block. Body is joined with `\n`.
/// Returns `None` if no executable shell block is found (or the block was
/// never closed).
/// Test: `extract_last_shell_block_*` unit tests.
pub(crate) fn extract_last_shell_block(text: &str) -> Option<String> {
    let mut last: Option<String> = None;
    let mut current_body: Option<Vec<String>> = None;
    let mut in_shell = false;
    for line in text.lines() {
        if let Some(lang) = code_fence_lang(line) {
            if let Some(body) = current_body.take() {
                // Closing fence
                if in_shell {
                    last = Some(body.join("\n"));
                }
                in_shell = false;
            } else {
                // Opening fence
                in_shell = is_executable_shell_lang(&lang);
                current_body = Some(Vec::new());
            }
        } else if let Some(body) = current_body.as_mut() {
            body.push(line.to_string());
        }
    }
    // Intentional: if `current_body` is Some here, it means the last fence
    // was opened but never closed (e.g. model truncated at max_tokens mid-block).
    // We discard the partial body — Ctrl+E should not paste an incomplete command.
    // This is distinct from a bug; the completed blocks accumulated in `last`
    // (if any) are returned instead.
    last
}

/// Detect a markdown table row: trimmed line starts with `|`.
///
/// Why: Table detection is the gate for box-drawing rendering — non-table
/// lines fall through to plain rendering.
/// What: Returns true if the trimmed line starts with `|`.
/// Test: Assert true for "| a | b |" and "  |x|", false for "hello" or "".
pub(crate) fn is_md_table_row(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('|')
}

/// Detect a markdown table separator row: cells contain only `-`, `:`, spaces.
///
/// Why: The separator row distinguishes header from body and confirms a block
/// is actually a markdown table (not just a line that happens to start with `|`).
/// What: Returns true if every non-empty cell after split-on-`|` is composed
/// solely of `-`, `:`, or whitespace, AND at least one `-` appears.
/// Test: Assert true for "|---|---|" and "|:--|--:|", false for "| a | b |".
pub(crate) fn is_md_table_separator(line: &str) -> bool {
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
/// Test: Assert "| a | b |" yields ["a", "b"]; "|x|y|z|" yields ["x","y","z"].
pub(crate) fn parse_md_table_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let mut parts: Vec<String> = trimmed.split('|').map(|s| s.trim().to_string()).collect();
    // Drop leading empty (from leading `|`).
    if parts.first().map(|s| s.is_empty()).unwrap_or(false) {
        parts.remove(0);
    }
    // Drop trailing empty (from trailing `|`).
    if parts.last().map(|s| s.is_empty()).unwrap_or(false) {
        parts.pop();
    }
    parts
}

/// Truncate a string to `max` display chars, appending `…` if shortened.
///
/// Why: Table cells must fit within column width budget; oversized content
/// gets a visual ellipsis so the user knows truncation happened.
/// What: Returns the input unchanged if it fits, otherwise the first
/// `max-1` chars + `…`. If `max == 0`, returns empty string.
/// Test: Assert "abc" with max=5 returns "abc"; "abcdef" with max=4 returns "abc…".
pub(crate) fn truncate_cell(s: &str, max: usize) -> String {
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
/// avoiding the layout split required by ratatui's `Table` widget. Box
/// characters give a clean visual frame that scans well in monospaced fonts.
/// What: Given header + body rows, computes per-column widths (max of header
/// and any body cell), clamps total table width to `available_width`, and
/// emits top border, header row, separator, body rows, bottom border. Border
/// glyphs use `Color::DarkGray`; cell content uses `body_color` if provided.
/// Test: Pass a 3x3 table (1 header row + 2 body rows), assert the returned
/// Vec has 6 lines (top, header, sep, 2 body, bottom), each starts with the
/// indent prefix, and the first/last lines contain `┌`/`└`.
pub(crate) fn render_markdown_table(
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

    // Normalize all rows to ncols by padding with empty cells.
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

    // Step 1: ideal column widths from content.
    let mut widths: Vec<usize> = (0..ncols)
        .map(|i| {
            let mut w = header_n[i].chars().count();
            for r in &body_n {
                w = w.max(r[i].chars().count());
            }
            w
        })
        .collect();

    // Step 2: compute total table width with " cell " padding (1 space each
    // side) and `│` borders. Total = indent + 1 (left border) + sum(2 + w_i)
    // + ncols (right borders, one per column).
    let indent_w = indent.chars().count();
    let frame_overhead = 1 + ncols; // left `│` + one `│` per column on right
    let padding_per_col = 2; // " " on each side of cell content
    let mut total: usize =
        indent_w + frame_overhead + widths.iter().map(|w| w + padding_per_col).sum::<usize>();

    // Step 3: if too wide, shrink the widest column repeatedly until we fit
    // or every column is at minimum width 1.
    let limit = available_width.max(indent_w + frame_overhead + ncols * (padding_per_col + 1));
    while total > available_width && available_width > 0 {
        // Find widest column with width > 1.
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
    let _ = limit; // referenced for clarity above; not used after shrink loop.

    // Step 4: build border row helper (top/sep/bottom variants).
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
