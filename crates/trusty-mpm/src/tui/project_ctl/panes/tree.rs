//! The Owner -> Project -> Sessions tree pane (#10476).
//!
//! Why: the owner asked for one tree in place of the Projects and Sessions
//! panes (#10476 comment 2; Architect Q9): an owner bar per repository owner,
//! projects with session counters under it, and sessions under each project,
//! workers indented under the session that launched them.
//! What: [`row_line`] builds one row's text from a [`TreeRow`] (pure, tested
//! without a terminal); [`render`] draws the rows as a stateful `List` whose
//! highlight follows [`crate::tui::project_ctl::state::TreeView::nav`].
//! Test: `owner_and_project_lines_show_counters`,
//! `tree_rows_render_counters_at_80x24`.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem},
};

use super::sessions::session_spans;
use crate::tui::project_ctl::state::{
    DeliverableLinkState, Pane, ProjectCtlState, TreeRow, TreeRowKind,
};

/// Marker for an open owner or project.
pub const OPEN_GLYPH: &str = "▾";
/// Marker for a closed owner or project.
pub const CLOSED_GLYPH: &str = "▸";

/// The open/closed marker for an expandable row.
fn marker(expanded: bool) -> &'static str {
    if expanded { OPEN_GLYPH } else { CLOSED_GLYPH }
}

/// `"<total> sessions, <live> live"`, the counter both levels show.
pub fn counter_text(total: usize, live: usize) -> String {
    let noun = if total == 1 { "session" } else { "sessions" };
    format!("{total} {noun}, {live} live")
}

/// Style of a counter: green while anything is live.
fn counter_style(live: usize) -> Style {
    if live > 0 {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Build the line for one tree row.
///
/// Why: kept pure so the counters and indentation are testable without a
/// terminal.
/// What: an owner bar reads `▾ acme  2 projects · 4 sessions, 2 live`; a
/// project, indented two columns, reads `▸ alpha  3 sessions, 2 live`; a
/// session, indented four columns plus two per worker level with a `↳`
/// before a worker, reuses [`session_spans`]; an anchor is a dimmed
/// `↳ parent <8-char id> (not listed)` line.
/// Test: `owner_and_project_lines_show_counters`,
/// `worker_rows_indent_and_anchor_is_dim`.
pub fn row_line(state: &ProjectCtlState, row: &TreeRow) -> Line<'static> {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    match &row.kind {
        TreeRowKind::Owner {
            name,
            projects,
            total,
            live,
            expanded,
        } => {
            let noun = if *projects == 1 {
                "project"
            } else {
                "projects"
            };
            Line::from(vec![
                Span::styled(format!("{} {name}  ", marker(*expanded)), bold),
                Span::raw(format!("{projects} {noun} · ")),
                Span::styled(counter_text(*total, *live), counter_style(*live)),
            ])
        }
        TreeRowKind::Project {
            name,
            total,
            live,
            expanded,
        } => Line::from(vec![
            Span::raw(format!("  {} ", marker(*expanded))),
            Span::styled(format!("{name}  "), bold),
            Span::styled(counter_text(*total, *live), counter_style(*live)),
        ]),
        TreeRowKind::Session {
            project,
            index,
            depth,
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
            let mut spans = vec![Span::raw(format!("    {}{lead}", "  ".repeat(*depth)))];
            spans.extend(session_spans(session, link));
            Line::from(spans)
        }
        TreeRowKind::Anchor { parent, depth } => {
            let short: String = parent.chars().take(8).collect();
            Line::from(Span::styled(
                format!("    {}↳ parent {short} (not listed)", "  ".repeat(*depth)),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ))
        }
    }
}

/// Draw the tree pane into `area`.
///
/// What: a bordered list titled `OWNERS / PROJECTS / SESSIONS (<n> owners)`,
/// cyan while focused, with the selected row highlighted.
pub fn render(frame: &mut Frame, area: Rect, state: &mut ProjectCtlState) {
    let focused = state.focus == Pane::Tree;
    let owners = state
        .tree
        .rows
        .iter()
        .filter(|r| matches!(r.kind, TreeRowKind::Owner { .. }))
        .count();
    let items: Vec<ListItem> = state
        .tree
        .rows
        .iter()
        .map(|row| ListItem::new(row_line(state, row)))
        .collect();
    let title_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let title =
        Line::from(format!("OWNERS / PROJECTS / SESSIONS ({owners} owners)")).style(title_style);
    let block = Block::default().borders(Borders::ALL).title(title);
    // #10476: record the rows' rectangle for the mouse hit test.
    state.tree.hit.tree_rows = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(list, area, state.tree.nav.state_mut());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::project_ctl::state::{NodeId, ProjectRow, SessionRow};
    use ratatui::{Terminal, backend::TestBackend};

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

    /// acme/alpha with a manager, its worker, and an orphan; acme/beta empty.
    fn state() -> ProjectCtlState {
        let mut state = ProjectCtlState {
            projects: vec![
                ProjectRow {
                    name: "alpha".to_string(),
                    repo_url: "https://github.com/acme/alpha".to_string(),
                    ..Default::default()
                },
                ProjectRow {
                    name: "beta".to_string(),
                    repo_url: "https://github.com/acme/beta".to_string(),
                    ..Default::default()
                },
            ],
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
        state.select_node(NodeId::Session {
            owner: "acme".to_string(),
            project: "alpha".to_string(),
            id: "w1".to_string(),
        });
        state
    }

    fn texts(state: &ProjectCtlState) -> Vec<String> {
        state
            .tree
            .rows
            .iter()
            .map(|r| line_text(&row_line(state, r)))
            .collect()
    }

    #[test]
    fn owner_and_project_lines_show_counters() {
        let state = state();
        let lines = texts(&state);
        assert_eq!(lines[0], "▾ acme  2 projects · 3 sessions, 2 live");
        assert_eq!(lines[1], "  ▾ alpha  3 sessions, 2 live");
        assert_eq!(lines.last().unwrap(), "  ▸ beta  0 sessions, 0 live");
    }

    #[test]
    fn worker_rows_indent_and_anchor_is_dim() {
        let state = state();
        let lines = texts(&state);
        assert!(lines[2].starts_with("    ● boss"), "{:?}", lines[2]);
        assert!(lines[3].starts_with("      ↳ ○ helper"), "{:?}", lines[3]);
        assert_eq!(lines[4], "    ↳ parent deadbeef (not listed)");
        assert!(lines[5].starts_with("      ↳ ● orphan"), "{:?}", lines[5]);
        let anchor = row_line(&state, &state.tree.rows[4]);
        assert!(anchor.spans[0].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn tree_rows_render_counters_at_80x24() {
        let mut state = state();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test backend");
        terminal
            .draw(|f| crate::tui::project_ctl::layout::render(f, &mut state))
            .expect("draw the projects view");
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            text.contains("acme  2 projects · 3 sessions, 2 live"),
            "{text}"
        );
        assert!(text.contains("alpha  3 sessions, 2 live"));
        assert!(text.contains("parent deadbeef (not listed)"));
        assert!(text.contains("OWNERS / PROJECTS / SESSIONS (1 owners)"));
    }
}
