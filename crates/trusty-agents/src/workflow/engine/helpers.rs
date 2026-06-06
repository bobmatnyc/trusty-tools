//! Post-phase file reconciliation + relocation helpers for the workflow engine.
//!
//! Why: The claude CLI subprocess sometimes anchors relative `write_file`
//! paths to the git repository root instead of `RunContext::working_dir`, so
//! `assignments.json`, `stubs/`, and generated source files can land at the
//! project root rather than under `out_dir` / `code_dir`. These best-effort
//! helpers detect that misroute (gated on a recency window) and relocate the
//! outputs so downstream phases (wave loop, QA) find them where expected.
//! What: `agent_uses_claude_code` reports whether an agent uses the claude-code
//! runner; the `reconcile_*` functions move misrouted source files into the
//! code target; the `relocate_*` functions move `assignments.json` + `stubs/`
//! into `out_dir`; `copy_dir_all` is a symlink-refusing recursive copy.
//! Test: `post_code_reconciles_files_from_project_root`,
//! `post_plan_relocates_assignments_json_from_git_root`,
//! `reconcile_code_outputs_against_divergent_dirs` in `executor`'s test module.

use crate::agents::AgentConfig;
use crate::workflow::config::{Assignments, safe_join};

/// #160: Max age of a stray `assignments.json` at the project root that we
/// will still treat as belonging to the just-finished plan phase. Anything
/// older is presumed to be a stale leftover from a prior run (possibly
/// abandoned) and is NOT relocated — silently moving an old file could mask
/// real plan failures by making the code phase see outdated waves.
const POST_PLAN_RELOCATION_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// #123: Returns true if `agent_name` is configured with `runner = "claude-code"`.
///
/// Why: The claude CLI subprocess sometimes anchors relative `write_file`
/// calls to the git repository root rather than `RunContext::working_dir`.
/// We only trigger the post-code reconciliation for runners with this known
/// behavior — for subprocess/inline runners the files always land in
/// `out_dir` and we shouldn't disturb anything at the project root.
/// What: Loads the agent TOML; on any failure returns false (best effort).
/// Test: Indirectly via `qa_receives_correct_path_for_claude_code_runner`.
pub(crate) fn agent_uses_claude_code(agent_name: &str) -> bool {
    AgentConfig::by_name(agent_name)
        .map(|c| c.agent.runner == crate::agents::RunnerKind::ClaudeCode)
        .unwrap_or(false)
}

/// #123: After the code phase, move files that the code agent (claude-code
/// runner) wrote to the project root into `out_dir`.
///
/// Why: Even with `current_dir(out_dir)` and `--add-dir out_dir`, the claude
/// CLI occasionally anchors relative `write_file` paths to the git repo root
/// (its inherited CWD). The QA agent runs pytest against `out_dir` (or the
/// `{{project_dir}}` discovered inside it), so files at the project root are
/// invisible to QA and produce false-negative test failures. This routine
/// reads `assignments.json` from `out_dir`, and for each listed path that is
/// (a) missing in `out_dir` and (b) present at the project root with a recent
/// mtime, moves it into `out_dir`.
/// What: Best-effort. Skips silently when no `assignments.json` is present
/// (legacy monolithic path), when the project root cannot be read, or when
/// individual moves fail. Recency check uses the same 10-minute window as
/// `POST_PLAN_RELOCATION_MAX_AGE` to avoid picking up stale leftovers.
/// Test: `post_code_reconciles_files_from_project_root` exercises the move.
async fn reconcile_code_outputs_from_project_root(
    out_dir: &std::path::Path,
) -> std::io::Result<()> {
    let project_root = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "reconcile_code_outputs: cannot read CWD");
            return Ok(());
        }
    };
    reconcile_code_outputs_from(&project_root, out_dir).await
}

