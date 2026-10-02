//! The ADR-0057 guard's ancestry route for a worktree that sits BEHIND its
//! merged pull request's head (#8849).
//!
//! Why: a review round's final commits land on a renamed `-rN` branch and are
//! pushed onto the pull request's head, so the worktree left on the earlier
//! round holds a HEAD that is an ancestor of what merged. Its branch name
//! still finds the MERGED pull request, but HEAD is not that pull request's
//! `headRefOid`, so the exact #7958 match does not fire, and the merge-tree
//! comparison that follows reads the later rounds' edits — and any later
//! `main` commit on the same files — as work this tree undid. Four clean trees
//! were refused that way on 2026-09-28 (PRs #8832, #8835, #8837, #8839).
//! Ancestry is what separates "behind the merged head" from "undid part of
//! it", which content alone cannot.
//!
//! What: [`head_inside_merged_pr`] grants only when all of these hold: GitHub
//! named a hex head commit AND a hex merge commit for the MERGED pull request,
//! `origin` was refreshed in this evaluation, HEAD is that head or one of its
//! ancestors, and the merge commit is on the refreshed base the pull request
//! merged into. That is route (c) of the 2026-09-22 owner ruling
//! (`core::worktree_carried_by_pr`), applied where the branch lookup already
//! found the pull request instead of only where it found none, and with the
//! merge itself proven on the base rather than taken from GitHub's word alone.
//! Only that one direction counts: a pull request whose head is BEHIND HEAD
//! leaves commits here the merge never saw.
//!
//! **Every other answer refuses (ADR-0045, ADR-0057 decision 6).** A missing
//! or non-hex head or merge commit, an unrefreshed `origin`, an unresolvable
//! HEAD, a "not an ancestor", and an ancestry probe that could not run each
//! return the sentence the refusal appends; none of them grants.
//! Test: `worktree_remove_ancestry_tests`.

use std::path::Path;

use trusty_mpm::core::worktree_carried_by_pr::MERGED_PR_ANCESTRY_CHECK;
use trusty_mpm::core::worktree_removal_facts::WorktreeRemovalProbe;

/// The merged pull request the ancestry route judges HEAD against (#8849).
pub(super) struct MergedHead<'a> {
    /// The pull request's own `headRefOid`.
    pub pr_head: &'a str,
    /// The squash or merge commit GitHub recorded for it.
    pub merge_commit: &'a str,
    /// The full ref it merged into, e.g. `origin/main`.
    pub base_ref: &'a str,
}

/// Is HEAD inside a MERGED pull request's head whose merge is on the base
/// (#8849)?
///
/// Why: see the module doc. It is a RELAXATION, so only a positive answer to
/// every question grants.
/// What: `Ok(())` grants. `Err(sentence)` names the first question that did
/// not answer yes; the caller appends it to the refusal it would have given
/// anyway. `refs_fresh` is whether `local_only_commits` answered in this
/// evaluation, which its production probe does only after refreshing `origin`.
/// Test: `worktree_8849_a_head_behind_the_merged_prs_head_is_reclaimable`,
/// `worktree_8849_a_commit_no_merged_head_carries_still_denies`,
/// `worktree_8849_a_merged_head_git_does_not_have_denies`,
/// `worktree_8849_a_merge_commit_not_on_the_base_denies`,
/// `worktree_8849_an_unrefreshable_origin_denies`.
pub(super) fn head_inside_merged_pr(
    target: &Path,
    merged: &MergedHead<'_>,
    refs_fresh: bool,
    probe: &dyn WorktreeRemovalProbe,
) -> Result<(), String> {
    let not = |why: String| {
        format!(" ADR-0057's `{MERGED_PR_ANCESTRY_CHECK}` route (#8849) does not apply: {why}.")
    };
    let pr_head = merged.pr_head.trim();
    let merge = merged.merge_commit.trim();
    if !is_hex_id(pr_head) || !is_hex_id(merge) {
        return Err(not(format!(
            "GitHub named no usable head commit (`{pr_head}`) or merge commit (`{merge}`) for \
             the merged pull request"
        )));
    }
    if !refs_fresh {
        return Err(not(
            "`origin` could not be refreshed, so its base cannot vouch for the merge".into(),
        ));
    }
    let head = probe
        .head_sha(target)
        .map_err(|e| not(format!("HEAD could not be resolved — {e}")))?;
    let head = head.trim();
    match probe.is_ancestor(target, head, pr_head) {
        Ok(true) => {}
        Ok(false) => {
            return Err(not(format!(
                "HEAD `{head}` is not the merged pull request's head `{pr_head}` or one of its \
                 ancestors, so it holds commits that pull request never carried"
            )));
        }
        Err(e) => {
            return Err(not(format!(
                "whether HEAD `{head}` is inside the merged head `{pr_head}` could not be \
                 established — {e}"
            )));
        }
    }
    match probe.is_ancestor(target, merge, merged.base_ref) {
        Ok(true) => Ok(()),
        Ok(false) => Err(not(format!(
            "the merge commit `{merge}` is not on `{}`",
            merged.base_ref
        ))),
        Err(e) => Err(not(format!(
            "whether the merge commit `{merge}` is on `{}` could not be established — {e}",
            merged.base_ref
        ))),
    }
}

/// A full or abbreviated git object id: non-empty ASCII hex.
fn is_hex_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "worktree_remove_ancestry_tests.rs"]
mod worktree_remove_ancestry_tests;
