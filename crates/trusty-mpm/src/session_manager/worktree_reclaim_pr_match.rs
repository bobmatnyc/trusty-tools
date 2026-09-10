//! Which MERGED pull request carried THIS worktree's work (#7267).
//!
//! Why: `tm session prune-worktrees --merged-prs --force` reclaimed 0 of 11
//! stale worktrees whose content was fully on `main`. Every lookup keyed on the
//! branch name git reports for the worktree, and none of the 11 branches was
//! the name its pull request was opened from — a review round renames
//! `<branch>` to `<branch>-r2`, and a re-cut branch takes a new name entirely
//! before the pull request opens. A merged pull request that carried the work
//! is landing evidence whatever the branch ended up being called, so the
//! lookup has to reach it by more than one route.
//!
//! What: [`resolve_landing`] is a three-rung ladder over the answer the branch
//! name already produced.
//!
//! 1. The branch's own pull request, from the caller. Any SETTLED answer —
//!    merged, open, closed-unmerged, or a failed lookup — is returned as-is;
//!    only `NoPr` and `Unknown` widen.
//! 2. The round-stem sibling, related by
//!    [`strip_round_suffix`] — the same equivalence `core::pr_cleanup::plan`
//!    uses to relate `foo` and `foo-r2`, and the same one the ADR-0057 removal
//!    guard's `landing_evidence` applies. Not a second matcher: that is the
//!    one implementation of "these two branch names are the same workstream".
//! 3. The head COMMIT. `gh pr list --state merged --search <sha>` finds the
//!    merged pull requests GitHub knows contain this worktree's HEAD, and one
//!    of them vouches for the tree when its `headRefOid` and that HEAD stand in
//!    an ancestor relationship either way round — the pull request was opened
//!    from this commit, from a descendant of it, or from an ancestor of it.
//!
//! **A widening rung may only ever produce `Merged`.** It never converts one
//! refusal into a different one: a rung that cannot answer — no repository, no
//! `gh`, a timeout, an unparseable reply, a commit git cannot resolve — leaves
//! rung 1's answer standing and logs the reason. Rung 1's answer is already a
//! refusal in every case that reaches here, so every failure path denies, which
//! is what ADR-0045 asks of a gate whose grant deletes a checkout. It also
//! keeps the operator-facing reason stable: a worktree with no pull request
//! still reports "no pull request found for this branch" when the network is
//! down, rather than a lookup failure that is really about the widening.
//!
//! The extra `gh` and `git` calls ride on `per_branch_fallback`, so the
//! `tm doctor` probe — which runs on a three-second budget and passes `false` —
//! pays for none of them. See [`resolve_with_index`].
//!
//! Test: `worktree_reclaim_pr_match_tests`.

use std::path::Path;

use serde::Deserialize;

use super::worktree_reclaim::{BranchPrState, PrIndex, pr_state_for_branch};
use super::worktree_reclaim_gh::{
    GH_TIMEOUT, gh_pr_list_command, resolve_daemon_gh_env, run_with_timeout,
};
use super::worktree_reclaim_gh_gate;
use super::worktree_repo_slug::repo_slug_for;
use super::worktree_safety::{git_command, git_stdout};
use crate::core::pr_cleanup::plan::strip_round_suffix;

/// How many merged pull requests one commit search may return.
///
/// Why: a commit is normally carried by one pull request; a handful is the
/// realistic ceiling for a commit that was cherry-picked around. A bound keeps
/// the ancestry probe below from turning into an unbounded run of `git` calls.
/// What: 20.
const COMMIT_SEARCH_LIMIT: usize = 20;

/// The `--json` field set the commit search asks `gh` for.
///
/// Why: `isCrossRepository` is requested for the reason `PR_JSON_FIELDS`
/// requests it — a fork's pull request says nothing about a local branch, and a
/// `gh` too old to report the field must fail the whole call rather than return
/// rows in which a fork is indistinguishable from this repository.
const COMMIT_SEARCH_JSON_FIELDS: &str = "number,headRefOid,isCrossRepository";

/// One MERGED pull request the commit search returned.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MergedPrHead {
    /// The pull request number.
    pub number: u64,
    /// The commit its head branch pointed at.
    #[serde(default, rename = "headRefOid")]
    pub head_ref_oid: String,
    /// True when that head branch lives in a FORK, not this repository.
    #[serde(default, rename = "isCrossRepository")]
    pub is_cross_repository: bool,
}

