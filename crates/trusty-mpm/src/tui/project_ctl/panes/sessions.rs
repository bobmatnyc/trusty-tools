//! Sessions pane (right column, 75% width) — DOC-35 §5 pane 2.
//!
//! Why: the selected project's sessions, numbered per DOC-16 §3.2, each
//! showing a lifecycle-state glyph, branch, and a one-line detail. The glyph
//! is driven ONLY by the session record's static `ManagedSessionState` word —
//! there is no live `/activity` polling in this issue's scope (#2119 wires
//! that live "awaiting approval" / "idle Nm" detail into this pane later).
//! What: [`state_glyph`] / [`session_line`] are pure builders (unit tested
//! without a terminal); [`render`] composes them into a ratatui stateful
//! `List`.
//! Test: `tests` covers the glyph rule and the line content.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem},
};

use crate::tui::project_ctl::state::{Pane, ProjectCtlState, SessionRow};

/// Glyph for a session whose runtime is currently running.
pub const ACTIVE_GLYPH: char = '●';
/// Glyph for a session whose workspace is being provisioned.
pub const PROVISIONING_GLYPH: char = '◍';
/// Glyph for a session with an intact, resumable, stopped runtime.
pub const STOPPED_GLYPH: char = '○';
/// Glyph for a session whose provisioning or runtime spawn failed.
pub const ERRORED_GLYPH: char = '✗';
/// Glyph for a decommissioned (tombstoned) session.
pub const DECOMMISSIONED_GLYPH: char = '⊘';

/// Pick the lifecycle-state glyph for one session's `state` word.
///
/// Why: the static `ManagedSessionState` word (`active`/`provisioning`/
/// `stopped`/`errored`/`decommissioned`, see `session_manager::record`) is
/// the only state this issue's skeleton has available — the live
/// "awaiting approval" / "idle Nm" detail the mockup shows comes from the
/// `/activity` endpoint, deferred to #2119.
/// What: maps each of the five known state words to its glyph; any other
/// (forward-compat) value falls back to [`STOPPED_GLYPH`].
/// Test: `state_glyph_maps_every_known_state`, `state_glyph_unknown_falls_back`.
pub fn state_glyph(state: &str) -> char {
    match state {
        "active" => ACTIVE_GLYPH,
        "provisioning" => PROVISIONING_GLYPH,
        "errored" => ERRORED_GLYPH,
        "decommissioned" => DECOMMISSIONED_GLYPH,
        _ => STOPPED_GLYPH,
    }
}

/// Build one numbered session row: `N. <glyph> <short-id>  <branch>  <detail>`.
///
/// Why: DOC-16 §3.2 numbers session rows so an operator can refer to one by
/// its list position; the detail column prefers the task description and
/// falls back to the raw state word when no task is recorded.
/// What: returns e.g. `1. ● 4f9ca1b2  main  ship the thing`.
/// Test: `session_line_shows_number_glyph_branch_and_task`,
/// `session_line_falls_back_to_state_word`.
pub fn session_line(number: usize, row: &SessionRow) -> Line<'static> {
    let glyph = state_glyph(&row.state);
    let branch = row.branch.clone().unwrap_or_else(|| "-".to_string());
    let detail = row.task.clone().unwrap_or_else(|| row.state.clone());
    Line::from(vec![
        Span::styled(
            format!("{number:>2}. "),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(format!("{glyph} "), glyph_style(&row.state)),
        Span::styled(
            format!("{}  ", row.short_id),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{branch}  ")),
        Span::styled(detail, Style::default().fg(Color::DarkGray)),
    ])
}

/// Style a state glyph by its lifecycle state.
fn glyph_style(state: &str) -> Style {
    match state {
        "active" => Style::default().fg(Color::Green),
        "provisioning" => Style::default().fg(Color::Yellow),
        "errored" => Style::default().fg(Color::Red),
        "decommissioned" => Style::default().fg(Color::DarkGray),
        _ => Style::default().fg(Color::Gray),
    }
}

/// Draw the Sessions pane into `area`.
///
/// Why: the single entry point [`super::super::layout::render`] calls for the
/// right column.
/// What: a bordered, titled (`SESSIONS — <project> (N)`) stateful `List`
/// scoped to `state.current_sessions()`; the title names the selected
/// project, or reads `SESSIONS` when none is selected (empty registry).
pub fn render(frame: &mut Frame, area: Rect, state: &mut ProjectCtlState) {
    let focused = state.focus == Pane::Sessions;
    let sessions = state.current_sessions().to_vec();
    let items: Vec<ListItem> = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| ListItem::new(session_line(i + 1, s)))
        .collect();
    let project_label = state.selected_project_name().unwrap_or("-");
    let title = Line::from(format!("SESSIONS — {project_label} ({})", sessions.len()))
        .style(title_style(focused));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(list, area, state.sessions_nav.state_mut());
}

/// Style the pane title, brighter when it holds focus.
fn title_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: &str) -> SessionRow {
        SessionRow {
            id: "4f9ca1b2ffff".to_string(),
            short_id: "4f9ca1b2".to_string(),
            name: "session".to_string(),
            branch: Some("feat/x".to_string()),
            task: Some("ship the thing".to_string()),
            state: state.to_string(),
            pending_decision: None,
            proposed_default: None,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn state_glyph_maps_every_known_state() {
        assert_eq!(state_glyph("active"), ACTIVE_GLYPH);
        assert_eq!(state_glyph("provisioning"), PROVISIONING_GLYPH);
        assert_eq!(state_glyph("stopped"), STOPPED_GLYPH);
        assert_eq!(state_glyph("errored"), ERRORED_GLYPH);
        assert_eq!(state_glyph("decommissioned"), DECOMMISSIONED_GLYPH);
    }

    #[test]
    fn state_glyph_unknown_falls_back() {
        assert_eq!(state_glyph("something-new"), STOPPED_GLYPH);
    }

    #[test]
    fn session_line_shows_number_glyph_branch_and_task() {
        let text = line_text(&session_line(1, &row("active")));
        assert!(text.starts_with(" 1. "), "missing number: {text}");
        assert!(text.contains(ACTIVE_GLYPH), "missing glyph: {text}");
        assert!(text.contains("4f9ca1b2"), "missing short id: {text}");
        assert!(text.contains("feat/x"), "missing branch: {text}");
        assert!(text.contains("ship the thing"), "missing task: {text}");
    }

    #[test]
    fn session_line_falls_back_to_state_word() {
        let mut r = row("stopped");
        r.task = None;
        let text = line_text(&session_line(2, &r));
        assert!(text.contains("stopped"), "missing state fallback: {text}");
    }
}
