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
//! scope here (mirrors why `c`/config is also a stub; a real launch form is a
//! natural follow-up alongside #2120's config form). Rather than silently
//! doing nothing, `l` shows a notice pointing at the existing
//! `tm sessions new` CLI verb, which already does the real work.
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
/// What: `Launch`/`Config` are stubs — see the module doc — that set an
/// explanatory notice with no daemon call. `Kill`/`Resume`/`Decommission`
/// call the matching mutating endpoint and, on success, request an immediate
/// re-poll ([`ProjectCtlState::request_repoll`]) so the fleet reflects the
/// change now. `Attach` fetches and displays the tmux attach command
/// (read-only — no repoll).
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
        PendingAction::Config(project) => {
            state.set_notice(format!(
                "config for '{project}' is not available yet (#2120)"
            ));
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
