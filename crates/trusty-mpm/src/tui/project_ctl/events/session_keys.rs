//! Keys for the session (right) column of the two-column MPM pane (#10754).
//!
//! Why: the session verbs act on the session selected in the right column
//! (#10754 S1), so they live with that column's own motion keys. Kept apart
//! from `events/tree.rs`, which owns the left column.
//! What: [`handle_session_key`] — Up/Down move between session rows, over
//! headers and detail lines. #10754: De1 key map (Bob 11-06), matching
//! `tm ls`: Enter opens the selected session (resume if needed, then
//! attach); Right expands it in place, or collapses it; `r` opens the rename
//! modal with the current name typed; `R` resumes only, through the
//! `unresumable` refusal; `n` starts a new session in its project. Left
//! collapses an expanded session, else returns focus to the left column (Esc
//! also collapses, in `handle_key`). `f`/`k`/`d`/`a` focus, kill,
//! decommission and attach-cmd the selected session, through the same gates
//! as before; `l` opens the launch form for the session's project (see
//! #10754); `g` toggles worker indentation.
//! Test: `two_column_tests.rs` (`tab_and_arrows_move_focus_between_columns`,
//! `session_verbs_act_on_the_right_column_session`,
//! `right_expands_a_session_and_details_render_under_it`,
//! `right_toggles_details_and_enter_does_not`,
//! `r_opens_the_rename_modal_prefilled_and_enter_submits`,
//! `n_starts_a_new_session_in_the_project_from_either_column`,
//! `n_in_an_empty_session_column_starts_a_new_session_and_l_opens_the_form`,
//! `l_opens_the_launch_form_and_enter_submits_the_request`,
//! `up_and_down_skip_detail_lines`); `session_ops_tests.rs`
//! (`enter_on_a_session_opens_it_between_suspend_and_resume`).

use crossterm::event::{KeyCode, KeyEvent};

use super::tree::{open_launch, request_focus, request_new_session};
use super::{
    PendingAction, request_decommission, request_kill, request_resume, selected_session_action,
};
use crate::tui::project_ctl::state::rename::RenameModal;
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
        // #10754: De1 key map (Bob 11-06) — Enter opens, as in `tm ls`.
        KeyCode::Enter => return selected_session_action(state, PendingAction::OpenSession),
        KeyCode::Right => {
            // #10754: De1 key map (Bob 11-06) — details moved here from Enter.
            if !state.toggle_session_details() {
                state.set_notice("select a session first");
            }
        }
        KeyCode::Char('g') => state.toggle_worker_indent(),
        // #10754: both reach the tree's project when the column is empty;
        // `l` opens the launch form (see #10754).
        KeyCode::Char('n') => return request_new_session(state),
        KeyCode::Char('l') => open_launch(state),
        KeyCode::Char('f') => return request_focus(state),
        KeyCode::Char('k') => return request_kill(state),
        KeyCode::Char('r') => open_rename(state),
        KeyCode::Char('R') => return request_resume(state),
        KeyCode::Char('d') => return request_decommission(state),
        KeyCode::Char('a') => return selected_session_action(state, PendingAction::Attach),
        _ => {}
    }
    None
}

/// `r`: open the rename modal on the selected session, its name typed.
///
/// #10754: De1 key map (Bob 11-06) — `r` renames, as in `tm ls`.
fn open_rename(state: &mut ProjectCtlState) {
    match state.selected_session() {
        Some(session) => {
            let modal = RenameModal::prefilled(session.id.clone(), session.name.clone());
            state.rename = Some(modal);
        }
        None => state.set_notice("select a session first"),
    }
}
