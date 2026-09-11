//! ratatui drawing for the `tm ls` session TUI (#7224).
//!
//! Why: kept apart from the event loop so the frame can be rendered against a
//! `TestBackend` at any size — 20x5, 80x24, 200x50 — without a daemon, a
//! terminal, or a key loop. That is what turns "no panic at a tiny size" and
//! "narrow terminals drop columns" into tests rather than claims.
//!
//! What: [`render`] draws a title line, the session table (columns chosen by
//! [`super::layout::visible_columns`] from the CURRENT frame width), a status
//! line, and the key footer — then overlays the confirm / rename / help modal
//! when one is open. Every region comes from a `Layout`, which clamps to the
//! available area rather than computing a `Rect` that could fall outside it.
//!
//! Test: `render_*` in `super::tests`.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Row, Table, Wrap},
};
use trusty_mpm::client::ManagedSessionSummary;

use super::layout::{self, Column, MARKER_WIDTH};
use super::state::{Mode, Severity, TuiState};

/// The footer key legend — the discoverability half of every action key.
const FOOTER: &str =
    "↑↓ move · Enter open · n new · r rename · d delete · R refresh · ? keys · q quit";

/// Draw one frame of the session TUI.
///
/// Why: one entry point so the modal overlay can never be drawn without the
/// list underneath it, and so the `TestBackend` tests exercise exactly what a
/// real terminal gets.
/// What: splits the area into title / table / status / footer, renders the
/// table for the columns that fit, records the scroll offset it used back into
/// `state` (so the next frame holds still), then overlays a modal for any mode
/// other than [`Mode::Browse`]. Takes `&mut TuiState` only for that scroll
/// write-back; nothing else about the state changes here.
/// Test: `render_at_eighty_by_twentyfour_drops_the_widest_columns`,
/// `render_at_two_hundred_by_fifty_shows_every_column`,
/// `render_tiny_terminal_does_not_panic`, `render_*_overlay_*`.
pub(crate) fn render(frame: &mut Frame, sessions: &[ManagedSessionSummary], state: &mut TuiState) {
    let area = frame.area();
    // #7224: the status region is sized from the wrapped message BEFORE the
    // split, so a multi-line refusal gets the rows it needs instead of being
    // clipped mid-token by a fixed one-row `Paragraph`. Capped so the table
    // keeps at least one row on a short terminal.
    let status_text = status_lines(state, area.width);
    let status_rows = (status_text.len().max(1) as u16)
        .min(area.height.saturating_sub(3).max(1))
        .max(1);
    let [title, body, status, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(status_rows),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(title_line(sessions, state))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        title,
    );
    render_table(frame, body, sessions, state);
    frame.render_widget(Paragraph::new(status_text), status);
    frame.render_widget(
        Paragraph::new(FOOTER).style(Style::default().fg(Color::DarkGray)),
        footer,
    );

    match state.mode() {
        Mode::Browse => {}
        Mode::Help => overlay(frame, area, "keys", help_body()),
        Mode::Confirm {
            index,
            force,
            stop_first,
            typed,
        } => {
            let name = sessions.get(*index).map_or("?", |s| s.name.as_str());
            let body = confirm_body(name, *force, *stop_first, typed);
            overlay(frame, area, "delete", body);
        }
        Mode::Rename { index, typed } => {
            let was = sessions.get(*index).map_or("?", |s| s.name.as_str());
            overlay(frame, area, "rename", rename_body(was, typed));
        }
        // #7395: the create flow — a project list, then optionally a path.
        // #7406: built to the popup's INNER width (its rect less the two
        // border columns), so no project row can wrap.
        Mode::New(flow) => {
            let inner = overlay_rect(area).width.saturating_sub(2) as usize;
            overlay(frame, area, "new session", new_session_body(flow, inner));
        }
    }
}

/// The one-line header above the table.
fn title_line(sessions: &[ManagedSessionSummary], state: &TuiState) -> String {
    if sessions.is_empty() {
        return "tm ls — no sessions".to_string();
    }
    format!(
        "tm ls — {} of {} session(s)",
        state.selected() + 1,
        sessions.len()
    )
}

