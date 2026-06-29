//! Single-box full-width banner compositor.
//!
//! Why: the previous two-separate-frames layout left visual gaps between panels.
//! One outer box with the version embedded in the top border is more cohesive,
//! and dropping the inner frames gives more usable width for content.
//! What: `render_two_panel_banner` (same public signature as before) produces a
//! single rounded box whose title bar carries `Trusty MPM v{VERSION}`. Inside,
//! wide terminals (≥ MIN_WIDTH_TWO_PANEL) get the clipped art image (left) and
//! info content (right) separated by a tinted `│` vertical divider (` │ `);
//! narrow terminals get a stacked layout inside the same box. Very narrow
//! (< MIN_WIDTH_BOX) returns `None` so callers can fall back to plain output.
//! Test: `single_box_title_bar_contains_version`, `image_clip_correct_rows`,
//! `right_col_no_inner_border`, `reconnect_label_in_narrow_box`,
//! `two_panel_compose_alignment`, `two_panel_version_present`,
//! `wide_layout_has_vertical_divider`.

use colored::Colorize as _;
use unicode_width::UnicodeWidthStr as _;

use super::{IMAGE_CLIP_COLS, clip_and_shade_image};
use crate::formatters::info_box::{WelcomeData, render_info_box_rows};

// ── Layout constants ──────────────────────────────────────────────────────────

/// Minimum terminal width for the wide two-column layout.
/// Below this threshold the narrow stacked layout is used inside the same outer box.
pub(crate) const MIN_WIDTH_TWO_PANEL: usize = 90;

/// Minimum terminal width to render any outer box at all.
const MIN_WIDTH_BOX: usize = 40;

/// Three-character gutter between the image column and the info column: ` │ `.
///
/// The middle character is a tinted `│` vertical divider; the flanking spaces
/// provide visual breathing room. Total gutter width must match the rendered
/// ` ` + `│` + ` ` = 3 display columns in every interior row.
const GUTTER: usize = 3;

/// Muted rust colour for outer-box borders.
const BORDER_R: u8 = 120;
const BORDER_G: u8 = 50;
const BORDER_B: u8 = 10;

// ── Box-drawing characters ─────────────────────────────────────────────────────
const TL: char = '╭';
const TR: char = '╮';
const BL: char = '╰';
const BR: char = '╯';
const HORIZ: char = '─';
const VERT: char = '│';
/// Bottom connector for the vertical divider where it meets the bottom border.
const DIVIDER_BOTTOM: char = '┴';

// ── Public entry point ────────────────────────────────────────────────────────

/// Render the single-box banner as a `String` (pure — no I/O, no screen-clear).
///
/// Why: a single outer box with version in the title bar is more cohesive than
/// two separate side-by-side frames. Signature is unchanged so callers need no edits.
/// What: wide terminals (≥ MIN_WIDTH_TWO_PANEL) show the clipped art image (left),
/// 1-char gutter, and info content (right) inside one `╭─╮` box. Narrow
/// terminals stack the same content vertically. Very narrow (< MIN_WIDTH_BOX)
/// returns `None`.
/// Test: `two_panel_compose_alignment`, `single_box_title_bar_contains_version`.
pub(crate) fn render_two_panel_banner(
    data: &WelcomeData,
    term_width: usize,
    reconnecting: bool,
) -> Option<String> {
    if term_width < MIN_WIDTH_BOX {
        return None;
    }

    let image_lines = clip_and_shade_image();
    let image_cols = IMAGE_CLIP_COLS;

    let inner = term_width.saturating_sub(2);

    if term_width >= MIN_WIDTH_TWO_PANEL {
        let right_col = inner.saturating_sub(image_cols + GUTTER);
        if right_col >= 10 {
            return Some(render_wide_box(
                data,
                term_width,
                reconnecting,
                &image_lines,
                image_cols,
                right_col,
            ));
        }
    }

    Some(render_narrow_box(
        data,
        term_width,
        reconnecting,
        &image_lines,
        image_cols,
    ))
}

// ── Title bar ────────────────────────────────────────────────────────────────

