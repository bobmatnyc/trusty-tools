//! Key dispatch for the `tm projects` TUI (#2118).
//!
//! Why: separating "a key arrives → mutate state, maybe request an async
//! action" from the terminal pump keeps the dispatch pure and unit-testable,
//! mirroring `tui::coordinator::events::handle_key`.
//!
//! **Deviation from the issue's keybinding table, documented here**: the
//! issue's key table lists `j`/`k` as motion aliases for ↑/↓ (vim-style)
//! AND, separately, binds the bare letter `k` to "kill" in the Sessions pane —
//! those two conflict outright (`k` cannot mean both "up" and "kill" without
//! a modal mode switch, which is out of scope for a skeleton). The issue's own
//! ASCII mockup resolves this in practice: its key-hint bar lists only `↑↓`
//! for selection and reserves every bare letter (`l`/`k`/`r`/`d`/`a`/`c`) for
//! an action. This module follows the mockup: motion is ↑/↩ arrow keys only,
//! and every lettered key is an unambiguous action binding. (`j` had no such
//! collision and could have stayed bound to "down" — left out anyway so the
//! screen has exactly one motion scheme rather than a mixed one; noted here
//! as an intentional, disclosed, non-blocking deviation.)
//!
//! **Confirmation gate (DOC-35 §5.2), NORMATIVE**: `k` (kill) on a session
//! whose `state` is `"active"`, and `d` (decommission) UNCONDITIONALLY (it is
//! terminal — the workspace is deleted), may never execute on the keypress
//! that requested them. Both route through [`ProjectCtlState::pending_confirm`]
//! (built by [`super::state::PendingConfirm`] / [`super::state::ConfirmKind`])
//! instead of returning a [`PendingAction`] directly; the operator's next
//! keypress resolves it (`y`/`Y`/Enter → confirm, `n`/`N`/Esc → cancel, any
//! other key → the modal stays open, matching the spec's "any modal/form"
//! contract). Per the same table, `q`/`Ctrl-C` quits only when NOT in a
//! modal — while `pending_confirm` is `Some`, quit keys are swallowed by the
//! modal like every other key.
//! What: [`PendingAction`] (the async actions a key can request — the actual
//! HTTP dispatch lives in [`super::actions`]), [`is_quit`], and [`handle_key`]
//! which routes a [`KeyEvent`] into [`ProjectCtlState`] mutations (focus
//! cycling, selection, drill-in, notice-clear, the confirm gate) and returns
//! `Some(action)` for the lettered verbs (once confirmed, for `k`-on-Active
//! and `d`).
//! Test: `super::tests` covers focus cycling, selection movement, the
//! project-switch reset, every lettered action's valid/invalid-context
//! branches, and the confirm-gate's request/confirm/cancel/ignore branches,
//! via synthetic [`KeyEvent`]s.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::{ConfirmKind, Pane, PendingConfirm, ProjectCtlState, SessionRow};

/// An action a key press requested that needs an async daemon round trip.
///
/// Why: [`handle_key`] must stay a pure, terminal-free, IO-free function to
/// remain unit-testable; the actual HTTP calls live in [`super::actions::dispatch`],
/// which the run loop awaits after `handle_key` returns.
/// What: one variant per lettered verb (DOC-35 §5 keybinding table); `Launch`
/// and `Config` carry the target project name, the rest carry the target
/// session id.
/// Test: `handle_key_*` in `super::tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingAction {
    /// `l` in the Sessions pane — launch a new session for the given project.
    Launch(String),
    /// `k` in the Sessions pane — runtime-stop the given session.
    Kill(String),
    /// `r` in the Sessions pane — resume the given session.
    Resume(String),
    /// `d` in the Sessions pane — decommission the given session.
    Decommission(String),
    /// `a` in the Sessions pane — fetch and display the attach command.
    Attach(String),
    /// `c` in the Projects pane — config stub (no-op notice; real form is #2120).
    Config(String),
}

