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
//!
//! **Deliverable/Milestone view (DOC-35 §10.8 `show`, #2383)**: `v` in the
//! Projects pane opens a read-only overlay for the selected project (see
//! `panes::deliverables_view`). Chosen because every other unshifted letter
//! is already bound in one pane or the other (`l`/`k`/`r`/`d`/`a` in
//! Sessions, `c` in Projects) or reserved by the confirm gate (`y`/`n`) —
//! `v` ("view") is free in every context and mnemonic for §10.8's read-only
//! `show`. While [`ProjectCtlState::deliverable_view`] is `Some`, EVERY key
//! routes through [`modal::handle_deliverable_view_key`] instead — the
//! identical "modal captures all input, `q`/`Ctrl-C` swallowed, `Esc` closes"
//! discipline [`modal::handle_confirm_key`] already establishes, plus `↑`/`↓`
//! to scroll and `v` itself as a second way to close (matching the dashboard
//! help overlay's "same key opens and closes" convention).
//!
//! **Config form (DOC-35 §6, #2120)**: `c` in the Projects pane opens a
//! fixed-field, tab-navigable config-edit form for the selected project (see
//! `state::ConfigFormView`, `panes::config_form`) — a THIRD modal, joining
//! the confirm gate and the Deliverable view. Opening is a pure state
//! mutation ([`ProjectCtlState::open_config_form`]), not a [`PendingAction`]
//! — unlike every other lettered verb, there is no async call to make just
//! to OPEN the form (the daemon round trip only happens on submit, see
//! [`PendingAction::SubmitConfig`]). While [`ProjectCtlState::config_form`]
//! is `Some`, EVERY key routes through [`modal::handle_config_form_key`]
//! instead, checked THIRD — after `pending_confirm` and `deliverable_view` —
//! in [`handle_key`]'s precedence chain. **Mutual exclusion, by
//! construction, not by an extra guard**: `c`'s match arm below is only ever
//! reached once both earlier `if state.X.is_some() { return ... }` checks
//! have fallen through, so the config form can never open while either
//! other modal is already showing; conversely, once it IS open, this
//! function returns before evaluating any other branch, so `v`/`k`/`d` etc.
//! can never open a second modal on top of it either.
//! What: [`PendingAction`] (the async actions a key can request — the actual
//! HTTP dispatch lives in [`super::actions`]), [`is_quit`], and [`handle_key`]
//! which routes a [`KeyEvent`] into [`ProjectCtlState`] mutations (focus
//! cycling, selection, drill-in, notice-clear, and the three modals) and
//! returns `Some(action)` for the lettered verbs (once confirmed, for
//! `k`-on-Active and `d`; immediately for the config form's submit key). The
//! three modals' own key handling lives in the sibling [`modal`] submodule
//! (pre-emptive 500-SLOC-cap split, #2120 — see that module's doc).
//! Test: `super::tests` covers focus cycling, selection movement, the
//! project-switch reset, every lettered action's valid/invalid-context
//! branches, the confirm-gate's request/confirm/cancel/ignore branches, and
//! the deliverable view's open/scroll/close branches, via synthetic
//! [`KeyEvent`]s; `tests::config_form_*` covers the third modal's
//! open/edit/submit/error/close branches.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::client::http_client::projects::PatchProjectArgs;

use super::state::{ConfirmKind, Pane, ProjectCtlState, SessionRow};

mod modal;
use modal::{handle_config_form_key, handle_confirm_key, handle_deliverable_view_key};

/// An action a key press requested that needs an async daemon round trip.
///
/// Why: [`handle_key`] must stay a pure, terminal-free, IO-free function to
/// remain unit-testable; the actual HTTP calls live in [`super::actions::dispatch`],
/// which the run loop awaits after `handle_key` returns.
/// What: one variant per lettered verb (DOC-35 §5 keybinding table) plus
/// [`PendingAction::SubmitConfig`] for the #2120 config form's submit
/// (opening the form is a pure state mutation — see the module doc — so
/// there is no `Config` variant here anymore). `Launch` and `SubmitConfig`
/// carry the target project name (`SubmitConfig` also carries the built
/// [`PatchProjectArgs`]); the rest carry the target session id.
/// Test: `handle_key_*` in `super::tests`.
#[derive(Debug, Clone)]
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
    /// The config form's submit key (DOC-35 §6, #2120) — PATCHes the built
    /// args for the named project; see [`super::actions::dispatch`] for the
    /// success/error handling (close-on-success, inline-error-on-failure).
    SubmitConfig(String, PatchProjectArgs),
}

