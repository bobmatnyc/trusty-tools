//! Ratatui rendering functions for the unified monitor dashboard.
//!
//! Why: separating the widget construction and frame drawing from the data
//! model keeps the render path independently compilable (behind the
//! `monitor-tui` feature) and narrows what needs a real terminal for smoke
//! tests.
//! What: `render` is the single entry-point the event loop calls each tick;
//! the remaining helpers build panel lines, blocks, badges, and the help
//! overlay.
//! Test: line content is unit-tested via `search_panel_lines` /
//! `memory_panel_lines`; the full render path is exercised by
//! `test_render_smoke` and `test_offline_panel_renders`.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};

use super::format::{format_count, format_uptime, help_text, status_badge, truncate};
use super::types::{
    DaemonPanel, DashboardState, Focus, KEY_HINT, MemoryData, PanelStatus, SearchData, VERSION,
};

use super::format::panel_layout;

/// Build the text lines for the trusty-search panel body.
///
/// Why: separating line construction from the ratatui widgets lets a test
/// assert the rendered content without a terminal backend.
/// What: returns the panel body as plain strings — aggregate stats then one
/// line per index; an offline panel shows its error, a connecting panel a
/// placeholder.
/// Test: `test_search_panel_renders`, `test_offline_panel_renders`.
pub fn search_panel_lines(panel: &DaemonPanel<SearchData>) -> Vec<String> {
    match &panel.status {
        PanelStatus::Connecting => vec![format!("connecting to {}…", panel.base_url)],
        PanelStatus::Offline { last_error } => vec![
            format!("daemon unreachable at {}", panel.base_url),
            format!("last error: {last_error}"),
            "retrying every 5s…".to_string(),
        ],
        PanelStatus::Online(data) => {
            let mut lines = vec![
                format!("Uptime:       {}", format_uptime(data.uptime_secs)),
                format!("Indexes:      {}", data.indexes.len()),
                format!("Total chunks: {}", format_count(data.total_chunks())),
                String::new(),
            ];
            if data.indexes.is_empty() {
                lines.push("(no indexes registered)".to_string());
            } else {
                for idx in &data.indexes {
                    lines.push(format!(
                        "{:<16} {:>10} chunks",
                        truncate(&idx.id, 16),
                        format_count(idx.chunk_count),
                    ));
                }
            }
            lines
        }
    }
}

/// Build the text lines for the trusty-memory panel body.
///
/// Why: mirrors [`search_panel_lines`] for testable, terminal-free rendering.
/// What: returns the panel body as plain strings — aggregate counts then one
/// line per palace; offline and connecting states render as for search.
/// Test: `test_memory_panel_renders`, `test_offline_panel_renders`.
pub fn memory_panel_lines(panel: &DaemonPanel<MemoryData>) -> Vec<String> {
    match &panel.status {
        PanelStatus::Connecting => vec![format!("connecting to {}…", panel.base_url)],
        PanelStatus::Offline { last_error } => vec![
            format!("daemon unreachable at {}", panel.base_url),
            format!("last error: {last_error}"),
            "retrying every 5s…".to_string(),
        ],
        PanelStatus::Online(data) => {
            let mut lines = vec![
                format!("Palaces:      {}", data.palace_count),
                format!("Drawers:      {}", format_count(data.total_drawers)),
                format!("Vectors:      {}", format_count(data.total_vectors)),
                format!("KG triples:   {}", format_count(data.total_kg_triples)),
                String::new(),
            ];
            if data.palaces.is_empty() {
                lines.push("(no palaces)".to_string());
            } else {
                for palace in &data.palaces {
                    let label = if palace.name.is_empty() {
                        truncate(&palace.id, 16)
                    } else {
                        truncate(&palace.name, 16)
                    };
                    lines.push(format!(
                        "{:<16} {:>10} vectors",
                        label,
                        format_count(palace.vector_count),
                    ));
                }
            }
            lines
        }
    }
}