/// #222: Reconcile code outputs against an explicit code target.
///
/// Why: When `--project-dir` is set, generated files belong in `code_dir`,
/// not `out_dir`. The plan-agent's `assignments.json` still lives in
/// `out_dir` (artifacts), so reconciliation has to read the manifest from
/// one path and check/move files into another.
/// What: Loads `assignments.json` from `assignments_dir`; for each listed
/// file, if it's missing in `code_target` but present at the git project
/// root (CWD) with a recent mtime, moves it into `code_target`. When
/// `assignments_dir == code_target` (legacy mode) delegates to the
/// pre-#222 `reconcile_code_outputs_from_project_root`.
/// Test: Indirect — covered by the existing
/// `post_code_reconciles_files_from_project_root` path when paths align;
/// the divergent path is exercised by manual smoke for `--project-dir .`.
pub(crate) async fn reconcile_code_outputs_against(
    assignments_dir: &std::path::Path,
    code_target: &std::path::Path,
) -> std::io::Result<()> {
    if assignments_dir == code_target {
        return reconcile_code_outputs_from_project_root(code_target).await;
    }
    let project_root = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "reconcile_code_outputs_against: cannot read CWD");
            return Ok(());
        }
    };
    let assignments = match Assignments::load(assignments_dir) {
        Some(a) => a,
        None => {
            tracing::debug!(
                assignments_dir = %assignments_dir.display(),
                "reconcile_code_outputs_against: no assignments.json; skipping"
            );
            return Ok(());
        }
    };
    let mut moved = 0usize;
    for wave in &assignments.waves {
        for file in &wave.files {
            let dest = match safe_join(code_target, &file.path) {
                Some(p) => p,
                None => continue,
            };
            if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
                continue;
            }
            let stray = project_root.join(&file.path);
            if !tokio::fs::try_exists(&stray).await.unwrap_or(false) {
                continue;
            }
            // When code_target effectively == project_root (e.g.
            // `--project-dir .`), the file is already where it belongs.
            if let (Ok(a), Ok(b)) = (std::fs::canonicalize(&stray), std::fs::canonicalize(&dest))
                && a == b
            {
                continue;
            }
            let meta = match tokio::fs::metadata(&stray).await {
                Ok(m) => m,
                Err(_) => continue,
            };
            let is_recent = meta
                .modified()
                .ok()
                .and_then(|mt| mt.elapsed().ok())
                .map(|age| age <= POST_PLAN_RELOCATION_MAX_AGE)
                .unwrap_or(false);
            if !is_recent {
                continue;
            }
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            if let Err(rename_err) = tokio::fs::rename(&stray, &dest).await {
                tracing::debug!(error = %rename_err, "rename failed; copy+delete");
                tokio::fs::copy(&stray, &dest).await?;
                let _ = tokio::fs::remove_file(&stray).await;
            }
            tracing::warn!(
                from = %stray.display(),
                to = %dest.display(),
                "code phase wrote file to git root — relocated to code_dir"
            );
            moved += 1;
        }
    }
    if moved > 0 {
        tracing::info!(
            moved,
            "reconcile_code_outputs_against: relocated files into code_dir"
        );
    }
    Ok(())
}