/// The facts [`resolve_landing`] needs from GitHub and git (#7267).
///
/// Why: a trait rather than four free functions so the ladder — the part that
/// decides what counts as landing evidence — is a pure function with unit
/// tests, exactly as the ADR-0057 guard's `WorktreeRemovalProbe` makes its
/// policy testable. The production implementation is [`GhLandingProbe`]; the
/// tests drive a fake.
/// What: every method that can fail returns `Err`, and the ladder treats an
/// `Err` as "this rung did not answer", never as "there is nothing there".
/// Test: `worktree_reclaim_pr_match_tests`.
pub(crate) trait LandingProbe {
    /// What GitHub says about the pull requests opened from `branch`.
    fn state_for_head(&self, registry_root: &Path, branch: &str) -> BranchPrState;
    /// The commit `worktree`'s HEAD points at, as a full object id.
    fn head_commit(&self, worktree: &Path) -> Result<String, String>;
    /// The MERGED pull requests GitHub reports as containing `sha`.
    fn merged_prs_containing(
        &self,
        registry_root: &Path,
        sha: &str,
    ) -> Result<Vec<MergedPrHead>, String>;
    /// Is `ancestor` an ancestor of — or the same commit as — `descendant`?
    fn is_ancestor(
        &self,
        worktree: &Path,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool, String>;
}

/// Widen a branch-name lookup to the pull request that actually carried the
/// work (#7267).
///
/// Why: the module doc's whole subject. A renamed or re-cut branch has landing
/// evidence that its NAME cannot reach.
/// What: rung 1's `exact` answer unless it is `NoPr` or `Unknown`, in which
/// case the round-stem sibling and then the head commit are asked. Only a
/// `Merged` answer from a widening rung replaces `exact`; everything else —
/// including every error — leaves it standing, so no path here can turn a
/// refusal into a grant without positive evidence of a merge.
/// Test: `a_round_sibling_with_a_merged_pr_is_reclaimable`,
/// `a_detached_worktree_can_still_match_by_head_commit`,
/// `a_renamed_branch_matches_the_pr_opened_from_its_head_commit`,
/// `no_merged_pr_under_any_stem_is_still_refused`,
/// `an_open_pr_on_the_branch_is_never_widened`,
/// `a_failed_head_commit_read_leaves_the_refusal_standing`,
/// `a_failed_commit_search_leaves_the_refusal_standing`,
/// `a_failed_ancestry_probe_leaves_the_refusal_standing`.
pub(crate) fn resolve_landing(
    worktree: &Path,
    registry_root: &Path,
    branch: Option<&str>,
    exact: BranchPrState,
    probe: &dyn LandingProbe,
) -> BranchPrState {
    // Rung 1: a settled answer about this branch is the answer. An OPEN pull
    // request on the branch itself is work in flight, and widening past it
    // would be the one mistake this ladder must not make.
    if !matches!(exact, BranchPrState::NoPr | BranchPrState::Unknown) {
        return exact;
    }
    if let Some(pr) = merged_round_sibling(registry_root, branch, probe) {
        return pr;
    }
    match merged_by_head_commit(worktree, registry_root, probe) {
        Some(pr) => pr,
        None => exact,
    }
}

/// Rung 2 — the MERGED pull request of this branch's round-stem sibling.
///
/// Why (#7267, and #7275 for the guard's half of the same problem): a review
/// round lands on `<branch>-r2` while the pull request keeps the name it was
/// opened from, so neither name finds the other by exact match. Both spellings
/// reduce to one stem, and asking GitHub for the OTHER spelling is one call.
/// What: `Some(Merged)` when the sibling's own lookup reports a merge; `None`
/// for every other answer, the failed lookup included — see the module doc on
/// why a rung never returns a different refusal. Only branches whose name
/// actually differs from their stem cost a call, and the reverse direction (a
/// stem worktree whose pull request was opened from `-r2`) is answered by
/// `PrIndex::state_for`'s own sibling scan without a network call at all.
/// Test: `a_round_sibling_with_a_merged_pr_is_reclaimable`,
/// `a_round_sibling_whose_stem_pr_is_open_is_not_widened`.
fn merged_round_sibling(
    registry_root: &Path,
    branch: Option<&str>,
    probe: &dyn LandingProbe,
) -> Option<BranchPrState> {
    let branch = branch?;
    let stem = strip_round_suffix(branch);
    if stem == branch {
        return None;
    }
    match probe.state_for_head(registry_root, stem) {
        BranchPrState::Merged { pr } => Some(BranchPrState::Merged { pr }),
        other => {
            tracing::debug!(
                branch = %branch,
                stem = %stem,
                state = ?other,
                "worktree-reclaim: the round-stem sibling carries no merged pull \
                 request either (#7267)"
            );
            None
        }
    }
}

/// Rung 3 — the MERGED pull request GitHub reports as containing this HEAD.
///
/// Why: a branch that was re-cut under a new name before its pull request
/// opened shares no name with it, and no stem relates them. The COMMIT does:
/// GitHub's own search resolves a full object id to the pull requests carrying
/// it, which is the evidence the manual salvage in #7267 reconstructed by hand
/// for all 11 worktrees.
/// What: `Some(Merged)` for the first returned pull request whose `headRefOid`
/// and this worktree's HEAD stand in an ancestor relationship either way round
/// — the pull request was opened from this commit, from a descendant of it, or
/// from an ancestor of it. A fork's row is skipped for the reason
/// `PrIndex::from_json` skips it. `None` on every failure, and on a search that
/// returned nothing this tree's HEAD belongs to.
///
/// Content this tree holds BEYOND the matched pull request's head is not this
/// gate's to catch and is not let through by it: `classify` gate 6 runs
/// `inspect_dirt` afterwards, whose unpushed-commit count subtracts
/// patch-equivalent commits (#6507) and refuses anything genuinely novel.
/// Test: `a_renamed_branch_matches_the_pr_opened_from_its_head_commit`,
/// `a_fork_pull_request_containing_the_commit_is_ignored`,
/// `an_unrelated_merged_pr_containing_no_ancestor_is_not_a_match`,
/// `a_failed_ancestry_probe_leaves_the_refusal_standing`.
fn merged_by_head_commit(
    worktree: &Path,
    registry_root: &Path,
    probe: &dyn LandingProbe,
) -> Option<BranchPrState> {
    let head = match probe.head_commit(worktree) {
        Ok(head) if !head.trim().is_empty() => head.trim().to_string(),
        Ok(_) => {
            tracing::debug!(
                path = %worktree.display(),
                "worktree-reclaim: this worktree named no HEAD commit, so the \
                 commit search cannot run (#7267)"
            );
            return None;
        }
        Err(e) => {
            tracing::warn!(
                path = %worktree.display(),
                "worktree-reclaim: the HEAD commit could not be read, so the merged \
                 pull request carrying this tree cannot be searched for — the \
                 branch-name answer stands (#7267): {e}"
            );
            return None;
        }
    };
    let rows = match probe.merged_prs_containing(registry_root, &head) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                path = %worktree.display(),
                head = %head,
                "worktree-reclaim: the merged-pull-request commit search did not \
                 answer — the branch-name answer stands (#7267): {e}"
            );
            return None;
        }
    };
    for row in rows {
        // #7267: a fork's pull request from a branch that happens to contain a
        // commit of this name is not this repository's landing evidence.
        if row.is_cross_repository {
            continue;
        }
        let oid = row.head_ref_oid.trim();
        if oid.is_empty() {
            continue;
        }
        if vouches_for_head(worktree, oid, &head, row.number, probe) {
            tracing::info!(
                path = %worktree.display(),
                pr = row.number,
                head = %head,
                "worktree-reclaim: this worktree's HEAD is carried by a merged pull \
                 request opened from a different branch name (#7267)"
            );
            return Some(BranchPrState::Merged { pr: row.number });
        }
    }
    None
}

