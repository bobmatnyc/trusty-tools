//! Keys for the session (right) column of the two-column MPM pane (#10754).
//!
//! Why: the session verbs act on the session selected in the right column
//! (#10754 S1), so they live with that column's own motion keys. Kept apart
//! from `events/tree.rs`, which owns the left column.
//! What: [`handle_session_key`] — Up/Down move between session rows, over
//! headers and detail lines. #10754: inline session details (Bob 11-06) —
//! Enter or Right expands the selected session in place, or collapses it;
//! Left collapses an expanded session, else returns focus to the left
//! column (Esc also collapses, in `handle_key`). `f`/`k`/`r`/`d`/`a` focus,
//! kill, resume, decommission and attach-cmd the selected session, through
//! the same gates as before; `l` launches in the selected project; `g`
//! toggles worker indentation.
//! Test: `two_column_tests.rs` (`tab_and_arrows_move_focus_between_columns`,
//! `session_verbs_act_on_the_right_column_session`,
//! `enter_expands_a_session_and_details_render_under_it`,
//! `up_and_down_skip_detail_lines`).

use crossterm::event::{KeyCode, KeyEvent};

use super::tree::{request_focus, request_launch};
use super::{
    PendingAction, request_decommission, request_kill, request_resume, selected_session_action,
};
use crate::tui::project_ctl::state::{Pane, ProjectCtlState};

/// Route one key while the session column has focus and no modal is open.
pub(super) fn handle_session_key(
    state: &mut ProjectCtlState,
    key: KeyEvent,
) -> Option<PendingAction> {
    match key.code {
        KeyCode::Up => state.session_step(false),
        KeyCode::Down => state.session_step(true),
        KeyCode::Left => {
            if !state.collapse_selected_session() {
                state.focus = Pane::Tree;
            }
        }
        KeyCode::Enter | KeyCode::Right => {
            // #10754: inline session details (Bob 11-06)
            if !state.toggle_session_details() {
                state.set_notice("select a session first");
            }
        }
        KeyCode::Char('g') => state.toggle_worker_indent(),
        KeyCode::Char('l') => return request_launch(state),
        KeyCode::Char('f') => return request_focus(state),
        KeyCode::Char('k') => return request_kill(state),
        KeyCode::Char('r') => return request_resume(state),
        KeyCode::Char('d') => return request_decommission(state),
        KeyCode::Char('a') => return selected_session_action(state, PendingAction::Attach),
        _ => {}
    }
    None
}
