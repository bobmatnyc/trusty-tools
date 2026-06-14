//! Formatting and layout helpers for the unified monitor dashboard.
//!
//! Why: pure functions that format data and compute layouts can be tested
//! without a terminal backend; keeping them here makes assertions in the test
//! suite straightforward.
//! What: uptime/count formatting, thousands grouping, string truncation,
//! help text, panel layout constraints, and the status-badge mapping.
//! Test: `test_uptime_format`, `test_format_count`, `test_truncate`,
//! `test_help_text_lists_bindings`, `test_layout_wide`, `test_layout_narrow`,
//! `test_status_badge`.

use ratatui::layout::{Constraint, Direction};
use ratatui::style::Color;

use super::types::{PanelStatus, WIDE_LAYOUT_MIN_COLS};

/// Format a daemon uptime in seconds as a compact `Xh Ym` string.
///
/// Why: the search panel shows uptime; raw seconds are hard to read.
/// What: returns `"{hours}h {minutes}m"`, e.g. `7440` → `"2h 4m"`. Sub-minute
/// uptimes show `"0h 0m"`.
/// Test: `test_uptime_format`.
pub fn format_uptime(secs: u64) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    format!("{hours}h {minutes}m")
}

/// Format a count with thousands separators and a `k` suffix above 10,000.
///
/// Why: large chunk and vector counts (19,400) are easier to scan abbreviated
/// (`19.4k`); small counts stay exact.
/// What: counts below 10,000 are grouped with commas (`1,200`); counts at or
/// above 10,000 are shown as `{n}k` with one decimal (`19.4k`).
/// Test: `test_format_count`.
pub fn format_count(n: u64) -> String {
    if n >= 10_000 {
        let thousands = n as f64 / 1000.0;
        format!("{thousands:.1}k")
    } else {
        group_thousands(n)
    }
}

/// Insert commas every three digits into a number.
///
/// Why: shared by [`format_count`] for the exact-count branch.
/// What: returns the decimal string of `n` with `,` group separators.
/// Test: covered via `test_format_count`.
fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let bytes = digits.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Compute the layout constraints for the two daemon panels.
///
/// Why: the wide/narrow decision is the dashboard's single responsive rule;
/// isolating it as a pure function makes it directly unit-testable.
/// What: returns `(Direction, [Constraint; 2])` — `Horizontal` with two equal
/// halves when `width >= WIDE_LAYOUT_MIN_COLS`, otherwise `Vertical` with two
/// equal halves so the panels stack.
/// Test: `test_layout_wide`, `test_layout_narrow`.
pub fn panel_layout(width: u16) -> (Direction, [Constraint; 2]) {
    if width >= WIDE_LAYOUT_MIN_COLS {
        (
            Direction::Horizontal,
            [Constraint::Percentage(50), Constraint::Percentage(50)],
        )
    } else {
        (
            Direction::Vertical,
            [Constraint::Percentage(50), Constraint::Percentage(50)],
        )
    }
}

/// The body text for the help overlay, one binding per line.
///
/// Why: kept separate so a test can assert every binding is documented.
/// What: returns the multi-line help string.
/// Test: `test_help_text_lists_bindings`.
pub fn help_text() -> String {
    [
        "  Tab     switch focus between the search and memory panels",
        "  r       reindex the first index of the focused search panel",
        "  ?       toggle this help overlay",
        "  Esc     close this help overlay",
        "  q       quit",
        "",
        "  Offline panels retry automatically every 5 seconds.",
    ]
    .join("\n")
}

/// The status badge `(glyph, label, colour)` for a panel.
///
/// Why: every panel header shows a coloured liveness badge; centralising the
/// mapping keeps the two panel renderers consistent and testable.
/// What: `● ONLINE` (green), `◌ CONNECTING` (yellow), `○ OFFLINE` (red).
/// Test: `test_status_badge`.
pub fn status_badge<T>(status: &PanelStatus<T>) -> (char, &'static str, Color) {
    match status {
        PanelStatus::Online(_) => ('●', "ONLINE", Color::Green),
        PanelStatus::Connecting => ('◌', "CONNECTING", Color::Yellow),
        PanelStatus::Offline { .. } => ('○', "OFFLINE", Color::Red),
    }
}

/// Truncate a string to `max` characters, appending an ellipsis when cut.
///
/// Why: index ids and palace names can be long; the fixed-width table columns
/// need bounded labels.
/// What: returns `s` unchanged when short enough, else its first `max - 1`
/// characters plus `…`.
/// Test: `test_truncate`.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}