/// Does the pull request whose head sat on `oid` vouch for `head`?
///
/// Why: the two commits can relate in either direction and both are evidence.
/// Equal means the pull request was opened from exactly this commit; `oid`
/// descended from `head` means everything here is inside what merged; `head`
/// descended from `oid` means the merge carried a prefix of this tree, and
/// `classify` gate 6 refuses whatever is left over.
/// What: true when the two commits are the same, and otherwise when either
/// ancestry holds. An ancestry probe that ERRORS, which is what an object this
/// checkout does not have looks like, answers false: the rung then finds no
/// match and the branch-name refusal stands.
/// Test: `a_renamed_branch_matches_the_pr_opened_from_its_head_commit`,
/// `an_unrelated_merged_pr_containing_no_ancestor_is_not_a_match`,
/// `a_failed_ancestry_probe_leaves_the_refusal_standing`.
fn vouches_for_head(
    worktree: &Path,
    oid: &str,
    head: &str,
    pr: u64,
    probe: &dyn LandingProbe,
) -> bool {
    // #7267: the commonest case — the pull request was opened from exactly this
    // commit — is settled by a string comparison, so it costs no subprocess and
    // no object this checkout might not hold.
    if oid.eq_ignore_ascii_case(head) {
        return true;
    }
    for (ancestor, descendant) in [(oid, head), (head, oid)] {
        match probe.is_ancestor(worktree, ancestor, descendant) {
            Ok(true) => return true,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    path = %worktree.display(),
                    pr = pr,
                    "worktree-reclaim: whether `{ancestor}` is an ancestor of \
                     `{descendant}` could not be established, so this pull request \
                     vouches for nothing (#7267): {e}"
                );
                return false;
            }
        }
    }
    false
}