/// The status region: the last outcome, wrapped to `width`, or nothing.
///
/// Why: a refusal from the daemon can run past 150 characters, and the one that
/// matters most — the delete guard's — ends in a session UUID. Wrapping it
/// through [`layout::wrap_message`] rather than letting a one-row `Paragraph`
/// clip it is what keeps that id whole (#7224).
/// What: one styled [`Line`] per wrapped row, red for a refusal and green for an
/// outcome; an empty `Vec` when there is no message.
/// Test: `render_wraps_a_long_refusal_without_cutting_the_uuid`,
/// `render_status_uses_one_row_when_there_is_no_message`.
fn status_lines(state: &TuiState, width: u16) -> Vec<Line<'static>> {
    let Some((text, severity)) = state.message() else {
        return Vec::new();
    };
    let color = match severity {
        Severity::Error => Color::Red,
        Severity::Info => Color::Green,
    };
    layout::wrap_message(text, width as usize, layout::STATUS_MAX_LINES)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, Style::default().fg(color))))
        .collect()
}

/// Render the session table into `area` for whatever columns fit its width.
///
/// Why: the column set has to be recomputed here rather than cached, because
/// `area` is whatever the terminal is RIGHT NOW — a resize is just a frame with
/// a different width, which is the whole of #7224's resize handling.
/// What: reserves one row for the header, computes the visible window through
/// [`layout::visible_window`], and writes the offset back into `state`.
fn render_table(
    frame: &mut Frame,
    area: Rect,
    sessions: &[ManagedSessionSummary],
    state: &mut TuiState,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let columns = layout::visible_columns(area.width);
    let rows_height = area.height.saturating_sub(1) as usize;
    let (start, len) = layout::visible_window(
        sessions.len(),
        state.selected(),
        rows_height,
        state.scroll(),
    );
    state.set_scroll(start);

    let rows: Vec<Row> = sessions[start..start + len]
        .iter()
        .enumerate()
        .map(|(offset, session)| row(&columns, session, start + offset == state.selected()))
        .collect();
    let header = Row::new(
        std::iter::once(String::new())
            .chain(columns.iter().map(|c| layout::header(*c).to_string()))
            .collect::<Vec<_>>(),
    )
    .style(Style::default().fg(Color::DarkGray));
    let widths: Vec<Constraint> = std::iter::once(Constraint::Length(MARKER_WIDTH))
        .chain(columns.iter().map(|c| match c {
            // NAME absorbs the leftover width so a wide terminal is not mostly
            // empty gutter; every other column keeps its fixed size.
            Column::Name => Constraint::Min(layout::width(Column::Name)),
            other => Constraint::Length(layout::width(*other)),
        }))
        .collect();
    frame.render_widget(
        Table::new(rows, widths).header(header).column_spacing(1),
        area,
    );
}

/// Build one table row, marked and highlighted when it is the selected one.
fn row(columns: &[Column], session: &ManagedSessionSummary, selected: bool) -> Row<'static> {
    let cells: Vec<String> = std::iter::once(if selected {
        "▸".to_string()
    } else {
        String::new()
    })
    .chain(columns.iter().map(|c| layout::cell(*c, session)))
    .collect();
    let style = match (selected, session.attached, session.unresumable) {
        (true, _, _) => Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        (false, _, true) => Style::default().fg(Color::Red),
        (false, true, _) => Style::default().fg(Color::Cyan),
        _ => Style::default(),
    };
    Row::new(cells).style(style)
}

/// Draw a centered modal with `title` over the list.
///
/// Why: the confirm and rename steps must be unmissable, and an overlay is the
/// only way to say "this key press means something different now" without
/// redrawing the whole surface.
/// What: a percentage-sized centered `Rect` (percentages cannot fall outside
/// the parent, which is what keeps a 20x5 terminal from panicking), `Clear`ed
/// then filled with a bordered paragraph.
fn overlay(frame: &mut Frame, area: Rect, title: &str, body: Vec<Line<'static>>) {
    let popup = overlay_rect(area);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(body).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} ")),
        ),
        popup,
    );
}

/// The centered modal's rect: 60% of the height, 80% of the width.
///
/// #7406: split out of [`overlay`] so a body can be built to the width it will
/// actually be drawn at. Percentages cannot fall outside the parent, which is
/// what keeps a 20x5 terminal from panicking.
fn overlay_rect(area: Rect) -> Rect {
    let [_, middle, _] = Layout::vertical([
        Constraint::Percentage(20),
        Constraint::Percentage(60),
        Constraint::Percentage(20),
    ])
    .areas(area);
    let [_, popup, _] = Layout::horizontal([
        Constraint::Percentage(10),
        Constraint::Percentage(80),
        Constraint::Percentage(10),
    ])
    .areas(middle);
    popup
}

