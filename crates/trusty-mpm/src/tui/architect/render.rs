//! ratatui drawing for the Architect view.
//!
//! Why: drawing reads `&ArchitectState` only, so a slow store or daemon never
//! stalls a frame.
//! What: [`render`] draws the tab bar with counts and a READ-ONLY badge
//! (Dc4, #10966), the open tab's list or the Live tracker, an optional
//! detail pane, and the tally footer.
//! Test: `architect_render_lists_rows_with_badges_and_counts`,
//! `architect_render_says_why_when_the_store_is_not_set`,
//! `live_tab_draws_the_tracker_list_only`.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap},
};

use super::live::live_lines;
use super::{ArchitectState, Fetch, Tab};
use crate::tui::theme::{Tone, badge, palette};

/// Footer hint on the Architect view.
pub const ARCHITECT_HINT: &str =
    "[Tab/←→] tab  [↑↓] select  [Enter] detail  [r] refresh  [Esc] home  [q] quit";

/// Draw the view into `area` (everything under the screen header).
pub fn render(frame: &mut Frame, area: Rect, state: &ArchitectState) {
    let [tabs, body, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .areas(area);
    render_tabs(frame, tabs, state);
    match state.tab {
        Tab::Live => render_live(frame, body, state),
        tab => render_list(frame, body, state, tab),
    }
    let p = palette();
    let text = match &state.dashboard {
        Fetch::Ready(d) => d.tally.clone(),
        Fetch::Waiting => "reading the Architect store…".to_string(),
        Fetch::Failed(e) => e.clone(),
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(p.muted)),
        footer,
    );
}

fn render_tabs(frame: &mut Frame, area: Rect, state: &ArchitectState) {
    let p = palette();
    let mut spans = vec![Span::styled(
        "ARCHITECT  ",
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
    )];
    for (i, tab) in Tab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(p.muted)));
        }
        let label = match state.count(*tab) {
            Some(n) => format!("{} {n}", tab.label()),
            None => tab.label().to_string(),
        };
        let style = if *tab == state.tab {
            p.selection().add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.text)
        };
        spans.push(Span::styled(format!(" {label} "), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    // Dc4: the view never writes, and says so.
    let word = "read-only";
    let width = word.chars().count() as u16 + 2;
    frame.render_widget(
        Paragraph::new(Line::from(badge(word, Tone::Info))),
        Rect::new(area.right().saturating_sub(width), area.y, width, 1),
    );
}

fn panel(title: &str) -> Block<'static> {
    let p = palette();
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.muted))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ))
}

fn message(frame: &mut Frame, area: Rect, title: &str, text: String) {
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(palette().muted))
            .wrap(Wrap { trim: true })
            .block(panel(title)),
        area,
    );
}

fn render_list(frame: &mut Frame, area: Rect, state: &ArchitectState, tab: Tab) {
    let snap = match &state.dashboard {
        Fetch::Ready(d) => d,
        Fetch::Waiting => return message(frame, area, tab.label(), "loading…".into()),
        Fetch::Failed(e) => return message(frame, area, tab.label(), e.clone()),
    };
    let rows = snap.rows(tab);
    if rows.is_empty() {
        return message(frame, area, tab.label(), "nothing open".into());
    }
    let (list_area, detail_area) = if state.expanded {
        let [a, b] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .areas(area);
        (a, Some(b))
    } else {
        (area, None)
    };
    let p = palette();
    let table = Table::new(
        rows.iter().map(|r| {
            Row::new(vec![
                Cell::from(r.id.clone()),
                Cell::from(Line::from(badge(&r.status, r.tone))),
                Cell::from(r.title.clone()),
                Cell::from(Span::styled(r.note.clone(), Style::default().fg(p.muted))),
            ])
        }),
        [
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Fill(3),
            Constraint::Fill(1),
        ],
    )
    .header(
        Row::new(["Id", "Status", "Title", "Context"])
            .style(Style::default().fg(p.muted).add_modifier(Modifier::BOLD)),
    )
    .column_spacing(2)
    .row_highlight_style(p.selection())
    .block(panel(tab.label()));
    let mut ts = TableState::default().with_selected(Some(state.selected));
    frame.render_stateful_widget(table, list_area, &mut ts);
    if let (Some(area), Some(row)) = (detail_area, rows.get(state.selected)) {
        let lines: Vec<Line> = row
            .fields
            .iter()
            .map(|(k, v)| {
                Line::from(vec![
                    Span::styled(format!("{k:<14}"), Style::default().fg(p.muted)),
                    Span::styled(v.clone(), Style::default().fg(p.text)),
                ])
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .block(panel(&format!("{} {}", tab.label(), row.id))),
            area,
        );
    }
}

// #10966: the Live tab is the tracker list and nothing else (Dc4, Q4).
fn render_live(frame: &mut Frame, area: Rect, state: &ArchitectState) {
    let title = Tab::Live.label();
    let snap = match &state.live {
        Fetch::Ready(l) => l,
        Fetch::Waiting => return message(frame, area, title, "loading…".into()),
        Fetch::Failed(e) => return message(frame, area, title, e.clone()),
    };
    if snap.sessions.is_empty() {
        return message(frame, area, title, "no sessions".into());
    }
    frame.render_widget(
        Paragraph::new(live_lines(snap, state.selected)).block(panel(title)),
        area,
    );
}
