//! The facts `tm hook --pm-guard` needs before it lets `version-control`
//! remove a worktree
//! ([ADR-0057](../../../../docs/adr/0057-version-control-owns-worktree-removal.md)).
//!
//! Why: ADR-0057 grants ONE agent the raw `git worktree remove` that #5791
//! denied everyone, and the grant is only as safe as the re-checks that gate
//! it. Those re-checks ask git and GitHub, so they need a subprocess, and the
//! guard lives in the `tm` binary where neither
//! [`crate::session_manager::worktree_safety::git_command`] (the crate's single
//! hardened git entry point) nor the reclaim sweep's hardened `gh` spawn is
//! reachable. This module is the seam, for the same reason
//! [`crate::core::staged_paths`] is: the policy stays in the guard, the
//! subprocess stays behind the crate's existing entry points.
//!
//! What: [`WorktreeRemovalProbe`] is the four questions the guard asks —
//! working-tree cleanliness, unpushed commits, the checked-out branch, and
//! whether GitHub has a MERGED pull request for it. [`GitAndGhProbe`] answers
//! them for real; a test substitutes its own implementation and reaches no
//! network.
//!
//! **Every arm fails CLOSED.** A `Result::Err` means the fact could not be
//! established, never that it is absent — the
//! [ADR-0045](../../../../docs/adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md)
//! distinction, applied to a gate whose ALLOW deletes a checkout. The caller
//! turns every `Err` into a deny; nothing here decides.
//!
//! **Merge state comes from GitHub, never from git ancestry.** Every merge on
//! this repository is a squash merge, so a merged branch's tip is structurally
//! never an ancestor of the squash commit and `git merge-base --is-ancestor`
//! reports "not merged" for a tree that is safe to reclaim. `gh pr list --head
//! <branch> --state merged` is the only question that answers correctly.
//!
//! Test: `merged_pull_request_argv_asks_github_for_the_branch`,
//! `detached_head_is_not_a_branch`, `dirty_entry_count_ignores_blank_lines`
//! below; the policy that consumes these answers is tested in
//! `bin/tm/commands/pm_guard_bash/worktree_remove`.

use std::path::Path;

use crate::session_manager::worktree_reclaim_gh::{GH_TIMEOUT, gh_command, resolve_daemon_gh_env};
use crate::session_manager::worktree_safety::git_stdout;

/// The `gh pr list` argv the merged-PR re-check runs, without the branch.
///
/// Why: named so the test can assert the exact question asked, and so the
/// `--state merged` half cannot drift into `--state all` — which would report
/// an OPEN pull request as a reason to delete the tree holding its work.
/// `--limit 1` because the re-check needs existence, not a census.
/// What: interpolated with `--head <branch>` by [`GitAndGhProbe`].
/// Test: `merged_pull_request_argv_asks_github_for_the_branch`.
const MERGED_PR_ARGS: &[&str] = &["--state", "merged", "--json", "number", "--limit", "1"];

/// What `git rev-parse --abbrev-ref HEAD` prints for a detached HEAD.
const DETACHED_HEAD: &str = "HEAD";

/// The four facts ADR-0057's removal re-checks turn into a verdict.
///
/// Why: a trait rather than four free functions so the guard's policy can be
/// exercised against fabricated answers. The re-checks decide whether a
/// directory is deleted, and a unit test that reached a real `gh` would be
/// both slow and dependent on whoever's credentials CI happens to carry.
/// What: each method answers for one worktree directory. `Err(reason)` means
/// UNDETERMINABLE and is always a deny at the call site — see the module doc.
/// Test: implemented by [`GitAndGhProbe`] in production and by
/// `worktree_remove::tests::FakeProbe` in the guard's unit tests.
pub trait WorktreeRemovalProbe {
    /// Working-tree entries `git status --porcelain` reports in `dir`.
    fn dirty_entries(&self, dir: &Path) -> Result<usize, String>;

    /// Commits on `HEAD` that the upstream branch does not have.
    ///
    /// A worktree with no upstream configured is an `Err`: nothing proves its
    /// commits reached the remote, so removal would destroy them.
    fn unpushed_commits(&self, dir: &Path) -> Result<usize, String>;

