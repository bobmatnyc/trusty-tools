//! Ratatui rendering for the memory TUI.
//!
//! Why: separating the ratatui widget calls from the content builders (in
//! `view.rs`) keeps the render path thin and lets the content be tested
//! without a terminal backend.
//! What: [`render`] is the single entry point the event loop calls each tick;
//! [`render_activity_and_stats`] and [`render_detail_pane`] are private
//! helpers that render the right-hand pane layouts.
//! Test: line content is unit-tested via the `*_lines` helpers in `view.rs`;
//! this glue is exercised by `test_render_smoke` and related smoke tests in
//! the `tests` module.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::monitor::memory_tui::MemoryFocus;
use crate::monitor::memory_tui::activity::spinner_tick;
use crate::monitor::memory_tui::state::MemoryTuiState;
use crate::monitor::memory_tui::view::{
    ALL_LABEL, KEY_HINT, drawer_detail_body, drawer_panel_lines, help_text, palace_lines_at,
    sort_label, stats_lines, title_line,
};
use crate::monitor::tui_common::{
    self, ACTIVITY_PERCENT, enter_tui, leave_tui, left_panel_width, panel_block,
};
use crate::monitor::utils::DaemonStatus;

/// Enter TUI mode and run the terminal event loop against `base_url`.
///
/// Why: separated from the daemon-URL resolution so a future CLI flag can
/// override the resolved address, and so terminal setup/teardown lives in one
/// place.
/// What: builds the client and state, enters raw mode + the alternate screen,
/// runs the event loop, and unconditionally restores the terminal even on error.
/// Test: terminal glue is exercised by launching the UI.
pub async fn run_with_url(base_url: String) -> anyhow::Result<()> {
    use crate::monitor::memory_client::MemoryClient;
    use crate::monitor::memory_tui::event_loop::run_loop;

    let mut client = MemoryClient::new(base_url.clone());
    let mut state = MemoryTuiState::new(base_url);

    let mut terminal = enter_tui()?;
    let result = run_loop(&mut terminal, &mut state, &mut client).await;
    leave_tui(&mut terminal)?;
    result
}