/// Text of `text` that fits `width` columns, ending in `…` when it was cut.
///
/// Why (#7406): the overlay wraps, so one row wider than the pane becomes two
/// rows and the list stops lining up. Cutting is the lesser loss — the head of
/// an `owner/repo` is what identifies it.
/// What: counts CHARACTERS, not bytes, so a multi-byte name is never split
/// mid-codepoint. A width under two leaves nothing to say and returns empty.
/// Test: `render_new_session_overlay_truncates_a_long_row`.
fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width < 2 {
        return String::new();
    }
    let mut cut: String = text.chars().take(width - 1).collect();
    cut.push('…');
    cut
}

/// The delete confirmation's text — the word required, and what typing it does.
///
/// `stop_first` (#7224) states the second leg up front: confirming an errored
/// row stops its runtime AND deletes the record, so the prompt must say so
/// before the operator agrees to it rather than after.
fn confirm_body(name: &str, force: bool, stop_first: bool, typed: &str) -> Vec<Line<'static>> {
    let ask = if force {
        format!("'{name}' is RUNNING. Type the word force, then Enter, to delete it.")
    } else if stop_first {
        crate::commands::picker_delete::errored_confirm_ask(name)
    } else {
        format!("Delete '{name}'? Type y, then Enter.")
    };
    vec![
        Line::from(ask),
        Line::from(String::new()),
        Line::from(format!("> {typed}▌")),
        Line::from("Esc cancels."),
    ]
}

/// The rename overlay's text.
fn rename_body(was: &str, typed: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(format!("Rename '{was}' to:")),
        Line::from(String::new()),
        Line::from(format!("> {typed}▌")),
        Line::from("Enter applies, Esc cancels."),
    ]
}

/// The new-session overlay's text: the project list, or the path entry (#7395).
///
/// Why: one body function for both steps keeps "which step am I on" a single
/// readable branch, and makes the free-text entry look exactly like the rename
/// overlay's — the operator has already learned that shape.
/// What: the picker step lists the windowed targets from
/// [`super::new_session::NewSessionFlow::rows`], each cut to `width` (#7406),
/// under a hint line carrying the window's position in the list and the typed
/// filter (#7421); the path step draws the typed buffer with the same cursor
/// block. Both name Esc as the way out.
/// Test: `render_new_session_overlay_lists_the_registered_projects`,
/// `render_new_session_overlay_shows_the_typed_path`,
/// `render_new_session_overlay_truncates_a_long_row`,
/// `render_new_session_overlay_shows_the_position_and_filter`.
fn new_session_body(flow: &super::new_session::NewSessionFlow, width: usize) -> Vec<Line<'static>> {
    if let Some(typed) = flow.typed() {
        return vec![
            Line::from("Path to a git checkout tm has not registered yet:"),
            Line::from(String::new()),
            Line::from(format!("> {typed}▌")),
            Line::from("Enter registers it and starts a session, Esc cancels."),
        ];
    }
    let mut lines = vec![
        Line::from("Start a new session in:"),
        Line::from(String::new()),
    ];
    lines.extend(flow.rows().iter().map(|r| Line::from(fit(r, width))));
    // #7421: without this the rows past the eighth simply looked absent.
    let hint = new_session_hint(flow);
    if !hint.is_empty() {
        lines.push(Line::from(fit(&hint, width)));
    }
    lines.push(Line::from(String::new()));
    lines.push(Line::from("Type to filter. Enter confirms, Esc cancels."));
    lines
}

/// The picker's position-and-filter hint, empty when it would say nothing.
///
/// A short list that has not been filtered has neither a position worth stating
/// nor a filter to echo, so it gets no line at all rather than a blank one.
fn new_session_hint(flow: &super::new_session::NewSessionFlow) -> String {
    let mut parts: Vec<String> = flow.position().into_iter().collect();
    if !flow.filter().is_empty() {
        parts.push(format!("filter: {}", flow.filter()));
    }
    parts.join("  ·  ")
}

/// The `?` key reference.
fn help_body() -> Vec<Line<'static>> {
    [
        "↑ / k, ↓ / j   move the selection",
        "g / G          first / last row",
        "PgUp / PgDn    move ten rows",
        "Enter          open (resume + attach) the selected session",
        "n              new session, in a registered project or a typed path",
        "r              rename it",
        "d              delete it, with a confirm step",
        "R              refresh the list from the daemon",
        "q / Esc        quit",
        "",
        "Any key closes this help.",
    ]
    .into_iter()
    .map(|l| Line::from(l.to_string()))
    .collect()
}
