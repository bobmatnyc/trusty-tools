//! ratatui widget assembly for the health screen.
//!
//! Why: keeping all the terminal-bound rendering glue in one place lets the
//! pure data, formatters, and line builders stay terminal-free and testable;
//! the renderer only assembles the widgets from those pure helpers.
//! What: the single [`render`] entry point the event loop calls, plus the
//! per-zone (header / collections / tab panel / footer) and per-tab
//! (health / logs / search / index) sub-renderers.
//! Test: line content is unit-tested via the pure builders in `screen` /
//! `format`; this glue is exercised by `render_health_smoke`.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem, ListState, Paragraph},
};

use crate::tui::health::activity::{activity_color, palace_activity};
use crate::tui::health::screen::{
    collections_lines, header_lines, health_tab_lines, index_tab_lines, service_name, tab_bar,
};
use crate::tui::health::types::{Daemon, HealthScreen, HealthTab, PalaceActivity};

/// Render the health screen into `area`-spanning `frame`.
///
/// Why: the single entry point the event loop calls when screen `[2]` is
/// active; keeps all the ratatui widget assembly in one place.
/// What: a vertical layout — a one-line title, a body split horizontally into
/// the two daemon panels (the focused one gets a bold cyan border), and the
/// shared status bar is drawn by the caller. Panel bodies come from the pure
/// `*_panel_lines` helpers.
/// Test: line content is unit-tested via `search_panel_lines` /
/// `memory_panel_lines`; this glue is exercised by `render_health_smoke`.
pub fn render(frame: &mut Frame, screen: &HealthScreen) {
    // Three-zone layout (issue #36):
    //   1. Header (2 lines: title + stats)
    //   2. Main (left collections list + right tabbed panel)
    //   3. Footer (search/command input + key hint)
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header (title + stats)
            Constraint::Min(6),    // main body
            Constraint::Length(3), // footer (search input)
        ])
        .split(frame.area());

    render_header(frame, outer[0], screen);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(30), // collections list
            Constraint::Min(20),    // tabbed panel
        ])
        .split(outer[1]);

    render_collections(frame, body[0], screen);
    render_tab_panel(frame, body[1], screen);
    render_footer(frame, outer[2], screen);
}

/// Render the header zone (title + resource snapshot).
///
/// Why: kept separate so the main `render` reads top-to-bottom.
/// What: draws the two `header_lines` inside a single bordered block.
fn render_header(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let online = match screen.focus {
        Daemon::Search => screen.search.is_online(),
        Daemon::Memory => screen.memory.is_online(),
    };
    let lines = header_lines(screen);
    let title_color = if online { Color::Green } else { Color::Red };
    let body: Vec<Line> = lines.into_iter().map(Line::from).collect();
    frame.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(title_color))
                .title(Span::styled(
                    format!(" {} ", service_name(screen.focus)),
                    Style::default()
                        .fg(title_color)
                        .add_modifier(Modifier::BOLD),
                )),
        ),
        area,
    );
}

/// Render the collections list (left panel).
///
/// Why: kept separate so the renderer remains small and one branch handles
/// the search-vs-memory title label.
/// What: draws a stateful `List` inside a bordered block titled
/// `COLLECTIONS (n)` for search or `PALACES (n)` for memory. The selected row
/// uses bold white-on-blue via `highlight_style`, which (unlike a manually
/// padded `Paragraph` row) is applied by ratatui across every inner cell of
/// the row regardless of the underlying text length — that is what eliminates
/// the unstyled "right gutter" the manual-padding fix struggled with.
/// Test: `render_health_smoke` exercises the layout end-to-end; row formatting
/// is covered by `collections_lines_format_each_row` and friends.
fn render_collections(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let rows = screen.focused_collections();
    let label = match screen.focus {
        Daemon::Search => "COLLECTIONS",
        Daemon::Memory => "PALACES",
    };
    let title = format!(" {label} ({}) ", rows.len());
    let lines = collections_lines(screen);
    // Switched from `Paragraph` + manual right-pad to the canonical `List` +
    // `ListState` pattern. The previous approach right-padded each row to
    // `area.width - 2` so the selected row's `Span::styled` background covered
    // the full inner width, but the residual "right gutter" the user reported
    // was actually an artefact of how ratatui's `Paragraph` clips trailing
    // styled spans on some terminals — the styled cells reached the inner
    // border but the final cell of the highlight could be reset by the
    // terminal's own SGR handling. A `List` widget styles entire rows via
    // `highlight_style`, applies the style to every cell from the inner-left
    // border to the inner-right border (independent of the item's text
    // length), and renders `HighlightSpacing::Always` so unselected rows align
    // with the selected one. This eliminates the gutter at the rendering layer
    // instead of relying on the input text being exactly inner-width chars
    // wide.
    // Apply per-row activity colour for memory palaces; search rows stay
    // default-styled. Idle palaces keep the default colour too, so only the
    // "something is happening" rows light up against the rest of the list.
    let items: Vec<ListItem<'static>> = lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let item = ListItem::new(line);
            if matches!(screen.focus, Daemon::Memory)
                && let Some(row) = rows.get(i)
            {
                let activity = palace_activity(row);
                if !matches!(activity, PalaceActivity::Idle) {
                    return item.style(Style::default().fg(activity_color(activity)));
                }
            }
            item
        })
        .collect();
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
        // `HighlightSpacing::Always` keeps every row aligned regardless of
        // whether a `highlight_symbol` is set; without it, unselected rows
        // would shift one column left when the selection changes.
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(screen.selected_collection.min(rows.len() - 1)));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