/// Testable inner routine for `reconcile_code_outputs_from_project_root`.
///
/// Why: Lets unit tests pass an explicit `project_root` instead of mutating
/// the process-wide `std::env::current_dir` (unsafe in multi-threaded test
/// runners, mirrors the pattern used by `relocate_plan_outputs_from`).
/// What: Reads `out_dir/assignments.json`; for each file listed in any wave,
/// if the path is missing in `out_dir` but present at `project_root` and
/// modified within the last 10 minutes, rename (or copy+delete) it to
/// `out_dir/<rel>`. Logs per-file actions at WARN.
/// Test: `post_code_reconciles_files_from_project_root` below.
pub(crate) async fn reconcile_code_outputs_from(
    project_root: &std::path::Path,
    out_dir: &std::path::Path,
) -> std::io::Result<()> {
    let assignments = match Assignments::load(out_dir) {
        Some(a) => a,
        None => {
            tracing::debug!(
                out_dir = %out_dir.display(),
                "reconcile_code_outputs: no assignments.json; skipping"
            );
            return Ok(());
        }
    };

    let mut moved = 0usize;
    let mut skipped_recent = 0usize;
    for wave in &assignments.waves {
        for file in &wave.files {
            // #114: Refuse to act on any path that escapes out_dir, even if
            // validate_file_path was bypassed. safe_join returns None for
            // any traversal attempt.
            let dest = match safe_join(out_dir, &file.path) {
                Some(p) => p,
                None => {
                    tracing::warn!(
                        path = %file.path,
                        "reconcile_code_outputs: refusing to act on unsafe path"
                    );
                    continue;
                }
            };
            if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
                // Happy path — claude-code wrote it where we expected.
                continue;
            }

            // Misroute candidate: same relative path under the project root.
            let stray = project_root.join(&file.path);
            if !tokio::fs::try_exists(&stray).await.unwrap_or(false) {
                continue;
            }

            // Recency gate: only move if mtime is recent enough to plausibly
            // belong to the just-finished code phase.
            let meta = match tokio::fs::metadata(&stray).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!(error = %e, path = %stray.display(),
                        "reconcile_code_outputs: stat failed");
                    continue;
                }
            };
            let is_recent = meta
                .modified()
                .ok()
                .and_then(|mt| mt.elapsed().ok())
                .map(|age| age <= POST_PLAN_RELOCATION_MAX_AGE)
                .unwrap_or(false);
            if !is_recent {
                skipped_recent += 1;
                continue;
            }

            // Ensure parent directory exists in out_dir before move.
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            if let Err(rename_err) = tokio::fs::rename(&stray, &dest).await {
                tracing::debug!(
                    error = %rename_err,
                    "reconcile_code_outputs: rename failed; falling back to copy+delete"
                );
                tokio::fs::copy(&stray, &dest).await?;
                if let Err(e) = tokio::fs::remove_file(&stray).await {
                    tracing::debug!(
                        error = %e,
                        path = %stray.display(),
                        "reconcile_code_outputs: could not remove stray file after copy"
                    );
                }
            }

            tracing::warn!(
                from = %stray.display(),
                to = %dest.display(),
                "code phase wrote file to git root instead of out_dir — relocated to out_dir"
            );
            moved += 1;
        }
    }

    if moved > 0 || skipped_recent > 0 {
        tracing::info!(
            moved = moved,
            skipped_too_old = skipped_recent,
            "reconcile_code_outputs: post-code reconciliation summary"
        );
    }
    Ok(())
}

pub(crate) async fn relocate_plan_outputs_from_project_root(
    out_dir: &std::path::Path,
) -> std::io::Result<()> {
    let project_root = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "relocate_plan_outputs: cannot read CWD");
            return Ok(());
        }
    };
    relocate_plan_outputs_from(&project_root, out_dir).await
}

