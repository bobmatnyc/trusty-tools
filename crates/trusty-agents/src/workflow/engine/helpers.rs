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
//! `reconcile_code_outputs_against_divergent_dirs` in `executor`'s test module;
//! the undeterminable-probe arms (#5551) in
//! `executor::tests::relocate_fail_closed`.

use crate::agents::AgentConfig;
use crate::workflow::config::{Assignments, safe_join};

/// #5551: Filesystem-presence probe used by every relocation gate here.
///
/// Why: These helpers ask "is the destination already there?" before renaming
/// or copying over it and then deleting the source. The question has three
/// answers — present, absent, and *undeterminable* — and the live trigger for
/// the third is a transient `EIO`/`ETIMEDOUT`/`ESTALE` on a network mount,
/// which no real-filesystem fixture can produce on demand. Routing every probe
/// through this trait is what makes the error arm reachable from a test.
/// What: `try_exists` mirrors `tokio::fs::try_exists` exactly — `NotFound` is a
/// definite answer and comes back as `Ok(false)`; only an unanswerable probe
/// yields `Err`.
/// Test: `reconcile_against_aborts_when_destination_probe_is_undeterminable`
/// and its siblings in `executor::tests::relocate_fail_closed`.
#[async_trait::async_trait]
pub(crate) trait ExistsProbe: Send + Sync {
    async fn try_exists(&self, path: &std::path::Path) -> std::io::Result<bool>;
}

/// The real-filesystem probe; every production call path uses this one.
pub(crate) struct FsExistsProbe;

#[async_trait::async_trait]
impl ExistsProbe for FsExistsProbe {
    async fn try_exists(&self, path: &std::path::Path) -> std::io::Result<bool> {
        tokio::fs::try_exists(path).await
    }
}

/// #5551: Name the path an undeterminable probe was asked about, preserving
/// the underlying `ErrorKind` so a caller can still tell EIO from ELOOP.
fn undeterminable(path: &std::path::Path, e: std::io::Error) -> std::io::Error {
    std::io::Error::new(
        e.kind(),
        format!(
            "presence of {} could not be determined ({e}); refusing to relocate over it",
            path.display()
        ),
    )
}

/// #5551: Fail the whole pass when any item was left unrelocated because its
/// presence was undeterminable, so a caller cannot read the summary as clean.
fn unresolved_result(op: &str, unresolved: Vec<String>) -> std::io::Result<()> {
    if unresolved.is_empty() {
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "{op}: {} path(s) left unrelocated because their presence could not be determined: {}",
        unresolved.len(),
        unresolved.join(", ")
    )))
}

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

/// #222: Reconcile code outputs against an explicit code target.
///
/// Why: When `--project-dir` is set, generated files belong in `code_dir`,
/// not `out_dir`. The plan-agent's `assignments.json` still lives in
/// `out_dir` (artifacts), so reconciliation has to read the manifest from
/// one path and check/move files into another.
/// What: Loads `assignments.json` from `assignments_dir`; for each listed
/// file, if it's missing in `code_target` but present at the git project
/// root (CWD) with a recent mtime, moves it into `code_target`. When
/// `assignments_dir == code_target` (legacy mode) delegates to
/// [`reconcile_code_outputs_from`].
/// Test: `reconcile_code_outputs_against_divergent_dirs`,
/// `reconcile_against_aborts_when_destination_probe_is_undeterminable`.
pub(crate) async fn reconcile_code_outputs_against(
    assignments_dir: &std::path::Path,
    code_target: &std::path::Path,
) -> std::io::Result<()> {
    let project_root = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "reconcile_code_outputs_against: cannot read CWD");
            return Ok(());
        }
    };
    reconcile_code_outputs_against_from(&project_root, assignments_dir, code_target, &FsExistsProbe)
        .await
}

