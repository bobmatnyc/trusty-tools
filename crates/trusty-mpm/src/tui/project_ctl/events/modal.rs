//! Modal key handlers for the `tm projects` TUI (#2118/#2383/#2120).
//!
//! Why: hoisted out of `events/mod.rs` pre-emptively (per CLAUDE.md's
//! SLOC-cap convention: "one public module per logical concept, a thin
//! `mod.rs` that re-exports, and sibling files with clear single
//! responsibilities") so adding the #2120 config-form key handler does not
//! push `events/mod.rs` over the 500-SLOC production cap. Each of this
//! screen's three modals (confirm gate, Deliverable/Milestone view, config
//! form) shares the identical "captures ALL input while open" discipline —
//! grouping their handlers here keeps that shared shape visible in one file
//! separate from the outer per-pane dispatch [`super::handle_key`] owns.
//! What: [`handle_confirm_key`] (DOC-35 §5.2), [`handle_deliverable_view_key`]
//! (DOC-35 §10.8, #2383), and [`handle_config_form_key`] (DOC-35 §6, #2120) —
//! each called from [`super::handle_key`] once its matching `Option` field on
//! [`ProjectCtlState`] is confirmed `Some`.
//! Test: `super::tests` (in `events/mod.rs`) exercises all three exclusively
//! through the public [`super::handle_key`] entry point, matching how the
//! real run loop calls it — there is no separate test module here, since
//! these functions have no meaningful behavior independent of that dispatch.

use crossterm::event::{KeyCode, KeyEvent};

use super::PendingAction;
use crate::tui::project_ctl::state::{ConfirmKind, PendingConfirm, ProjectCtlState};

/// Resolve the currently open confirmation gate against one key event.
///
/// Why: split out of [`super::handle_key`] so the "confirm gate captures ALL
/// input" contract (DOC-35 §5.2, plus the spec's "`q`/`Ctrl-C` not in a
/// modal" / "`Esc` any modal/form" rules) lives in one place, independent of
/// whatever pane happened to have focus when the gate opened.
/// What: `y`/`Y`/Enter → clears the gate and returns the confirmed
/// [`PendingAction`] (`Kill` or `Decommission`, per [`ConfirmKind`]);
/// `n`/`N`/Esc → clears the gate, sets a "cancelled" notice, returns `None`;
/// any other key (including `q`/Ctrl-C) → the gate stays open, returns
/// `None` — a destructive action can only ever be resolved by an explicit
/// yes/no, never dismissed as a side effect of an unrelated keypress.
pub(super) fn handle_confirm_key(
    state: &mut ProjectCtlState,
    key: KeyEvent,
) -> Option<PendingAction> {
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

/// Route one key event while the Deliverable/Milestone view is open (DOC-35
/// §10.8, #2383).
///
/// Why: split out of [`super::handle_key`] for the same reason
/// [`handle_confirm_key`] is — the "this modal captures ALL input" contract
/// lives in one place. Unlike the confirm gate (which resolves to an action
/// on `y`/`n`), this view is read-only, so every branch returns `None` — it
/// never produces a [`PendingAction`].
/// What: `↑`/`↓` scroll the body by one line; `Esc` or `v` closes the view;
/// every other key (including `q`/`Ctrl-C`) is swallowed, matching the
/// confirm gate's "not in a modal" quit rule.
/// Test: `super::tests::deliverable_view_*` / `super::tests::v_*`.
pub(super) fn handle_deliverable_view_key(
    state: &mut ProjectCtlState,
    key: KeyEvent,
) -> Option<PendingAction> {
    match key.code {
        KeyCode::Up => state.scroll_deliverable_view(-1),
        KeyCode::Down => state.scroll_deliverable_view(1),
        KeyCode::Esc | KeyCode::Char('v') => state.close_deliverable_view(),
        _ => {}
    }
    None
}

/// Route one key event while the config form is open (DOC-35 §6, #2120).
///
/// Why: split out of [`super::handle_key`] for the same "modal captures ALL
/// input" reason as the other two handlers in this module — but UNLIKE
/// them, this modal is editable, so most keys mutate a field buffer rather
/// than being swallowed outright.
/// What: `Esc` closes WITHOUT submitting (discards unsaved edits — matches
/// every other modal's Esc convention). `Tab`/`Shift+Tab` cycle the focused
/// field (scoped to the form — the outer pane-focus Tab is unreachable here,
/// see the module doc on [`super`]). `Backspace` removes the last character
/// of the focused field's buffer. `Enter` submits (see
/// [`submit_config_form`] — chosen because Enter has no other meaning while
/// this modal is open: fields are single-line buffers navigated via Tab, not
/// newline-accepting, so there is no ambiguity with "insert a newline").
/// Every OTHER key, including a bare `q`/`Ctrl-C` that would otherwise quit,
/// is treated as a typed character and appended to the focused field's
/// buffer — deliberately, matching "modal captures ALL input": an operator
/// setting `gh_user` to a value containing 'q' must not have that keystroke
/// misinterpreted as quit.
/// Test: `super::tests::config_form_*`.
pub(super) fn handle_config_form_key(
    state: &mut ProjectCtlState,
    key: KeyEvent,
) -> Option<PendingAction> {
    match key.code {
        KeyCode::Esc => {
            state.close_config_form();
            None
        }
        KeyCode::Tab => {
            state.config_form_focus_next();
            None
        }
        KeyCode::BackTab => {
            state.config_form_focus_prev();
            None
        }
        KeyCode::Backspace => {
            state.config_form_backspace();
            None
        }
        KeyCode::Enter => submit_config_form(state),
        KeyCode::Char(c) => {
            state.config_form_push_char(c);
            None
        }
        _ => None,
    }
}

/// Build the [`PendingAction::SubmitConfig`] for the open config form's
/// current edits, or reject the submit inline when nothing changed.
///
/// Why: the single seam [`handle_config_form_key`]'s `Enter` arm calls;
/// keeping the "diff, then either error-inline or return an action" logic
/// here (rather than inline in the match arm) keeps that arm one line, like
/// every other [`super::handle_key`] verb.
/// What: `None` config form (caller error — [`handle_config_form_key`] only
/// calls this when `state.config_form.is_some()`) or an unedited form (empty
/// diff) → [`ProjectCtlState::set_config_form_error`], returns `None` (stays
/// open). An edited form → [`crate::project_config::merge_patch_args`] over
/// the diff, returns `Some(PendingAction::SubmitConfig)`; the form stays
/// open until [`super::super::actions::dispatch`] resolves it (closes on
/// success, sets the inline error on failure — never closed here).
fn submit_config_form(state: &mut ProjectCtlState) -> Option<PendingAction> {
    let form = state.config_form.as_ref()?;
    let edits = form.diff_edits();
    if edits.is_empty() {
        state.set_config_form_error("no changes to submit");
        return None;
    }
    let args = crate::project_config::merge_patch_args(&edits);
    let name = form.project_name.clone();
    Some(PendingAction::SubmitConfig(name, args))
}
