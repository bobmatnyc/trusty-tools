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
//! **Every check fails CLOSED, the daemon answer included.** An `Err` from the
//! probe means the fact could not be established, which denies. So does an
//! `Err` in `live_owners`: a daemon that is down, timed out, or replied
//! unparseably has proved nothing about who holds the tree, and an empty vec
//! from such a reply would read as "nobody". That is why this takes a
//! `Result` rather than a slice, and why the caller uses
//! `live_shared_tree_writers_or_deny` rather than the HEAD-move rule's
//! fail-open sibling. All of it is the
//! [ADR-0045](../../../../../../../docs/adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md)
//! distinction, applied to a gate whose ALLOW deletes a checkout.
//!
//! **A missing upstream does not short-circuit the merged-PR answer (#7232).**
//! `gh pr merge --delete-branch` deletes the remote branch, so `@{upstream}`
//! stops resolving on every squash-merged worktree. `unpushed-commits` used to
//! return the moment it could not resolve one, which denied every tree the
//! sanctioned merge flow produces before `merged-pull-request` — the check that
//! establishes those commits landed — was ever asked. The missing upstream is
//! now carried forward instead: a MERGED pull request plus a clean tree grants,
//! and no merged pull request still denies. Order is otherwise untouched, so
//! `clean-tree` still runs first and a dirty tree still denies regardless of
//! merge state.
//!
//! Test: `allows_worktree_remove_from_version_control_on_clean_merged_unowned_tree`,
//! `denies_worktree_remove_from_version_control_when_tree_dirty`,
//! `denies_worktree_remove_from_version_control_when_commits_are_unpushed`,
//! `denies_worktree_remove_from_version_control_when_no_merged_pr`,
//! `a_merged_pr_clears_a_worktree_whose_upstream_is_gone`,
//! `no_upstream_and_no_merged_pr_denies_and_names_the_branch`,
//! `no_upstream_and_an_unanswerable_merged_pr_lookup_denies`,
//! `a_dirty_tree_denies_even_when_merged_with_no_upstream`,
//! `denies_worktree_remove_from_version_control_when_another_agent_holds_lock`,
//! `denies_worktree_remove_from_version_control_when_the_owner_query_fails`
//! in `super::worktree_remove`.

use std::path::Path;