/// Compute a centred sub-rectangle for the help overlay.
///
/// Why: the help overlay floats in the middle of the terminal regardless of
/// size.
/// What: returns a [`Rect`] of `width`×`height` (clamped to `area`) centred in
/// `area`.
/// Test: side-effect-free geometry, exercised by `render` smoke tests.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// Build the bordered block for a daemon panel, highlighting it when focused.
///
/// Why: the focused panel must be visually distinct; both panels share this
/// border-building logic.
/// What: returns a [`Block`] with a styled title carrying the panel name plus
/// its status badge; a focused block gets a thick cyan border.
fn panel_block(name: &str, badge: (char, &str, Color), focused: bool) -> Block<'static> {
    let (glyph, label, color) = badge;
    let title = Line::from(vec![
        Span::styled(
            format!(" {name} "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{glyph} {label} "), Style::default().fg(color)),
    ]);
    let border_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title)
}

/// Render the help overlay listing every key binding.
///
/// Why: the `?` key shows a floating reference of every binding.
/// What: clears a centred rectangle and draws the [`help_text`] in a block.
fn render_help_overlay(frame: &mut Frame) {
    let area = centered_rect(56, 11, frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(help_text())
            .style(Style::default().fg(Color::White))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Help — press ? or Esc to close "),
            ),
        area,
    );
}

/// Build the search panel badge, folding the daemon version into the label.
///
/// Why: the panel title shows `SEARCH ● ONLINE vX.Y.Z`; the version comes from
/// the payload only when online.
/// What: returns the [`status_badge`] glyph/colour with a label that appends
/// the version when the panel is online.
fn search_version_badge(panel: &DaemonPanel<SearchData>) -> (char, &'static str, Color) {
    status_badge(&panel.status)
}

/// Build the memory panel badge (see [`search_version_badge`]).
///
/// Why: mirrors [`search_version_badge`] for the memory panel.
/// What: delegates to [`status_badge`] with the memory panel's status.
/// Test: exercised by `test_render_smoke`.
fn memory_version_badge(panel: &DaemonPanel<MemoryData>) -> (char, &'static str, Color) {
    status_badge(&panel.status)
}

/// Draw the unified monitor dashboard frame.
///
/// Why: the single entry point the event loop calls each tick.
/// What: a vertical layout — a two-line header (title + key hint / last
/// action) and a flexing body split into the two daemon panels. The body split
/// is horizontal on wide terminals and vertical on narrow ones, per
/// [`panel_layout`]. When `show_help` is set a centred overlay floats on top.
/// Test: line content is unit-tested via `search_panel_lines` /
/// `memory_panel_lines`; this glue is exercised by `test_render_smoke`.
pub fn render(frame: &mut Frame, state: &DashboardState) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(6)])
        .split(frame.area());

    // Header block.
    let header_lines = vec![
        Line::from(Span::styled(
            format!(" trusty-monitor v{VERSION} "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            state
                .last_action
                .clone()
                .unwrap_or_else(|| KEY_HINT.to_string()),
            Style::default().fg(Color::Gray),
        )),
    ];
    frame.render_widget(
        Paragraph::new(header_lines).block(Block::default().borders(Borders::ALL)),
        outer[0],
    );

    // Body: two daemon panels, side by side or stacked.
    let (direction, constraints) = panel_layout(frame.area().width);
    let panels = Layout::default()
        .direction(direction)
        .constraints(constraints)
        .split(outer[1]);

    let search_block = panel_block(
        "SEARCH",
        search_version_badge(&state.search),
        state.focus == Focus::Search,
    );
    frame.render_widget(
        Paragraph::new(
            search_panel_lines(&state.search)
                .into_iter()
                .map(Line::from)
                .collect::<Vec<_>>(),
        )
        .block(search_block),
        panels[0],
    );

    let memory_block = panel_block(
        "MEMORY",
        memory_version_badge(&state.memory),
        state.focus == Focus::Memory,
    );
    frame.render_widget(
        List::new(
            memory_panel_lines(&state.memory)
                .into_iter()
                .map(ListItem::new)
                .collect::<Vec<_>>(),
        )
        .block(memory_block),
        panels[1],
    );

    if state.show_help {
        render_help_overlay(frame);
    }
}