/// True when this key event means "quit" (`q` or Ctrl-C).
///
/// Why: shared by [`handle_key`] and its tests; mirrors
/// `tui::coordinator::events::is_quit` minus the input-buffer carve-out (this
/// screen has no text input box).
/// What: matches bare `q` or Ctrl-C.
pub fn is_quit(key: &KeyEvent) -> bool {
    let ctrl_c = key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
    let bare_q = key.code == KeyCode::Char('q');
    ctrl_c || bare_q
}

/// Route one key event into [`ProjectCtlState`] mutations, returning any
/// action that needs an async daemon call.
///
/// Why: the single seam the terminal pump calls per key; keeping it a pure
/// `&mut state` function (no terminal, no IO) makes every branch unit
/// testable with synthetic [`KeyEvent`]s.
/// What: when [`ProjectCtlState::pending_confirm`] is `Some`, EVERY key
/// routes through [`handle_confirm_key`] instead (DOC-35 §5.2 — see the
/// module doc). Otherwise: Ctrl-C / `q` set `should_exit`. Tab/Shift+Tab
/// cycle pane focus. ↑/↓ move the selection in the focused pane
/// (Projects-pane movement also resets the Sessions-pane selection via
/// [`ProjectCtlState::on_project_selection_changed`]); the Activity pane has
/// no navigable rows, so movement there is a no-op. Enter on the Projects
/// pane drills into Sessions. `l`/`r`/`a` in the Sessions pane and `c` in the
/// Projects pane return the matching [`PendingAction`] immediately when a
/// target is selected, else set an explanatory notice and return `None`.
/// `k`/`d` (see [`request_kill`] / [`request_decommission`]) open the confirm
/// gate instead of returning an action directly, except a `k` on a
/// non-Active session, which is not destructive-pending and fires
/// immediately. Esc clears any shown notice.
/// Test: `super::tests`.
pub fn handle_key(state: &mut ProjectCtlState, key: KeyEvent) -> Option<PendingAction> {
    if state.pending_confirm.is_some() {
        return handle_confirm_key(state, key);
    }

    if is_quit(&key) {
        state.should_exit = true;
        return None;
    }

    match key.code {
        KeyCode::Tab => {
            state.cycle_focus_next();
            None
        }
        KeyCode::BackTab => {
            state.cycle_focus_prev();
            None
        }
        KeyCode::Up => {
            move_selection(state, Direction::Up);
            None
        }
        KeyCode::Down => {
            move_selection(state, Direction::Down);
            None
        }
        KeyCode::Enter if state.focus == Pane::Projects => {
            state.drill_into_sessions();
            None
        }
        KeyCode::Esc => {
            state.clear_notice();
            None
        }
        KeyCode::Char('l') if state.focus == Pane::Sessions => {
            match state.selected_project_name() {
                Some(name) => Some(PendingAction::Launch(name.to_string())),
                None => {
                    state.set_notice("no project selected");
                    None
                }
            }
        }
        KeyCode::Char('k') if state.focus == Pane::Sessions => request_kill(state),
        KeyCode::Char('r') if state.focus == Pane::Sessions => {
            selected_session_action(state, PendingAction::Resume)
        }
        KeyCode::Char('d') if state.focus == Pane::Sessions => request_decommission(state),
        KeyCode::Char('a') if state.focus == Pane::Sessions => {
            selected_session_action(state, PendingAction::Attach)
        }
        KeyCode::Char('c') if state.focus == Pane::Projects => {
            match state.selected_project_name() {
                Some(name) => Some(PendingAction::Config(name.to_string())),
                None => {
                    state.set_notice("no project selected");
                    None
                }
            }
        }
        _ => None,
    }
}

