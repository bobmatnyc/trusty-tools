//! Vertical layout + rendering for the coordinator TUI skeleton.
//!
//! Why: the spec (DOC-13 §3) pins a Claude-Code-like shape — input box on
//! TOP, the live session list in the MIDDLE (controller bullet always row 0),
//! and a status/key bar at the BOTTOM. Keeping the frame composition here,
//! separate from state and key dispatch, isolates the only terminal-touching
//! render path and keeps each file under the SLOC cap.
//! What: [`render`] draws the three regions for one frame; the small helpers
//! ([`input_line`], [`status_bar_text`]) build the text the layout shows and are
//! reused by tests. The session list is built from the pure row-builders in
//! [`super::rows`], so the *content* is unit-tested without a terminal.
//! Test: text helpers are unit-tested in `super::tests`; the full `render` is
//! exercised by launching the TUI (terminal glue).

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use super::rows::{active_row_two_col, controller_bullet_row, session_row};
use super::state::CoordinatorState;

/// The prompt prefix shown in the input box.
///
/// Why: the spec's mockup shows a `coordinator ›` prompt; a constant keeps the
/// prompt single-sourced between the renderer and its test.
/// What: the literal `coordinator › `.
/// Test: `input_line_shows_prompt_and_cursor`.
pub const INPUT_PROMPT: &str = "coordinator › ";

/// The bottom status/key-hint bar text.
///
/// Why: the spec's status bar lists the slash commands + global keys; the
/// skeleton shows the (stubbed) command names plus the keys Child #1 implements.
/// What: a one-line hint string drawn reversed at the bottom.
/// Test: `status_bar_lists_keys`.
pub const STATUS_BAR_HINT: &str =
    "/new /sessions /attach /stop /resume /kill /help  ·  ↵ send  ·  ↑↓/jk select  ·  q quit";

/// The synthetic right-column status shown for the controller bullet.
///
/// Why: the controller has no daemon summary; its right column is a synthetic
/// readiness string (§3.3). A constant keeps the skeleton value in one place.
/// What: the literal `idle · 0 delegations · ready`.
/// Test: `controller_status_is_synthetic`.
pub const CONTROLLER_STATUS: &str = "idle · 0 delegations · ready";

/// Build the input-box line: prompt + buffer + a cursor glyph.
///
/// Why: kept pure so a test can assert the rendered prompt and trailing cursor
/// without a terminal frame.
/// What: returns `coordinator › <buffer>_` (the trailing `_` is the cursor).
/// Test: `input_line_shows_prompt_and_cursor`.
pub fn input_line(state: &CoordinatorState) -> String {
    format!("{INPUT_PROMPT}{}_", state.input)
}

/// Build the bottom status-bar text.
///
/// Why: a single helper keeps the bar text consistent and testable.
/// What: returns [`STATUS_BAR_HINT`] (Child #1 has no transient last-action
/// note; later children may fold one in here).
/// Test: `status_bar_lists_keys`.
pub fn status_bar_text() -> &'static str {
    STATUS_BAR_HINT
}

/// Build the unified session-list items: controller row 0, then sessions.
///
/// Why: the list always leads with the controller bullet and renders the
/// selected row in the two-column form; building the `ListItem`s here (from the
/// pure row-builders) keeps `render` short and the row content testable.
/// What: returns one [`ListItem`] per row — the controller at index 0 (its
/// right column the synthetic status), then each session, with the selected row
/// expanded to `[id] │ [summary]` via [`active_row_two_col`].
/// Test: row *content* is covered by the `super::rows` tests; ordering here is
/// exercised by launching the TUI.
fn session_list_items(state: &CoordinatorState) -> Vec<ListItem<'static>> {
    let mut items: Vec<ListItem<'static>> = vec![ListItem::new(build_controller_row(state))];
    for (idx, session) in state.sessions.iter().enumerate() {
        // Row 0 is the controller, so session `idx` lives at selection `idx + 1`.
        let selected = state.selected == idx + 1;
        let line: Line<'static> = if selected {
            active_row_two_col(&session.short_id, &session.summary)
        } else {
            session_row(&session.short_id, &session.prefix, &session.status)
        };
        items.push(ListItem::new(line));
    }
    items
}

/// Build the controller row, honouring whether it is the active selection.
///
/// Why: keeps the selected/unselected branch out of [`session_list_items`].
/// What: delegates to [`controller_bullet_row`] with the synthetic status and
/// the current selection state.
/// Test: covered by `controller_bullet_row` tests + `controller_status_is_synthetic`.
fn build_controller_row(state: &CoordinatorState) -> Line<'static> {
    controller_bullet_row(CONTROLLER_STATUS, state.controller_selected())
}

/// Draw the coordinator TUI frame: input (top), list (middle), status (bottom).
///
/// Why: the single entry point the event loop calls each tick; composing the
/// vertical layout in one place matches the dashboard's `render` convention.
/// What: a vertical [`Layout`] — a 3-row input box, a flexing session list, and
/// a 1-row status bar; the session list pins the controller at row 0 and
/// highlights the selected row in two columns.
/// Test: the text/content helpers are unit-tested; this terminal-touching path
/// is exercised by launching the TUI.
pub fn render(frame: &mut Frame, state: &CoordinatorState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // input box (top)
            Constraint::Min(4),    // session list (middle)
            Constraint::Length(1), // status / key bar (bottom)
        ])
        .split(frame.area());

    // Input box (top).
    let input = Paragraph::new(input_line(state))
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(input, chunks[0]);

    // Session list (middle): controller bullet pinned as row 0.
    let title = Line::from(format!("SESSIONS ({})", state.sessions.len()));
    let list = List::new(session_list_items(state)).block(
        Block::default().borders(Borders::ALL).title(
            title.style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ),
    );
    frame.render_widget(list, chunks[1]);

    // Status / key bar (bottom).
    let status = Paragraph::new(Line::from(status_bar_text())).style(
        Style::default()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::REVERSED),
    );
    frame.render_widget(status, chunks[2]);
}