/// Render the top border with `Trusty MPM v{VERSION}` embedded.
///
/// Why: embedding the version in the border line eliminates the separate
/// wordmark block, saving vertical space and giving the banner a branded frame.
/// What: produces `╭──── Trusty MPM vX.Y.Z ────…────╮` of exactly `term_width`
/// display columns. The `─` corners are tinted rust; the label text is plain.
/// Test: `single_box_title_bar_contains_version`.
fn render_title_bar(term_width: usize) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let label = format!(" Trusty MPM v{version} ");
    let label_cols = label.len(); // ASCII-only
    let inner = term_width.saturating_sub(2);
    let dashes_left = 4usize.min(inner.saturating_sub(label_cols));
    let dashes_right = inner.saturating_sub(dashes_left + label_cols);
    let left = tint_border(&format!("{TL}{}", HORIZ.to_string().repeat(dashes_left)));
    let right = tint_border(&format!("{}{TR}", HORIZ.to_string().repeat(dashes_right)));
    format!("{left}{label}{right}")
}

// ── Wide two-column layout ────────────────────────────────────────────────────

/// Render the wide (≥ MIN_WIDTH_TWO_PANEL) two-column single-box banner.
///
/// Why: wide terminals have enough room to show the art image (left) and info
/// (right) side by side without wrapping. A tinted vertical divider makes the
/// column boundary visually explicit without adding a full inner frame.
/// What: title bar on top, `left_col`-wide left col + ` │ ` divider (3 chars,
/// tinted rust) + `right_col`-wide right col, bottom border with `┴` at the
/// divider column. Height = max(image rows, info rows).
/// Test: `two_panel_compose_alignment`, `right_col_no_inner_border`,
/// `wide_layout_has_vertical_divider`.
fn render_wide_box(
    data: &WelcomeData,
    term_width: usize,
    reconnecting: bool,
    image_lines: &[String],
    left_col: usize,
    right_col: usize,
) -> String {
    let inner = left_col + GUTTER + right_col; // = term_width - 2

    let right_lines = build_right_lines(data, right_col);

    // Pad image rows to left_col. Image lines are already per-char colorized;
    // measure display width via strip_ansi.
    let mut left_lines: Vec<String> = image_lines
        .iter()
        .map(|row| {
            let raw_cols = strip_ansi(row).width();
            let pad = left_col.saturating_sub(raw_cols);
            format!("{row}{}", " ".repeat(pad))
        })
        .collect();

    if reconnecting {
        left_lines.push(" ".repeat(left_col));
        let rcon = "Reconnecting...";
        let rcon_cols = rcon.width();
        let pad_l = (left_col.saturating_sub(rcon_cols)) / 2;
        let pad_r = left_col.saturating_sub(rcon_cols + pad_l);
        left_lines.push(format!(
            "{}{}{}",
            " ".repeat(pad_l),
            rcon,
            " ".repeat(pad_r)
        ));
    }

    let height = left_lines.len().max(right_lines.len());

    let mut out = String::new();
    out.push_str(&render_title_bar(term_width));
    out.push('\n');

    for i in 0..height {
        let left = left_lines
            .get(i)
            .cloned()
            .unwrap_or_else(|| " ".repeat(left_col));
        let right = right_lines
            .get(i)
            .cloned()
            .unwrap_or_else(|| " ".repeat(right_col));
        out.push_str(&tint_border(&VERT.to_string()));
        out.push_str(&left);
        out.push_str(&format!(" {} ", tint_border(&VERT.to_string())));
        out.push_str(&right);
        out.push_str(&tint_border(&VERT.to_string()));
        out.push('\n');
    }

    // Bottom border: ┴ at the divider column (left_col+1 within inner dashes).
    // `inner = left_col + GUTTER + right_col` with right_col ≥ 10, so the
    // subtraction `inner - left_col - 2` is always ≥ right_col - 1 > 0.
    let bottom = format!(
        "{BL}{}{DIVIDER_BOTTOM}{}{BR}",
        HORIZ.to_string().repeat(left_col + 1),
        HORIZ.to_string().repeat(inner - left_col - 2)
    );
    out.push_str(&tint_border(&bottom));
    out.push('\n');

    out
}

// ── Narrow stacked layout ─────────────────────────────────────────────────────

