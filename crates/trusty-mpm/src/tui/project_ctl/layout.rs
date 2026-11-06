//! Frame layout for the `tm projects` 4-pane TUI (#2118, DOC-35 §5).
//!
//! Why: the spec pins a fixed vertical `Layout` — the main row on top, a
//! fixed-height Activity strip, and a 1-row action bar — mirroring the
//! `Constraint` conventions already used by `tui::coordinator::layout` and
//! `tui::health::render`. #10754: two-column MPM pane (Bob 11-06) — the main
//! row splits into the Owner -> Project tree on the left and the selected
//! node's sessions on the right (Architect Q5). Composing the `Rect`s here
//! (rather than in each pane module) keeps the frame's shape in one place.
//! What: [`render`] draws the full frame each tick, delegating each region's
//! content to its [`super::panes`] submodule; [`main_columns`] splits the
//! main row.
//! Test: `main_columns_split_30_70_with_a_floor`;
//! `two_column_tests.rs` (`render_100x30_shows_both_columns`,
//! `narrow_terminal_renders_only_the_focused_column`).

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
};

use super::panes::{actions_bar, activity, config_form, deliverables_view, session_list, tree};
use super::state::{Pane, ProjectCtlState};

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

/// Below this terminal width only the focused column shows (Architect Q5).
pub const TWO_COLUMN_MIN_WIDTH: u16 = 60;

/// The narrowest the left column draws, in columns (Architect Q5).
const LEFT_MIN_COLS: u16 = 24;

/// Split the main row into `(left, right)` columns.
///
/// Why: the owner/project tree needs only a short column; the sessions take
/// the rest (#10754, Architect Q5).
/// What: at [`TWO_COLUMN_MIN_WIDTH`] or wider, the left column is 30% of the
/// width, at least [`LEFT_MIN_COLS`], and the right one the rest. Narrower,
/// one column fills the row: the session column while it has focus, else the
/// tree. The column not drawn is `None`.
/// Test: `main_columns_split_30_70_with_a_floor`.
pub fn main_columns(area: Rect, focus: Pane) -> (Option<Rect>, Option<Rect>) {
    // #10754: two-column MPM pane (Bob 11-06)
    if area.width < TWO_COLUMN_MIN_WIDTH {
        return if focus == Pane::Sessions {
            (None, Some(area))
        } else {
            (Some(area), None)
        };
    }
    // #10754: u32 math, so a very wide terminal cannot overflow u16.
    let tenths = u16::try_from(u32::from(area.width) * 3 / 10).unwrap_or(area.width);
    let left = tenths.max(LEFT_MIN_COLS);
    let [l, r] = Layout::horizontal([Constraint::Length(left), Constraint::Min(0)]).areas(area);
    (Some(l), Some(r))
}

/// Draw one full frame: the two columns on top, Activity below, the action
/// bar at the bottom.
///
/// Why: the single entry point the run loop's `terminal.draw` calls each
/// tick.
/// What: splits `frame.area()` vertically into the main row (flexing, floored
/// at [`MIN_MAIN_ROWS`]), the Activity strip ([`ACTIVITY_ROWS`]), and the
/// 1-row action bar; [`main_columns`] splits the main row between the tree
/// and the session column (#10754), and the rightmost drawn column carries
/// the version label. When
/// [`ProjectCtlState::deliverable_view`] is `Some` (DOC-35 §10.8, #2383) or
/// [`ProjectCtlState::config_form`] is `Some` (DOC-35 §6, #2120), a centred
/// overlay floats on top of the whole frame afterward — a strict addition,
/// never replacing a pane, mirroring the dashboard help-overlay's "draw base
/// frame, then float overlay" sequencing. The two are mutually exclusive by
/// construction (see `events`'s module doc), so at most one overlay ever
/// renders in a given frame. #10754: inline session details (Bob 11-06) —
/// session details draw inside the session column, so no session overlay
/// remains.
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
    let (left, right) = main_columns(outer[0], state.focus);
    if let Some(area) = left {
        tree::render(frame, area, state, right.is_none());
    }
    if let Some(area) = right {
        session_list::render(frame, area, state, true);
    }
    activity::render(frame, outer[1], state);
    actions_bar::render(frame, outer[2], state);
    state.tree.hit.bar_focus = actions_bar::focus_button(state, outer[2]);

    if let Some(view) = &state.deliverable_view {
        deliverables_view::render(frame, view);
    }
    if let Some(view) = &state.config_form {
        config_form::render(frame, view);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_columns_split_30_70_with_a_floor() {
        let (l, r) = main_columns(Rect::new(0, 0, 100, 20), Pane::Tree);
        assert_eq!((l.unwrap().width, r.unwrap().width), (30, 70));
        let (l, r) = main_columns(Rect::new(0, 0, 60, 20), Pane::Tree);
        assert_eq!(
            (l.unwrap().width, r.unwrap().width),
            (24, 36),
            "24-col floor"
        );
        let narrow = Rect::new(0, 0, 59, 20);
        assert_eq!(main_columns(narrow, Pane::Tree), (Some(narrow), None));
        assert_eq!(main_columns(narrow, Pane::Activity), (Some(narrow), None));
        assert_eq!(main_columns(narrow, Pane::Sessions), (None, Some(narrow)));
        // #10754: the 30% split cannot overflow on the widest terminal.
        let (l, r) = main_columns(Rect::new(0, 0, u16::MAX, 20), Pane::Tree);
        assert_eq!(l.unwrap().width, 19660);
        assert_eq!(r.unwrap().width, u16::MAX - 19660);
    }
}
