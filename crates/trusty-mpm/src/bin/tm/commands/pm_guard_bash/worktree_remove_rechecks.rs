//! The four re-checks that gate `version-control`'s `git worktree remove`
//! ([ADR-0057](../../../../../../../docs/adr/0057-version-control-owns-worktree-removal.md)).
//!
//! Why: ADR-0057 turns #5791's blanket deny into a grant for one agent name,
//! and the grant would be worth less than the deny if the guard took the
//! agent's word for "this tree is merged and clean". So the guard establishes
//! every precondition itself. Split into its own module because
//! `worktree_remove` owns a different question — WHO is asking and WHICH
//! directory the command names — and folding both into one file would put the
//! 500-SLOC production cap in reach of the next addition to either.
//!
//! What: [`evaluate_removal_rechecks`] runs the four checks in cost order and
//! returns `Some(reason)` naming the first that did not pass. The scope check
//! (`e`) is not here: it is lexical, needs no subprocess, and runs in
//! `worktree_remove` before this module is reached, so an out-of-scope target
//! never costs a daemon round trip.
//!
//! **Every check fails CLOSED.** An `Err` from the probe means the fact could
//! not be established, which denies — the
//! [ADR-0045](../../../../../../../docs/adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md)
//! distinction, applied to a gate whose ALLOW deletes a checkout.
//!
//! Test: `allows_worktree_remove_from_version_control_on_clean_merged_unowned_tree`,
//! `denies_worktree_remove_from_version_control_when_tree_dirty`,
//! `denies_worktree_remove_from_version_control_when_no_merged_pr`,
//! `denies_worktree_remove_from_version_control_when_another_agent_holds_lock`
//! in `super::worktree_remove`.

use std::path::Path;

use trusty_mpm::core::worktree_removal_facts::WorktreeRemovalProbe;

/// The re-check names a deny quotes, so an agent can act on the refusal.
///
/// Why: "removal denied" sends the model round the loop again; "the tree has
/// 3 uncommitted files" tells it what to do. Named as constants so the deny
/// text and the tests cannot disagree about the spelling.
/// What: the `check` slug in [`recheck_deny`]'s message.
/// Test: each is asserted by the test that provokes its deny.
pub(crate) const CHECK_WORKTREE_SCOPE: &str = "worktree-scope";
/// See [`CHECK_WORKTREE_SCOPE`].
pub(crate) const CHECK_CLEAN_TREE: &str = "clean-tree";
/// See [`CHECK_WORKTREE_SCOPE`].
pub(crate) const CHECK_UNPUSHED_COMMITS: &str = "unpushed-commits";
/// See [`CHECK_WORKTREE_SCOPE`].
pub(crate) const CHECK_SOLE_OWNER: &str = "sole-owner";
/// See [`CHECK_WORKTREE_SCOPE`].
pub(crate) const CHECK_MERGED_PULL_REQUEST: &str = "merged-pull-request";
/// See [`CHECK_WORKTREE_SCOPE`] — the identity half, checked in
/// [`super::worktree_remove`] before any of these run.
pub(crate) const CHECK_DISPATCH_IDENTITY: &str = "dispatch-identity";

/// Build the deny text for a re-check that did not pass.
///
/// Why: one shape for all five, so a new check cannot ship a message that omits
/// the fallback or the reason. It names the check, the directory, the specific
/// finding, and the command that works when this path does not.
/// What: the `permissionDecisionReason` string.
/// Test: as the module doc.
pub(crate) fn recheck_deny(check: &str, target: &Path, detail: &str) -> String {
    format!(
        "Worktree removal denied — ADR-0057 re-check `{check}` did not pass for {}: {detail} \
         `version-control` may remove a worktree directly only when the guard can establish, \
         itself, that the target is a harness worktree, that it holds no uncommitted or \
         untracked files and no unpushed commits, that its branch has a MERGED pull request on \
         GitHub, and that no other live agent or managed session holds it. A fact the guard \
         cannot establish is never read as absent. Use \
         `tm session prune-worktrees --merged-prs --force` instead — it reports every tree it \
         spared and why. `rm -rf` on the directory is not the workaround: it destroys unsaved \
         work and leaves a registry entry git still believes in.",
        target.display()
    )
}

/// Run the four subprocess-backed re-checks against `target`.
///
/// Why: the whole safety case for ADR-0057. Each answer comes from git, GitHub
/// or the daemon rather than from the calling agent, which is what makes the
/// grant narrower than the deny it replaces.
/// What: `None` when every check passed and the removal may proceed;
/// `Some(reason)` naming the first that did not. Order is by cost — the two
/// local git questions, then the daemon answer the caller already has in hand,
/// then the network call to GitHub — so a dirty tree never pays for a `gh`
/// round trip.
///
/// `live_owners` is passed in rather than queried here because the daemon call
/// is async and this policy is not; the caller makes it through the same
/// `live_shared_tree_writers` route ADR-0048 decision 10's HEAD-move rule uses,
/// so both rules read one answer built one way.
/// Test: as the module doc.
pub(crate) fn evaluate_removal_rechecks(
    target: &Path,
    live_owners: &[String],
    probe: &dyn WorktreeRemovalProbe,
) -> Option<String> {
    match probe.dirty_entries(target) {
        Ok(0) => {}
        Ok(n) => {
            return Some(recheck_deny(
                CHECK_CLEAN_TREE,
                target,
                &format!(
                    "`git status --porcelain` reports {n} uncommitted or untracked entr(ies)."
                ),
            ));
        }
        Err(e) => return Some(recheck_deny(CHECK_CLEAN_TREE, target, &e)),
    }

    match probe.unpushed_commits(target) {
        Ok(0) => {}
        Ok(n) => {
            return Some(recheck_deny(
                CHECK_UNPUSHED_COMMITS,
                target,
                &format!("{n} commit(s) on HEAD are not on the upstream branch."),
            ));
        }
        Err(e) => return Some(recheck_deny(CHECK_UNPUSHED_COMMITS, target, &e)),
    }

    if !live_owners.is_empty() {
        return Some(recheck_deny(
            CHECK_SOLE_OWNER,
            target,
            &format!(
                "the daemon reports {} still writing here: {}.",
                live_owners.len(),
                live_owners.join(", ")
            ),
        ));
    }

    let branch = match probe.branch(target) {
        Ok(b) => b,
        Err(e) => return Some(recheck_deny(CHECK_MERGED_PULL_REQUEST, target, &e)),
    };
    match probe.merged_pull_requests(target, &branch) {
        Ok(0) => Some(recheck_deny(
            CHECK_MERGED_PULL_REQUEST,
            target,
            &format!(
                "GitHub has no MERGED pull request for `{branch}`. Ancestry is not an \
                 acceptable substitute — a squash merge leaves the branch tip no ancestry \
                 relationship to the squash commit."
            ),
        )),
        Ok(_) => None,
        Err(e) => Some(recheck_deny(CHECK_MERGED_PULL_REQUEST, target, &e)),
    }
}
