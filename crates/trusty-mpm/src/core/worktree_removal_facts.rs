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
//! **Which repository is STATED, never inferred (#7057).** The `--repo` slug
//! comes from the target worktree's own `origin` remote
//! ([`crate::session_manager::worktree_repo_slug`]); a worktree whose
//! repository cannot be established denies rather than asking `gh` to guess,
//! and the deny message names the repository that WAS searched so a
//! wrong-repository answer is visible instead of reading as "no pull request".
//!
//! **"Does this tree hold work" is ONE question, asked in one place (#7185).**
//! This module used to count `git status --porcelain` lines itself, while the
//! reclaim sweep asked
//! [`crate::session_manager::worktree_safety::count_dirty_files`]. The two
//! disagreed about the harness's own `.trusty-mpm-worktree` ownership marker,
//! which the harness writes into every isolated worktree it provisions: the
//! sweep excuses it, this module counted it, so every managed worktree was
//! dirty by construction here and none could be reclaimed from inside a live
//! session. The guard now routes through that one implementation.
//!
//! Test: `merged_pull_request_argv_asks_github_for_the_branch`,
//! `detached_head_is_not_a_branch`,
//! `the_harness_ownership_marker_alone_leaves_the_tree_clean`,
//! `a_real_untracked_file_beside_the_marker_still_counts` below; the policy
//! that consumes these answers is tested in
//! `bin/tm/commands/pm_guard_bash/worktree_remove`.

use std::path::Path;

use crate::session_manager::worktree_reclaim_gh::{
    GH_TIMEOUT, gh_pr_list_command, resolve_daemon_gh_env,
};
use crate::session_manager::worktree_repo_slug::repo_slug_for;
use crate::session_manager::worktree_safety::{count_dirty_files, git_stdout};

/// The `gh pr list` argv the merged-PR re-check runs, without the branch.
///
/// Why: named so the test can assert the exact question asked, and so the
/// `--state merged` half cannot drift into `--state all` — which would report
/// an OPEN pull request as a reason to delete the tree holding its work.
/// `--limit 1` because the re-check needs existence, not a census.
/// What: interpolated with `--repo <owner/repo> --head <branch>` by
/// [`GitAndGhProbe`].
/// Test: `merged_pull_request_argv_asks_github_for_the_branch`.
const MERGED_PR_ARGS: &[&str] = &["--state", "merged", "--json", "number", "--limit", "1"];

/// What the merged-PR re-check learned, and WHERE it looked (#7057).
///
/// Why: a count of zero is the answer both a branch with no merged pull request
/// and a lookup aimed at the wrong repository produce. The two used to be
/// indistinguishable in the deny message, which is how a prune run against
/// `1m-consulting/adaptive-crm` reported "no pull request found" for branches
/// whose pull requests had merged — while `gh` was answering for
/// `hotstats/hotstats-product-poc`. Returning the repository alongside the
/// count is what lets the refusal name it.
/// What: `count` is how many MERGED pull requests GitHub reported; `repo` is
/// the `[host/]owner/repo` that was asked, resolved from the worktree's own
/// `origin` — host-qualified when that remote is not on github.com, so the
/// refusal distinguishes the wrong SERVER as well as the wrong repository
/// (#7057).
/// Test: `deny_names_the_repository_the_merged_pr_lookup_searched` in
/// `bin/tm/commands/pm_guard_bash/worktree_remove`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MergedPrLookup {
    /// MERGED pull requests GitHub reported for the branch.
    pub count: usize,
    /// The `[host/]owner/repo` that was searched.
    pub repo: String,
}

