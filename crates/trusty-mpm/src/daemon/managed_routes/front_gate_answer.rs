//! The FRONT gate's answer: launch the withheld runtime (#1360, AC-15).
//!
//! Why: a session the FRONT gate escalated sits in `Provisioning` with its
//! runtime withheld until a human answers. The answer route needs the launch
//! as one call, and a test needs the same launch over a fake adapter and an
//! explicit launch-spec directory (#9843). Moved out of `lifecycle.rs`, which
//! is at its line-cap ceiling.
//! What: [`spawn_runtime_for`] builds the production adapter and calls
//! [`launch_withheld`], the testable body. [`front_gate_verdict`] reads the
//! answer text; [`deny_front_gate`] closes a denied decision without launch.
//! Test: `front_gate_answer_unblocks_spawn` in tests/session_manager_mvp.rs;
//! this file's `tests` module.

use std::path::Path;
use std::sync::Arc;

use tracing::warn;

use crate::daemon::state::DaemonState;
use crate::runtime::{RuntimeAdapter, build_adapter};
use crate::session_manager::decommission_force::ProvisioningDirt;
use crate::session_manager::{
    AnswerOutcome, DecisionId, ManagedSessionId, ManagedSessionState, SessionManager, SessionRecord,
};

/// Perform the withheld spawn (Step 3) for a FRONT-gate-escalated session.
///
/// Why: when the FRONT gate escalates (#1360), the spawn is WITHHELD and the
/// session sits in `Provisioning` with a `pending_decision`. After a human
/// resolves it via `POST …/answer`, the runtime must actually start (AC-15).
/// What: resolves the workspace and the `gh` env, builds the runtime adapter,
/// and runs [`launch_withheld`] against the production launch-spec root.
/// Callers invoke this only for a session still in `Provisioning`.
/// Test: `front_gate_answer_unblocks_spawn` in tests/session_manager_mvp.rs.
pub async fn spawn_runtime_for(
    state: &Arc<DaemonState>,
    record: &SessionRecord,
) -> Result<(), String> {
    let mgr = state.session_manager().await;
    let adapter = build_adapter(
        record.runtime,
        mgr.tmux_driver(),
        None,
        state.framework_root(),
    );
    let gh_env = super::lifecycle::resolve_gh_env(state, &launch_workspace(record)).await;
    let spec_dir = crate::runtime::launch_spec::LaunchSpec::root();
    launch_withheld(&mgr, record, adapter.as_ref(), &gh_env, spec_dir.as_deref()).await
}

/// The directory the withheld runtime starts in.
fn launch_workspace(record: &SessionRecord) -> std::path::PathBuf {
    record
        .workspace_path
        .clone()
        .unwrap_or_else(|| record.cwd.clone())
}

/// Launch `record`'s withheld runtime through `adapter` (#9843 seam).
///
/// Why: the launch must be testable with a counting adapter and a driver
/// whose runtime never comes up, without the production launch-spec root.
/// What: marks the record `Active` with its workspace, spawns the runtime
/// (marking the record errored on a spawn error), then runs the post-send
/// launch check against `spec_dir`. A failed state write, a spawn error, or a
/// runtime the check saw never come up is `Err`, which the answer route
/// settles as `DeliveryFailed`.
/// Test: `spawn_runtime_errs_when_the_runtime_never_came_up`,
/// `spawn_runtime_refuses_to_launch_when_the_state_write_fails`,
/// `front_gate_runtime_that_never_started_settles_delivery_failed`.
pub(crate) async fn launch_withheld(
    mgr: &SessionManager,
    record: &SessionRecord,
    adapter: &dyn RuntimeAdapter,
    gh_env: &[(String, String)],
    spec_dir: Option<&Path>,
) -> Result<(), String> {
    let workspace = launch_workspace(record);
    if let Err(e) = mgr
        .set_workspace(&record.id, workspace.clone(), ManagedSessionState::Active)
        .await
    {
        // #9843 (FO-2): never launch behind a record that still reads
        // `Provisioning`.
        warn!(id = %record.id, "spawn_runtime_for: set_workspace failed: {e}");
        return Err(format!("the session could not be marked active: {e}"));
    }

    if let Err(e) = adapter.spawn(
        &record.tmux_name,
        &workspace,
        &record.task,
        &record.id.to_string(),
        gh_env,
    ) {
        warn!(
            id = %record.id,
            name = %record.tmux_name,
            "spawn_runtime_for: runtime adapter spawn failed: {e}"
        );
        let _ = mgr
            .mark_errored(&record.id, &format!("spawn failed: {e}"))
            .await;
        return Err(e.to_string());
    }
    // #8233: an accepted keystroke is not a started runtime.
    let tmux = mgr.tmux_driver();
    // #9843: a runtime that never came up is a failed launch, not a delivered one.
    match super::launch_verify::record_spawn_outcome_in(mgr, tmux.as_ref(), record, spec_dir).await
    {
        Some(msg) => Err(msg),
        None => Ok(()),
    }
}