/// Manual `PartialEq` (not derived): [`PatchProjectArgs`] — a contract type
/// owned by `client/http_client/projects.rs` (#2114/#2115, not modified by
/// #2120) — derives `Serialize` only, no `PartialEq`. Comparing
/// [`PendingAction::SubmitConfig`]'s two payloads via their serialized JSON
/// `Value` is a pragmatic structural-equality substitute that needs no
/// upstream change to the contract type; every other variant compares its
/// plain `String` payload directly.
impl PartialEq for PendingAction {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Launch(a), Self::Launch(b)) => a == b,
            (Self::Kill(a), Self::Kill(b)) => a == b,
            (Self::Resume(a), Self::Resume(b)) => a == b,
            (Self::Decommission(a), Self::Decommission(b)) => a == b,
            (Self::Attach(a), Self::Attach(b)) => a == b,
            (Self::SubmitConfig(name_a, args_a), Self::SubmitConfig(name_b, args_b)) => {
                name_a == name_b
                    && serde_json::to_value(args_a).ok() == serde_json::to_value(args_b).ok()
            }
            _ => false,
        }
    }
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
/// module doc); else when [`ProjectCtlState::deliverable_view`] is `Some`,
/// through [`handle_deliverable_view_key`]; else when
/// [`ProjectCtlState::config_form`] is `Some`, through
/// [`handle_config_form_key`] (#2120 — checked THIRD, see the module doc for
/// why this ordering makes the three modals mutually exclusive by
/// construction). Otherwise: Ctrl-C / `q` set `should_exit`. Tab/Shift+Tab
/// cycle pane focus. ↑/↓ move the selection in the focused pane
/// (Projects-pane movement also resets the Sessions-pane selection via
/// [`ProjectCtlState::on_project_selection_changed`]); the Activity pane has
/// no navigable rows, so movement there is a no-op. Enter on the Projects
/// pane drills into Sessions. `l`/`r`/`a` in the Sessions pane return the
/// matching [`PendingAction`] immediately when a target is selected, else set
/// an explanatory notice and return `None`. `c` in the Projects pane calls
/// [`ProjectCtlState::open_config_form`] directly (a pure state mutation, NOT
/// a [`PendingAction`] — see the module doc) when a project is selected, else
/// sets the same explanatory notice. `k`/`d` (see [`request_kill`] /
/// [`request_decommission`]) open the confirm gate instead of returning an
/// action directly, except a `k` on a non-Active session, which is not
/// destructive-pending and fires immediately. Esc clears any shown notice.
/// Test: `super::tests`.
pub fn handle_key(state: &mut ProjectCtlState, key: KeyEvent) -> Option<PendingAction> {
    if state.pending_confirm.is_some() {
        return handle_confirm_key(state, key);
    }

    if state.deliverable_view.is_some() {
        return handle_deliverable_view_key(state, key);
    }

    if state.config_form.is_some() {
        return handle_config_form_key(state, key);
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
                Some(name) => {
                    let name = name.to_string();
                    state.open_config_form(name);
                }
                None => state.set_notice("no project selected"),
            }
            None
        }
        KeyCode::Char('v') if state.focus == Pane::Projects => {
            match state.selected_project_name() {
                Some(name) => {
                    let name = name.to_string();
                    state.open_deliverable_view(name);
                }
                None => state.set_notice("no project selected"),
            }
            None
        }
        _ => None,
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

// Split into `events/tests.rs` (basename EXACTLY `tests.rs`, giving it the
// 1500-SLOC test-file cap rather than counting against this file's
// 500-SLOC production cap — pre-emptive split, #2120, mirroring this file's
// own `modal.rs` split).
#[cfg(test)]
mod tests;
