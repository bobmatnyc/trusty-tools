//! Keys for the tree (left) column (#10476).
//!
//! Why: the tree replaced the Projects pane (Architect Q9). Kept apart from
//! `events/mod.rs`, which owns the modal precedence chain.
//! What: [`handle_tree_key`] — Up/Down move; Left collapses an owner or
//! climbs to it; Right expands an owner or steps into its first project;
//! Enter toggles an owner. #10754: two-column MPM pane (Bob 11-06) — Right
//! or Enter on a project moves focus into the session column. `g` toggles
//! worker indentation. `c`/`v` act on a project node, `l` and `n` on its
//! project (#10754: De1 key map (Bob 11-06) — `n` starts a new session, as in
//! `tm ls`). The session verbs `f`/`k`/`r`/`R`/`d`/`a` belong to the session
//! column; here they set a notice and act on nothing. `request_launch`,
//! `request_new_session` and `request_focus` are shared with the session
//! column.
//! Test: `events::tests` (`enter_on_owner_toggles_and_on_project_enters_sessions`,
//! `kill_resume_decommission_attach_on_bar_or_project_act_on_nothing`),
//! `two_column_tests` (`n_starts_a_new_session_in_the_project_from_either_column`).

use crossterm::event::{KeyCode, KeyEvent};

use super::PendingAction;
use crate::tui::project_ctl::state::{NodeId, ProjectCtlState};

/// Route one key while the tree pane has focus and no modal is open.
pub(super) fn handle_tree_key(state: &mut ProjectCtlState, key: KeyEvent) -> Option<PendingAction> {
    match key.code {
        KeyCode::Up => state.tree_step(false),
        KeyCode::Down => state.tree_step(true),
        KeyCode::Left => state.tree_left(),
        KeyCode::Right => state.tree_right(),
        KeyCode::Enter => enter(state),
        KeyCode::Char('g') => state.toggle_worker_indent(),
        KeyCode::Char('l') => return request_launch(state),
        // #10754: De1 key map (Bob 11-06)
        KeyCode::Char('n') => return request_new_session(state),
        // #10754: two-column MPM pane (Bob 11-06) — the session verbs act on
        // the right column only, so a key here never hits a session unseen.
        KeyCode::Char('f' | 'k' | 'r' | 'R' | 'd' | 'a') => {
            state.set_notice("select a session first")
        }
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
        Some(NodeId::Owner(_)) | None => {
            state.set_notice("no project selected");
            None
        }
    }
}

/// Enter: toggle an owner; on a project, enter the session column.
///
/// #10754: two-column MPM pane (Bob 11-06) — a project has no children left.
fn enter(state: &mut ProjectCtlState) {
    match state.selected_node() {
        Some(NodeId::Owner(_)) => state.toggle_selected(),
        Some(NodeId::Project { .. }) => state.focus_sessions(),
        None => {}
    }
}

/// `l`: launch a session in the selected project, from either column.
///
/// #10754: in the session column the target is the selected session's
/// project when the left column has an owner selected.
pub(super) fn request_launch(state: &mut ProjectCtlState) -> Option<PendingAction> {
    project_action(state, PendingAction::Launch)
}

/// `n`: start a new session in the selected project and attach, as `tm ls`.
///
/// #10754: De1 key map (Bob 11-06) — same target as [`request_launch`].
pub(super) fn request_new_session(state: &mut ProjectCtlState) -> Option<PendingAction> {
    project_action(state, PendingAction::NewSession)
}

/// The left column's project, else the selected session's project, as `build(name)`.
fn project_action(
    state: &mut ProjectCtlState,
    build: fn(String) -> PendingAction,
) -> Option<PendingAction> {
    let project = state
        .selected_project_name()
        .map(str::to_string)
        .or_else(|| {
            let id = state.selected_session()?.id.clone();
            state
                .sessions_by_project
                .iter()
                .find(|(_, rows)| rows.iter().any(|s| s.id == id))
                .map(|(name, _)| name.clone())
        });
    match project {
        Some(name) => Some(build(name)),
        None => {
            state.set_notice("no project selected");
            None
        }
    }
}

/// `f`: focus the selected session's existing tmux window or iTerm2 tab.
pub(super) fn request_focus(state: &mut ProjectCtlState) -> Option<PendingAction> {
    match state.selected_session() {
        Some(session) => Some(PendingAction::Focus(session.name.clone())),
        None => {
            state.set_notice("select a session first");
            None
        }
    }
}
