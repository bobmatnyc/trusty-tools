//! Dispatch of the `l` launch form's submit (#10754).
//!
//! See #10754 (owner ask ar-T5f, "tm ls launch").
//!
//! Why: a launch from the tui must take the same route as `tm sessions new
//! <checkout> --git-ref <ref> --task <task>`: the shared
//! [`TrustyCommand::ManagedNew`] intent run by [`CommandExecutor`], which
//! POSTs `/api/v1/sessions/managed`. DOC-35 §3.2 resolves the checkout and
//! default branch from the project's registry entry, so the operator types
//! only the task and, optionally, a ref.
//! What: [`launch_command`] builds the intent from a [`LaunchRequest`] and the
//! registry [`Project`]; [`run`] fetches the project, runs the intent and
//! folds the result into a notice (`launched <name>` plus a repoll, or the
//! error text). It never attaches: the new session shows in the session
//! column on the repoll.
//! Test: `launch_command_carries_project_task_and_ref`,
//! `launch_command_defaults_the_ref_to_the_projects_default_branch`,
//! `launch_command_refuses_a_project_with_no_local_checkout`,
//! `launch_reports_the_error_when_the_daemon_is_unreachable`.

use crate::client::{CommandExecutor, CommandResult, DaemonClient, TrustyCommand};
use crate::project::{Project, local_checkout_for};
use crate::runtime::RuntimeKind;

use super::drive::Screen;
use super::state::ProjectCtlState;
use super::state::launch::LaunchRequest;

/// Build the `ManagedNew` intent a submitted launch form asks for.
///
/// Why: see the module doc; a pure builder keeps the request checkable
/// without a daemon.
/// What: `repo_url` is the project's local checkout
/// ([`local_checkout_for`]); the ref is the form's, else the project's
/// `default_branch`. The remaining fields match `tm sessions new`'s defaults:
/// the `claude-code` runtime, task injection on, no deliverable, and
/// `force_new` so the daemon starts a fresh session. A `repo_url` that names
/// no local directory is an error naming the project.
/// Test: see the module doc.
pub(crate) fn launch_command(
    request: &LaunchRequest,
    project: &Project,
) -> Result<TrustyCommand, String> {
    let Some(checkout) = local_checkout_for(project) else {
        return Err(format!(
            "launch failed: project '{}' names no local checkout — re-register it with \
             the path of its checkout",
            request.project
        ));
    };
    Ok(TrustyCommand::ManagedNew {
        repo_url: checkout.display().to_string(),
        git_ref: request
            .git_ref
            .clone()
            .unwrap_or_else(|| project.default_branch.clone()),
        task: request.task.clone(),
        name_hint: None,
        runtime: Some(RuntimeKind::ClaudeCode.as_str().to_string()),
        inject_task: None,
        deliverable_id: None,
        force_new: true,
    })
}

/// Run one submitted launch form and show its outcome.
///
/// Why: the single seam [`super::actions::dispatch_on`] calls for
/// [`super::events::PendingAction::Launch`].
/// What: first sets `launching <project>…` and draws one frame on `screen`,
/// so the operator sees the launch start before any await. Then fetches the
/// registry project, builds [`launch_command`], and runs it on a
/// [`CommandExecutor`] over `client`. A spawned session sets
/// `launched <name>` and requests a repoll; any failure (registry fetch,
/// missing checkout, daemon refusal) sets its error text as the notice. Only
/// a failed draw is an `Err`.
/// Test: `launch_reports_the_error_when_the_daemon_is_unreachable`,
/// `launch_draws_the_launching_notice_before_the_spawn_is_sent`.
pub(crate) async fn run(
    state: &mut ProjectCtlState,
    client: &DaemonClient,
    request: LaunchRequest,
    screen: &mut dyn Screen,
) -> anyhow::Result<()> {
    // See #10754: the pending notice is on screen before the first await.
    state.set_notice(format!("launching {}…", request.project));
    screen.draw(state)?;
    let project = match client.registry_get_project(&request.project).await {
        Ok(project) => project,
        Err(e) => {
            state.set_notice(format!("launch failed: {e:#}"));
            return Ok(());
        }
    };
    let command = match launch_command(&request, &project) {
        Ok(command) => command,
        Err(msg) => {
            state.set_notice(msg);
            return Ok(());
        }
    };
    match CommandExecutor::from_daemon_client(client.clone())
        .execute(command)
        .await
    {
        CommandResult::ManagedSpawned { name, .. } => {
            state.set_notice(format!("launched {name}"));
            state.request_repoll();
        }
        CommandResult::Error(msg) => state.set_notice(format!("launch failed: {msg}")),
        other => state.set_notice(format!("launch: unexpected result {other:?}")),
    }
    Ok(())
}

#[cfg(test)]
#[path = "launch_ops_tests.rs"]
mod tests;
