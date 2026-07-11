//! Activity pane (bottom strip, 4 rows) — DOC-35 §5 pane 3, SKELETON ONLY.
//!
//! Why: the design spec sources this pane from `GET .../{id}/activity`
//! (`state`, `summary`, `pending_decision`, `proposed_default`), but per
//! #2118's scope that live wiring is deferred to #2119. This skeleton instead
//! renders the focused session's already-polled STATIC record fields —
//! `state`, `task`, `pending_decision`, `proposed_default` — which travel
//! through [`super::super::poll::session_to_row`] on the same fleet poll the
//! Sessions pane uses, at zero extra HTTP cost.
//! What: [`activity_lines`] is a pure builder (unit tested without a
//! terminal); [`render`] wraps it in a bordered `Paragraph`.
//! Test: `tests` covers the no-selection, no-pending, and pending-decision
//! text shapes.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};

use crate::tui::project_ctl::state::{Pane, ProjectCtlState};

/// Build the Activity pane's body lines for the currently focused session.
///
/// Why: a pure text builder keeps the "no selection" / "no pending decision" /
/// "pending decision" shapes testable without a terminal.
/// What: with no session selected, a single placeholder line naming the
/// deferred live wiring (#2119). Otherwise: a header line (`session <name>
/// (<short-id>)`), a state line naming the static `state` word plus the
/// pending-decision question and proposed default when present (else
/// "no pending decision"), and a task line when the record has one.
/// Test: `activity_lines_no_selection`, `activity_lines_no_pending_decision`,
/// `activity_lines_shows_pending_decision`.
pub fn activity_lines(state: &ProjectCtlState) -> Vec<String> {
    let Some(session) = state.selected_session() else {
        return vec![
            "no session selected".to_string(),
            "(live activity polling is deferred to #2119 — this pane shows only the \
             already-fetched session record)"
                .to_string(),
        ];
    };

    let mut lines = vec![format!("session {} ({})", session.name, session.short_id)];

    let decision_segment = match (&session.pending_decision, &session.proposed_default) {
        (Some(q), Some(d)) => format!(" · pending: \"{q}\" · proposed default: {d}"),
        (Some(q), None) => format!(" · pending: \"{q}\""),
        _ => " · no pending decision".to_string(),
    };
    lines.push(format!("state: {}{decision_segment}", session.state));

    if let Some(task) = &session.task {
        lines.push(format!("task: {task}"));
    }

    lines
}

/// Draw the Activity pane into `area`.
///
/// Why: the single entry point [`super::super::layout::render`] calls for the
/// bottom strip.
/// What: a bordered, titled (`ACTIVITY`) `Paragraph` over [`activity_lines`];
/// the title is styled cyan/bold when this pane has focus.
pub fn render(frame: &mut Frame, area: Rect, state: &ProjectCtlState) {
    let focused = state.focus == Pane::Activity;
    let title_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let lines: Vec<Line<'static>> = activity_lines(state).into_iter().map(Line::from).collect();
    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Line::from("ACTIVITY").style(title_style)),
    );
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::project_ctl::state::{ProjectRow, SessionRow};

    fn state_with_session(session: SessionRow) -> ProjectCtlState {
        let mut state = ProjectCtlState {
            projects: vec![ProjectRow {
                name: "widget".to_string(),
                repo_url: "https://github.com/acme/widget".to_string(),
                live_count: 1,
                total_count: 1,
            }],
            ..Default::default()
        };
        state
            .sessions_by_project
            .insert("widget".to_string(), vec![session]);
        state.projects_nav.sync_len(state.projects.len());
        state.sessions_nav.sync_len(state.current_sessions().len());
        state
    }

    fn base_session() -> SessionRow {
        SessionRow {
            id: "4f9ca1b2ffff".to_string(),
            short_id: "4f9ca1b2".to_string(),
            name: "widget-session".to_string(),
            branch: Some("main".to_string()),
            task: Some("ship the thing".to_string()),
            state: "active".to_string(),
            pending_decision: None,
            proposed_default: None,
        }
    }

    #[test]
    fn activity_lines_no_selection() {
        let state = ProjectCtlState::default();
        let lines = activity_lines(&state);
        assert!(lines[0].contains("no session selected"));
    }

    #[test]
    fn activity_lines_no_pending_decision() {
        let state = state_with_session(base_session());
        let lines = activity_lines(&state);
        assert!(lines.iter().any(|l| l.contains("state: active")));
        assert!(lines.iter().any(|l| l.contains("no pending decision")));
        assert!(lines.iter().any(|l| l.contains("ship the thing")));
    }

    #[test]
    fn activity_lines_shows_pending_decision() {
        let mut session = base_session();
        session.pending_decision = Some("write to ci.yml?".to_string());
        session.proposed_default = Some("yes".to_string());
        let state = state_with_session(session);
        let lines = activity_lines(&state);
        assert!(lines.iter().any(|l| l.contains("write to ci.yml?")));
        assert!(lines.iter().any(|l| l.contains("proposed default: yes")));
    }
}
