//! Async action dispatch for the `tm projects` TUI (#2118).
//!
//! Why: [`super::events::handle_key`] stays pure (no IO) and returns a
//! [`PendingAction`] describing what the operator asked for; this module is
//! the one place that awaits the matching [`DaemonClient`] call and folds the
//! result into a notice, mirroring `tui::coordinator`'s
//! `dispatch_submitted_line` split between pure routing and async dispatch.
//!
//! **Launch is a stub, by design** (documented per the issue's request):
//! spawning a managed session requires a `task` description and a `git_ref`
//! (see [`crate::client::ManagedSpawnRequest`]) that a 4-pane skeleton has no
//! text-entry surface to collect — building that form is explicitly out of
//! scope here. Rather than silently doing nothing, `l` shows a notice
//! pointing at the existing `tm sessions new` CLI verb, which already does
//! the real work. Config (`c`) is NO LONGER a stub — #2120 replaced it with a
//! real fixed-field form; see [`PendingAction::SubmitConfig`]'s handling
//! below.
//!
//! **Decommission has no confirmation step**, matching the issue's keybinding
//! table verbatim (`d` → decommission, no modal). This is a permanent,
//! destructive action (the workspace is deleted); a confirmation modal would
//! be a reasonable fast-follow but is out of scope for this skeleton.
//!
//! What: [`dispatch`] matches a [`PendingAction`] to its `DaemonClient` call
//! and calls [`ProjectCtlState::set_notice`] with the outcome, requesting an
//! immediate re-poll after any action that changed the fleet.
//! Test: the pure notice text is not independently unit-tested (it is a
//! terminal string, not branch logic); the routing itself is covered by
//! `events::tests` (which `PendingAction` a key produces). The live HTTP calls
//! are exercised manually / by `tests/session_manager_mvp.rs`'s coverage of
//! the underlying `DaemonClient` methods.

use crate::client::DaemonClient;

use super::events::PendingAction;
use super::state::ProjectCtlState;

/// Route one [`PendingAction`] to its `DaemonClient` call and update `state`.
///
/// Why: the single async seam the run loop calls after `handle_key` returns
/// `Some(action)`.
/// What: `Launch` is a stub — see the module doc — that sets an explanatory
/// notice with no daemon call. `Kill`/`Resume`/`Decommission` call the
/// matching mutating endpoint and, on success, request an immediate re-poll
/// ([`ProjectCtlState::request_repoll`]) so the fleet reflects the change
/// now. `Attach` fetches and displays the tmux attach command (read-only —
/// no repoll). `SubmitConfig` (DOC-35 §6, #2120) PATCHes the config form's
/// built args: on success, CLOSES the form (`close_config_form`), sets a
/// success notice, and requests a repoll (matching the other mutating
/// verbs); on failure, the form STAYS OPEN and the error renders INLINE in
/// it (`set_config_form_error`) rather than as a transient notice — the
/// explicit #2120 requirement that a rejected submit never discards the
/// operator's other unsaved edits.
pub(crate) async fn dispatch(
    state: &mut ProjectCtlState,
    client: &DaemonClient,
    action: PendingAction,
) {
    match action {
        PendingAction::Launch(project) => {
            state.set_notice(format!(
                "launch form not built yet — run `tm sessions new --repo-url <url> --task <task>` for '{project}' (see actions.rs doc)"
            ));
        }
        PendingAction::Kill(id) => {
            let result = client.runtime_stop_managed_session(&id).await;
            apply_mutation_result(state, result, "killed");
        }
        PendingAction::Resume(id) => {
            let result = client.resume_managed_session(&id).await;
            apply_mutation_result(state, result, "resumed");
        }
        PendingAction::Decommission(id) => {
            let result = client.decommission_managed_session(&id).await;
            apply_mutation_result(state, result, "decommissioned");
        }
        PendingAction::Attach(id) => match client.managed_session_attach_cmd(&id).await {
            Ok(resp) => state.set_notice(format!("attach: {}", resp.attach_cmd)),
            Err(e) => state.set_notice(format!("attach failed: {e}")),
        },
        PendingAction::SubmitConfig(name, args) => {
            match client.registry_patch_project(&name, &args).await {
                Ok(project) => {
                    state.close_config_form();
                    state.set_notice(format!("updated config for '{}'", project.name));
                    state.request_repoll();
                }
                Err(e) => state.set_config_form_error(format!("{e}")),
            }
        }
    }
}

/// Fold a mutating endpoint's result into a notice, requesting a repoll on success.
///
/// Why: `Kill`/`Resume`/`Decommission` share the identical
/// result-to-notice-plus-repoll shape; factoring it out keeps [`dispatch`]'s
/// match arms one line each.
/// What: on `Ok`, sets a `"{verb} {name} — now {state}"` notice and calls
/// [`ProjectCtlState::request_repoll`]; on `Err`, sets a `"{verb} failed: {e}"`
/// notice with no repoll.
fn apply_mutation_result(
    state: &mut ProjectCtlState,
    result: anyhow::Result<crate::client::ManagedSessionSummary>,
    verb: &str,
) {
    match result {
        Ok(summary) => {
            state.set_notice(format!("{verb} {} — now {}", summary.name, summary.state));
            state.request_repoll();
        }
        Err(e) => state.set_notice(format!("{verb} failed: {e}")),
    }
}