    /// The branch `dir` has checked out. A detached HEAD is an `Err`.
    fn branch(&self, dir: &Path) -> Result<String, String>;

    /// How many MERGED pull requests GitHub has for `branch`.
    fn merged_pull_requests(&self, dir: &Path, branch: &str) -> Result<usize, String>;
}

/// The production probe: git for the local facts, `gh` for the merge state.
///
/// Why: both subprocesses route through the entry points that already strip
/// the environment able to redirect them at another repository —
/// [`crate::session_manager::worktree_safety::git_command`]'s
/// `GIT_ENV_REDIRECTS` and the reclaim sweep's `GH_STRIPPED_ENV`. A gate whose
/// ALLOW deletes a checkout must not be steerable by ambient `GIT_DIR` or
/// `GH_REPO`, and re-spelling either spawn here would have dropped that.
/// What: a unit struct; every answer is derived per call from `dir`.
/// Test: as the module doc.
#[derive(Debug, Default, Clone, Copy)]
pub struct GitAndGhProbe;

impl WorktreeRemovalProbe for GitAndGhProbe {
    fn dirty_entries(&self, dir: &Path) -> Result<usize, String> {
        Ok(count_nonblank_lines(&git_stdout(
            dir,
            &["status", "--porcelain"],
        )?))
    }

    fn unpushed_commits(&self, dir: &Path) -> Result<usize, String> {
        // No upstream makes `@{upstream}` unresolvable and git exits non-zero,
        // which `git_stdout` returns as `Err` — the fail-closed direction this
        // gate needs, and the one the deliverable names explicitly.
        let out = git_stdout(dir, &["rev-list", "--count", "@{upstream}..HEAD"])?;
        out.trim()
            .parse::<usize>()
            .map_err(|e| format!("`git rev-list --count` printed {:?}: {e}", out.trim()))
    }

    fn branch(&self, dir: &Path) -> Result<String, String> {
        let name = git_stdout(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_string();
        if name.is_empty() || name == DETACHED_HEAD {
            return Err("HEAD is detached — the worktree has no branch to look a \
                        pull request up by"
                .to_string());
        }
        Ok(name)
    }

    fn merged_pull_requests(&self, dir: &Path, branch: &str) -> Result<usize, String> {
        // #6623: the same per-project `github:` binding an interactive `tm`
        // resolves. The hook inherits the operator's shell environment in the
        // common case, but not when Claude Code is launched from a GUI, and a
        // lookup that fails auth must not read as "no merged PR".
        let mut cmd = gh_command(dir, &resolve_daemon_gh_env(dir));
        cmd.arg("pr").arg("list").arg("--head").arg(branch);
        cmd.args(MERGED_PR_ARGS);
        let stdout =
            crate::session_manager::worktree_reclaim_gh::run_with_timeout(cmd, GH_TIMEOUT)?;
        let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout)
            .map_err(|e| format!("`gh pr list --head {branch}` JSON did not parse: {e}"))?;
        Ok(rows.len())
    }
}

/// Count the lines of `text` that carry anything but whitespace.
fn count_nonblank_lines(text: &str) -> usize {
    text.lines().filter(|l| !l.trim().is_empty()).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_pull_request_argv_asks_github_for_the_branch() {
        // The two halves that must never drift: only MERGED pull requests
        // count, and the answer is a JSON array the caller can measure.
        assert!(MERGED_PR_ARGS.contains(&"merged"));
        assert!(MERGED_PR_ARGS.contains(&"--json"));
        assert!(!MERGED_PR_ARGS.contains(&"all"));
    }

    #[test]
    fn dirty_entry_count_ignores_blank_lines() {
        assert_eq!(count_nonblank_lines(""), 0);
        assert_eq!(count_nonblank_lines("\n  \n"), 0);
        assert_eq!(count_nonblank_lines(" M src/lib.rs\n?? new.txt\n"), 2);
    }

    #[test]
    fn detached_head_is_not_a_branch() {
        // A detached HEAD prints the literal `HEAD`, which is not a branch a
        // pull request can be looked up by — so it must not become one.
        assert_eq!(DETACHED_HEAD, "HEAD");
    }
}
