//! Row clicks and the wheel across the two MPM columns (#10754).
//!
//! Why: the main row holds two lists, and each keeps its own selection and
//! scroll offset (#10754 S1). The hit test picks the column from the
//! rectangles the last frame recorded, so a column not drawn (below 60
//! terminal columns) has no target. Kept out of `events/mouse.rs` (cap).
//! What: [`click_row`] selects the row under the cursor in either column and
//! focuses that column. An owner click toggles it and a session click opens
//! the details overlay, as Enter does; a project click only selects.
//! [`wheel`] moves the selection of the column under the cursor.
//! Test: `two_column_tests.rs` (`click_selects_and_focuses_either_column`,
//! `wheel_scrolls_the_column_under_the_cursor`); `events::mouse::tests`.

use ratatui::layout::Position;

use super::mouse::row_at;
use crate::tui::project_ctl::state::{NodeId, Pane, ProjectCtlState};

/// Select (and focus) the row drawn at `(column, row)`, in either column.
pub(super) fn click_row(state: &mut ProjectCtlState, column: u16, row: u16) {
    let hit = state.tree.hit;
    if let Some(index) = row_at(hit.tree_rows, state.tree.nav.offset(), column, row) {
        if state.select_row(index) {
            state.focus = Pane::Tree;
            if matches!(state.selected_node(), Some(NodeId::Owner(_))) {
                state.toggle_selected();
            }
        }
        return;
    }
    let offset = state.session_col.nav.offset();
    if let Some(index) = row_at(hit.session_rows, offset, column, row)
        && state.select_session_row(index)
    {
        // #10754: two-column MPM pane (Bob 11-06)
        state.focus = Pane::Sessions;
        state.open_details();
    }
}

/// Move the selection of the column under `(column, row)` and focus it.
pub(super) fn wheel(state: &mut ProjectCtlState, column: u16, row: u16, down: bool) {
    let at = Position::new(column, row);
    if state.tree.hit.tree_rows.contains(at) {
        state.focus = Pane::Tree;
        state.tree_step(down);
    } else if state.tree.hit.session_rows.contains(at) {
        state.focus = Pane::Sessions;
        state.session_step(down);
    }
}