/// Draw the memory TUI frame.
///
/// Why: the single entry point the event loop calls each tick.
/// What: a 4-row vertical layout — title bar, the PALACES / right-pane split,
/// the RECALL input bar, and the key-hint footer. The right pane is itself
/// split vertically into an ACTIVITY feed (top 60 %) and a STATISTICS panel
/// (bottom 40 %), both scoped to the selected palace — or aggregated when "All"
/// is selected. A centred help overlay floats on top when `show_help` is set.
/// Test: line content is unit-tested via the `*_lines` helpers; this glue is
/// exercised by `test_render_smoke`.
pub fn render(frame: &mut Frame, state: &mut MemoryTuiState) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Min(4),    // panels
            Constraint::Length(3), // recall input
            Constraint::Length(1), // key hint
        ])
        .split(area);

    // Title bar.
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title_line(state),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );

    // PALACES on the left, the ACTIVITY / STATISTICS stack on the right.
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(left_panel_width(area.width)),
            Constraint::Min(10),
        ])
        .split(rows[1]);

    let list_focused = state.focus == MemoryFocus::List;
    // Drive the spinner animation from the wall clock so each frame advances
    // without an explicit app tick.
    let now = chrono::Utc::now();
    let tick = spinner_tick();
    let rendered_rows = palace_lines_at(state, now, tick);
    let palace_items: Vec<ListItem> = rendered_rows
        .iter()
        .map(|row| {
            // Row styling — the List widget renders the *selection* highlight
            // via `highlight_style` so the row content carries only its base
            // colour. Activity-state rows colour the whole row to keep the
            // spinner glyph and its label visually linked.
            let style = if row.is_header || row.is_all {
                // Group headers and the "All" row share the bold-yellow style.
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if let Some(color) = row.activity.and_then(|a| a.color()) {
                Style::default().fg(color)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(row.text.clone(), style)))
        })
        .collect();

    // When the inline filter is active or carries text, split the left column
    // vertically so the filter input renders above the palace list.
    let show_filter_bar = state.filter_active || !state.filter.is_empty();
    let (filter_area, list_area) = if show_filter_bar {
        let inner = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(3)])
            .split(split[0]);
        (Some(inner[0]), inner[1])
    } else {
        (None, split[0])
    };

    if let Some(area) = filter_area {
        let border_color = if state.filter_active {
            Color::Yellow
        } else {
            Color::DarkGray
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("🔍 ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    state.filter.as_str().to_string(),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    if state.filter_active { "_" } else { "" },
                    Style::default().fg(Color::Cyan),
                ),
            ]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(
                        Style::default()
                            .fg(border_color)
                            .add_modifier(Modifier::BOLD),
                    )
                    .title(Span::styled(
                        " FILTER ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
            ),
            area,
        );
    }

    // Scroll the PALACES list so the selected row stays visible: the panel
    // height minus its two border rows is the visible window. Both the scroll
    // anchor and the ratatui ListState selection index must reference the
    // *displayed* row (filter + sort + grouping reorder the rendered rows
    // relative to `state.palaces`), so we look up the visible row index of
    // the currently selected palace once and use it for both.
    let palace_visible = list_area.height.saturating_sub(2) as usize;
    // Resolve the highlight row from the rows we are about to render so the
    // selection index matches one-for-one. Group headers (non-selectable) are
    // skipped — if the cursor maps to a header we fall back to row 0 ("All").
    let visible_row = rendered_rows
        .iter()
        .position(|row| row.selected && !row.is_header)
        .unwrap_or(0);
    state.sync_scroll_to(visible_row, palace_visible);
    let palace_title = format!("PALACES [{}]", sort_label(state.sort_key));
    // The List widget handles the selection highlight via highlight_style +
    // HighlightSpacing::Always so there is no unstyled gutter between the row
    // text and the right border. The leading `> ` symbol replaces the old
    // inline marker that used to be baked into the row text.
    let highlight_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let mut palace_state = ListState::default()
        .with_offset(state.scroll_offset)
        .with_selected(Some(visible_row));
    frame.render_stateful_widget(
        List::new(palace_items)
            .block(panel_block(&palace_title, list_focused))
            .highlight_style(highlight_style)
            .highlight_symbol("> ")
            .highlight_spacing(HighlightSpacing::Always),
        list_area,
        &mut palace_state,
    );

    // Right pane: when the drawer-detail view is open, split the right area
    // horizontally so DRAWERS + STATISTICS stack on the left (~40 %) and the
    // detail content fills the right column (~60 %). Otherwise the right area
    // is the existing ACTIVITY (top) over STATISTICS (bottom) stack.
    if state.drawer_detail_open {
        let right_split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(split[1]);
        render_activity_and_stats(frame, state, right_split[0]);
        render_detail_pane(frame, state, right_split[1]);
    } else {
        render_activity_and_stats(frame, state, split[1]);
    }

    // RECALL input bar.
    let input_focused = state.focus == MemoryFocus::Input;
    let cursor = if input_focused { "_" } else { "" };
    let input_style = if input_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("RECALL ▶ ", Style::default().fg(Color::Yellow)),
            Span::styled(format!("{}{cursor}", state.input), input_style),
        ]))
        .block(panel_block("RECALL", input_focused)),
        rows[2],
    );

    // Key-hint footer.
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            KEY_HINT,
            Style::default().fg(Color::DarkGray),
        ))),
        rows[3],
    );

    if state.show_help {
        tui_common::render_help_overlay(frame, &help_text());
    }
}

