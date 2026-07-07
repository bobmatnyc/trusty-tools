//! Startup hygiene for managed base clones (#1709).
//!
//! Why: the protected base clone used by the in-project spawn path (#1706) must
//! always be on the default branch and up to date with the remote when the
//! daemon starts. Without hygiene, a stale or diverged base clone silently
//! yields sessions on an outdated branch, confusing agents. Running a short
//! fetch + hard-reset sequence at daemon startup keeps every base clone
//! current. Dead worktrees from previous sessions are also pruned to prevent
//! the base clone's worktree list from growing indefinitely.
//!
//! CRITICAL SAFETY INVARIANT (#2177): the hygiene sweep must NEVER discard
//! local commits or uncommitted changes. Before the hard-reset step runs, the
//! base clone's current branch is checked for unpushed commits (ahead of its
//! origin counterpart) and a dirty working tree; either condition SKIPS the
//! reset entirely and logs a warning instead. This closed a data-loss bug
//! where an unconditional `git reset --hard origin/<branch>` on every daemon
//! startup silently discarded committed-but-unpushed work.
//! What: [`get_default_branch`] reads the origin/HEAD symref; [`run_hygiene_for_base`]
//! runs fetch, a safety-gated hard-reset, and worktree prune for one base
//! clone directory; [`run_hygiene_for_all_bases`] walks
//! `<repos_root>/<owner>/<repo>/` and calls the per-base function for each;
//! wired into daemon startup via [`super::super::serve_http`] (the main
//! `serve_http` function in daemon/mod.rs).
//! Test: `get_default_branch_returns_none_for_non_git` and
//! `run_hygiene_skips_missing_dir` unit tests; `decide_reset_*` unit tests
//! cover the pure decision logic; `hygiene_*` integration tests exercise real
//! temp git repos for the ahead/dirty/clean/recovery-ref cases; integration
//! coverage via daemon startup tests.

use std::path::Path;

use tracing::{info, warn};

/// Decision on whether the destructive `git reset --hard` step may proceed.
///
/// Why: the reset decision has several independent inputs (ahead-count,
/// working-tree cleanliness, detached/unknown states) that are easiest to
/// reason about — and unit-test — as a small pure function separated from the
/// git-shelling plumbing that gathers those inputs.
/// What: two variants — `Reset` (safe to fast-forward the base clone to
/// origin) and `Skip(reason)` (refuse, carrying a human-readable reason for
/// the warning log).
/// Test: `decide_reset_*` unit tests below.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResetDecision {
    Reset,
    Skip(String),
}

/// Decide whether a hard-reset may proceed, given ahead-count and dirty state.
///
/// Why: centralizes the data-loss-prevention rule in one pure, testable place
/// rather than scattering conditionals through the git-shelling code. Any
/// input that could not be determined (detached HEAD, no upstream, a failed
/// `git` invocation) is treated conservatively as "do not reset" — the reset
/// is only ever allowed when we have positive confirmation the branch is not
/// ahead of origin and the working tree is clean.
/// What: `ahead_count: None` (unknown — detached HEAD, no upstream, or a
/// `rev-list` failure) always yields `Skip`. `dirty: None` (a `status`
/// failure) always yields `Skip`. `ahead_count: Some(n) if n > 0` yields
/// `Skip` (unpushed commits would be discarded). `dirty: Some(true)` yields
/// `Skip` (uncommitted changes would be discarded). Only `ahead_count:
/// Some(0)` combined with `dirty: Some(false)` yields `Reset`.
/// Test: `decide_reset_ahead_skips`, `decide_reset_dirty_skips`,
/// `decide_reset_unknown_ahead_skips`, `decide_reset_unknown_dirty_skips`,
/// `decide_reset_clean_and_even_resets`.
fn decide_reset(ahead_count: Option<usize>, dirty: Option<bool>) -> ResetDecision {
    let Some(ahead) = ahead_count else {
        return ResetDecision::Skip(
            "branch ahead-count unknown (detached HEAD, no upstream, or git error); \
             refusing to discard local work"
                .to_string(),
        );
    };
    let Some(is_dirty) = dirty else {
        return ResetDecision::Skip(
            "working tree status unknown (git error); refusing hard reset".to_string(),
        );
    };
    if ahead > 0 {
        return ResetDecision::Skip(format!(
            "{ahead} commit(s) ahead of origin (unpushed); refusing to discard local work"
        ));
    }
    if is_dirty {
        return ResetDecision::Skip("uncommitted changes present; refusing hard reset".to_string());
    }
    ResetDecision::Reset
}

