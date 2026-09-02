//! In-project session preparation and the resume-path compiled-prompt refresh.
//!
//! Why: split out of `lifecycle.rs` (#4752), which sits at its frozen SLOC
//! budget. Both items here answer one question — "what must be on disk before
//! this session's process starts?" — so they belong together rather than
//! padding the lifecycle state machine.
//! What: [`prepare_inproject_session`] (the #1913 preparation call the
//! in-project spawn paths make directly) and
//! [`refresh_resume_compiled_prompt`] (the #4752 fatal compiled-prompt
//! write the resume path needs because it never runs `prepare_session*`).
//! Test: `prepare_inproject_session_writes_statusline`,
//! `prepare_inproject_session_emits_stage_events_in_order`,
//! `refresh_resume_compiled_prompt_writes_the_project_local_file`,
//! `refresh_resume_compiled_prompt_reports_an_actionable_failure`.

use tracing::{info, warn};

use crate::session_manager::ManagedSessionId;

/// Run the session-preparation pipeline for an in-project worktree, logging
/// (never propagating) any failure.
///
/// Why (#1913): [`spawn_managed_inproject`] has no clone step to wrap this call
/// in — unlike `spawn_managed`'s clone branch and `spawn_managed_local`, which
/// both get preparation "for free" as part of `WorkspaceProvisioner::provision_in`
/// — so it must invoke [`crate::core::session_launch::prepare_session_with_repo_url`]
/// directly. Extracted to a named, `fw`-parameterised function (rather than
/// inlined) so it is unit-testable against a hermetic [`crate::core::paths::FrameworkPaths::under`]
/// tempdir without touching the operator's real `~/.trusty-mpm`/`~/.claude`.
/// What: calls `prepare_session_for_managed(fw, worktree, Some(repo_url),
/// session_id)` — #4832 threads the id so the compiled prompt lands in this
/// session's own directory rather than the unmanaged `local` bucket.
/// On success, logs the deployed-agent count AND the deployed-skill count
/// (`report.skill_deploy.deployed.len()` — #1917; previously only the agent
/// count was logged, so a skill-deploy no-op was invisible here too). On
/// failure, logs a `tracing::warn!` and returns — mirroring
/// `WorkspaceProvisioner::provision_in`'s non-fatal handling of the identical
/// call, so a prep failure never blocks the session from spawning.
/// Test: `prepare_inproject_session_writes_statusline` in this module's `tests`
/// submodule.
pub(super) fn prepare_inproject_session(
    fw: &crate::core::paths::FrameworkPaths,
    session_id: &ManagedSessionId,
    worktree: &std::path::Path,
    repo_url: &str,
) -> Result<(), String> {
    match crate::core::session_launch::prepare_session_for_managed(
        fw,
        worktree,
        Some(repo_url),
        &session_id.to_string(),
    ) {
        Ok(report) => {
            info!(
                id = %session_id,
                deployed = report.deploy.deployed.len(),
                skills_deployed = report.skill_deploy.deployed.len(),
                worktree = %worktree.display(),
                "spawn_managed (inproject): session prepared"
            );
            // Issue #2149: a roster-deploy failure no longer aborts
            // preparation, so surface it loudly here rather than letting it
            // hide behind a low `deployed`/`skills_deployed` count.
            // #6649 folded the asset-hygiene lines in beside them, through the
            // one shared reporter.
            crate::core::session_launch::log_prep_findings(
                &report.roster_errors,
                &report.asset_notices,
                format_args!(
                    "spawn_managed (inproject) session {session_id} at {}",
                    worktree.display()
                ),
            );
        }
        // #4752: a compiled-prompt write failure refuses the spawn — the caller
        // propagates this string. Every other prep failure stays non-fatal
        // (#2149) and only warns.
        Err(e) if e.is_fatal() => {
            warn!(
                id = %session_id,
                worktree = %worktree.display(),
                "spawn_managed (inproject): refusing to spawn: {e}"
            );
            return Err(e.to_string());
        }
        Err(e) => {
            warn!(
                id = %session_id,
                worktree = %worktree.display(),
                "spawn_managed (inproject): session prep failed (non-fatal): {e}"
            );
        }
    }
    Ok(())
}

/// Refresh the project's compiled PM prompt on the RESUME path.
///
/// Why (#4752): `resume_managed` never calls `prepare_session*` on its healthy
/// path (`resume_self_heal` → `ensure_status_line` → `ensure_deployment_complete`
/// → `spawn_resume`), so without this a resumed session would run against
/// whatever the last start wrote — or nothing at all. Composing through the
/// SAME seam the spawn uses means the bytes written here are the bytes
/// `spawn_resume` is about to pass to `--append-system-prompt-file`.
/// What: composes the project-resolved prompt, writes it to
/// [`crate::core::instruction_pipeline::compiled_prompt_path`], and on failure
/// returns the operator-facing message from
/// [`crate::core::instruction_pipeline::instructions_failure_message`] —
/// the caller refuses the resume with it.
/// Test: `refresh_resume_compiled_prompt_writes_the_project_local_file`,
/// `refresh_resume_compiled_prompt_reports_an_actionable_failure`.
pub(super) fn refresh_resume_compiled_prompt(
    workspace: &std::path::Path,
    session_id: &ManagedSessionId,
) -> Result<(), String> {
    // #4752: delegates to the shared entry point so the daemon-resume,
    // fresh-start, and bare-`tm` in-place relaunch paths cannot drift apart.
    // #4832: the write is per-session, so the resumed session's own id selects
    // the directory — a resume must refresh the file the spawn will read, not
    // a sibling session's.
    crate::core::instruction_pipeline::refresh_compiled_prompt(workspace, &session_id.to_string())
}

#[cfg(test)]
#[path = "session_prep_tests.rs"]
mod tests;
