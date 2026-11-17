//! The Owner -> Project tree: the left column of the MPM pane (#10476).
//!
//! Why: the owner asked for one tree in place of the Projects pane (#10476
//! comment 2; Architect Q9): an owner bar per repository owner and projects
//! with session counters under it. #10754: two-column MPM pane (Bob 11-06) —
//! the sessions moved to the right column (`super::session_list`).
//! What: [`row_line`] builds one row's text from a [`TreeRow`] (pure, tested
//! without a terminal); [`render`] draws the rows as a stateful `List` whose
//! highlight follows [`crate::tui::project_ctl::state::TreeView::nav`].
//! Test: `owner_and_project_lines_show_counters`,
//! `tree_rows_render_counters_at_80x24`.

use crate::tui::theme::palette;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem},
};

use crate::tui::project_ctl::state::{Pane, ProjectCtlState, TreeRow, TreeRowKind};

/// Marker for an open owner.
pub const OPEN_GLYPH: &str = "▾";
/// Marker for a closed owner.
pub const CLOSED_GLYPH: &str = "▸";
/// Marker for a project, a leaf of the tree (#10754).
pub const PROJECT_GLYPH: &str = "·";

/// The open/closed marker for an owner row.
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
        Style::default().fg(palette().ok)
    } else {
        Style::default().fg(palette().muted)
    }
}

/// `"<total> · <live> live"`, the counter a narrow column falls back to.
pub fn short_counter_text(total: usize, live: usize) -> String {
    format!("{total} · {live} live")
}

/// Build the full line for one tree row.
///
/// Why: kept pure so the counters are testable without a terminal.
/// What: an owner bar reads `▾ acme  2 projects · 4 sessions, 2 live`; a
/// project, indented two columns, reads `· alpha  3 sessions, 2 live`.
/// Test: `owner_and_project_lines_show_counters`.
pub fn row_line(row: &TreeRow) -> Line<'static> {
    row_line_fit(row, usize::MAX)
}

/// Build the line for one tree row, fitted to `width` columns.
///
/// Why: #10754: two-column MPM pane (Bob 11-06) — the left column is 30% of
/// the terminal, too narrow for the full counters, and they must stay
/// visible.
/// What: the [`row_line`] text when it fits. Otherwise the owner drops its
/// project count and both kinds show [`short_counter_text`]; a name still too
/// long is cut with `…` so the counter is never the part cut off.
/// Test: `narrow_left_column_keeps_the_counters`.
pub fn row_line_fit(row: &TreeRow, width: usize) -> Line<'static> {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let (lead, lead_style, name, extra, total, live) = match &row.kind {
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
            let lead = format!("{} ", marker(*expanded));
            (
                lead,
                bold,
                name,
                format!("{projects} {noun} · "),
                *total,
                *live,
            )
        }
        TreeRowKind::Project { name, total, live } => {
            let lead = format!("  {PROJECT_GLYPH} ");
            (lead, Style::default(), name, String::new(), *total, *live)
        }
    };
    let len = |s: &str| s.chars().count();
    let full = counter_text(total, live);
    let fits = len(&lead) + len(name) + 2 + len(&extra) + len(&full) <= width;
    let (extra, counters) = if fits {
        (extra, full)
    } else {
        (String::new(), short_counter_text(total, live))
    };
    let room = width.saturating_sub(len(&lead) + 2 + len(&counters)).max(1);
    let name = if len(name) > room {
        let cut: String = name.chars().take(room.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        name.clone()
    };
    let mut spans = vec![
        Span::styled(lead, lead_style),
        Span::styled(format!("{name}  "), bold),
    ];
    if !extra.is_empty() {
        spans.push(Span::raw(extra));
    }
    spans.push(Span::styled(counters, counter_style(live)));
    Line::from(spans)
}

/// The style of a column title: accent and bold while the column is focused.
pub fn title_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(palette().accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

/// Draw the tree (left) column into `area`.
///
/// What: a bordered list titled `OWNERS / PROJECTS (<n>)`, accent while
/// focused, with the selected row highlighted. #10754: the
/// [`crate::tui::VERSION_LABEL`] sits right-aligned on the top border only
/// when `version` is set — the rightmost drawn column carries it.
pub fn render(frame: &mut Frame, area: Rect, state: &mut ProjectCtlState, version: bool) {
    let focused = state.focus == Pane::Tree;
    let owners = state
        .tree
        .rows
        .iter()
        .filter(|r| matches!(r.kind, TreeRowKind::Owner { .. }))
        .count();
    let width = usize::from(area.width.saturating_sub(2));
    let items: Vec<ListItem> = state
        .tree
        .rows
        .iter()
        .map(|row| ListItem::new(row_line_fit(row, width)))
        .collect();
    let title = Line::from(format!("OWNERS / PROJECTS ({owners})")).style(title_style(focused));
    let mut block = Block::default().borders(Borders::ALL).title(title);
    // #10476: version in tui header (Bob 11-05)
    if version {
        block = block.title(Line::from(crate::tui::VERSION_LABEL).right_aligned());
    }
    // #10476: record the rows' rectangle for the mouse hit test.
    state.tree.hit.tree_rows = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_style(palette().selection().add_modifier(Modifier::BOLD))
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
        state.select_node(NodeId::Project {
            owner: "acme".to_string(),
            name: "alpha".to_string(),
        });
        state
    }

    fn texts(state: &ProjectCtlState) -> Vec<String> {
        state
            .tree
            .rows
            .iter()
            .map(|r| line_text(&row_line(r)))
            .collect()
    }

    #[test]
    fn narrow_left_column_keeps_the_counters() {
        let state = state();
        let fit = |i: usize, width: usize| line_text(&row_line_fit(&state.tree.rows[i], width));
        assert_eq!(fit(0, 60), "▾ acme  2 projects · 3 sessions, 2 live");
        assert_eq!(fit(0, 22), "▾ acme  3 · 2 live");
        assert_eq!(fit(1, 22), "  · alpha  3 · 2 live");
        assert_eq!(
            fit(1, 18),
            "  · a…  3 · 2 live",
            "the name is cut, not the counter"
        );
        assert_eq!(fit(1, 18).chars().count(), 18);
    }

    #[test]
    fn owner_and_project_lines_show_counters() {
        let state = state();
        let lines = texts(&state);
        assert_eq!(lines[0], "▾ acme  2 projects · 3 sessions, 2 live");
        assert_eq!(lines[1], "  · alpha  3 sessions, 2 live");
        // #10754: the left column holds owner and projects only.
        assert_eq!(lines[2], "  · beta  0 sessions, 0 live");
        assert_eq!(lines.len(), 3, "{lines:?}");
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
        // #10754: the 24-column left column shows the short counters.
        assert!(text.contains("▾ acme  3 · 2 live"), "{text}");
        assert!(text.contains("· alpha  3 · 2 live"), "{text}");
        assert!(text.contains("parent deadbeef (not listed)"));
        assert!(text.contains("OWNERS / PROJECTS (1)"));
    }

    // #10476: version in tui header (Bob 11-05)
    #[test]
    fn projects_view_top_line_shows_tm_version_at_80_columns() {
        let version = format!("tm {}", env!("CARGO_PKG_VERSION"));
        let mut state = state();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test backend");
        terminal
            .draw(|f| crate::tui::project_ctl::layout::render(f, &mut state))
            .expect("draw the projects view");
        let buffer = terminal.backend().buffer();
        let top: String = (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect();
        assert!(top.contains(&version), "top line lacks {version}: {top:?}");
        assert!(
            top.contains("OWNERS / PROJECTS (1)"),
            "title displaced: {top:?}"
        );
    }
}