/// Render the right-side tabbed panel (HEALTH / LOGS / SEARCH).
///
/// Why: keeps the per-tab body switch in one place.
/// What: draws the tab bar on the first body line; the active tab is bold
/// cyan, others dimmed. Below the bar, dispatches to a per-tab renderer.
fn render_tab_panel(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " DETAILS ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    // Tab bar line.
    let mut spans: Vec<Span> = Vec::new();
    for (label, active) in tab_bar(screen.tab) {
        let style = if active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw("  "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), split[0]);

    // Active tab body.
    match screen.tab {
        HealthTab::Health => render_health_tab(frame, split[1], screen),
        HealthTab::Logs => render_logs_tab(frame, split[1], screen),
        HealthTab::Search => render_search_tab(frame, split[1], screen),
        HealthTab::Index => render_index_tab(frame, split[1], screen),
    }
}

/// Render the HEALTH tab body.
fn render_health_tab(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let lines: Vec<Line> = health_tab_lines(screen)
        .into_iter()
        .map(Line::from)
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Render the LOGS tab body (scrollable ring buffer).
fn render_logs_tab(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let buf = screen.focused_logs();
    if buf.lines.is_empty() {
        let hint = "Log streaming not available — start daemon with RUST_LOG=debug";
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            ))),
            area,
        );
        return;
    }
    let height = area.height as usize;
    let total = buf.lines.len();
    // The view shows the bottom `height` lines minus the operator's scroll
    // offset. Auto-scroll means `scroll_offset == 0` (tail visible).
    let end = total.saturating_sub(buf.scroll_offset);
    let start = end.saturating_sub(height);
    let body: Vec<Line> = buf
        .lines
        .iter()
        .skip(start)
        .take(end - start)
        .map(|l| Line::from(l.clone()))
        .collect();
    frame.render_widget(Paragraph::new(body), area);
}

/// Render the SEARCH/RECALL tab body.
fn render_search_tab(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let lines = if screen.search_query.is_empty() {
        vec![
            Line::from(Span::styled(
                "Type a query in the search bar below.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(match screen.focus {
                Daemon::Search => "Searches the focused index for code chunks.",
                Daemon::Memory => "Recalls memories from the focused palace.",
            }),
        ]
    } else {
        vec![
            Line::from(Span::styled(
                format!("Query: {}", screen.search_query),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("(press Enter in the search bar to run — execution not yet wired)"),
        ]
    };
    frame.render_widget(Paragraph::new(lines), area);
}

/// Render the INDEX tab body.
///
/// Why: kept separate so [`render_tab_panel`]'s match arm stays one line.
/// What: draws the `index_tab_lines` as a `Paragraph`. Section headers (`-- … --`)
/// render in cyan to match the existing HEALTH-tab section style.
fn render_index_tab(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let lines: Vec<Line> = index_tab_lines(screen)
        .into_iter()
        .map(|l| {
            if l.starts_with("--") {
                Line::from(Span::styled(
                    l,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(l)
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Render the footer zone: search input bar + key hint.
fn render_footer(frame: &mut Frame, area: ratatui::layout::Rect, screen: &HealthScreen) {
    let cursor = if screen.search_input_focused { "_" } else { "" };
    let prompt = match screen.focus {
        Daemon::Search => "SEARCH ▶",
        Daemon::Memory => "RECALL ▶",
    };
    let input_line = format!("{prompt} {}{cursor}", screen.search_query);
    let input_style = if screen.search_input_focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    frame.render_widget(
        Paragraph::new(Line::from(input_line))
            .style(input_style)
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

/// Render one daemon panel into `area`.
///
/// Why: the two panels share their bordered-block + line-list layout; only the
/// title, body, and highlight differ.
/// What: draws a bordered [`Paragraph`] whose title carries the panel name,
/// coloured by liveness; a focused panel gets a bold cyan border.
/// Kept for callers that want the legacy side-by-side panel; the new
/// per-service render uses its own header / collections / tab helpers.
#[allow(dead_code)]
fn render_panel(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    name: &str,
    lines: &[String],
    online: bool,
    focused: bool,
) {
    let title_color = if online { Color::Green } else { Color::Red };
    let border_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let body: Vec<Line> = lines.iter().map(|l| Line::from(l.clone())).collect();
    frame.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(Span::styled(
                    format!(" {name} "),
                    Style::default()
                        .fg(title_color)
                        .add_modifier(Modifier::BOLD),
                )),
        ),
        area,
    );
}
