//! Frame layout for the `tm projects` 4-pane TUI (#2118, DOC-35 §5).
//!
//! Why: the spec pins a fixed vertical/horizontal `Layout` — Projects (25%) |
//! Sessions (75%) on top, a fixed-height Activity strip, and a 1-row action
//! bar — mirroring the `Constraint` conventions already used by
//! `tui::coordinator::layout` and `tui::health::render`. Composing the
//! `Rect`s here (rather than in each pane module) keeps the frame's shape in
//! one place.
//! What: [`render`] draws the full frame each tick, delegating each region's
//! content to its [`super::panes`] submodule.
//! Test: the pane submodules unit-test their own content; this terminal-
//! touching composition is exercised by launching the TUI.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
};

use super::panes::{actions_bar, activity, projects, sessions};
use super::state::ProjectCtlState;

/// Minimum height (rows, including the two border rows) of the main
/// Projects/Sessions row.
///
/// Why: floors the main row so a short terminal still shows a handful of
/// project/session rows rather than collapsing to nothing; taller terminals
/// flex the row larger via `Constraint::Min`.
const MIN_MAIN_ROWS: u16 = 7;

/// Fixed height (rows, including the two border rows) of the Activity pane.
///
/// Why: DOC-35 §5 pins this pane at `Constraint::Length(4)`.
const ACTIVITY_ROWS: u16 = 4;

/// Draw one full frame: Projects | Sessions on top, Activity below, the
/// action bar at the bottom.
///
/// Why: the single entry point the run loop's `terminal.draw` calls each
/// tick.
/// What: splits `frame.area()` vertically into the main row (flexing, floored
/// at [`MIN_MAIN_ROWS`]), the Activity strip ([`ACTIVITY_ROWS`]), and the
/// 1-row action bar; splits the main row horizontally 25/75 for
/// Projects/Sessions per DOC-35 §5.
pub fn render(frame: &mut Frame, state: &mut ProjectCtlState) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(MIN_MAIN_ROWS),
            Constraint::Length(ACTIVITY_ROWS),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let main_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(outer[0]);

    projects::render(frame, main_row[0], state);
    sessions::render(frame, main_row[1], state);
    activity::render(frame, outer[1], state);
    actions_bar::render(frame, outer[2], state);
}