impl MergedPrLookup {
    /// The answer `count` merged pull requests in `repo`.
    ///
    /// Why: the struct is `#[non_exhaustive]`, so the `tm` binary's fake probe
    /// — a different crate — cannot build one with a struct expression.
    #[must_use]
    pub fn new(count: usize, repo: impl Into<String>) -> Self {
        Self {
            count,
            repo: repo.into(),
        }
    }
}

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
    /// Working-tree entries in `dir` that represent WORK.
    ///
    /// #7185: modified or staged tracked files, untracked-but-not-ignored
    /// files, and anything under `.trusty-mpm/` that is not disposable
    /// bookkeeping — but NOT the `.trusty-mpm-worktree` ownership marker the
    /// harness itself writes into every worktree it provisions.
    fn dirty_entries(&self, dir: &Path) -> Result<usize, String>;

    /// Commits on `HEAD` that the upstream branch does not have.
    ///
    /// A worktree with no upstream configured is an `Err`: nothing proves its
    /// commits reached the remote, so removal would destroy them.
    fn unpushed_commits(&self, dir: &Path) -> Result<usize, String>;

    /// The branch `dir` has checked out. A detached HEAD is an `Err`.
    fn branch(&self, dir: &Path) -> Result<String, String>;

    /// How many MERGED pull requests GitHub has for `branch`, and in WHICH
    /// repository the question was asked (#7057).
    fn merged_pull_requests(&self, dir: &Path, branch: &str) -> Result<MergedPrLookup, String>;
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
        // #7185: the reclaim sweep's own count, not a second one. A plain
        // `git status --porcelain` line count here read the harness's
        // `.trusty-mpm-worktree` ownership marker as unsaved work, so every
        // isolated worktree was dirty by construction and none could be
        // reclaimed from inside a live session.
        count_dirty_files(dir)
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

    fn merged_pull_requests(&self, dir: &Path, branch: &str) -> Result<MergedPrLookup, String> {
        // #7057: the repository — and its host, when that is not github.com —
        // comes from THIS worktree's `origin`, not from whatever `gh` would
        // infer at this working directory. An origin that cannot be read or
        // parsed is an `Err`, which denies — never a lookup aimed at a guess.
        let repo = repo_slug_for(dir)?;
        // #6623: the same per-project `github:` binding an interactive `tm`
        // resolves. The hook inherits the operator's shell environment in the
        // common case, but not when Claude Code is launched from a GUI, and a
        // lookup that fails auth must not read as "no merged PR".
        // #6867: through the same gate the reclaim survey uses — this call has
        // the identical hang shape, and a `dir` whose `gh` has wedged must stop
        // being polled here too. Its own key: the argv asks a DIFFERENT
        // question (merged only) from `pr_state_for_branch`'s, so the two must
        // never share a reply. #7057: the repository is part of that key —
        // two directories resolving to different repositories do not have the
        // same answer for the same branch name.
        let stdout = crate::session_manager::worktree_reclaim_gh_gate::shared()
            .poll(dir, &format!("merged-count:{repo}:{branch}"), || {
                let mut cmd = gh_pr_list_command(dir, &resolve_daemon_gh_env(dir), &repo);
                cmd.arg("--head").arg(branch);
                cmd.args(MERGED_PR_ARGS);
                crate::session_manager::worktree_reclaim_gh::run_with_timeout(cmd, GH_TIMEOUT)
            })
            .map_err(|f| format!("{f} (repository searched: {repo})"))?;
        let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).map_err(|e| {
            format!("`gh pr list --repo {repo} --head {branch}` JSON did not parse: {e}")
        })?;
        Ok(MergedPrLookup {
            count: rows.len(),
            repo,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::session_manager::decommission::WORKTREE_SENTINEL_FILE;

    #[test]
    fn merged_pull_request_argv_asks_github_for_the_branch() {
        // The two halves that must never drift: only MERGED pull requests
        // count, and the answer is a JSON array the caller can measure.
        assert!(MERGED_PR_ARGS.contains(&"merged"));
        assert!(MERGED_PR_ARGS.contains(&"--json"));
        assert!(!MERGED_PR_ARGS.contains(&"all"));
    }

    #[test]
    fn detached_head_is_not_a_branch() {
        // A detached HEAD prints the literal `HEAD`, which is not a branch a
        // pull request can be looked up by — so it must not become one.
        assert_eq!(DETACHED_HEAD, "HEAD");
    }

    /// Run `git -C <dir> <args>`, panicking with git's own stderr on failure.
    fn git_ok(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("fixture: `git {}` could not be run: {e}", args.join(" ")));
        assert!(
            out.status.success(),
            "fixture: `git {}` failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A checkout with one commit, standing in for a harness worktree.
    fn checkout(tmp: &Path) -> PathBuf {
        let repo = tmp.join("tree");
        std::fs::create_dir_all(&repo).expect("fixture: create repo dir");
        git_ok(&repo, &["init", "--initial-branch=main"]);
        git_ok(&repo, &["config", "user.email", "ci@test.invalid"]);
        git_ok(&repo, &["config", "user.name", "CI"]);
        git_ok(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("README.md"), "base\n").expect("fixture: write README");
        git_ok(&repo, &["add", "README.md"]);
        git_ok(&repo, &["commit", "-m", "base"]);
        repo
    }

    /// 🔴 #7185: the harness writes `.trusty-mpm-worktree` into every isolated
    /// worktree it provisions, so counting it made EVERY managed worktree
    /// permanently un-reclaimable from inside a live session — the `clean-tree`
    /// gate denied a tree whose only untracked entry was the harness's own
    /// marker. Fails on 9b57099a5, where `dirty_entries` counts every porcelain
    /// line.
    #[test]
    fn the_harness_ownership_marker_alone_leaves_the_tree_clean() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = checkout(tmp.path());
        std::fs::write(repo.join(WORKTREE_SENTINEL_FILE), b"").expect("write marker");

        assert_eq!(
            GitAndGhProbe.dirty_entries(&repo).expect("status readable"),
            0,
            "the harness's own ownership marker is not the agent's unsaved work"
        );
    }

    /// 🔴 The excusal is for that ONE name and nothing else: a real untracked
    /// file beside the marker still denies, so #7185 cannot become a hole the
    /// next uncommitted rescue file falls through.
    #[test]
    fn a_real_untracked_file_beside_the_marker_still_counts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = checkout(tmp.path());
        std::fs::write(repo.join(WORKTREE_SENTINEL_FILE), b"").expect("write marker");
        std::fs::write(repo.join("rescued.rs"), "fn main() {}\n").expect("write work");

        assert_eq!(
            GitAndGhProbe.dirty_entries(&repo).expect("status readable"),
            1,
            "unsaved work beside the marker must still deny removal"
        );
    }

    /// A modified TRACKED file is work too — the marker excusal must not have
    /// widened into "ignore everything the harness could have touched".
    #[test]
    fn a_modified_tracked_file_counts_even_with_the_marker_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = checkout(tmp.path());
        std::fs::write(repo.join(WORKTREE_SENTINEL_FILE), b"").expect("write marker");
        std::fs::write(repo.join("README.md"), "edited\n").expect("edit README");

        assert_eq!(
            GitAndGhProbe.dirty_entries(&repo).expect("status readable"),
            1
        );
    }
}