/// What a front-gate answer's text asks for (#9843).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrontGateVerdict {
    /// Launch the withheld runtime.
    Approve,
    /// Close the decision and never launch.
    Deny,
    /// Neither; refused before the claim.
    Unclear,
}

/// Answers that deny a front-gate launch, after [`fold`].
const DENY_WORDS: [&str; 7] = ["no", "n", "deny", "denied", "reject", "cancel", "abort"];

/// Answers that approve a front-gate launch, after [`fold`].
const APPROVE_WORDS: [&str; 6] = ["yes", "y", "approve", "approved", "proceed", "ok"];

/// Trim, drop one leading `/` (a typed `/deny`), and lowercase.
fn fold(answer: &str) -> String {
    let trimmed = answer.trim();
    trimmed
        .strip_prefix('/')
        .unwrap_or(trimmed)
        .to_ascii_lowercase()
}

/// Classify a front-gate answer (#9843, Q2).
///
/// Why: every transport (`/deny`, `tm session answer`, the TUI) carries only
/// the answer text, and the front gate launched on any text, so "no" ran a
/// task the gate had escalated.
/// What: a deny word first, so it wins even over a proposed default of "no";
/// then an approve word or the exact proposed default; anything else is
/// [`FrontGateVerdict::Unclear`], which never launches.
/// Test: `front_gate_verdict_matrix`.
pub(crate) fn front_gate_verdict(answer: &str, proposed_default: Option<&str>) -> FrontGateVerdict {
    let folded = fold(answer);
    if DENY_WORDS.contains(&folded.as_str()) {
        FrontGateVerdict::Deny
    } else if APPROVE_WORDS.contains(&folded.as_str())
        || proposed_default.is_some_and(|d| !d.trim().is_empty() && d.trim() == answer.trim())
    {
        FrontGateVerdict::Approve
    } else {
        FrontGateVerdict::Unclear
    }
}

/// The 422 text for front-gate answer text that is neither approve nor deny.
pub(crate) fn unclear_front_gate_answer(id: &ManagedSessionId) -> String {
    format!(
        "session {id} is waiting at its front gate; answer {} or the proposed default to \
         launch it, or {} to close it without launch. Nothing was changed and the decision \
         is still pending",
        APPROVE_WORDS.join("/"),
        DENY_WORDS.join("/")
    )
}

/// Close a claimed front-gate decision without launching (#9843, Q1, Q3).
///
/// Why: a denied task must never run, and `Stopped`/`Errored` are both
/// resumable, so the session ends `Decommissioned`.
/// What: settles the claim `Denied` first, so a crash or a failed teardown
/// still leaves a record resume refuses, then decommissions with
/// [`ProvisioningDirt::Refuse`]. A teardown error is `Err` (the route's 500);
/// the decision stays closed and nothing launches.
/// Test: `front_gate_deny_closes_the_decision_without_launch`,
/// `front_gate_deny_teardown_failure_is_500_and_stays_unlaunchable`.
pub(crate) async fn deny_front_gate(
    mgr: &SessionManager,
    id: &ManagedSessionId,
    decision_id: Option<DecisionId>,
) -> Result<Option<DecisionId>, String> {
    mgr.settle_answer(id, decision_id, AnswerOutcome::Denied)
        .await;
    let report = mgr
        .decommission_reporting(id, None, ProvisioningDirt::Refuse)
        .await
        .map_err(|e| {
            format!(
                "the front-gate launch of session {id} was denied and will not run, but \
                 its teardown failed: {e}; decommission it once that is addressed"
            )
        })?;
    if let Some(why) = report.workspace_kept_reason {
        warn!(id = %id, "front gate denied; workspace kept: {why}");
    }
    Ok(decision_id)
}

#[cfg(test)]
#[path = "front_gate_answer_tests.rs"]
pub(crate) mod tests;
