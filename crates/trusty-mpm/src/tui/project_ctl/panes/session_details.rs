//! Read-only session details overlay (#10476).
//!
//! Why: the owner asked that Enter on a session open its details (#10476
//! comment 2). The overlay shows what the poll already holds and makes no
//! daemon call of its own.
//! What: [`detail_lines`] is the pure field projection; [`render`] floats it
//! over the frame, as `deliverables_view` does.
//! Test: `detail_lines_show_fields_and_parent`.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use super::deliverables_view::centered_rect;
use crate::tui::project_ctl::state::{ProjectCtlState, SessionRow};

const OVERLAY_WIDTH: u16 = 76;
const OVERLAY_HEIGHT: u16 = 18;

/// The key line at the foot of the overlay.
pub const DETAILS_KEYS: &str =
    "[f] focus window  [k] kill  [r] resume  [d] decommission  [a] attach-cmd  [Esc] close";

/// The head of [`DETAILS_KEYS`], which a click treats as the Focus button.
const FOCUS_LABEL: &str = "[f] focus window";

/// Rows reserved for [`DETAILS_KEYS`], which wraps inside the overlay.
const KEYS_ROWS: u16 = 2;

/// One `label: value` line per field, `-` for an absent value.
///
/// What: name, id, project, state, branch, task, parent (with its name when
/// the project lists it), pending decision, deliverable, workspace and last
/// activity.
/// Test: `detail_lines_show_fields_and_parent`.
pub fn detail_lines(state: &ProjectCtlState, project: &str, s: &SessionRow) -> Vec<String> {
    let or_dash = |v: Option<&str>| {
        v.filter(|v| !v.trim().is_empty())
            .unwrap_or("-")
            .to_string()
    };
    let parent = s.parent_session_id.as_deref().map(|pid| {
        let name = state
            .sessions_by_project
            .get(project)
            .and_then(|rows| rows.iter().find(|r| r.id == pid))
            .map_or("not listed", |r| r.name.as_str());
        format!("{pid} ({name})")
    });
    vec![
        format!("name:        {}", s.name),
        format!("id:          {}", s.id),
        format!("project:     {project}"),
        format!("state:       {}", s.state),
        format!("branch:      {}", or_dash(s.branch.as_deref())),
        format!("task:        {}", or_dash(s.task.as_deref())),
        format!("parent:      {}", or_dash(parent.as_deref())),
        format!("decision:    {}", or_dash(s.pending_decision.as_deref())),
        format!("deliverable: {}", or_dash(s.deliverable_id.as_deref())),
        format!("workspace:   {}", or_dash(s.workspace_path.as_deref())),
        format!("last active: {}", or_dash(s.last_activity_at.as_deref())),
    ]
}

/// Draw the overlay for the session [`ProjectCtlState::details_session`] names.
///
/// What: the fields fill the overlay; the key line sits in its bottom
/// `KEYS_ROWS` rows. #10476: a long field wrapping can no longer move the key
/// line, so the click target recorded in [`ProjectCtlState::tree`] is exact.
/// Test: `overlay_records_the_focus_button_on_its_label`.
pub fn render(frame: &mut Frame, state: &mut ProjectCtlState) {
    let Some((project, session)) = state.details_session() else {
        return;
    };
    let area = centered_rect(OVERLAY_WIDTH, OVERLAY_HEIGHT, frame.area());
    frame.render_widget(Clear, area);
    let title = Line::from(Span::styled(
        format!("Session — {}", session.name),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let lines: Vec<Line> = detail_lines(state, project, session)
        .into_iter()
        .map(Line::from)
        .collect();
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [body, keys] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(KEYS_ROWS)]).areas(inner);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            DETAILS_KEYS,
            Style::default().fg(Color::DarkGray),
        )))
        .wrap(Wrap { trim: false }),
        keys,
    );
    let label = Rect {
        height: 1,
        width: FOCUS_LABEL.len() as u16,
        ..keys
    }
    .intersection(keys);
    state.tree.hit.details = Some(area);
    state.tree.hit.details_focus = (!label.is_empty()).then_some(label);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, name: &str, parent: Option<&str>) -> SessionRow {
        SessionRow {
            id: id.to_string(),
            name: name.to_string(),
            state: "active".to_string(),
            branch: Some("main".to_string()),
            task: Some(String::new()),
            parent_session_id: parent.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn overlay_records_the_focus_button_on_its_label() {
        let mut state = ProjectCtlState::default();
        let mut long = row("s1", "boss", None);
        // A field that wraps must not move the key line.
        long.workspace_path = Some("x".repeat(300));
        state
            .sessions_by_project
            .insert("alpha".to_string(), vec![long]);
        state.tree.details = Some("s1".to_string());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).expect("backend");
        terminal
            .draw(|f| render(f, &mut state))
            .expect("draw the overlay");

        let button = state.tree.hit.details_focus.expect("the button");
        let label: String = (button.left()..button.right())
            .map(|x| {
                terminal.backend().buffer()[(x, button.y)]
                    .symbol()
                    .to_string()
            })
            .collect();
        assert_eq!(label, FOCUS_LABEL);
        let overlay = state.tree.hit.details.expect("the overlay");
        assert!(overlay.y < button.y && button.y < overlay.bottom());
    }

    #[test]
    fn detail_lines_show_fields_and_parent() {
        let mut state = ProjectCtlState::default();
        let child = row("c1", "helper", Some("m1"));
        state.sessions_by_project.insert(
            "alpha".to_string(),
            vec![row("m1", "boss", None), child.clone()],
        );
        let lines = detail_lines(&state, "alpha", &child);
        assert!(lines.contains(&"name:        helper".to_string()));
        assert!(lines.contains(&"project:     alpha".to_string()));
        assert!(
            lines.contains(&"task:        -".to_string()),
            "blank task is -"
        );
        assert!(lines.contains(&"parent:      m1 (boss)".to_string()));

        let orphan = row("o1", "orphan", Some("gone"));
        let lines = detail_lines(&state, "alpha", &orphan);
        assert!(lines.contains(&"parent:      gone (not listed)".to_string()));
    }
}
