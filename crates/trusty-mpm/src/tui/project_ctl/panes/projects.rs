//! Projects pane (left column, 25% width) — DOC-35 §5 pane 1.
//!
//! Why: one row per registered project with an aggregate-state glyph and its
//! live session count, so an operator can scan fleet health across every
//! project without drilling into any one of them.
//! What: [`aggregate_glyph`] / [`project_line`] are pure builders (unit
//! tested without a terminal); [`render`] composes them into a ratatui
//! stateful `List`.
//! Test: `tests` covers the glyph rule and the line content.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem},
};

use crate::tui::project_ctl::state::{Pane, ProjectCtlState, ProjectRow};

/// Glyph shown when a project has at least one live (active/provisioning) session.
pub const LIVE_GLYPH: char = '●';
/// Glyph shown when a project has no live sessions.
pub const IDLE_GLYPH: char = '○';

/// Pick the aggregate-state glyph for one project row.
///
/// Why: DOC-35 §5 calls for "one row per registered project, aggregate-state
/// glyph, live session count" — this is the glyph half of that contract.
/// What: [`LIVE_GLYPH`] when `row.live_count > 0`, else [`IDLE_GLYPH`].
/// Test: `aggregate_glyph_reflects_live_count`.
pub fn aggregate_glyph(row: &ProjectRow) -> char {
    if row.live_count > 0 {
        LIVE_GLYPH
    } else {
        IDLE_GLYPH
    }
}

/// Build one project row's display line: `<glyph><live_count>  <name>`.
///
/// Why: a pure builder keeps the row TEXT testable without a terminal; the
/// `▸` focus marker is applied by the `List` widget's `highlight_symbol` in
/// [`render`], not baked into this line.
/// What: returns e.g. `●3  trusty-tools`.
/// Test: `project_line_shows_glyph_count_and_name`.
pub fn project_line(row: &ProjectRow) -> Line<'static> {
    let glyph = aggregate_glyph(row);
    let glyph_style = if row.live_count > 0 {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Line::from(vec![
        Span::styled(format!("{glyph}{}", row.live_count), glyph_style),
        Span::raw("  "),
        Span::styled(
            row.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ])
}

/// Draw the Projects pane into `area`.
///
/// Why: the single entry point [`super::super::layout::render`] calls for the
/// left column.
/// What: a bordered, titled (`PROJECTS (N)`) stateful `List`; the title and
/// the `▸` highlight symbol are styled cyan/bold when this pane has focus, so
/// the operator can see at a glance which pane Tab will act on.
pub fn render(frame: &mut Frame, area: Rect, state: &mut ProjectCtlState) {
    let focused = state.focus == Pane::Projects;
    let items: Vec<ListItem> = state
        .projects
        .iter()
        .map(|p| ListItem::new(project_line(p)))
        .collect();
    let title =
        Line::from(format!("PROJECTS ({})", state.projects.len())).style(title_style(focused));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ")
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(list, area, state.projects_nav.state_mut());
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

    fn row(live: usize) -> ProjectRow {
        ProjectRow {
            name: "widget".to_string(),
            repo_url: "https://github.com/acme/widget".to_string(),
            live_count: live,
            total_count: live + 1,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn aggregate_glyph_reflects_live_count() {
        assert_eq!(aggregate_glyph(&row(0)), IDLE_GLYPH);
        assert_eq!(aggregate_glyph(&row(1)), LIVE_GLYPH);
    }

    #[test]
    fn project_line_shows_glyph_count_and_name() {
        let text = line_text(&project_line(&row(3)));
        assert!(
            text.starts_with("●3"),
            "expected leading glyph+count: {text}"
        );
        assert!(text.contains("widget"), "missing name: {text}");
    }

    #[test]
    fn project_line_idle_uses_hollow_glyph() {
        let text = line_text(&project_line(&row(0)));
        assert!(text.starts_with("○0"), "expected hollow glyph: {text}");
    }
}
