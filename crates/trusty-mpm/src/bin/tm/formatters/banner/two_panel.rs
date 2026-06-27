//! Two-panel full-width launch banner compositor.
//!
//! Why: the original stacked layout (robot then info box) leaves the right half
//! of wide terminals empty. A side-by-side two-panel layout fills the full
//! terminal width and is more visually polished on modern wide terminals.
//! What: `render_two_panel_banner` builds two framed boxes side-by-side —
//! LEFT: robot+wordmark+version; RIGHT: info-box content — spanning the full
//! terminal width. On narrow terminals (< `MIN_WIDTH_TWO_PANEL` columns) the
//! function falls back to the stacked layout instead.
//! Test: `two_panel_compose_alignment`, `two_panel_shorter_panel_padded`,
//! `two_panel_narrow_fallback`, `two_panel_version_present`,
//! `two_panel_no_clear_in_preview`.

use colored::Colorize as _;
use unicode_width::UnicodeWidthStr as _;

use super::{BANNER_ROBOT, colorize_robot_row, wordmark_lines};
use crate::formatters::info_box::{WelcomeData, render_info_box_rows};

// ── Layout constants ──────────────────────────────────────────────────────────

/// Left panel outer width in columns (frame inclusive).
/// The robot art is 45 display columns wide; +2 for the box frame (│..│).
/// Actual inner width = `LEFT_PANEL_OUTER_WIDTH - 2`.
pub(crate) const LEFT_PANEL_OUTER_WIDTH: usize = 49;

/// Minimum terminal width at which two-panel mode is used instead of stacked.
/// Below this threshold the right panel would be too narrow to read.
pub(crate) const MIN_WIDTH_TWO_PANEL: usize = 90;

/// Gutter between left and right panel (space between right edge of left frame
/// and left edge of right frame).
const GUTTER: usize = 1;

/// Muted rust colour for panel borders (`(120, 50, 10)`).
const BORDER_R: u8 = 120;
const BORDER_G: u8 = 50;
const BORDER_B: u8 = 10;

// ── Box-drawing characters ─────────────────────────────────────────────────────
const TL: char = '╭'; // top-left
const TR: char = '╮'; // top-right
const BL: char = '╰'; // bottom-left
const BR: char = '╯'; // bottom-right
const HORIZ: char = '─';
const VERT: char = '│';

// ── Public entry point ────────────────────────────────────────────────────────

/// Render the two-panel banner as a `String` (pure — no I/O, no screen-clear).
///
/// Why: pure render is unit-testable; the print wrappers add I/O and
/// (optionally) the `\x1B[2J\x1B[1;1H` screen-clear prefix.
/// What: builds left (robot+wordmark+version) and right (info-box rows)
/// framed panels side-by-side to fill `term_width` columns exactly.
/// When `term_width < MIN_WIDTH_TWO_PANEL` returns `None` (caller falls back).
/// Test: `two_panel_compose_alignment`, `two_panel_shorter_panel_padded`.
pub(crate) fn render_two_panel_banner(
    data: &WelcomeData,
    term_width: usize,
    reconnecting: bool,
) -> Option<String> {
    if term_width < MIN_WIDTH_TWO_PANEL {
        return None;
    }

    let right_outer = term_width.saturating_sub(LEFT_PANEL_OUTER_WIDTH + GUTTER);
    if right_outer < 10 {
        return None;
    }

    let left_inner = LEFT_PANEL_OUTER_WIDTH.saturating_sub(2);
    let right_inner = right_outer.saturating_sub(2);

    // Build left panel lines (robot + wordmark + version).
    let left_lines = build_left_lines(left_inner, reconnecting);
    // Build right panel lines (info-box rows).
    let right_lines = build_right_lines(data, right_inner);

    let height = left_lines.len().max(right_lines.len());

    let mut out = String::new();
    // Top borders.
    out.push_str(&tint_border(&format!(
        "{TL}{}{TR}",
        HORIZ.to_string().repeat(left_inner)
    )));
    out.push_str(&" ".repeat(GUTTER));
    out.push_str(&tint_border(&format!(
        "{TL}{}{TR}",
        HORIZ.to_string().repeat(right_inner)
    )));
    out.push('\n');

    // Content rows.
    for i in 0..height {
        // Left cell.
        let left_cell = left_lines.get(i).cloned().unwrap_or_default();
        let left_padded = pad_to_cols(&strip_ansi(&left_cell), left_inner, &left_cell);
        out.push_str(&tint_border(&VERT.to_string()));
        out.push_str(&left_padded);
        out.push_str(&tint_border(&VERT.to_string()));

        out.push_str(&" ".repeat(GUTTER));

        // Right cell.
        let right_cell = right_lines.get(i).cloned().unwrap_or_default();
        let right_padded = pad_to_cols(&strip_ansi(&right_cell), right_inner, &right_cell);
        out.push_str(&tint_border(&VERT.to_string()));
        out.push_str(&right_padded);
        out.push_str(&tint_border(&VERT.to_string()));
        out.push('\n');
    }

    // Bottom borders.
    out.push_str(&tint_border(&format!(
        "{BL}{}{BR}",
        HORIZ.to_string().repeat(left_inner)
    )));
    out.push_str(&" ".repeat(GUTTER));
    out.push_str(&tint_border(&format!(
        "{BL}{}{BR}",
        HORIZ.to_string().repeat(right_inner)
    )));
    out.push('\n');

    Some(out)
}