/// Render the narrow (< MIN_WIDTH_TWO_PANEL) stacked single-box banner.
///
/// Why: on terminals narrower than MIN_WIDTH_TWO_PANEL the right column would
/// be too narrow to read. Stacking art image then info vertically inside the same
/// outer box preserves the brand frame while fitting the content.
/// What: title bar, centred image rows, optional "Reconnecting..." label,
/// left-aligned info rows, bottom border. All inside the single outer box.
/// Test: `reconnect_label_in_narrow_box`.
fn render_narrow_box(
    data: &WelcomeData,
    term_width: usize,
    reconnecting: bool,
    image_lines: &[String],
    _image_cols: usize,
) -> String {
    let inner = term_width.saturating_sub(2);

    let mut out = String::new();
    out.push_str(&render_title_bar(term_width));
    out.push('\n');

    // Image rows — centred within inner.
    for row in image_lines {
        let raw_cols = strip_ansi(row).width();
        let pad_l = (inner.saturating_sub(raw_cols)) / 2;
        let pad_r = inner.saturating_sub(raw_cols + pad_l);
        out.push_str(&tint_border(&VERT.to_string()));
        out.push_str(&" ".repeat(pad_l));
        out.push_str(row);
        out.push_str(&" ".repeat(pad_r));
        out.push_str(&tint_border(&VERT.to_string()));
        out.push('\n');
    }

    // Reconnecting label.
    if reconnecting {
        let label = "Reconnecting...";
        let label_cols = label.width();
        let pad_l = (inner.saturating_sub(label_cols)) / 2;
        let pad_r = inner.saturating_sub(label_cols + pad_l);
        out.push_str(&tint_border(&VERT.to_string()));
        out.push_str(&" ".repeat(pad_l));
        out.push_str(label);
        out.push_str(&" ".repeat(pad_r));
        out.push_str(&tint_border(&VERT.to_string()));
        out.push('\n');
    }

    // Info rows — left-aligned, truncated/padded to inner.
    for row in build_right_lines(data, inner) {
        let bare = strip_ansi(&row);
        let bare_cols = bare.width();
        let pad = inner.saturating_sub(bare_cols);
        out.push_str(&tint_border(&VERT.to_string()));
        out.push_str(&row);
        out.push_str(&" ".repeat(pad));
        out.push_str(&tint_border(&VERT.to_string()));
        out.push('\n');
    }

    let bottom = format!("{BL}{}{BR}", HORIZ.to_string().repeat(inner));
    out.push_str(&tint_border(&bottom));
    out.push('\n');

    out
}

// ── Right-column builder ──────────────────────────────────────────────────────

/// Build right-column content lines from the welcome data, padded to `inner_width`.
///
/// Why: the right panel mirrors the existing info-box content; reusing
/// `render_info_box_rows` avoids duplicating the row-building logic.
/// What: calls `render_info_box_rows(data)` to get content rows, then pads
/// or truncates each to `inner_width` display columns.
/// Test: `two_panel_compose_alignment`.
fn build_right_lines(data: &WelcomeData, inner_width: usize) -> Vec<String> {
    let rows = render_info_box_rows(data);
    rows.into_iter()
        .map(|row| {
            let bare = strip_ansi(&row);
            let cols = bare.width();
            if cols <= inner_width {
                format!("{row}{}", " ".repeat(inner_width - cols))
            } else {
                truncate_to_cols(row, inner_width)
            }
        })
        .collect()
}

// ── String utilities ──────────────────────────────────────────────────────────

/// Strip ANSI escape sequences from `s`, returning display-only text.
///
/// Why: ANSI escapes are zero display width but non-zero byte length; stripping
/// them lets us compute correct display widths and padding amounts.
/// What: walks the string, discarding `\x1B[…m` SGR sequences.
/// Test: `strip_ansi_removes_color_codes`.
pub(crate) fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1B' {
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

/// Truncate `s` to at most `max_cols` display columns, appending `…`.
///
/// Why: right-panel rows wider than `inner_width` would push the border char
/// out of position.
/// What: accumulates char widths, stops before exceeding `max_cols - 1` columns,
/// then appends `…` (U+2026). Returns `s` unchanged when it already fits.
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
    format!("{out}\u{2026}{}", " ".repeat(pad))
}

