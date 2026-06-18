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
    widgets::{Block, Borders, HighlightSpacing, List, ListItem, Paragraph},
};

use super::nav::MIN_VISIBLE_ROWS;
use super::rows::{active_row_two_col, controller_bullet_row, numbered_session_row};
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
pub const STATUS_BAR_HINT: &str = "/new /sessions /attach /stop /resume /kill /help  ·  ↵ send  ·  ↑↓/jk select  ·  PgUp/PgDn scroll  ·  q quit";

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

/// The status-bar indicator shown when the daemon answered its last probe.
///
/// Why: Child #2 polls the daemon live; the operator needs an at-a-glance
/// signal that the list is fresh. A constant keeps the reachable glyph + label
/// single-sourced between the renderer and its test.
/// What: the literal `daemon ●`.
/// Test: `daemon_indicator_reflects_reachability`.
pub const DAEMON_REACHABLE: &str = "daemon ●";

/// The status-bar indicator shown when the daemon is unreachable.
///
/// Why: the counterpart to [`DAEMON_REACHABLE`] — a distinct glyph so a
/// daemon-down state reads differently from a healthy one, mirroring the cleared
/// (empty) session list `coord_poll_daemon` produces on failure.
/// What: the literal `daemon ✗`.
/// Test: `daemon_indicator_reflects_reachability`.
pub const DAEMON_UNREACHABLE: &str = "daemon ✗";

/// Pick the daemon-reachability indicator for the current state.
///
/// Why: keeps the reachable/unreachable branch out of [`render`] and gives the
/// indicator a pure, testable seam.
/// What: returns [`DAEMON_REACHABLE`] when `state.daemon_reachable`, else
/// [`DAEMON_UNREACHABLE`].
/// Test: `daemon_indicator_reflects_reachability`.
pub fn daemon_indicator(state: &CoordinatorState) -> &'static str {
    if state.daemon_reachable {
        DAEMON_REACHABLE
    } else {
        DAEMON_UNREACHABLE
    }
}

/// The status-bar hint shown when the daemon reports the catalog is stale (HR-3).
///
/// Why: DOC-17 §HR-3 requires a non-intrusive "update available" signal plus a
/// hint at the rebuild action; naming the literal here single-sources it between
/// the renderer and its test. The trailing command names the apply path so the
/// operator knows HOW to act on the offer.
/// What: the literal `catalog: ↑updates available (tm catalog apply)`.
/// Test: `catalog_indicator_reflects_staleness`.
pub const CATALOG_STALE_HINT: &str = "catalog: ↑updates available (tm catalog apply)";

/// The catalog-staleness hint for the current state, if any.
///
/// Why: the status bar only shows the rebuild offer when there IS an update, and
/// only when the daemon is reachable (a down daemon's flag is meaningless). A
/// pure helper keeps that branch out of [`render`] and testable without a
/// terminal.
/// What: returns `Some(`[`CATALOG_STALE_HINT`]`)` when the daemon is reachable
/// AND `catalog_stale`, else `None`.
/// Test: `catalog_indicator_reflects_staleness`.
pub fn catalog_indicator(state: &CoordinatorState) -> Option<&'static str> {
    if state.daemon_reachable && state.catalog_stale {
        Some(CATALOG_STALE_HINT)
    } else {
        None
    }
}

/// Build the unified, numbered session-list items: controller row 0, then
/// numbered sessions (1, 2, 3, …).
///
/// Why: STUI-1 (#1278) requires a NUMBERED list whose selected session expands
/// to the two-column `[id] │ [summary]` form. Building the `ListItem`s here (from
/// the pure row-builders) keeps `render` short and the row content testable; the
/// row highlight itself is applied by the `List` widget's `highlight_style` in
/// [`render`] (driven by `state.nav`), so this only owns the per-row TEXT.
/// What: returns one [`ListItem`] per row — the controller at index 0 (its right
/// column the synthetic status), then each session prefixed with its 1-based
/// list number via [`numbered_session_row`], with the selected session expanded
/// to `N. [id] │ [summary]` via [`active_row_two_col`].
/// Test: row *content* is covered by the `super::rows` tests; ordering here is
/// exercised by launching the TUI.
fn session_list_items(state: &CoordinatorState) -> Vec<ListItem<'static>> {
    let selected = state.selected();
    let mut items: Vec<ListItem<'static>> = vec![ListItem::new(build_controller_row(state))];
    for (idx, session) in state.sessions.iter().enumerate() {
        // Row 0 is the controller, so session `idx` lives at selection `idx + 1`
        // and is shown to the operator as list number `idx + 1`.
        let number = idx + 1;
        let is_selected = selected == number;
        let line: Line<'static> = if is_selected {
            active_row_two_col(&session.short_id, &session.summary)
        } else {
            numbered_session_row(number, &session.short_id, &session.prefix, &session.status)
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
/// STUI-1 makes the list a STATEFUL, scrolling widget — it takes `&mut state` so
/// it can hand `state.nav`'s wrapped [`ListState`](ratatui::widgets::ListState)
/// to `render_stateful_widget`, which both reads the preserved selection/offset
/// and nudges the offset to keep the selection visible within the live viewport.
/// What: a vertical [`Layout`] — a 3-row input box, a flexing session list whose
/// middle region is floored at [`MIN_VISIBLE_ROWS`] rows plus the two border
/// rows, and a 1-row status bar; the session list pins the controller at row 0,
/// numbers the sessions, highlights the selected row via `highlight_style`, and
/// expands the selected session to two columns.
/// Test: the text/content helpers are unit-tested; this terminal-touching path
/// is exercised by launching the TUI.
pub fn render(frame: &mut Frame, state: &mut CoordinatorState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // input box (top)
            // The list floors at five visible rows + the two border rows so the
            // STUI-1 "minimum 5 visible rows" contract holds; it flexes larger on
            // a taller terminal and ratatui scrolls within whatever height it gets.
            Constraint::Min((MIN_VISIBLE_ROWS + 2) as u16), // session list (middle)
            Constraint::Length(1),                          // status / key bar (bottom)
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

    // Compose the immutable pieces (items, title, status text) BEFORE borrowing
    // `state.nav` mutably for the stateful render, so the borrows do not overlap.
    let items = session_list_items(state);
    let title = Line::from(format!("SESSIONS ({})", state.sessions.len()));
    let catalog_segment = catalog_indicator(state)
        .map(|hint| format!("  ·  {hint}"))
        .unwrap_or_default();
    let status_text = format!(
        "{}  ·  {}{}",
        status_bar_text(),
        daemon_indicator(state),
        catalog_segment
    );

    // Session list (middle): controller bullet pinned as row 0, numbered sessions
    // below, the selected row highlighted, scrolling driven by `state.nav`.
    let list = List::new(items)
        .block(
            Block::default().borders(Borders::ALL).title(
                title.style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ),
        )
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
        // Keep every row aligned whether or not it is the selection (mirrors the
        // health screen's collections list).
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(list, chunks[1], state.nav.state_mut());

    // Status / key bar (bottom): key hints on the left, the live daemon-
    // reachability indicator on the right, plus the HR-3 catalog-staleness hint
    // when the daemon reports an available update.
    let status = Paragraph::new(Line::from(status_text)).style(
        Style::default()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::REVERSED),
    );
    frame.render_widget(status, chunks[2]);
}