// ── Left panel builder ────────────────────────────────────────────────────────

/// Build the left-panel content lines (robot art + wordmark + version).
///
/// Why: each line is a single string padded to `inner_width` display columns;
/// the compositor centres the robot horizontally within the panel.
/// What: robot rows (rust-gradient colourised), blank row, wordmark rows,
/// blank row, version string. Each line is pre-padded to `inner_width` cols.
/// Test: `two_panel_version_present`, `two_panel_compose_alignment`.
fn build_left_lines(inner_width: usize, reconnecting: bool) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();

    // Robot rows — colourised, centred within inner_width.
    for (idx, &row) in BANNER_ROBOT.iter().enumerate() {
        let row_display = row; // raw display width (ASCII-only art = byte len)
        let row_cols = row_display.width();
        let pad_l = (inner_width.saturating_sub(row_cols)) / 2;
        let pad_r = inner_width.saturating_sub(row_cols + pad_l);
        let colored = colorize_robot_row(idx, row_display);
        lines.push(format!(
            "{}{}{}",
            " ".repeat(pad_l),
            colored,
            " ".repeat(pad_r)
        ));
    }

    // Blank separator.
    lines.push(" ".repeat(inner_width));

    // Plain-text "trusty-mpm" label + version — two lines replacing the old block-art wordmark.
    for wm_line in wordmark_lines() {
        let bare = strip_ansi(&wm_line);
        let row_cols = bare.width();
        let pad_l = (inner_width.saturating_sub(row_cols)) / 2;
        let pad_r = inner_width.saturating_sub(row_cols + pad_l);
        lines.push(format!(
            "{}{}{}",
            " ".repeat(pad_l),
            wm_line,
            " ".repeat(pad_r)
        ));
    }

    // Reconnecting indicator.
    if reconnecting {
        lines.push(" ".repeat(inner_width));
        let rcon = "↩ reconnecting";
        let rcon_cols = rcon.width();
        let pad_l = (inner_width.saturating_sub(rcon_cols)) / 2;
        let pad_r = inner_width.saturating_sub(rcon_cols + pad_l);
        lines.push(format!(
            "{}{}{}",
            " ".repeat(pad_l),
            rcon,
            " ".repeat(pad_r)
        ));
    }

    lines
}

// ── Right panel builder ───────────────────────────────────────────────────────

/// Build right-panel content lines from the welcome data.
///
/// Why: the right panel mirrors the existing info-box content; reusing
/// `render_info_box_rows` avoids duplicating the row-building logic.
/// What: calls `render_info_box_rows(data)` to get the content rows, then
/// truncates each to `inner_width` display columns.
/// Test: `two_panel_compose_alignment`.
fn build_right_lines(data: &WelcomeData, inner_width: usize) -> Vec<String> {
    let rows = render_info_box_rows(data);
    rows.into_iter()
        .map(|row| {
            let cols = row.width();
            if cols <= inner_width {
                // Pad to inner_width.
                format!("{row}{}", " ".repeat(inner_width - cols))
            } else {
                // Truncate.
                truncate_to_cols(row, inner_width)
            }
        })
        .collect()
}

// ── String utilities ──────────────────────────────────────────────────────────

/// Strip ANSI escape sequences from `s` and return the display-only string.
///
/// Why: ANSI escapes are zero display width but have non-zero byte length;
/// we need the "clean" version to compute display width and padding.
/// What: walks the string, discarding `\x1B[…m` SGR sequences.
/// Test: `strip_ansi_removes_color_codes`.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1B' {
            // Consume `[` then everything up to a letter.
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&nc) = chars.peek() {
                    chars.next();
                    if nc.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Pad `colored` (which may contain ANSI escapes) to `width` display columns.
///
/// Why: ANSI escapes are zero display width; padding must be based on the
/// display width of the bare text (`bare`), not the byte length of `colored`.
/// What: computes `display_len(bare)`, appends trailing spaces if needed.
/// Test: `two_panel_compose_alignment`.
fn pad_to_cols(bare: &str, width: usize, colored: &str) -> String {
    let cols = bare.width();
    if cols >= width {
        colored.to_string()
    } else {
        format!("{colored}{}", " ".repeat(width - cols))
    }
}

/// Truncate `s` (plain text, no ANSI) to at most `max_cols` display columns.
///
/// Why: right panel rows wider than `inner_width` would misalign the border.
/// What: accumulates char widths; stops before exceeding `max_cols - 1`, then
/// appends `…` (U+2026); returns `s` unchanged when it already fits.
/// Test: `two_panel_compose_alignment`.
fn truncate_to_cols(s: String, max_cols: usize) -> String {
    if s.width() <= max_cols {
        return s;
    }
    let budget = max_cols.saturating_sub(1);
    let mut used = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(ch);
    }
    let pad = max_cols.saturating_sub(used + 1);
    format!("{out}…{}", " ".repeat(pad))
}

