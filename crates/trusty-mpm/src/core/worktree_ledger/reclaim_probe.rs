//! The production [`ReclaimProbe`]: git for HEAD and ancestry, `gh` for a
//! merged PR's head commit (#8994 S3a).
//!
//! Why: gate F1 and the post-merge rule must read the same sources the reclaim
//! sweep trusts, and must say "unresolved" whenever they cannot answer.
//! What: [`GitReclaimProbe`]. `head_of` is a bounded `git symbolic-ref -q
//! HEAD`; `merged_head` is one `gh pr view` through the reclaim sweep's `gh`
//! gate, pinned to the repository's own slug; `post_merge` is a bounded
//! `git merge-base --is-ancestor`. Read-only.
//! Test: `core::worktree_ledger::reclaim_probe::tests`.

use std::path::Path;

use serde::Deserialize;

use super::reclaim_plan::{HeadAnswer, PostMerge, ReclaimProbe};
use super::stale_probe::{OFFLINE_REASON, is_branch_name};
use crate::session_manager::git_ceiling::bounded_git_output;
use crate::session_manager::worktree_reclaim_gh::{
    GH_TIMEOUT, gh_command, resolve_daemon_gh_env, run_with_timeout,
};
use crate::session_manager::worktree_reclaim_gh_gate;
use crate::session_manager::worktree_repo_slug::repo_slug_for;
use crate::session_manager::worktree_safety::git_command;

/// [`ReclaimProbe`] over git and `gh`.
///
/// Test: `head_of_reads_branch_detached_and_unanswerable`,
/// `post_merge_reads_ancestry_from_git`, `merged_head_parses_only_a_merged_same_repo_pr`.
#[derive(Debug, Default)]
pub struct GitReclaimProbe {
    /// `tm worktrees --no-fetch`: no `gh` call.
    offline: bool,
}

impl GitReclaimProbe {
    /// A probe that makes no `gh` call (`tm worktrees --no-fetch`).
    pub fn offline() -> Self {
        Self { offline: true }
    }
}

/// The exit code of a bounded `git -C <dir> <args>`, and its stdout.
fn git_exit(dir: &Path, args: &[&str]) -> Result<(i32, String), String> {
    let out = bounded_git_output(git_command(dir, args))
        .map_err(|e| format!("`git {}` could not be run: {e}", args.join(" ")))?;
    let code = out
        .status
        .code()
        .ok_or_else(|| format!("`git {}` was killed ({})", args.join(" "), out.status))?;
    Ok((
        code,
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// One row of `gh pr view --json headRefOid,state,isCrossRepository`.
#[derive(Debug, Deserialize)]
struct PrHeadRow {
    #[serde(rename = "headRefOid")]
    head_ref_oid: String,
    state: String,
    #[serde(rename = "isCrossRepository")]
    is_cross_repository: bool,
}

/// The head commit of a merged, same-repository pull request, from `gh`'s JSON.
///
/// Why: only a merged PR of this repository vouches for the branch, and the
/// commit lands in git's argv, so it must be a full object id.
/// Test: `merged_head_parses_only_a_merged_same_repo_pr`.
pub(super) fn parse_merged_head(json: &str) -> Result<String, String> {
    let row: PrHeadRow =
        serde_json::from_str(json).map_err(|e| format!("`gh pr view` reply unparsable: {e}"))?;
    if row.state != "MERGED" {
        return Err(format!("the pull request is {}, not merged", row.state));
    }
    if row.is_cross_repository {
        return Err("the pull request comes from a fork".into());
    }
    let oid = row.head_ref_oid.trim();
    if !matches!(oid.len(), 40 | 64) || !oid.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("`{oid}` is not a commit id"));
    }
    Ok(oid.to_string())
}

impl ReclaimProbe for GitReclaimProbe {
    fn head_of(&self, tree: &Path) -> HeadAnswer {
        match git_exit(tree, &["symbolic-ref", "-q", "HEAD"]) {
            Ok((0, name)) if !name.is_empty() => HeadAnswer::Ref(name),
            Ok((1, _)) => HeadAnswer::Detached,
            Ok((code, _)) => {
                HeadAnswer::Unresolved(format!("`git symbolic-ref -q HEAD` exited {code}"))
            }
            Err(why) => HeadAnswer::Unresolved(why),
        }
    }

    fn merged_head(&self, repo: &Path, pr: u64) -> Result<String, String> {
        // #8994 S3a: `--no-fetch` makes no `gh` call.
        if self.offline {
            return Err(OFFLINE_REASON.into());
        }
        let slug = repo_slug_for(repo)?;
        let number = pr.to_string();
        let json = worktree_reclaim_gh_gate::shared()
            .poll(repo, &format!("pr-head:{pr}"), || {
                let gh_env = resolve_daemon_gh_env(repo, &slug)?;
                let identity = gh_env.describe();
                let mut cmd = gh_command(repo, &gh_env);
                cmd.args(["pr", "view", &number, "--repo", &slug, "--json"])
                    .arg("headRefOid,state,isCrossRepository");
                run_with_timeout(cmd, GH_TIMEOUT).map_err(|f| f.with_identity(identity))
            })
            .map_err(|f| format!("`gh pr view {pr}` failed: {f}"))?;
        parse_merged_head(&json)
    }

    fn post_merge(&self, tree: &Path, branch: &str, pr_head: &str) -> PostMerge {
        if !is_branch_name(branch) {
            return PostMerge::Unresolved(format!("`{branch}` is not a branch name"));
        }
        if pr_head.is_empty() || !pr_head.chars().all(|c| c.is_ascii_hexdigit()) {
            return PostMerge::Unresolved(format!("`{pr_head}` is not a commit id"));
        }
        let tip = format!("refs/heads/{branch}");
        match git_exit(tree, &["merge-base", "--is-ancestor", &tip, pr_head]) {
            Ok((0, _)) => PostMerge::NothingAfter,
            Ok((1, _)) => PostMerge::CommitsAfter,
            Ok((code, _)) => PostMerge::Unresolved(format!(
                "`git merge-base --is-ancestor {tip} {pr_head}` exited {code}"
            )),
            Err(why) => PostMerge::Unresolved(why),
        }
    }
}

#[cfg(test)]
#[path = "reclaim_probe_tests.rs"]
mod tests;
