//! The FRONT gate's answer: launch the withheld runtime (#1360, AC-15).
//!
//! Why: a session the FRONT gate escalated sits in `Provisioning` with its
//! runtime withheld until a human answers. The answer route needs the launch
//! as one call, and a test needs the same launch over a fake adapter and an
//! explicit launch-spec directory (#9843). Moved out of `lifecycle.rs`, which
//! is at its line-cap ceiling.
//! What: [`spawn_runtime_for`] builds the production adapter and calls
//! [`launch_withheld`], the testable body.
//! Test: `front_gate_answer_unblocks_spawn` in tests/session_manager_mvp.rs;
//! this file's `tests` module.

use std::path::Path;
use std::sync::Arc;

use tracing::warn;

use crate::daemon::state::DaemonState;
use crate::runtime::{RuntimeAdapter, build_adapter};
use crate::session_manager::{ManagedSessionState, SessionManager, SessionRecord};

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
/// launch check against `spec_dir`.
/// Test: `spawn_runtime_errs_when_the_runtime_never_came_up`.
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
        warn!(id = %record.id, "spawn_runtime_for: set_workspace failed: {e}");
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
    super::launch_verify::record_spawn_outcome_in(mgr, tmux.as_ref(), record, spec_dir).await;
    Ok(())
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

/// Classify a front-gate answer (#9843 seam: the pre-fix behaviour, under
/// which the text is never read and every answer launches).
pub(crate) fn front_gate_verdict(answer: &str, proposed_default: Option<&str>) -> FrontGateVerdict {
    let _ = (answer, proposed_default);
    FrontGateVerdict::Approve
}
