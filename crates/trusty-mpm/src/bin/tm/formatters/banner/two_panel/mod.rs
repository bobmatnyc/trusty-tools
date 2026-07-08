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
//! `wide_layout_has_vertical_divider` in the sibling `tests` module.

use colored::Colorize as _;
use unicode_width::UnicodeWidthStr as _;

use super::load_and_shade_banner;
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
/// What: loads the operator's banner art (disk override or embedded default)
/// via `load_and_shade_banner`, then delegates to `render_two_panel_banner_with_image`.
/// Wide terminals (≥ MIN_WIDTH_TWO_PANEL) show the clipped art image (left),
/// 1-char gutter, and info content (right) inside one `╭─╮` box. Narrow
/// terminals stack the same content vertically. Very narrow (< MIN_WIDTH_BOX)
/// returns `None`.
/// Test: `two_panel_compose_alignment`, `single_box_title_bar_contains_version`.
pub(crate) fn render_two_panel_banner(
    data: &WelcomeData,
    term_width: usize,
    reconnecting: bool,
) -> Option<String> {
    let (image_lines, image_cols) = load_and_shade_banner();
    render_two_panel_banner_with_image(data, term_width, reconnecting, &image_lines, image_cols)
}

/// Render the single-box banner from an explicit, already-shaded art image.
///
/// Why: issue #2224 — `render_two_panel_banner` used to load banner art from
/// disk (`~/.trusty-mpm/banner.txt`) internally, so every test exercising it
/// was unintentionally coupled to whatever the *operator's own machine*
/// happened to have on disk. A hand-customised banner containing legitimate
/// rounded box-drawing glyphs (`╭ ╮ ╰ ╯`, e.g. custom robot-face art) made
/// `right_col_no_inner_border` panic on that machine even though the renderer
/// itself was working correctly — the test had no control over its input.
/// Splitting the disk read out into this pure, parameterised function lets
/// tests pass a fixed, known art source (`DEFAULT_BANNER_ART`) so results are
/// deterministic and independent of `$HOME` state, while `render_two_panel_banner`
/// keeps the exact same production behaviour for real callers.
/// What: same rendering logic as before, but `image_lines`/`image_cols` are
/// caller-supplied instead of being loaded from disk internally.
/// Test: all `two_panel::tests` call this directly with a fixed art source;
/// `render_two_panel_banner` (disk-backed) is covered indirectly through it.
pub(crate) fn render_two_panel_banner_with_image(
    data: &WelcomeData,
    term_width: usize,
    reconnecting: bool,
    image_lines: &[String],
    image_cols: usize,
) -> Option<String> {
    if term_width < MIN_WIDTH_BOX {
        return None;
    }

    let inner = term_width.saturating_sub(2);

    if term_width >= MIN_WIDTH_TWO_PANEL {
        let right_col = inner.saturating_sub(image_cols + GUTTER);
        if right_col >= 10 {
            return Some(render_wide_box(
                data,
                term_width,
                reconnecting,
                image_lines,
                image_cols,
                right_col,
            ));
        }
    }

    Some(render_narrow_box(
        data,
        term_width,
        reconnecting,
        image_lines,
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
//
// The test suite lives in the sibling `tests.rs` file rather than an inline
// `#[cfg(test)] mod tests { ... }` block: it is classified as a test file
// under the workspace's 500/1500 SLOC dual cap (basename `tests.rs`), so it
// carries the much larger 1500-line test-file budget instead of counting
// against this file's 500-line production cap (see issue #2224).
#[cfg(test)]
pub(crate) mod tests;
