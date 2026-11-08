//! Keys for the tree (left) column (#10476).
//!
//! Why: the tree replaced the Projects pane (Architect Q9). Kept apart from
//! `events/mod.rs`, which owns the modal precedence chain.
//! What: [`handle_tree_key`] — Up/Down move; Left collapses an owner or
//! climbs to it; Right expands an owner or steps into its first project;
//! Enter toggles an owner. #10754: two-column MPM pane (Bob 11-06) — Right
//! or Enter on a project moves focus into the session column. `g` toggles
//! worker indentation. `c`/`v` act on a project node, `n` and `l` on its
//! project (#10754: De1 key map (Bob 11-06) — `n` starts a new session, as in
//! `tm ls`; `l` opens the launch form, see #10754). The session verbs
//! `f`/`k`/`r`/`R`/`d`/`a` belong to the session column; here they set a
//! notice and act on nothing. `request_new_session`, `open_launch` and
//! `request_focus` are shared with the session column.
//! Test: `events::tests` (`enter_on_owner_toggles_and_on_project_enters_sessions`,
//! `kill_resume_decommission_attach_on_bar_or_project_act_on_nothing`),
//! `two_column_tests` (`n_starts_a_new_session_in_the_project_from_either_column`).

use crossterm::event::{KeyCode, KeyEvent};

use super::PendingAction;
use crate::tui::project_ctl::state::launch::LaunchModal;
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
        // #10754: De1 key map (Bob 11-06)
        KeyCode::Char('n') => return request_new_session(state),
        // See #10754: `l` opens the launch form for the project.
        KeyCode::Char('l') => open_launch(state),
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

/// `n`: start a new session in the selected project and attach, as `tm ls`.
///
/// What: the target is [`target_project`]'s; with none, a notice.
/// Test: `n_starts_a_new_session_in_the_project_from_either_column`,
/// `n_in_an_empty_session_column_starts_a_new_session_and_l_opens_the_form`.
pub(super) fn request_new_session(state: &mut ProjectCtlState) -> Option<PendingAction> {
    match target_project(state) {
        Some(name) => Some(PendingAction::NewSession(name)),
        None => {
            state.set_notice("no project selected");
            None
        }
    }
}

/// `l`: open the launch form on the selected project.
///
/// Why: DOC-35 §5.2 binds `l` to `launch` on a project or session node; the
/// form collects the task and ref the launch call needs. See #10754.
/// What: opens [`LaunchModal`] on [`target_project`]'s project; with none
/// (an owner bar with no session selected), a notice and no form.
/// Test: `l_opens_the_launch_form_for_the_selected_nodes_project`,
/// `l_opens_the_launch_form_and_enter_submits_the_request`.
pub(super) fn open_launch(state: &mut ProjectCtlState) {
    match target_project(state) {
        Some(name) => state.launch = Some(LaunchModal::open(name)),
        None => state.set_notice("no project selected"),
    }
}

/// The project `n` and `l` act on.
///
/// What: the left column's project, else the selected session's project (an
/// owner selected on the left). #10754: an empty session column falls back
/// to the tree's project too.
fn target_project(state: &ProjectCtlState) -> Option<String> {
    state
        .selected_project_name()
        .map(str::to_string)
        .or_else(|| {
            let id = state.selected_session()?.id.clone();
            state
                .sessions_by_project
                .iter()
                .find(|(_, rows)| rows.iter().any(|s| s.id == id))
                .map(|(name, _)| name.clone())
        })
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