/// Testable inner routine for [`reconcile_code_outputs_against`], with the
/// project root and the presence probe supplied explicitly.
///
/// Why: The outer function reads the project root from the process-wide CWD
/// and always probes the real filesystem, so neither the divergent-directory
/// layout nor the undeterminable-probe arm is reachable from a test without
/// this seam.
/// What: Same behavior as the outer function; `probe` answers every
/// destination/stray presence question.
/// Test: `reconcile_against_aborts_when_destination_probe_is_undeterminable`.
pub(crate) async fn reconcile_code_outputs_against_from(
    project_root: &std::path::Path,
    assignments_dir: &std::path::Path,
    code_target: &std::path::Path,
    probe: &dyn ExistsProbe,
) -> std::io::Result<()> {
    if assignments_dir == code_target {
        return reconcile_code_outputs_from(project_root, code_target, probe).await;
    }
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
    let mut unresolved: Vec<String> = Vec::new();
    for wave in &assignments.waves {
        for file in &wave.files {
            let dest = match safe_join(code_target, &file.path) {
                Some(p) => p,
                None => continue,
            };
            // #5551: an undeterminable destination probe would otherwise read
            // as "absent" and rename the stray over a real generated output.
            match probe.try_exists(&dest).await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(error = %undeterminable(&dest, e), "aborting this file's relocation");
                    unresolved.push(file.path.clone());
                    continue;
                }
            }
            let stray = project_root.join(&file.path);
            // #5551: a failed stray probe is not a clean skip — account for it.
            match probe.try_exists(&stray).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    tracing::error!(error = %undeterminable(&stray, e), "aborting this file's relocation");
                    unresolved.push(file.path.clone());
                    continue;
                }
            }
            // When code_target effectively == project_root (e.g.
            // `--project-dir .`), the file is already where it belongs.
            if let (Ok(a), Ok(b)) = (std::fs::canonicalize(&stray), std::fs::canonicalize(&dest))
                && a == b
            {
                continue;
            }
            // #5551: an unstattable stray is not a clean skip either — the
            // recency gate cannot be evaluated, so the file goes unaccounted.
            let meta = match tokio::fs::metadata(&stray).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!(error = %undeterminable(&stray, e), "aborting this file's relocation");
                    unresolved.push(file.path.clone());
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
                continue;
            }
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            if let Err(rename_err) = tokio::fs::rename(&stray, &dest).await {
                tracing::debug!(error = %rename_err, "rename failed; copy+delete");
                tokio::fs::copy(&stray, &dest).await?;
                if let Err(e) = tokio::fs::remove_file(&stray).await {
                    tracing::debug!(error = %e, path = %stray.display(),
                        "could not remove stray file after copy");
                }
            }
            tracing::warn!(
                from = %stray.display(),
                to = %dest.display(),
                "code phase wrote file to git root — relocated to code_dir"
            );
            moved += 1;
        }
    }
    if moved > 0 || !unresolved.is_empty() {
        tracing::info!(
            moved,
            unresolved = unresolved.len(),
            "reconcile_code_outputs_against: relocated files into code_dir"
        );
    }
    unresolved_result("reconcile_code_outputs_against", unresolved)
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
    probe: &dyn ExistsProbe,
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
    let mut unresolved: Vec<String> = Vec::new();
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
            // #5551: an undeterminable destination probe would otherwise read
            // as "absent" and rename the stray over a real generated output.
            match probe.try_exists(&dest).await {
                // Happy path — claude-code wrote it where we expected.
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(error = %undeterminable(&dest, e), "aborting this file's relocation");
                    unresolved.push(file.path.clone());
                    continue;
                }
            }

            // Misroute candidate: same relative path under the project root.
            let stray = project_root.join(&file.path);
            // #5551: a failed stray probe is not a clean skip — account for it.
            match probe.try_exists(&stray).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    tracing::error!(error = %undeterminable(&stray, e), "aborting this file's relocation");
                    unresolved.push(file.path.clone());
                    continue;
                }
            }

            // Recency gate: only move if mtime is recent enough to plausibly
            // belong to the just-finished code phase.
            let meta = match tokio::fs::metadata(&stray).await {
                Ok(m) => m,
                // #5551: same as above — record it rather than skipping silently.
                Err(e) => {
                    tracing::error!(error = %undeterminable(&stray, e), "aborting this file's relocation");
                    unresolved.push(file.path.clone());
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

    if moved > 0 || skipped_recent > 0 || !unresolved.is_empty() {
        tracing::info!(
            moved = moved,
            skipped_too_old = skipped_recent,
            unresolved = unresolved.len(),
            "reconcile_code_outputs: post-code reconciliation summary"
        );
    }
    unresolved_result("reconcile_code_outputs", unresolved)
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
    relocate_plan_outputs_from(&project_root, out_dir, &FsExistsProbe).await
}

/// Testable inner routine: same behavior as
/// `relocate_plan_outputs_from_project_root` but with the project root
/// explicitly passed in so tests can supply a simulated CWD without
/// mutating the process-wide `std::env::current_dir` (which is unsafe in
/// multi-threaded test runners).
pub(crate) async fn relocate_plan_outputs_from(
    project_root: &std::path::Path,
    out_dir: &std::path::Path,
    probe: &dyn ExistsProbe,
) -> std::io::Result<()> {
    let out_asg = out_dir.join("assignments.json");
    // #5551: `assignments.json` is the plan manifest the code phase and QA
    // read; an undeterminable probe must never unblock overwriting it.
    if probe
        .try_exists(&out_asg)
        .await
        .map_err(|e| undeterminable(&out_asg, e))?
    {
        // Happy path — plan-agent wrote it where we expected. Nothing to do.
        return Ok(());
    }

    let root_asg = project_root.join("assignments.json");
    // #5551: a failed source probe is not "nothing misrouted".
    let root_asg_exists = probe
        .try_exists(&root_asg)
        .await
        .map_err(|e| undeterminable(&root_asg, e))?;
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
    // #5551: both probes gate a rename whose ENOTEMPTY fallback merge-copies
    // over `out_stubs` and then `remove_dir_all`s the whole source tree.
    if probe
        .try_exists(&root_stubs)
        .await
        .map_err(|e| undeterminable(&root_stubs, e))?
        && !probe
            .try_exists(&out_stubs)
            .await
            .map_err(|e| undeterminable(&out_stubs, e))?
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
                // #5551: a failed copy left the "relocated" WARN below to fire
                // anyway, reporting a move that did not happen.
                copy_dir_all(&root_stubs, &out_stubs)?;
                if let Err(e) = std::fs::remove_dir_all(&root_stubs) {
                    tracing::debug!(error = %e, path = %root_stubs.display(),
                        "could not remove stray stubs/ after copy");
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