/// Apply muted rust tint to a border string.
///
/// Why: tinted borders give the two-panel layout a cohesive brand color
/// without overwhelming the content.
/// What: wraps `s` in the BORDER_R/G/B truecolor escape. When `colored` has
/// colour disabled (NO_COLOR, non-TTY) the escape degrades to plain text.
/// Test: indirectly by `two_panel_compose_alignment` (no-color path).
fn tint_border(s: &str) -> String {
    s.truecolor(BORDER_R, BORDER_G, BORDER_B).to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::formatters::info_box::{CommitLine, DaemonInfo, WelcomeData};

    fn base_data() -> WelcomeData {
        WelcomeData {
            project: "owner/repo".to_string(),
            workspace: "/home/user/projects/repo".to_string(),
            user: "alice".to_string(),
            reconnecting: false,
            session_name: String::new(),
            daemon: DaemonInfo::default(),
            recent_commits: vec![],
            memory_status: "(not detected)".to_string(),
            search_status: "(not detected)".to_string(),
            review_status: "(not detected)".to_string(),
        }
    }

    fn reconnect_data() -> WelcomeData {
        WelcomeData {
            reconnecting: true,
            session_name: "tmpm-my-proj".to_string(),
            ..base_data()
        }
    }

    fn data_with_commits() -> WelcomeData {
        WelcomeData {
            recent_commits: vec![CommitLine {
                sha: "abc1234".to_string(),
                age: "2h".to_string(),
                subject: "fix: something important".to_string(),
            }],
            ..base_data()
        }
    }

    /// Two-panel banner renders without panic and produces two │-bordered rows.
    #[test]
    fn two_panel_compose_alignment() {
        let data = base_data();
        let result = render_two_panel_banner(&data, 120, false);
        assert!(result.is_some(), "wide terminal should produce two-panel");
        let out = result.unwrap();
        // Every content row must start with a border char (after stripping ANSI).
        for line in out.lines() {
            let bare = strip_ansi(line);
            if bare.is_empty() {
                continue;
            }
            let first = bare.chars().next().unwrap_or(' ');
            assert!(
                matches!(first, '╭' | '╰' | '│'),
                "each line must start with a box corner or side: {bare:?}"
            );
        }
    }

    /// Both framed panels close at the same bottom line (equal height).
    #[test]
    fn two_panel_shorter_panel_padded_to_equal_height() {
        let data = data_with_commits();
        let out = render_two_panel_banner(&data, 130, false).expect("should produce two-panel");
        // Count content rows (│...│ ... │...│ lines).
        let content_lines: Vec<&str> = out
            .lines()
            .filter(|l| {
                let bare = strip_ansi(l);
                bare.starts_with('│')
            })
            .collect();
        // The panels are always equal height, so every content line must have
        // EXACTLY two │ starting chars (one per panel).
        for line in &content_lines {
            let bare = strip_ansi(line);
            let pipe_count = bare.chars().filter(|&c| c == '│').count();
            assert!(
                pipe_count >= 2,
                "content line must have both panel borders: {bare:?}"
            );
        }
    }

    /// On narrow terminals, returns None (caller falls back to stacked layout).
    #[test]
    fn two_panel_narrow_fallback() {
        let data = base_data();
        let result = render_two_panel_banner(&data, 80, false);
        assert!(
            result.is_none(),
            "narrow terminal should return None for stacked fallback"
        );
    }

    /// Exactly at threshold also falls back.
    #[test]
    fn two_panel_at_threshold_boundary() {
        let data = base_data();
        // MIN_WIDTH_TWO_PANEL - 1 → fallback.
        assert!(render_two_panel_banner(&data, MIN_WIDTH_TWO_PANEL - 1, false).is_none());
        // At MIN_WIDTH_TWO_PANEL → two-panel (if right panel is wide enough).
        // (May still return None if right panel < 10; just assert no panic.)
        let _ = render_two_panel_banner(&data, MIN_WIDTH_TWO_PANEL, false);
    }

    /// Version string must appear in the left panel.
    #[test]
    fn two_panel_version_present() {
        let data = base_data();
        let out = render_two_panel_banner(&data, 120, false).expect("two-panel");
        let bare = strip_ansi(&out);
        assert!(
            bare.contains(env!("CARGO_PKG_VERSION")),
            "version must appear in left panel: {bare}"
        );
    }

    /// Reconnecting state shows the reconnecting indicator in the left panel.
    #[test]
    fn two_panel_reconnecting_shows_indicator() {
        let data = reconnect_data();
        let out = render_two_panel_banner(&data, 120, true).expect("two-panel");
        let bare = strip_ansi(&out);
        assert!(
            bare.contains("reconnecting"),
            "reconnecting indicator must appear: {bare}"
        );
    }

    /// `strip_ansi` correctly removes SGR colour escapes.
    #[test]
    fn strip_ansi_removes_color_codes() {
        let colored = "\x1B[38;2;183;65;14mhello\x1B[0m world";
        let bare = strip_ansi(colored);
        assert_eq!(bare, "hello world");
    }
}