/// Read the short name of the currently checked-out branch, if any.
///
/// Why: the ahead-count and reset-target checks need to know which branch is
/// actually checked out in the base clone (not merely the repo's configured
/// default branch) — resetting a detached HEAD or misidentifying the branch
/// could silently discard work on the wrong ref.
/// What: runs `git -C <base_path> symbolic-ref --short HEAD`; returns `None`
/// on any failure, including detached HEAD (where `symbolic-ref` exits
/// non-zero because `HEAD` does not point at a branch).
/// Test: covered indirectly via `hygiene_*` integration tests (a normal
/// branch checkout resolves; detached HEAD is exercised through
/// `decide_reset_unknown_ahead_skips`, which models the `None` case this
/// function would produce).
fn current_branch(base_path: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

/// Count commits on `branch` that are not yet on `origin/<branch>`.
///
/// Why: this is the direct measure of "would a hard reset to origin discard
/// committed work" — the data-loss bug this module fixes.
/// What: runs `git -C <base_path> rev-list --count origin/<branch>..<branch>`
/// and parses the count. Returns `None` if the command fails (e.g. no
/// `origin/<branch>` upstream exists) or the output does not parse as a
/// number — both are treated as "unknown" by [`decide_reset`], which refuses
/// to reset on unknown input.
/// Test: `hygiene_ahead_branch_is_not_reset` integration test (via
/// [`run_hygiene_for_base`]).
fn ahead_count(base_path: &Path, branch: &str) -> Option<usize> {
    let range = format!("origin/{branch}..{branch}");
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["rev-list", "--count", &range])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Determine whether the working tree has uncommitted changes.
///
/// Why: a hard reset discards uncommitted modifications just as readily as
/// unpushed commits; this is the second half of the data-loss guard.
/// What: runs `git -C <base_path> status --porcelain`; `Some(true)` if any
/// output is produced (dirty), `Some(false)` if output is empty (clean),
/// `None` if the command itself fails.
/// Test: `hygiene_dirty_tree_is_not_reset` integration test (via
/// [`run_hygiene_for_base`]).
fn is_dirty(base_path: &Path) -> Option<bool> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(!out.stdout.is_empty())
}

/// Best-effort write of a recovery ref pointing at the pre-reset HEAD.
///
/// Why: defense-in-depth — even when the ahead/dirty checks correctly clear a
/// reset to proceed, leaving a cheap breadcrumb to the prior HEAD costs
/// nothing and gives a manual recovery path (`git reset --hard
/// refs/trusty-mpm/pre-hygiene/<branch>`) if some unanticipated case still
/// loses work. This must never abort the sweep: any failure here is logged
/// and swallowed, matching the file's existing "every step logged, no step is
/// fatal" pattern.
/// What: resolves the current HEAD sha via `git rev-parse HEAD`, then runs
/// `git update-ref refs/trusty-mpm/pre-hygiene/<branch> <sha>`. Both steps are
/// best-effort; failures are logged via `warn!` and otherwise ignored.
/// Test: `hygiene_recovery_ref_written_before_reset` integration test (via
/// [`run_hygiene_for_base`]).
fn write_recovery_ref(base_path: &Path, branch: &str) {
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["rev-parse", "HEAD"])
        .output();
    let sha = match head {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Ok(out) => {
            warn!(
                path = %base_path.display(),
                "inproject-hygiene: recovery-ref rev-parse failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            return;
        }
        Err(e) => {
            warn!(path = %base_path.display(), "inproject-hygiene: recovery-ref rev-parse error: {e}");
            return;
        }
    };
    if sha.is_empty() {
        return;
    }

    let refname = format!("refs/trusty-mpm/pre-hygiene/{branch}");
    let update = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["update-ref", &refname, &sha])
        .output();
    match update {
        Ok(out) if out.status.success() => {
            info!(path = %base_path.display(), refname = %refname, sha = %sha, "inproject-hygiene: recovery ref written");
        }
        Ok(out) => {
            warn!(
                path = %base_path.display(),
                "inproject-hygiene: recovery-ref update-ref failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => {
            warn!(path = %base_path.display(), "inproject-hygiene: recovery-ref update-ref error: {e}");
        }
    }
}