/// The pull-request state for one surveyed worktree, index first (#7267).
///
/// Why: the survey and the pre-delete re-check both need the same answer, and
/// before this they each carried their own copy of the #6561 per-branch retry.
/// One function is what keeps the widening from reaching only half the
/// destructive path.
/// What: the bulk index's answer, then — only when `per_branch_fallback` is set
/// — the #6561 targeted retry for a branch a truncated or failed index could
/// not resolve, then [`resolve_landing`]. With `per_branch_fallback` clear the
/// index's answer is returned unchanged, which is what keeps the `tm doctor`
/// probe inside its three-second budget.
/// Test: `resolve_with_index_without_fallback_never_calls_the_probe`,
/// `resolve_with_index_retries_a_truncated_index_per_branch`,
/// `survey_reclaims_a_round_sibling_of_a_merged_pr`.
pub(crate) fn resolve_with_index(
    worktree: &Path,
    registry_root: &Path,
    branch: Option<&str>,
    index: &PrIndex,
    per_branch_fallback: bool,
    probe: &dyn LandingProbe,
) -> BranchPrState {
    let mut pr = index.state_for(branch);
    if !per_branch_fallback {
        return pr;
    }
    // #6561: a truncated or FAILED bulk lookup retries per-branch. The bulk
    // call and the targeted one can fail for different reasons (a page limit is
    // not an auth failure), and only the targeted one resolves a branch older
    // than the bulk window.
    if matches!(
        pr,
        BranchPrState::Unknown | BranchPrState::LookupFailed { .. }
    ) && !index.is_complete()
        && let Some(branch) = branch
    {
        pr = probe.state_for_head(registry_root, branch);
    }
    resolve_landing(worktree, registry_root, branch, pr, probe)
}

/// The production [`LandingProbe`] — real `gh`, real `git` (#7267).
///
/// Why: every call it makes goes through the module that already owns that
/// capability — `worktree_reclaim::pr_state_for_branch` for a branch's
/// pull requests, `worktree_reclaim_gh` for spawning `gh` under the resolved
/// identity and the hang gate, `worktree_safety::git_command` for a `git` that
/// ambient config cannot steer. Nothing here re-implements one of those.
/// What: the four probe methods, each failing with a string the ladder logs.
/// Test: exercised through the reclaim sweep; the ladder's own decisions are
/// unit-tested against a fake in `worktree_reclaim_pr_match_tests`.
pub(crate) struct GhLandingProbe;

impl LandingProbe for GhLandingProbe {
    fn state_for_head(&self, registry_root: &Path, branch: &str) -> BranchPrState {
        pr_state_for_branch(registry_root, branch)
    }

    fn head_commit(&self, worktree: &Path) -> Result<String, String> {
        git_stdout(worktree, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string())
    }

    fn merged_prs_containing(
        &self,
        registry_root: &Path,
        sha: &str,
    ) -> Result<Vec<MergedPrHead>, String> {
        // #7057: the repository comes from this root's own `origin`, never from
        // whatever `gh` would infer at this working directory.
        let repo = repo_slug_for(registry_root)?;
        // #6867: through the same hang gate every other `gh` poll uses, under a
        // key naming the question — a commit search is not the per-branch query
        // and the two must never share a reply.
        let stdout = worktree_reclaim_gh_gate::shared()
            .poll(registry_root, &format!("merged-sha:{repo}:{sha}"), || {
                let gh_env = resolve_daemon_gh_env(registry_root);
                let mut cmd = gh_pr_list_command(registry_root, &gh_env, &repo);
                cmd.args(["--state", "merged", "--search", sha, "--limit"])
                    .arg(COMMIT_SEARCH_LIMIT.to_string())
                    .args(["--json", COMMIT_SEARCH_JSON_FIELDS]);
                run_with_timeout(cmd, GH_TIMEOUT)
            })
            .map_err(|f| format!("{f} (repository searched: {repo})"))?;
        serde_json::from_str(&stdout).map_err(|e| {
            format!("`gh pr list --repo {repo} --search {sha}` JSON did not parse: {e}")
        })
    }

    fn is_ancestor(
        &self,
        worktree: &Path,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool, String> {
        // `git merge-base --is-ancestor` answers by EXIT CODE — 0 yes, 1 no,
        // anything else a fault — so this is the one git call in the module
        // that cannot go through `git_stdout`, which reads a non-zero exit as
        // an error.
        let out = git_command(
            worktree,
            &["merge-base", "--is-ancestor", ancestor, descendant],
        )
        .output()
        .map_err(|e| format!("`git merge-base --is-ancestor` could not be run: {e}"))?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(format!(
                "`git merge-base --is-ancestor {ancestor} {descendant}` failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }
}

#[cfg(test)]
#[path = "worktree_reclaim_pr_match_tests.rs"]
mod worktree_reclaim_pr_match_tests;