/// Apply muted rust tint to a border string.
///
/// Why: tinted borders give the layout a cohesive brand colour without
/// overwhelming the content.
/// What: wraps `s` in the BORDER_R/G/B truecolor escape; degrades to plain
/// text when `colored` has colour disabled (NO_COLOR, non-TTY).
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

    /// Single-box banner renders without panic; every line starts with a border char.
    #[test]
    fn two_panel_compose_alignment() {
        colored::control::set_override(false);
        let data = base_data();
        let result = render_two_panel_banner(&data, 120, false);
        assert!(result.is_some(), "wide terminal should produce a banner");
        let out = result.unwrap();
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
        colored::control::unset_override();
    }

    /// Title bar must embed the crate version.
    #[test]
    fn single_box_title_bar_contains_version() {
        colored::control::set_override(false);
        let data = base_data();
        let out =
            render_two_panel_banner(&data, 120, false).expect("wide terminal produces banner");
        let first_line = out.lines().next().unwrap_or("");
        let bare = strip_ansi(first_line);
        assert!(
            bare.contains(env!("CARGO_PKG_VERSION")),
            "title bar must contain CARGO_PKG_VERSION: {bare:?}"
        );
        assert!(
            bare.starts_with('╭'),
            "title bar must start with ╭: {bare:?}"
        );
        colored::control::unset_override();
    }

    /// Clipped image has exactly IMAGE_CLIP_ROWS rows.
    #[test]
    fn image_clip_correct_rows() {
        let lines = super::super::clip_and_shade_image();
        assert_eq!(
            lines.len(),
            super::super::IMAGE_CLIP_ROWS,
            "clipped image must have exactly IMAGE_CLIP_ROWS ({}) rows, got {}",
            super::super::IMAGE_CLIP_ROWS,
            lines.len()
        );
        assert!(
            lines.len() >= 20 && lines.len() <= 28,
            "clipped rows ({}) must be within 20–28 (CLIP_ROW_END=24)",
            lines.len()
        );
    }

    /// Each row of the clipped image has exactly IMAGE_CLIP_COLS display columns.
    #[test]
    fn image_clip_correct_cols() {
        colored::control::set_override(false);
        let lines = super::super::clip_and_shade_image();
        let expected = super::super::IMAGE_CLIP_COLS;
        for (i, line) in lines.iter().enumerate() {
            let bare = strip_ansi(line);
            let width = bare.width();
            assert_eq!(
                width, expected,
                "row {i} display width ({width}) must equal IMAGE_CLIP_COLS ({expected})"
            );
        }
        colored::control::unset_override();
    }

    /// Non-space art characters always emit truecolor escapes.
    #[test]
    fn image_shading_emits_truecolor() {
        // clip_and_shade_image emits raw ANSI truecolor escapes directly
        // (not via the `colored` global state) so this test needs no override.
        let lines = super::super::clip_and_shade_image();
        let any_colored = lines.iter().any(|l| l.contains("\x1B[38;2;"));
        assert!(
            any_colored,
            "clipped image must always emit truecolor escapes for non-space chars"
        );
    }

    /// The right-column content has no inner-box border chars (╭╮╰╯).
    #[test]
    fn right_col_no_inner_border() {
        colored::control::set_override(false);
        let data = base_data();
        let out = render_two_panel_banner(&data, 120, false).expect("banner");
        for line in out.lines() {
            let bare = strip_ansi(line);
            if bare.starts_with('╭') || bare.starts_with('╰') {
                continue;
            }
            let inner: String = bare.chars().skip(1).collect();
            let inner_trimmed = inner.trim_end_matches('│');
            for ch in ['╭', '╮', '╰', '╯'] {
                assert!(
                    !inner_trimmed.contains(ch),
                    "inner border char {ch:?} must not appear inside content rows: {bare:?}"
                );
            }
        }
        colored::control::unset_override();
    }

    /// Reconnecting label appears in the stacked narrow-fallback box.
    #[test]
    fn reconnect_label_in_narrow_box() {
        colored::control::set_override(false);
        let data = reconnect_data();
        let out = render_two_panel_banner(&data, 80, true).expect("narrow box");
        let bare = strip_ansi(&out);
        assert!(
            bare.contains("Reconnecting..."),
            "narrow box must contain 'Reconnecting...' label: {bare:.200}"
        );
        colored::control::unset_override();
    }

    /// Reconnecting label is absent from a normal (non-reconnect) narrow banner.
    #[test]
    fn narrow_box_no_reconnect_label_when_not_reconnecting() {
        colored::control::set_override(false);
        let data = base_data();
        let out = render_two_panel_banner(&data, 80, false).expect("narrow box");
        let bare = strip_ansi(&out);
        assert!(
            !bare.contains("Reconnecting..."),
            "normal narrow box must not contain 'Reconnecting...' label"
        );
        colored::control::unset_override();
    }

    /// Both panels reach equal height (shorter one is padded).
    #[test]
    fn two_panel_shorter_panel_padded_to_equal_height() {
        let data = data_with_commits();
        colored::control::set_override(false);
        let out = render_two_panel_banner(&data, 130, false).expect("wide banner");
        for line in out.lines() {
            let bare = strip_ansi(line);
            if bare.starts_with('│') {
                assert!(
                    bare.ends_with('│'),
                    "content line must end with │: {bare:?}"
                );
            }
        }
        colored::control::unset_override();
    }

    /// Very narrow terminal (< MIN_WIDTH_BOX) returns None.
    #[test]
    fn two_panel_narrow_fallback() {
        let data = base_data();
        assert!(
            render_two_panel_banner(&data, MIN_WIDTH_BOX - 1, false).is_none(),
            "very narrow terminal must return None"
        );
    }

    /// Terminals narrower than MIN_WIDTH_TWO_PANEL but ≥ MIN_WIDTH_BOX get a stacked box.
    #[test]
    fn two_panel_stacked_on_medium_width() {
        let data = base_data();
        let result = render_two_panel_banner(&data, 80, false);
        assert!(
            result.is_some(),
            "80-col terminal must produce a stacked box (not None)"
        );
    }

    /// At the wide-mode threshold boundary, no panic.
    #[test]
    fn two_panel_at_threshold_boundary() {
        let data = base_data();
        let _ = render_two_panel_banner(&data, MIN_WIDTH_TWO_PANEL - 1, false);
        let _ = render_two_panel_banner(&data, MIN_WIDTH_TWO_PANEL, false);
    }

    /// Version string must appear in the output (in the title bar).
    #[test]
    fn two_panel_version_present() {
        colored::control::set_override(false);
        let data = base_data();
        let out = render_two_panel_banner(&data, 120, false).expect("two-panel");
        let bare = strip_ansi(&out);
        assert!(
            bare.contains(env!("CARGO_PKG_VERSION")),
            "version must appear in banner: {bare:.100}"
        );
        colored::control::unset_override();
    }

    /// The two-panel banner must not repeat the version in the right panel:
    /// the title bar already shows it.  The full banner has exactly one copy.
    #[test]
    fn right_panel_omits_version_line() {
        colored::control::set_override(false);
        let data = base_data();
        let version = env!("CARGO_PKG_VERSION");
        let out = render_two_panel_banner(&data, 120, false).expect("wide banner");
        let bare = strip_ansi(&out);
        let count = bare.matches(version).count();
        assert_eq!(
            count, 1,
            "version string must appear exactly once (title bar only); found {count}"
        );
        colored::control::unset_override();
    }

    /// Reconnecting state shows the reconnecting label in the wide layout.
    #[test]
    fn two_panel_reconnecting_shows_indicator() {
        colored::control::set_override(false);
        let data = reconnect_data();
        let out = render_two_panel_banner(&data, 120, true).expect("two-panel");
        let bare = strip_ansi(&out);
        assert!(
            bare.contains("Reconnecting..."),
            "reconnecting indicator must appear: {bare:.200}"
        );
        colored::control::unset_override();
    }

    /// `strip_ansi` correctly removes SGR colour escapes.
    #[test]
    fn strip_ansi_removes_color_codes() {
        let colored = "\x1B[38;2;183;65;14mhello\x1B[0m world";
        let bare = strip_ansi(colored);
        assert_eq!(bare, "hello world");
    }

    /// Wide layout has a `│` vertical divider between columns; every line is
    /// exactly `term_width` display columns wide.
    #[test]
    fn wide_layout_has_vertical_divider() {
        colored::control::set_override(false);
        let data = base_data();
        let out = render_two_panel_banner(&data, 120, false).expect("wide banner");
        let mut checked = 0usize;
        for line in out.lines() {
            let bare = strip_ansi(line);
            if bare.is_empty() {
                continue;
            }
            assert_eq!(bare.width(), 120, "line width mismatch: {bare:?}");
            if bare.starts_with('│') {
                let chars: Vec<char> = bare.chars().collect();
                let inner = &chars[1..chars.len().saturating_sub(1)];
                assert!(inner.contains(&'│'), "no interior │ divider in: {bare:?}");
                checked += 1;
            }
        }
        assert!(checked > 0, "no content lines found in wide banner");
        colored::control::unset_override();
    }
}
