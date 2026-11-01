//! Frame layout for the `tm projects` 4-pane TUI (#2118, DOC-35 §5).
//!
//! Why: the spec pins a fixed vertical `Layout` — the main row on top, a
//! fixed-height Activity strip, and a 1-row action bar. #10476: the
//! Owner -> Project -> Sessions tree fills the main row, replacing the
//! Projects (25%) | Sessions (75%) split (Architect Q9) — mirroring the `Constraint` conventions already used by
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

use super::panes::{actions_bar, activity, config_form, deliverables_view, session_details, tree};
use super::state::ProjectCtlState;

/// Minimum height (rows, including the two border rows) of the main tree row.
///
/// Why: floors the main row so a short terminal still shows a handful of
/// project/session rows rather than collapsing to nothing; taller terminals
/// flex the row larger via `Constraint::Min`.
const MIN_MAIN_ROWS: u16 = 7;

/// Fixed height (rows, including the two border rows) of the Activity pane.
///
/// Why: DOC-35 §5 pins this pane at `Constraint::Length(4)`.
const ACTIVITY_ROWS: u16 = 4;

/// Draw one full frame: the tree on top, Activity below, the action bar at
/// the bottom.
///
/// Why: the single entry point the run loop's `terminal.draw` calls each
/// tick.
/// What: splits `frame.area()` vertically into the main row (flexing, floored
/// at [`MIN_MAIN_ROWS`]), the Activity strip ([`ACTIVITY_ROWS`]), and the
/// 1-row action bar; the tree fills the main row (#10476). When
/// [`ProjectCtlState::deliverable_view`] is `Some` (DOC-35 §10.8, #2383) or
/// [`ProjectCtlState::config_form`] is `Some` (DOC-35 §6, #2120), a centred
/// overlay floats on top of the whole frame afterward — a strict addition,
/// never replacing a pane, mirroring the dashboard help-overlay's "draw base
/// frame, then float overlay" sequencing. The two are mutually exclusive by
/// construction (see `events`'s module doc), so at most one overlay ever
/// renders in a given frame. The #10476 session details overlay is the
/// fourth, drawn when [`crate::tui::project_ctl::state::TreeView::details`]
/// is `Some`.
pub fn render(frame: &mut Frame, state: &mut ProjectCtlState) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(MIN_MAIN_ROWS),
            Constraint::Length(ACTIVITY_ROWS),
            Constraint::Length(1),
        ])
        .split(frame.area());

    // #10476: only what this frame draws may be clicked.
    state.tree.hit = Default::default();
    tree::render(frame, outer[0], state);
    activity::render(frame, outer[1], state);
    actions_bar::render(frame, outer[2], state);
    state.tree.hit.bar_focus = actions_bar::focus_button(state, outer[2]);

    if let Some(view) = &state.deliverable_view {
        deliverables_view::render(frame, view);
    }
    if let Some(view) = &state.config_form {
        config_form::render(frame, view);
    }
    if state.tree.details.is_some() {
        session_details::render(frame, state);
    }
}
