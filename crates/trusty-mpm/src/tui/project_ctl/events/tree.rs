//! Keys for the tree pane and its details overlay (#10476).
//!
//! Why: the tree replaced the Projects and Sessions panes (Architect Q9), so
//! the keys those panes split between them now act on the selected tree node.
//! Kept apart from `events/mod.rs`, which owns the modal precedence chain.
//! What: [`handle_tree_key`] — Up/Down move; Left collapses or climbs to the
//! parent; Right expands or steps into the first child; Enter toggles an
//! owner or project and opens the details overlay on a session; `f` focuses
//! the session's existing window; `g` toggles worker indentation. The verbs
//! are unchanged: `c`/`v` act on a project node (as in the Projects pane),
//! `l` on the selected node's project, and
//! `k`/`r`/`d`/`a` act only on a session node (on an owner, project or
//! anchor they set a notice and act on nothing). [`handle_details_key`] is
//! the overlay's modal handler.
//! Test: `events::tests` (`enter_on_owner_and_project_toggles`,
//! `enter_on_session_opens_details_and_returns_no_pending_action`,
//! `esc_closes_details`, `f_in_details_returns_focus`,
//! `kill_resume_decommission_attach_on_bar_or_project_act_on_nothing`).

use crossterm::event::{KeyCode, KeyEvent};

use super::{
    PendingAction, request_decommission, request_kill, request_resume, selected_session_action,
};
use crate::tui::project_ctl::state::{NodeId, ProjectCtlState};

/// Route one key while the tree pane has focus and no modal is open.
pub(super) fn handle_tree_key(state: &mut ProjectCtlState, key: KeyEvent) -> Option<PendingAction> {
    match key.code {
        KeyCode::Up => state.tree_step(false),
        KeyCode::Down => state.tree_step(true),
        KeyCode::Left => state.tree_left(),
        KeyCode::Right => state.tree_right(),
        KeyCode::Enter => enter(state),
        KeyCode::Char('f') => return request_focus(state),
        KeyCode::Char('g') => state.toggle_worker_indent(),
        KeyCode::Char('l') => {
            return match state.selected_project_name() {
                Some(name) => Some(PendingAction::Launch(name.to_string())),
                None => {
                    state.set_notice("no project selected");
                    None
                }
            };
        }
        KeyCode::Char('k') => return request_kill(state),
        KeyCode::Char('r') => return request_resume(state),
        KeyCode::Char('d') => return request_decommission(state),
        KeyCode::Char('a') => return selected_session_action(state, PendingAction::Attach),
        KeyCode::Char('c') => {
            if let Some(name) = project_node_name(state) {
                state.open_config_form(name);
            }
        }
        KeyCode::Char('v') => {
            if let Some(name) = project_node_name(state) {
                state.open_deliverable_view(name);
            }
        }
        _ => {}
    }
    None
}

/// The selected project node's name, for `c`/`v`.
///
/// Before the tree, `c`/`v` were bound in the Projects pane only, so a session
/// node ignores them. An owner bar or an empty tree has no project: notice.
fn project_node_name(state: &mut ProjectCtlState) -> Option<String> {
    match state.selected_node() {
        Some(NodeId::Project { name, .. }) => Some(name.clone()),
        Some(NodeId::Session { .. } | NodeId::Anchor { .. }) => None,
        Some(NodeId::Owner(_)) | None => {
            state.set_notice("no project selected");
            None
        }
    }
}

/// Enter: toggle an owner or project; open details on a session.
///
/// #10476: before the tree, Enter on a session did nothing. A click on a row
/// runs this too (`events::mouse`).
pub(super) fn enter(state: &mut ProjectCtlState) {
    match state.selected_node() {
        Some(NodeId::Owner(_) | NodeId::Project { .. }) => state.toggle_selected(),
        Some(NodeId::Session { .. }) => {
            state.open_details();
        }
        Some(NodeId::Anchor { .. }) => state.set_notice("this parent session is not listed"),
        None => {}
    }
}

/// `f`: focus the selected session's existing tmux window or iTerm2 tab.
fn request_focus(state: &mut ProjectCtlState) -> Option<PendingAction> {
    match state.selected_session() {
        Some(session) => Some(PendingAction::Focus(session.name.clone())),
        None => {
            state.set_notice("select a session first");
            None
        }
    }
}

/// Close a details overlay whose session has left the fleet; `true` if it did.
///
/// #10476: the overlay draws nothing for that session, so the key and mouse
/// paths share this one rule and neither leaves the input dead.
/// Test: `details_swallow_quit_and_close_when_session_leaves`,
/// `mouse_closes_details_when_session_leaves`.
pub(super) fn close_stale_details(state: &mut ProjectCtlState) -> bool {
    if state.tree.details.is_none() {
        return false;
    }
    let shown = state.details_session().map(|(_, s)| s.id.clone());
    let selected = state.selected_session().map(|s| s.id.clone());
    if shown.is_some() && shown == selected {
        return false;
    }
    state.close_details();
    state.set_notice("that session is no longer listed");
    true
}

/// Route one key while the read-only details overlay is open.
///
/// What: the overlay is modal, like the other three: `Esc` (or Enter) closes
/// it, `f` focuses the session, `k`/`r`/`d`/`a` act on the shown session
/// exactly as in the tree, and every other key (quit included) is
/// swallowed. When the shown session has left the fleet, any key closes the
/// overlay with a notice.
/// Test: `esc_closes_details`, `f_in_details_returns_focus`,
/// `details_swallow_quit_and_close_when_session_leaves`.
pub(super) fn handle_details_key(
    state: &mut ProjectCtlState,
    key: KeyEvent,
) -> Option<PendingAction> {
    if close_stale_details(state) {
        return None;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Enter => {
            state.close_details();
            None
        }
        KeyCode::Char('f') => request_focus(state),
        KeyCode::Char('k') => request_kill(state),
        KeyCode::Char('r') => request_resume(state),
        KeyCode::Char('d') => request_decommission(state),
        KeyCode::Char('a') => selected_session_action(state, PendingAction::Attach),
        _ => None,
    }
}