/// Testable inner routine: same behavior as
/// `relocate_plan_outputs_from_project_root` but with the project root
/// explicitly passed in so tests can supply a simulated CWD without
/// mutating the process-wide `std::env::current_dir` (which is unsafe in
/// multi-threaded test runners).
pub(crate) async fn relocate_plan_outputs_from(
    project_root: &std::path::Path,
    out_dir: &std::path::Path,
) -> std::io::Result<()> {
    let out_asg = out_dir.join("assignments.json");
    if tokio::fs::try_exists(&out_asg).await.unwrap_or(false) {
        // Happy path — plan-agent wrote it where we expected. Nothing to do.
        return Ok(());
    }

    let root_asg = project_root.join("assignments.json");
    let root_asg_exists = tokio::fs::try_exists(&root_asg).await.unwrap_or(false);
    if !root_asg_exists {
        // Nothing misrouted. plan-agent just didn't produce assignments.json
        // at all — the code phase's existing "legacy monolithic" fallback
        // path will log that decision clearly.
        return Ok(());
    }

    // Recency check: only relocate if the file was touched recently enough
    // to plausibly belong to the plan phase we just finished.
    let meta = tokio::fs::metadata(&root_asg).await?;
    let is_recent = meta
        .modified()
        .ok()
        .and_then(|mt| mt.elapsed().ok())
        .map(|age| age <= POST_PLAN_RELOCATION_MAX_AGE)
        .unwrap_or(false);
    if !is_recent {
        tracing::debug!(
            path = %root_asg.display(),
            "found assignments.json at project root but it's too old to be from this plan phase; ignoring"
        );
        return Ok(());
    }

    // Ensure out_dir exists before the move.
    tokio::fs::create_dir_all(out_dir).await?;

    // Try rename first (atomic on same filesystem); fall back to copy+delete.
    if let Err(rename_err) = tokio::fs::rename(&root_asg, &out_asg).await {
        tracing::debug!(error = %rename_err, "rename failed, falling back to copy+delete");
        tokio::fs::copy(&root_asg, &out_asg).await?;
        // Best-effort delete; if we can't remove it we still succeeded in
        // seeding out_dir, which is what the code phase needs.
        if let Err(e) = tokio::fs::remove_file(&root_asg).await {
            tracing::debug!(error = %e, "could not remove stray assignments.json at project root");
        }
    }

    tracing::warn!(
        from = %root_asg.display(),
        to = %out_asg.display(),
        "plan phase wrote assignments.json to git root instead of out_dir — relocated to out_dir"
    );

    // Also relocate stubs/ if the plan-agent put it at the project root.
    let root_stubs = project_root.join("stubs");
    let out_stubs = out_dir.join("stubs");
    if tokio::fs::try_exists(&root_stubs).await.unwrap_or(false)
        && !tokio::fs::try_exists(&out_stubs).await.unwrap_or(false)
    {
        // Only relocate if recent, using directory mtime as a proxy.
        let stubs_meta = tokio::fs::metadata(&root_stubs).await?;
        let stubs_recent = stubs_meta
            .modified()
            .ok()
            .and_then(|mt| mt.elapsed().ok())
            .map(|age| age <= POST_PLAN_RELOCATION_MAX_AGE)
            .unwrap_or(false);
        if stubs_recent {
            if let Err(e) = tokio::fs::rename(&root_stubs, &out_stubs).await {
                // Cross-device or non-empty-target; try recursive copy.
                tracing::debug!(error = %e, "stubs rename failed, copying recursively");
                if let Err(copy_err) = copy_dir_all(&root_stubs, &out_stubs) {
                    tracing::warn!(error = %copy_err, "failed to copy stubs/ from project root");
                } else {
                    let _ = std::fs::remove_dir_all(&root_stubs);
                }
            }
            tracing::warn!(
                from = %root_stubs.display(),
                to = %out_stubs.display(),
                "plan phase wrote stubs/ to git root instead of out_dir — relocated to out_dir"
            );
        }
    }

    Ok(())
}

/// Recursively copy a directory tree from `src` to `dst`, refusing symlinks.
///
/// Why: CRIT-3 (#92): the previous implementation followed symlinks, which
/// let an attacker with write access to a source tree redirect the copy into
/// sensitive directories. We now query `symlink_metadata` (does NOT follow
/// symlinks) and skip any symlink entry with a warning.
/// What: Creates `dst` if absent, then copies every regular file and
/// subdirectory recursively. Symlinks are skipped and logged. Existing files
/// in `dst` are overwritten.
/// Test: Covered indirectly via the stubs-relocation path in
/// `relocate_plan_outputs_from`; symlink behavior verified by code review.
fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        // CRIT-3 (#92): `symlink_metadata` does NOT follow symlinks; this is
        // the only correct way to refuse to traverse them.
        let file_type = entry.path().symlink_metadata()?.file_type();
        if file_type.is_symlink() {
            tracing::warn!(
                path = ?entry.path(),
                "copy_dir_all: skipping symlink to avoid following arbitrary paths"
            );
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