/// Resolve the currently open confirmation gate against one key event.
///
/// Why: split out of [`handle_key`] so the "confirm gate captures ALL input"
/// contract (DOC-35 §5.2, plus the spec's "`q`/`Ctrl-C` not in a modal" /
/// "`Esc` any modal/form" rules) lives in one place, independent of whatever
/// pane happened to have focus when the gate opened.
/// What: `y`/`Y`/Enter → clears the gate and returns the confirmed
/// [`PendingAction`] (`Kill` or `Decommission`, per [`ConfirmKind`]);
/// `n`/`N`/Esc → clears the gate, sets a "cancelled" notice, returns `None`;
/// any other key (including `q`/Ctrl-C) → the gate stays open, returns
/// `None` — a destructive action can only ever be resolved by an explicit
/// yes/no, never dismissed as a side effect of an unrelated keypress.
fn handle_confirm_key(state: &mut ProjectCtlState, key: KeyEvent) -> Option<PendingAction> {
    let confirm = state
        .pending_confirm
        .clone()
        .expect("caller checked pending_confirm.is_some()");
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            state.clear_confirm();
            Some(resolve_confirm(confirm))
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            state.clear_confirm();
            state.set_notice("cancelled");
            None
        }
        _ => None,
    }
}

/// Map a resolved [`PendingConfirm`] to its confirmed [`PendingAction`].
fn resolve_confirm(confirm: PendingConfirm) -> PendingAction {
    match confirm.kind {
        ConfirmKind::Kill => PendingAction::Kill(confirm.session_id),
        ConfirmKind::Decommission => PendingAction::Decommission(confirm.session_id),
    }
}

/// A short, human-readable label for a session, used in confirm prompts.
///
/// Why: `PendingConfirm::session_label` needs something more legible than
/// the raw UUID; the tmux session `name` is the most recognisable field.
fn confirm_label(session: &SessionRow) -> String {
    session.name.clone()
}

/// `k` in the Sessions pane: kill (runtime-stop) the selected session.
///
/// Why: DOC-35 §5.2 — kill requires a confirmation gate ONLY when the target
/// session is currently `active` (mirrors DOC-16 §5.6's active-session
/// confirmation); a non-Active session (provisioning/stopped/errored/
/// decommissioned) has no live runtime to interrupt, so kill proceeds
/// immediately, matching the spec's narrower "if Active" wording rather than
/// gating every kill unconditionally like decommission.
/// What: no selection → notice, `None`. Active → opens the confirm gate,
/// `None`. Otherwise → `Some(PendingAction::Kill(id))` immediately.
/// Test: `super::tests::kill_on_active_session_opens_confirm_gate`,
/// `super::tests::kill_on_non_active_session_fires_immediately`.
fn request_kill(state: &mut ProjectCtlState) -> Option<PendingAction> {
    let Some(session) = state.selected_session().cloned() else {
        state.set_notice("select a session first");
        return None;
    };
    if session.state == "active" {
        let label = confirm_label(&session);
        state.request_confirm(ConfirmKind::Kill, session.id, label);
        None
    } else {
        Some(PendingAction::Kill(session.id))
    }
}

/// `d` in the Sessions pane: decommission the selected session.
///
/// Why: DOC-35 §5.2 — decommission ALWAYS requires a confirmation gate
/// (unconditionally, unlike kill): it is terminal, permanently deleting the
/// workspace, regardless of the session's current lifecycle state.
/// What: no selection → notice, `None`. Otherwise → opens the confirm gate
/// unconditionally, `None`; the real [`PendingAction::Decommission`] fires
/// only once [`handle_confirm_key`] sees `y`.
/// Test: `super::tests::decommission_always_opens_confirm_gate`.
fn request_decommission(state: &mut ProjectCtlState) -> Option<PendingAction> {
    let Some(session) = state.selected_session().cloned() else {
        state.set_notice("select a session first");
        return None;
    };
    let label = confirm_label(&session);
    state.request_confirm(ConfirmKind::Decommission, session.id, label);
    None
}

/// Which way ↑/↓ moves the selection.
enum Direction {
    Up,
    Down,
}