/// Read the default branch for a base clone by inspecting `origin/HEAD`.
///
/// Why: the hard-reset step needs the default branch name; reading the symref
/// is more reliable than hardcoding `main` across diverse repositories.
/// What: runs `git -C <base_path> symbolic-ref --short refs/remotes/origin/HEAD`
/// and returns the short branch name (e.g. `main`) on success, or `None` if git
/// fails or there is no `origin/HEAD` symref (the caller falls back to `main`).
/// Test: `get_default_branch_returns_none_for_non_git` (unit).
pub fn get_default_branch(base_path: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // symbolic-ref returns e.g. "origin/main"; strip the "origin/" prefix.
    let branch = branch
        .strip_prefix("origin/")
        .unwrap_or(&branch)
        .to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

/// Run hygiene for a single base clone: fetch, safety-gated hard-reset, prune worktrees.
///
/// Why: each step targets a distinct failure mode — fetch syncs the remote object
/// store; hard-reset (when safe) discards any accidental local modifications;
/// worktree prune cleans up working-tree entries for worktrees whose paths no
/// longer exist (left behind by decommissioned sessions). All steps are
/// non-fatal: a failure is logged as a warning and the next step still runs,
/// so a transient git error does not prevent the other steps from running.
/// Critically (#2177), the hard-reset step is gated: it only runs when the
/// checked-out branch has zero unpushed commits ahead of its origin
/// counterpart AND the working tree is clean, so it can never discard local
/// work. When it does proceed, a recovery ref is written first as a
/// defense-in-depth breadcrumb.
/// What: (1) `git -C <base_path> fetch origin`; (2) resolves the current
/// branch via [`current_branch`] and, if resolvable, its ahead-count via
/// [`ahead_count`] and dirty state via [`is_dirty`]; feeds both into
/// [`decide_reset`] — on `Skip`, logs a warning and does not reset; on
/// `Reset`, writes a recovery ref via [`write_recovery_ref`] then runs `git
/// -C <base_path> reset --hard origin/<default-branch>` (default branch via
/// [`get_default_branch`], falls back to `"main"`); (3) `git -C <base_path>
/// worktree prune`.
/// Test: `run_hygiene_skips_missing_dir` (unit — directory absent → early
/// return); `hygiene_ahead_branch_is_not_reset`,
/// `hygiene_dirty_tree_is_not_reset`, `hygiene_clean_branch_is_reset`,
/// `hygiene_recovery_ref_written_before_reset` (integration, real temp git
/// repos).
pub fn run_hygiene_for_base(base_path: &Path) -> Result<(), String> {
    if !base_path.join(".git").exists() {
        return Ok(());
    }

    info!(path = %base_path.display(), "inproject-hygiene: running for base clone");

    // Step 1: fetch from origin.
    let fetch = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["fetch", "origin"])
        .output();
    match fetch {
        Ok(out) if out.status.success() => {
            info!(path = %base_path.display(), "inproject-hygiene: fetch OK");
        }
        Ok(out) => {
            warn!(
                path = %base_path.display(),
                "inproject-hygiene: fetch failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => {
            warn!(path = %base_path.display(), "inproject-hygiene: fetch error: {e}");
        }
    }

    // Step 2: safety-gated hard-reset to the default branch (#2177 — never
    // discard local commits or uncommitted changes).
    let default_branch = get_default_branch(base_path).unwrap_or_else(|| "main".to_string());
    let checked_out = current_branch(base_path);
    let ahead = checked_out
        .as_deref()
        .and_then(|b| ahead_count(base_path, b));
    let dirty = is_dirty(base_path);

    match decide_reset(ahead, dirty) {
        ResetDecision::Skip(reason) => {
            warn!(
                path = %base_path.display(),
                branch = %checked_out.as_deref().unwrap_or("<unknown/detached>"),
                "inproject-hygiene: SKIP reset — {reason}"
            );
        }
        ResetDecision::Reset => {
            if let Some(branch) = checked_out.as_deref() {
                write_recovery_ref(base_path, branch);
            }

            let target = format!("origin/{default_branch}");
            let reset = std::process::Command::new("git")
                .arg("-C")
                .arg(base_path)
                .args(["reset", "--hard", &target])
                .output();
            match reset {
                Ok(out) if out.status.success() => {
                    info!(path = %base_path.display(), branch = %default_branch, "inproject-hygiene: reset OK");
                }
                Ok(out) => {
                    warn!(
                        path = %base_path.display(),
                        "inproject-hygiene: reset failed ({}): {}",
                        out.status,
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
                Err(e) => {
                    warn!(path = %base_path.display(), "inproject-hygiene: reset error: {e}");
                }
            }
        }
    }

    // Step 3: prune stale worktrees.
    let prune = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["worktree", "prune"])
        .output();
    match prune {
        Ok(out) if out.status.success() => {
            info!(path = %base_path.display(), "inproject-hygiene: worktree prune OK");
        }
        Ok(out) => {
            warn!(
                path = %base_path.display(),
                "inproject-hygiene: worktree prune failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => {
            warn!(path = %base_path.display(), "inproject-hygiene: worktree prune error: {e}");
        }
    }

    Ok(())
}

/// Run hygiene for every base clone under `repos_root`.
///
/// Why: at daemon startup all managed base clones should be freshened in one
/// pass so sessions spawned shortly after startup always see a current default
/// branch. Walking the two-level `<owner>/<repo>` layout avoids hardcoding
/// specific project paths.
/// What: enumerates `<repos_root>/<owner>/` directories, then for each
/// `<owner>/<repo>` subdirectory calls [`run_hygiene_for_base`]. Non-git
/// directories are silently skipped. All errors are logged as warnings; no
/// single repo failure prevents the rest from being processed.
/// Test: `run_hygiene_for_all_bases_skips_missing_root` (unit).
pub fn run_hygiene_for_all_bases(repos_root: &Path) {
    if !repos_root.is_dir() {
        return;
    }

    info!(root = %repos_root.display(), "inproject-hygiene: starting startup sweep");

    let owner_dirs = match std::fs::read_dir(repos_root) {
        Ok(d) => d,
        Err(e) => {
            warn!(root = %repos_root.display(), "inproject-hygiene: cannot read repos root: {e}");
            return;
        }
    };

    for owner_entry in owner_dirs.flatten() {
        let owner_path = owner_entry.path();
        if !owner_path.is_dir() {
            continue;
        }
        let repo_dirs = match std::fs::read_dir(&owner_path) {
            Ok(d) => d,
            Err(e) => {
                warn!(path = %owner_path.display(), "inproject-hygiene: cannot read owner dir: {e}");
                continue;
            }
        };
        for repo_entry in repo_dirs.flatten() {
            let base_path = repo_entry.path();
            if !base_path.is_dir() {
                continue;
            }
            if let Err(e) = run_hygiene_for_base(&base_path) {
                warn!(path = %base_path.display(), "inproject-hygiene: error: {e}");
            }
        }
    }

    info!(root = %repos_root.display(), "inproject-hygiene: startup sweep complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_default_branch_returns_none_for_non_git() {
        // A non-git directory must return None cleanly.
        let tmp = std::env::temp_dir();
        assert!(get_default_branch(&tmp).is_none());
    }

    #[test]
    fn run_hygiene_skips_missing_dir() {
        // A path that does not have .git must return Ok(()) immediately.
        let tmp = std::env::temp_dir();
        let result = run_hygiene_for_base(&tmp);
        assert!(
            result.is_ok(),
            "should skip non-git dir cleanly: {result:?}"
        );
    }

    #[test]
    fn run_hygiene_for_all_bases_skips_missing_root() {
        // A non-existent repos root must complete without panicking.
        let missing = std::path::Path::new("/tmp/trusty-nonexistent-repos-root-hygiene-test");
        run_hygiene_for_all_bases(missing); // must not panic
    }

    // ── decide_reset: pure decision-logic unit tests (#2177) ──────────────

    #[test]
    fn decide_reset_ahead_skips() {
        // Any unpushed commit must refuse the reset, even on a clean tree.
        match decide_reset(Some(1), Some(false)) {
            ResetDecision::Skip(reason) => assert!(reason.contains("ahead")),
            ResetDecision::Reset => panic!("an ahead branch must never be reset"),
        }
    }

    #[test]
    fn decide_reset_dirty_skips() {
        // A dirty tree must refuse the reset, even when not ahead.
        match decide_reset(Some(0), Some(true)) {
            ResetDecision::Skip(reason) => assert!(reason.contains("uncommitted")),
            ResetDecision::Reset => panic!("a dirty tree must never be reset"),
        }
    }

    #[test]
    fn decide_reset_unknown_ahead_skips() {
        // Detached HEAD / no upstream / rev-list failure (ahead=None) must
        // conservatively refuse, regardless of the dirty state.
        match decide_reset(None, Some(false)) {
            ResetDecision::Skip(_) => {}
            ResetDecision::Reset => panic!("unknown ahead-count must never be reset"),
        }
    }

    #[test]
    fn decide_reset_unknown_dirty_skips() {
        // A `git status` failure (dirty=None) must conservatively refuse,
        // regardless of the ahead-count.
        match decide_reset(Some(0), None) {
            ResetDecision::Skip(_) => {}
            ResetDecision::Reset => panic!("unknown dirty-state must never be reset"),
        }
    }

    #[test]
    fn decide_reset_clean_and_even_resets() {
        // The only case that may proceed: zero ahead AND confirmed clean.
        assert_eq!(decide_reset(Some(0), Some(false)), ResetDecision::Reset);
    }
}