use trusty_mpm::core::worktree_removal_facts::{UpstreamComparison, WorktreeRemovalProbe};

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
/// round trip. #7232: `unpushed-commits` answering
/// [`UpstreamComparison::NoUpstream`] does NOT return; it is carried to the
/// merged-PR check, which then has to supply the evidence the missing upstream
/// cannot.
///
/// `live_owners` is passed in rather than queried here because the daemon call
/// is async and this policy is not; the caller makes it over the same
/// shared-tree route ADR-0048 decision 10's HEAD-move rule uses, through
/// [`crate::commands::pm_guard_dispatch::live_shared_tree_writers_or_deny`],
/// whose `Err` arm is what keeps an unanswered daemon from reading as an empty
/// owner set.
/// Test: as the module doc, plus
/// `denies_worktree_remove_from_version_control_when_the_owner_query_fails`.
pub(crate) fn evaluate_removal_rechecks(
    target: &Path,
    live_owners: Result<&[String], &str>,
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

    // #7232: a missing upstream no longer returns here. `gh pr merge
    // --delete-branch` deletes the remote branch, so it is missing on every
    // squash-merged worktree, and returning made the merged-PR check below —
    // the one that establishes the commits landed — unreachable.
    let upstream = match probe.unpushed_commits(target) {
        Ok(UpstreamComparison::Ahead(0)) => UpstreamComparison::Ahead(0),
        // #7275: being ahead of the upstream is not the same as holding work
        // nobody has. `gh pr merge --delete-branch` deletes the remote branch,
        // so the tracking ref goes stale on every squash-merged worktree and
        // this arm refused five clean, merged trees on 2026-09-09. What matters
        // is whether the content is on the base, which is asked directly.
        Ok(UpstreamComparison::Ahead(n)) => match probe.merge_into_base_is_a_noop(target) {
            Ok(true) => UpstreamComparison::Ahead(0),
            Ok(false) => {
                return Some(recheck_deny(
                    CHECK_UNPUSHED_COMMITS,
                    target,
                    &format!(
                        "{n} commit(s) on HEAD are not on the upstream branch, and merging \
                         HEAD into the base would still change files — that work is on no \
                         remote. Inspect it with `git -C {dir} diff --name-only $(git -C \
                         {dir} rev-parse --abbrev-ref origin/HEAD)...HEAD`.",
                        dir = target.display()
                    ),
                ));
            }
            Err(e) => {
                return Some(recheck_deny(
                    CHECK_UNPUSHED_COMMITS,
                    target,
                    &format!(
                        "{n} commit(s) on HEAD are not on the upstream branch, and whether \
                         their content is already on the base could not be established: {e}"
                    ),
                ));
            }
        },
        Ok(UpstreamComparison::NoUpstream) => UpstreamComparison::NoUpstream,
        // `UpstreamComparison` is `#[non_exhaustive]`, so a variant this build
        // does not know is reachable. It is a fact this policy cannot weigh,
        // which is the ADR-0045 undeterminable case: deny.
        Ok(_) => {
            return Some(recheck_deny(
                CHECK_UNPUSHED_COMMITS,
                target,
                "the upstream comparison returned an answer this build does not understand.",
            ));
        }
        Err(e) => return Some(recheck_deny(CHECK_UNPUSHED_COMMITS, target, &e)),
    };

    match live_owners {
        Ok([]) => {}
        Ok(owners) => {
            return Some(recheck_deny(
                CHECK_SOLE_OWNER,
                target,
                &format!(
                    "the daemon reports {} still writing here: {}.",
                    owners.len(),
                    owners.join(", ")
                ),
            ));
        }
        Err(detail) => {
            return Some(recheck_deny(
                CHECK_SOLE_OWNER,
                target,
                &format!(
                    "the daemon did not answer who holds this tree — {detail}. Silence is not \
                     an empty owner set, and a removal made on it would be the one this check \
                     exists to prevent. `tm doctor` reports an unhealthy daemon and \
                     `tm restart` clears it."
                ),
            ));
        }
    }

    let branch = match probe.branch(target) {
        Ok(b) => b,
        Err(e) => return Some(recheck_deny(CHECK_MERGED_PULL_REQUEST, target, &e)),
    };
    // #7232: when there is no upstream, this answer is the ONLY evidence the
    // tree's commits reached the remote, so the deny says so rather than
    // leaving the operator to reconcile two silent re-checks.
    let no_upstream_note = if upstream == UpstreamComparison::NoUpstream {
        format!(
            " `{branch}` also tracks no upstream — `gh pr merge --delete-branch` deletes the \
             remote branch — so a MERGED pull request is the only remaining evidence that \
             these commits reached GitHub, and there is none."
        )
    } else {
        String::new()
    };
    match probe.merged_pull_requests(target, &branch) {
        // #7057: the repository is named. "No merged pull request" is what a
        // lookup aimed at the WRONG repository says too, so the answer is
        // useless without knowing where it was asked.
        // #7275: a round-N sibling never carries a pull request of its own —
        // the review round lands on `<branch>-r2` and `version-control` pushes
        // it onto the PR's head name, so `<branch>` (or `<branch>-r2`) is left
        // with no MERGED row that can ever appear. Its content IS on the base,
        // which is the fact the pull-request question was standing in for, so
        // that fact is asked for directly before denying.
        Ok(lookup) if lookup.count == 0 => match probe.merge_into_base_is_a_noop(target) {
            Ok(true) => None,
            Ok(false) => Some(recheck_deny(
                CHECK_MERGED_PULL_REQUEST,
                target,
                &format!(
                    "GitHub has no MERGED pull request for `{branch}` in `{repo}` (resolved \
                     from this worktree's `origin` remote), and merging it into the base \
                     would still change files — it holds work no merge has carried. Ancestry \
                     is not an acceptable substitute — a squash merge leaves the branch tip \
                     no ancestry relationship to the squash commit. Inspect the residue with \
                     `git -C {dir} diff --name-only $(git -C {dir} rev-parse --abbrev-ref \
                     origin/HEAD)...HEAD`.{no_upstream_note}",
                    repo = lookup.repo,
                    dir = target.display()
                ),
            )),
            Err(e) => Some(recheck_deny(
                CHECK_MERGED_PULL_REQUEST,
                target,
                &format!(
                    "GitHub has no MERGED pull request for `{branch}` in `{repo}`, and whether \
                     its content is already on the base could not be established: {e}",
                    repo = lookup.repo
                ),
            )),
        },
        Ok(_) => None,
        // #7232: the branch is named here too. A failed lookup used to quote
        // only the probe's error, so a deny an operator had to act on did not
        // say which branch had been asked about.
        Err(e) => Some(recheck_deny(
            CHECK_MERGED_PULL_REQUEST,
            target,
            &format!("the MERGED pull request lookup for `{branch}` did not answer: {e}"),
        )),
    }
}
