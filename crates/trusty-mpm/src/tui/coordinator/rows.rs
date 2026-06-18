//! Pure row-builders for the coordinator session list.
//!
//! Why: the coordinator TUI renders one bullet for the controller plus one row
//! per managed session, and the *selected* session expands into a two-column
//! `[id] │ [summary]` line. Keeping the text/`Line` construction here — free of
//! any terminal or daemon dependency — lets the list content be unit-tested
//! exactly as the dashboard's `sidebar_items` are, satisfying the spec's
//! "pure/unit (no terminal, no network)" test tier (DOC-13 §9).
//! What: free functions that build ratatui [`Line`]s (and a couple of `String`
//! helpers the tests assert on) for the controller bullet, a non-selected
//! session row, and the selected two-column active row. No live data is wired
//! here — Child #1 feeds these placeholder rows; Child #2/#3 swap in the live
//! feed + real `last_summary`.
//! Test: `super::tests` covers `controller_bullet_row`, `session_short_id`, and
//! the two-column split via `active_row_two_col`.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// The glyph drawn for the always-present controller bullet (row 0).
///
/// Why: the spec pins the controller as row 0 and always-active (`●`); a named
/// constant keeps the glyph in one place so the controller and an active
/// session render the same bullet.
/// What: the filled-circle bullet `●`.
/// Test: asserted in `controller_bullet_row` via the rendered span text.
pub const CONTROLLER_GLYPH: char = '●';

/// The glyph drawn for an idle / placeholder session row.
///
/// Why: non-active sessions use a hollow bullet (`○`) per the spec's bullet
/// legend (§3.2); Child #2 maps real `SessionStatus` to colored glyphs, but the
/// skeleton only needs the idle glyph for its placeholder rows.
/// What: the hollow-circle bullet `○`.
/// Test: asserted in `session_row_renders_bullet_and_prefix`.
pub const SESSION_GLYPH: char = '○';

/// The column separator drawn between a selected row's id and its summary.
///
/// Why: the spec's "active row" contract (§3.3) separates the short id from the
/// last-summarized message with a `│` glyph; centralising it keeps the split
/// rule and its unit test in agreement.
/// What: the box-drawing vertical bar `│`.
/// Test: asserted in `active_row_two_col_splits_on_bar`.
pub const COLUMN_SEPARATOR: char = '│';

/// Build the controller bullet line (always row 0 of the session list).
///
/// Why: the coordinator itself is represented by a pinned first row whose right
/// half is a synthetic status (`idle · N delegations · ready`), distinct from a
/// managed session. Rendering it through a dedicated builder keeps that contract
/// explicit and testable without a terminal.
/// What: returns a [`Line`] of `● controller   <status>`; when `selected` the
/// whole line is bold/cyan to mirror the dashboard's selection highlight.
/// Test: `controller_bullet_row` asserts the glyph, label, and status text.
pub fn controller_bullet_row(status: &str, selected: bool) -> Line<'static> {
    let bullet_style = Style::default().fg(Color::Green);
    let label_style = if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    Line::from(vec![
        Span::styled(format!("{CONTROLLER_GLYPH} "), bullet_style),
        Span::styled("controller", label_style),
        Span::raw("   "),
        Span::styled(status.to_string(), Style::default().fg(Color::DarkGray)),
    ])
}

/// Build a non-selected managed-session row.
///
/// Why: rows beneath the controller show a hollow bullet, the session's short
/// id, its routing prefix, and a dimmed one-line status — the compact form for
/// every row that is not the active selection (§3.2).
/// What: returns a [`Line`] of `○ <short-id> <prefix>   <status>` with the
/// status dimmed.
/// Test: `session_row_renders_bullet_and_prefix`.
pub fn session_row(short_id: &str, prefix: &str, status: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{SESSION_GLYPH} "),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            format!("{short_id} "),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{prefix}   ")),
        Span::styled(status.to_string(), Style::default().fg(Color::DarkGray)),
    ])
}

/// Build a numbered, non-selected managed-session row: `N. ○ <id> <prefix> …`.
///
/// Why: STUI-1 (#1278) renders the list NUMBERED (1, 2, 3, …) so an operator can
/// refer to a session by its list position; the number is purely a display index
/// (1-based), distinct from the session id. Prefixing it here keeps the numbering
/// rule in the pure row layer where it is unit-testable, and keeps [`session_row`]
/// available for the un-numbered controller-adjacent rendering paths.
/// What: returns a [`Line`] of `<n>. ` (dimmed) followed by the standard
/// [`session_row`] spans (`○ <short-id> <prefix>   <status>`). The number is
/// right-padded to two columns so single- and double-digit numbers align.
/// Test: `numbered_session_row_prefixes_number`.
pub fn numbered_session_row(
    number: usize,
    short_id: &str,
    prefix: &str,
    status: &str,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{number:>2}. "),
        Style::default().fg(Color::DarkGray),
    )];
    spans.extend(session_row(short_id, prefix, status).spans);
    Line::from(spans)
}

/// Build the selected session's two-column active row: `[id] │ [summary]`.
///
/// Why: the headline feature of the coordinator list is that the *active* row
/// expands to show what the session is doing. The summary source (daemon
/// `last_summary`) lands in Child #3; Child #1 renders whatever placeholder
/// summary it is handed so the column contract (§3.3) exists and is testable.
/// What: returns a [`Line`] of `● <short-id> │ <summary>`, the whole row
/// bold/cyan to mark it as the selection; the `│` separator is the single
/// source of the column split.
/// Test: `active_row_two_col_splits_on_bar`.
pub fn active_row_two_col(short_id: &str, summary_placeholder: &str) -> Line<'static> {
    let accent = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    Line::from(vec![
        Span::styled(format!("{CONTROLLER_GLYPH} "), accent),
        Span::styled(format!("{short_id} "), accent),
        Span::styled(
            format!("{COLUMN_SEPARATOR} "),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(summary_placeholder.to_string(), accent),
    ])
}

/// Truncate a session id to its short, human-readable 8-char form.
///
/// Why: the list's left column shows the first 8 hex chars of the UUID, not the
/// full id (§3.3); a shared helper keeps the rule identical to the dashboard's
/// `short_session` so the two surfaces never diverge.
/// What: returns the first 8 characters of `id` (fewer if `id` is shorter).
/// Test: `session_short_id_truncates`.
pub fn session_short_id(id: &str) -> String {
    id.chars().take(8).collect()
}
