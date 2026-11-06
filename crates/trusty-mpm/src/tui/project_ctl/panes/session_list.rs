//! The session column: the right column of the MPM pane (#10754).
//!
//! Why: the owner asked for the sessions list beside the owner/project tree
//! (Bob 11-06 00:47Z), so the tree stays short and a project's sessions read
//! as one list.
//! What: [`row_line`] builds one line from a
//! [`SessionColRow`] (pure, tested without a terminal): a project header in
//! bold with its counters, a session indented two columns per worker level
//! with `↳` before a worker (reusing [`session_spans`]), or a dimmed anchor.
//! Rows under a header indent two more columns. [`empty_hint`] is the line
//! shown when there is nothing to list. [`render`] draws the column and
//! records its rows' rectangle for the mouse.
//! Test: `worker_rows_indent_and_anchor_is_dim`, `empty_hint_names_the_case`;
//! `two_column_tests.rs`.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem, Paragraph},
};

use super::sessions::session_spans;
use super::tree::{counter_text, title_style};
use crate::tui::project_ctl::state::{
    DeliverableLinkState, NodeId, Pane, ProjectCtlState, SessionColRow, TreeRowKind,
};

/// Build the line for one session-column row; `grouped` adds the two-column
/// indent rows take under a project header.
pub fn row_line(state: &ProjectCtlState, row: &SessionColRow, grouped: bool) -> Line<'static> {
    let base = if grouped { "  " } else { "" };
    match row {
        SessionColRow::Header {
            project,
            total,
            live,
        } => Line::from(vec![
            Span::styled(
                format!("{project}  "),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                counter_text(*total, *live),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        SessionColRow::Session {
            project,
            index,
            depth,
            ..
        } => {
            let Some(session) = state
                .sessions_by_project
                .get(project)
                .and_then(|rows| rows.get(*index))
            else {
                return Line::from("");
            };
            let link = session
                .deliverable_id
                .as_deref()
                .map_or(DeliverableLinkState::Unknown, |id| {
                    state.deliverable_link_state(id)
                });
            let lead = if *depth > 0 { "↳ " } else { "" };
            let mut spans = vec![Span::raw(format!("{base}{}{lead}", "  ".repeat(*depth)))];
            spans.extend(session_spans(session, link));
            Line::from(spans)
        }
        SessionColRow::Anchor { parent, depth } => {
            let short: String = parent.chars().take(8).collect();
            Line::from(Span::styled(
                format!("{base}{}↳ parent {short} (not listed)", "  ".repeat(*depth)),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ))
        }
    }
}

/// The name the left selection shows: an owner's spelling or a project name.
fn source_name(state: &ProjectCtlState) -> Option<String> {
    let selected = state.selected_node()?;
    state
        .tree
        .rows
        .iter()
        .find(|r| &r.id == selected)
        .map(|r| match &r.kind {
            TreeRowKind::Owner { name, .. } | TreeRowKind::Project { name, .. } => name.clone(),
        })
}

/// The line an empty session column shows.
///
/// Test: `empty_hint_names_the_case`.
pub fn empty_hint(state: &ProjectCtlState) -> String {
    let name = source_name(state).unwrap_or_default();
    match state.selected_node() {
        Some(NodeId::Project { .. }) => format!("no sessions in {name} — [l] launch one"),
        Some(NodeId::Owner(_)) => format!("no sessions under {name}"),
        None => "no project selected".to_string(),
    }
}

/// Draw the session (right) column into `area`.
///
/// What: a bordered list titled `SESSIONS — <name> (<n>)`, cyan while
/// focused, with the selected session highlighted, or the [`empty_hint`].
/// `version` puts [`crate::tui::VERSION_LABEL`] on the top border.
pub fn render(frame: &mut Frame, area: Rect, state: &mut ProjectCtlState, version: bool) {
    let focused = state.focus == Pane::Sessions;
    let count = state
        .session_col
        .rows
        .iter()
        .filter(|r| r.session_id().is_some())
        .count();
    let title = match source_name(state) {
        Some(name) => format!("SESSIONS — {name} ({count})"),
        None => "SESSIONS".to_string(),
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title).style(title_style(focused)));
    if version {
        block = block.title(Line::from(crate::tui::VERSION_LABEL).right_aligned());
    }
    state.tree.hit.session_rows = block.inner(area);
    if state.session_col.rows.is_empty() {
        let hint = Span::styled(empty_hint(state), Style::default().fg(Color::DarkGray));
        frame.render_widget(Paragraph::new(Line::from(hint)).block(block), area);
        return;
    }
    let grouped = state
        .session_col
        .rows
        .iter()
        .any(|r| matches!(r, SessionColRow::Header { .. }));
    let items: Vec<ListItem> = state
        .session_col
        .rows
        .iter()
        .map(|row| ListItem::new(row_line(state, row, grouped)))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_spacing(HighlightSpacing::Always);
    let col = &mut state.session_col;
    if col.selected.is_none() {
        // Nothing is selected until the operator enters the column.
        col.nav.state_mut().select(None);
    }
    frame.render_stateful_widget(list, area, col.nav.state_mut());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::project_ctl::state::{ProjectRow, SessionRow};

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn session(id: &str, name: &str, state: &str, parent: Option<&str>) -> SessionRow {
        SessionRow {
            id: id.to_string(),
            name: name.to_string(),
            state: state.to_string(),
            parent_session_id: parent.map(str::to_string),
            ..Default::default()
        }
    }

    fn project(name: &str) -> ProjectRow {
        ProjectRow {
            name: name.to_string(),
            repo_url: format!("https://github.com/acme/{name}"),
            ..Default::default()
        }
    }

    /// acme/alpha with a manager, its worker, and an orphan; acme/beta empty.
    fn state() -> ProjectCtlState {
        let mut state = ProjectCtlState {
            projects: vec![project("alpha"), project("beta")],
            ..Default::default()
        };
        state.sessions_by_project.insert(
            "alpha".to_string(),
            vec![
                session("m1", "boss", "active", None),
                session("w1", "helper", "stopped", Some("m1")),
                session("o1", "orphan", "active", Some("deadbeefcafe")),
            ],
        );
        state.select_node(NodeId::Project {
            owner: "acme".to_string(),
            name: "alpha".to_string(),
        });
        state
    }

    fn texts(state: &ProjectCtlState, grouped: bool) -> Vec<String> {
        state
            .session_col
            .rows
            .iter()
            .map(|r| line_text(&row_line(state, r, grouped)))
            .collect()
    }

    /// Moved from `panes::tree` (#10754): workers indent under their parent,
    /// and a parent the project does not list draws as a dimmed anchor.
    #[test]
    fn worker_rows_indent_and_anchor_is_dim() {
        let state = state();
        let lines = texts(&state, false);
        assert!(lines[0].starts_with("● boss"), "{:?}", lines[0]);
        assert!(lines[1].starts_with("  ↳ ○ helper"), "{:?}", lines[1]);
        assert_eq!(lines[2], "↳ parent deadbeef (not listed)");
        assert!(lines[3].starts_with("  ↳ ● orphan"), "{:?}", lines[3]);
        let anchor = row_line(&state, &state.session_col.rows[2], false);
        assert!(anchor.spans[0].style.add_modifier.contains(Modifier::DIM));
        // Under a project header every row indents two more columns.
        let grouped = texts(&state, true);
        assert!(grouped[1].starts_with("    ↳ ○ helper"), "{:?}", grouped[1]);
    }

    #[test]
    fn empty_hint_names_the_case() {
        let mut state = state();
        state.select_node(NodeId::Project {
            owner: "acme".to_string(),
            name: "beta".to_string(),
        });
        assert!(state.session_col.rows.is_empty());
        assert_eq!(empty_hint(&state), "no sessions in beta — [l] launch one");
        state.sessions_by_project.clear();
        state.select_node(NodeId::Owner("acme".to_string()));
        assert_eq!(empty_hint(&state), "no sessions under acme");
        assert_eq!(
            empty_hint(&ProjectCtlState::default()),
            "no project selected"
        );
    }
}
