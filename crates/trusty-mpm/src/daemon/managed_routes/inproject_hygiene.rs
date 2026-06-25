//! Startup hygiene for managed base clones (#1709).
//!
//! Why: the protected base clone used by the in-project spawn path (#1706) must
//! always be on the default branch and up to date with the remote when the
//! daemon starts. Without hygiene, a stale or diverged base clone silently
//! yields sessions on an outdated branch, confusing agents. Running a short
//! fetch + hard-reset sequence at daemon startup keeps every base clone
//! current. Dead worktrees from previous sessions are also pruned to prevent
//! the base clone's worktree list from growing indefinitely.
//! What: [`get_default_branch`] reads the origin/HEAD symref; [`run_hygiene_for_base`]
//! runs fetch, hard-reset, and worktree prune for one base clone directory;
//! [`run_hygiene_for_all_bases`] walks `<repos_root>/<owner>/<repo>/` and calls
//! the per-base function for each; wired into daemon startup via
//! [`super::super::serve_http`] (the main `serve_http` function in daemon/mod.rs).
//! Test: `get_default_branch_returns_none_for_non_git` and
//! `run_hygiene_skips_missing_dir` unit tests; integration coverage via daemon
//! startup tests.

use std::path::Path;

use tracing::{info, warn};

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
    let branch = branch.strip_prefix("origin/").unwrap_or(&branch).to_string();
    if branch.is_empty() { None } else { Some(branch) }
}

/// Run hygiene for a single base clone: fetch, hard-reset to default, prune worktrees.
///
/// Why: each step targets a distinct failure mode — fetch syncs the remote object
/// store; hard-reset discards any accidental local modifications; worktree prune
/// cleans up working-tree entries for worktrees whose paths no longer exist (left
/// behind by decommissioned sessions). All three steps are non-fatal: a failure
/// is logged as a warning and the next step still runs, so a transient git error
/// does not prevent the other steps from running.
/// What: (1) `git -C <base_path> fetch origin`; (2) determines the default branch
/// via [`get_default_branch`] (falls back to `"main"`); (3) `git -C <base_path>
/// reset --hard origin/<branch>`; (4) `git -C <base_path> worktree prune`.
/// Test: `run_hygiene_skips_missing_dir` (unit — directory absent → early return).
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

    // Step 2: hard-reset to the default branch.
    let branch = get_default_branch(base_path).unwrap_or_else(|| "main".to_string());
    let target = format!("origin/{branch}");
    let reset = std::process::Command::new("git")
        .arg("-C")
        .arg(base_path)
        .args(["reset", "--hard", &target])
        .output();
    match reset {
        Ok(out) if out.status.success() => {
            info!(path = %base_path.display(), branch = %branch, "inproject-hygiene: reset OK");
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
        assert!(result.is_ok(), "should skip non-git dir cleanly: {result:?}");
    }

    #[test]
    fn run_hygiene_for_all_bases_skips_missing_root() {
        // A non-existent repos root must complete without panicking.
        let missing = std::path::Path::new("/tmp/trusty-nonexistent-repos-root-hygiene-test");
        run_hygiene_for_all_bases(missing); // must not panic
    }
}