/// Render the ACTIVITY / DRAWERS panel stacked over STATISTICS into `area`.
///
/// Why: the right-hand stack is rendered in two layouts — the normal mode
/// fills the whole right column, and the drawer-detail split-pane mode
/// (issue #215) shrinks it to the left ~40 % of the right column to make
/// room for the detail pane. Extracting the rendering avoids duplicating
/// the activity/stats logic across both branches.
/// What: vertically splits `area` by `tui_common::ACTIVITY_PERCENT` and
/// renders the drawer-page list (or fallback event log) into the top
/// region and the [`stats_lines`] readout into the bottom region. The
/// drawer-pane focus highlight is preserved exactly as before — only the
/// surrounding container changed.
/// Test: covered indirectly by `test_render_with_drawer_pane_focus_marker`
/// (normal layout) and `test_render_with_drawer_detail_open` (split layout).
fn render_activity_and_stats(frame: &mut Frame, state: &MemoryTuiState, area: Rect) {
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(ACTIVITY_PERCENT),
            Constraint::Percentage(100 - ACTIVITY_PERCENT),
        ])
        .split(area);

    // ACTIVITY panel — when a single palace is selected (issue #184) the
    // panel renders a paged drawer list; for "All palaces" it falls back to
    // the streamed event log so cross-palace events stay visible.
    let scope = state.scope_filter();
    let drawer_pane_focused = state.focus == MemoryFocus::DrawerPane;
    // Issue #215: surface the focus state in the panel title so the
    // operator can tell which zone owns the cursor at a glance. The `▶`
    // glyph mirrors the recall bar's leading marker for consistency.
    let activity_title = match scope {
        Some(id) if drawer_pane_focused => format!("DRAWER ▶ {id}"),
        Some(id) => format!("ACTIVITY — {id}"),
        None if drawer_pane_focused => format!("DRAWER ▶ {ALL_LABEL}"),
        None => format!("ACTIVITY — {ALL_LABEL}"),
    };
    let activity_height = right[0].height.saturating_sub(2) as usize;
    let drawer_total = state
        .selected
        .checked_sub(1)
        .and_then(|i| state.palaces.get(i))
        .map(|p| p.drawer_count)
        .unwrap_or(0);
    let drawer_lines = drawer_panel_lines(state, drawer_total);
    // Issue #215: render the drawer page as a stateful List so the
    // ratatui highlight can mark the focused drawer row. The header line
    // (row 0 of `drawer_lines` when the slice is non-empty) is included so
    // the page indicator stays visible; only data rows are selectable,
    // tracked via `drawer_cursor + 1` to skip the header offset.
    if !drawer_lines.is_empty() {
        let take = activity_height.max(1);
        let visible_lines: Vec<String> = drawer_lines.into_iter().take(take).collect();
        let items: Vec<ListItem> = visible_lines
            .iter()
            .map(|s| ListItem::new(s.clone()))
            .collect();
        // The first line is the header ("drawers N–M of T (page p)"); data
        // rows follow at index 1.. so the selection index is the cursor
        // plus 1 when the drawer pane has focus.
        let selected_row = if drawer_pane_focused && !state.drawer_list.drawers.is_empty() {
            Some((state.drawer_cursor + 1).min(visible_lines.len().saturating_sub(1)))
        } else {
            None
        };
        let highlight_style = Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let mut list_state = ListState::default().with_selected(selected_row);
        frame.render_stateful_widget(
            List::new(items)
                .block(panel_block(&activity_title, drawer_pane_focused))
                .highlight_style(highlight_style)
                .highlight_symbol("> ")
                .highlight_spacing(HighlightSpacing::Always),
            right[0],
            &mut list_state,
        );
    } else {
        // Fallback: streamed event log / placeholder lines.
        let fallback_items: Vec<ListItem> = if state.log.has_scoped(scope) {
            state
                .log
                .tail_scoped(scope, activity_height.max(1))
                .map(|line| ListItem::new(line.as_str()))
                .collect()
        } else if state.daemon_status == DaemonStatus::Connecting {
            // Distinguish "first poll has not completed" from "genuinely no
            // activity" so the panel doesn't look broken on startup.
            vec![ListItem::new("Loading…")]
        } else {
            vec![ListItem::new("(no activity yet)")]
        };
        frame.render_widget(
            List::new(fallback_items).block(panel_block(&activity_title, drawer_pane_focused)),
            right[0],
        );
    }

    // STATISTICS panel — counts and sizes for the selection.
    let stats_items: Vec<ListItem> = stats_lines(state).into_iter().map(ListItem::new).collect();
    frame.render_widget(
        List::new(stats_items).block(panel_block("STATISTICS", false)),
        right[1],
    );
}

/// Render the drawer-detail content as a stable split pane (issue #215).
///
/// Why: the previous floating-modal renderer used [`ratatui::widgets::Clear`]
/// over a computed centred rect, which produced a blank pane on many
/// terminal sizes (small viewports, narrow widths, or split window
/// arrangements). Anchoring the detail view inside a Layout-allocated
/// rectangle removes the geometry computation and keeps the pane visible
/// at every terminal size.
/// What: draws a bordered `Paragraph` into `area` containing the body text
/// returned by [`drawer_detail_body`], wrapped and scrolled by
/// `state.drawer_detail_scroll`. The title is `"DETAIL — <id-prefix>"`
/// where `<id-prefix>` is the first 8 characters of the drawer id selected
/// when the detail view was opened. Falls back to "DETAIL" when the drawer
/// list is empty or the index is out of range.
/// Test: smoke-tested via `test_render_with_drawer_detail_open`.
fn render_detail_pane(frame: &mut Frame, state: &MemoryTuiState, area: Rect) {
    // Resolve the drawer id to surface in the title. `drawer_detail_idx`
    // was recorded as the drawer-cursor position at the moment `Enter`
    // opened the pane, so it indexes `drawer_list.drawers` directly.
    let id_prefix = state
        .drawer_list
        .drawers
        .get(state.drawer_detail_idx)
        .map(|d| {
            let n = d.id.len().min(8);
            d.id[..n].to_string()
        })
        .unwrap_or_default();
    let title = if id_prefix.is_empty() {
        " DETAIL ".to_string()
    } else {
        format!(" DETAIL — {id_prefix} ")
    };

    // Match the filter-bar / focused-input convention: yellow + bold for an
    // active zone. DrawerPane focus is implied while this pane is visible.
    let border_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));

    let body = drawer_detail_body(state);
    let para = Paragraph::new(body)
        .style(Style::default().fg(Color::White))
        .wrap(Wrap { trim: false })
        .scroll((state.drawer_detail_scroll as u16, 0))
        .block(block);
    frame.render_widget(para, area);
}