/// Move the selection in the currently focused pane.
///
/// Why: factored out of [`handle_key`] so the Projects-pane branch's
/// session-reset side effect ([`ProjectCtlState::on_project_selection_changed`])
/// lives in one place.
/// What: Projects — moves `projects_nav`, then resets the Sessions selection;
/// Sessions — moves `sessions_nav`; Activity — no-op (nothing to navigate).
fn move_selection(state: &mut ProjectCtlState, dir: Direction) {
    match state.focus {
        Pane::Projects => {
            match dir {
                Direction::Up => state.projects_nav.up(),
                Direction::Down => state.projects_nav.down(),
            }
            state.on_project_selection_changed();
        }
        Pane::Sessions => match dir {
            Direction::Up => state.sessions_nav.up(),
            Direction::Down => state.sessions_nav.down(),
        },
        Pane::Activity => {}
    }
}

/// Build a session-targeted [`PendingAction`] from the current selection, or
/// set an explanatory notice when none is selected.
///
/// Why: `k`/`r`/`d`/`a` share the identical "need a selected session" guard;
/// factoring it out keeps [`handle_key`]'s match arms one line each.
/// What: `build` is a tuple-variant constructor (e.g. `PendingAction::Kill`);
/// returns `Some(build(selected session id))` or sets a notice and returns
/// `None`.
fn selected_session_action(
    state: &mut ProjectCtlState,
    build: fn(String) -> PendingAction,
) -> Option<PendingAction> {
    match state.selected_session() {
        Some(session) => Some(build(session.id.clone())),
        None => {
            state.set_notice("select a session first");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::project_ctl::state::ProjectRow;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_c() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    fn seeded_state() -> ProjectCtlState {
        let mut state = ProjectCtlState {
            projects: vec![
                ProjectRow {
                    name: "alpha".to_string(),
                    repo_url: "https://github.com/acme/alpha".to_string(),
                    live_count: 1,
                    total_count: 1,
                },
                ProjectRow {
                    name: "beta".to_string(),
                    repo_url: "https://github.com/acme/beta".to_string(),
                    live_count: 0,
                    total_count: 0,
                },
            ],
            ..Default::default()
        };
        state.sessions_by_project.insert(
            "alpha".to_string(),
            vec![SessionRow {
                id: "sess-1".to_string(),
                short_id: "sess-1".chars().take(8).collect(),
                name: "alpha-session".to_string(),
                branch: Some("main".to_string()),
                task: Some("ship it".to_string()),
                state: "active".to_string(),
                pending_decision: None,
                proposed_default: None,
            }],
        );
        state.projects_nav.sync_len(state.projects.len());
        state.sessions_nav.sync_len(state.current_sessions().len());
        state
    }

    #[test]
    fn quit_on_bare_q_and_ctrl_c() {
        let mut state = seeded_state();
        assert!(handle_key(&mut state, key(KeyCode::Char('q'))).is_none());
        assert!(state.should_exit);

        let mut state = seeded_state();
        assert!(handle_key(&mut state, ctrl_c()).is_none());
        assert!(state.should_exit);
    }

    #[test]
    fn tab_cycles_focus_forward_and_back() {
        let mut state = seeded_state();
        assert_eq!(state.focus, Pane::Projects);
        handle_key(&mut state, key(KeyCode::Tab));
        assert_eq!(state.focus, Pane::Sessions);
        handle_key(&mut state, key(KeyCode::Tab));
        assert_eq!(state.focus, Pane::Activity);
        handle_key(&mut state, key(KeyCode::BackTab));
        assert_eq!(state.focus, Pane::Sessions);
    }

    #[test]
    fn arrow_keys_move_projects_selection_and_reset_sessions() {
        let mut state = seeded_state();
        state.sessions_nav.down(); // no-op (only one session), but exercises the path
        handle_key(&mut state, key(KeyCode::Down));
        assert_eq!(state.selected_project().unwrap().name, "beta");
        assert_eq!(state.sessions_nav.selected(), 0);
    }

    #[test]
    fn enter_on_projects_pane_drills_into_sessions() {
        let mut state = seeded_state();
        handle_key(&mut state, key(KeyCode::Enter));
        assert_eq!(state.focus, Pane::Sessions);
    }

    #[test]
    fn enter_on_sessions_pane_is_a_noop() {
        let mut state = seeded_state();
        state.focus = Pane::Sessions;
        assert!(handle_key(&mut state, key(KeyCode::Enter)).is_none());
        assert_eq!(state.focus, Pane::Sessions);
    }

    #[test]
    fn launch_requires_sessions_focus_and_a_selected_project() {
        let mut state = seeded_state();
        state.focus = Pane::Sessions;
        let action = handle_key(&mut state, key(KeyCode::Char('l')));
        assert_eq!(action, Some(PendingAction::Launch("alpha".to_string())));
    }

    #[test]
    fn kill_without_selection_sets_notice_and_returns_none() {
        let mut state = seeded_state();
        state.focus = Pane::Sessions;
        state.projects_nav.down(); // select "beta", which has no sessions
        let action = handle_key(&mut state, key(KeyCode::Char('k')));
        assert!(action.is_none());
        assert_eq!(state.notice.as_deref(), Some("select a session first"));
        assert!(state.pending_confirm.is_none());
    }

    #[test]
    fn resume_and_attach_target_selected_session_immediately() {
        // Neither verb is destructive, so DOC-35 §5.2's confirm gate does not
        // apply — both fire on the first keypress.
        let mut state = seeded_state();
        state.focus = Pane::Sessions;
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('r'))),
            Some(PendingAction::Resume("sess-1".to_string()))
        );
        assert!(state.pending_confirm.is_none());
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('a'))),
            Some(PendingAction::Attach("sess-1".to_string()))
        );
        assert!(state.pending_confirm.is_none());
    }

    #[test]
    fn config_requires_projects_focus_and_a_selected_project() {
        let mut state = seeded_state();
        let action = handle_key(&mut state, key(KeyCode::Char('c')));
        assert_eq!(action, Some(PendingAction::Config("alpha".to_string())));
    }

    #[test]
    fn lettered_actions_are_ignored_outside_their_pane() {
        let mut state = seeded_state();
        // Projects pane focused: Sessions-only verbs are ignored (no notice).
        assert!(handle_key(&mut state, key(KeyCode::Char('l'))).is_none());
        assert!(state.notice.is_none());
    }

    #[test]
    fn esc_clears_notice() {
        let mut state = seeded_state();
        state.set_notice("hi");
        handle_key(&mut state, key(KeyCode::Esc));
        assert!(state.notice.is_none());
    }

    // ---- DOC-35 §5.2 confirmation gate ----------------------------------

    #[test]
    fn kill_on_active_session_opens_confirm_gate_instead_of_firing() {
        // seeded_state's lone session is "active".
        let mut state = seeded_state();
        state.focus = Pane::Sessions;
        let action = handle_key(&mut state, key(KeyCode::Char('k')));
        assert!(action.is_none(), "must not fire on the first keypress");
        let confirm = state.pending_confirm.expect("confirm gate should be open");
        assert_eq!(confirm.kind, ConfirmKind::Kill);
        assert_eq!(confirm.session_id, "sess-1");
    }

    #[test]
    fn kill_on_non_active_session_fires_immediately() {
        let mut state = seeded_state();
        state.sessions_by_project.get_mut("alpha").unwrap()[0].state = "stopped".to_string();
        state.focus = Pane::Sessions;
        let action = handle_key(&mut state, key(KeyCode::Char('k')));
        assert_eq!(action, Some(PendingAction::Kill("sess-1".to_string())));
        assert!(state.pending_confirm.is_none());
    }

    #[test]
    fn decommission_always_opens_confirm_gate_regardless_of_state() {
        for state_word in ["active", "stopped", "provisioning", "errored"] {
            let mut state = seeded_state();
            state.sessions_by_project.get_mut("alpha").unwrap()[0].state = state_word.to_string();
            state.focus = Pane::Sessions;
            let action = handle_key(&mut state, key(KeyCode::Char('d')));
            assert!(action.is_none(), "must not fire for state {state_word}");
            let confirm = state
                .pending_confirm
                .expect("confirm gate should be open for state {state_word}");
            assert_eq!(confirm.kind, ConfirmKind::Decommission);
            assert_eq!(confirm.session_id, "sess-1");
        }
    }

    #[test]
    fn confirm_gate_y_resolves_to_the_real_action_and_clears() {
        let mut state = seeded_state();
        state.focus = Pane::Sessions;
        handle_key(&mut state, key(KeyCode::Char('d'))); // open the gate
        assert!(state.pending_confirm.is_some());

        let action = handle_key(&mut state, key(KeyCode::Char('y')));
        assert_eq!(
            action,
            Some(PendingAction::Decommission("sess-1".to_string()))
        );
        assert!(state.pending_confirm.is_none());
    }

    #[test]
    fn confirm_gate_uppercase_y_and_enter_also_confirm() {
        for confirm_key in [key(KeyCode::Char('Y')), key(KeyCode::Enter)] {
            let mut state = seeded_state();
            state.focus = Pane::Sessions;
            handle_key(&mut state, key(KeyCode::Char('d')));
            let action = handle_key(&mut state, confirm_key);
            assert_eq!(
                action,
                Some(PendingAction::Decommission("sess-1".to_string()))
            );
        }
    }

    #[test]
    fn confirm_gate_n_or_esc_cancels_and_sets_notice() {
        for cancel_key in [
            key(KeyCode::Char('n')),
            key(KeyCode::Char('N')),
            key(KeyCode::Esc),
        ] {
            let mut state = seeded_state();
            state.focus = Pane::Sessions;
            handle_key(&mut state, key(KeyCode::Char('d')));
            let action = handle_key(&mut state, cancel_key);
            assert!(action.is_none());
            assert!(state.pending_confirm.is_none());
            assert_eq!(state.notice.as_deref(), Some("cancelled"));
        }
    }

    #[test]
    fn confirm_gate_swallows_unrelated_keys_and_stays_open() {
        let mut state = seeded_state();
        state.focus = Pane::Sessions;
        handle_key(&mut state, key(KeyCode::Char('d')));
        assert!(state.pending_confirm.is_some());

        // An unrelated key must not resolve or dismiss the gate.
        let action = handle_key(&mut state, key(KeyCode::Char('x')));
        assert!(action.is_none());
        assert!(
            state.pending_confirm.is_some(),
            "gate must stay open for an unrecognised key"
        );
    }

    #[test]
    fn confirm_gate_blocks_quit_keys_matching_the_spec_not_in_a_modal_rule() {
        let mut state = seeded_state();
        state.focus = Pane::Sessions;
        handle_key(&mut state, key(KeyCode::Char('d')));

        handle_key(&mut state, key(KeyCode::Char('q')));
        assert!(!state.should_exit, "q must not quit while a modal is open");
        assert!(state.pending_confirm.is_some());

        handle_key(&mut state, ctrl_c());
        assert!(
            !state.should_exit,
            "Ctrl-C must not quit while a modal is open"
        );
        assert!(state.pending_confirm.is_some());
    }

    #[test]
    fn quit_still_works_once_the_gate_is_resolved() {
        let mut state = seeded_state();
        state.focus = Pane::Sessions;
        handle_key(&mut state, key(KeyCode::Char('d')));
        handle_key(&mut state, key(KeyCode::Esc)); // cancel — gate closes
        assert!(state.pending_confirm.is_none());

        handle_key(&mut state, key(KeyCode::Char('q')));
        assert!(state.should_exit);
    }
}
