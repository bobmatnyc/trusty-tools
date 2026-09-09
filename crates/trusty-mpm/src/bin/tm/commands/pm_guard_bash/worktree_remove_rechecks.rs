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
//! **A MERGED pull request is required, always (#7275 round 2).** Round 1 let
//! an empty merge-tree grant on its own: with no pull request in evidence, a
//! branch that was never pushed and carries one empty or self-reverting commit
//! cleared every re-check — clean by being clean, `unpushed-commits` by
//! reporting `NoUpstream`, `sole-owner` by holding no live claim — and the
//! guard deleted a tree GitHub had never seen. "Would landing this change
//! anything" is evidence of landing only once something is known to have
//! landed. [`landing_evidence`] now establishes that first, for the branch
//! itself or for its round-stripped sibling, and the merge-tree question is
//! asked only afterwards, against that pull request's own base.
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
//! `denies_worktree_remove_from_version_control_when_the_owner_query_fails`,
//! `an_empty_merge_tree_without_any_merged_pr_still_denies`,
//! `a_round_sibling_whose_related_pr_merged_is_reclaimable`,
//! `a_related_merged_pr_with_a_non_empty_merge_tree_denies_with_the_residue`,
//! `the_merge_tree_is_judged_against_the_merged_prs_own_base`
//! in `super::worktree_remove`.

use std::path::Path;

use trusty_mpm::core::worktree_removal_facts::{
    MergedPrLookup, UpstreamComparison, WorktreeRemovalProbe,
};

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
    // #7275 round 2: this arm no longer resolves anything by itself. Being
    // ahead of the upstream is not the same as holding work nobody has — `gh pr
    // merge --delete-branch` deletes the remote branch, so the tracking ref goes
    // stale on every squash-merged worktree and a strict reading refused five
    // clean, merged trees on 2026-09-09. But round 1 answered that by asking the
    // merge-tree question HERE, before any pull request was in evidence, which
    // let a never-pushed branch pass. The comparison is now carried to
    // [`landing_evidence`], which supplies the pull request first.
    let upstream = match probe.unpushed_commits(target) {
        Ok(c @ (UpstreamComparison::Ahead(_) | UpstreamComparison::NoUpstream)) => c,
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
    // #7232: when there is no upstream, a MERGED pull request is the ONLY
    // evidence the tree's commits reached the remote, so the deny says so
    // rather than leaving the operator to reconcile two silent re-checks.
    let no_upstream_note = if upstream == UpstreamComparison::NoUpstream {
        format!(
            " `{branch}` also tracks no upstream — `gh pr merge --delete-branch` deletes the \
             remote branch — so a MERGED pull request is the only remaining evidence that \
             these commits reached GitHub, and there is none."
        )
    } else {
        String::new()
    };
    let landed = match landing_evidence(target, &branch, probe, &no_upstream_note) {
        Ok(l) => l,
        Err(deny) => return Some(deny),
    };
    // #7232, unchanged: a branch whose OWN pull request merged has DIRECT
    // evidence its commits reached GitHub, so a clean tree grants whether or
    // not the merge deleted its upstream. The merge-tree question is a
    // RELAXATION, needed only where that direct evidence is missing — a branch
    // sitting ahead of an upstream that still resolves, or a round sibling
    // whose own name no pull request ever carried.
    let ahead = matches!(upstream, UpstreamComparison::Ahead(n) if n > 0);
    if landed.is_own && !ahead {
        return None;
    }
    residue_deny(target, &branch, &landed, upstream, probe)
}

/// A MERGED pull request that vouches for this worktree, and the base it merged
/// into (#7275 round 2).
///
/// Why: everything downstream of here relaxes the guard, so this is the fact
/// that must exist before any relaxation is reachable. Round 1 had no such
/// gate — a `count == 0` answer went straight to the merge-tree question — and
/// a branch GitHub had never seen passed it.
/// What: `is_own` when GitHub has a MERGED pull request for the checked-out
/// branch itself; otherwise the round-stripped stem is asked, which is how
/// `core::pr_cleanup::plan::is_pr_branch` relates `foo` and `foo-r2` and is the
/// only widening permitted. `base` is that pull request's own `baseRefName`,
/// as `core::pr_cleanup::unlanded` reads `view.base_ref_name`. Anything else —
/// no pull request, an unanswerable lookup, a merged row carrying no base — is
/// the deny, returned ready to hand back.
/// Test: `an_empty_merge_tree_without_any_merged_pr_still_denies`,
/// `a_round_sibling_whose_related_pr_merged_is_reclaimable`,
/// `the_merge_tree_is_judged_against_the_merged_prs_own_base`.
fn landing_evidence(
    target: &Path,
    branch: &str,
    probe: &dyn WorktreeRemovalProbe,
    no_upstream_note: &str,
) -> Result<Landed, String> {
    // #7232: the branch is named in every failure here. A failed lookup used to
    // quote only the probe's error, so a deny an operator had to act on did not
    // say which branch had been asked about.
    let own = probe
        .merged_pull_requests(target, branch)
        .map_err(|e| lookup_failed(target, branch, &e))?;
    if own.count > 0 {
        return base_of(target, branch, &own, true);
    }
    // #7275: a round-N sibling never carries a pull request of its own — the
    // review round lands on `<branch>-r2` and `version-control` pushes it onto
    // the PR's head name, so the sibling is left with no MERGED row that can
    // ever appear. Its own PR is the related one, so that one is asked for.
    let stem = trusty_mpm::core::pr_cleanup::plan::strip_round_suffix(branch);
    if stem != branch {
        let related = probe
            .merged_pull_requests(target, stem)
            .map_err(|e| lookup_failed(target, stem, &e))?;
        if related.count > 0 {
            return base_of(target, stem, &related, false);
        }
    }
    Err(recheck_deny(
        CHECK_MERGED_PULL_REQUEST,
        target,
        &format!(
            "GitHub has no MERGED pull request for `{branch}` in `{repo}` (resolved from this \
             worktree's `origin` remote){also}. Neither ancestry nor content-equivalence is an \
             acceptable substitute: a squash merge leaves the branch tip no ancestry \
             relationship to the squash commit, and a branch that was never pushed merges into \
             its base as a no-op exactly the way a landed one does. Open a pull request for \
             this branch, or reclaim the tree with `tm session prune-worktrees --merged-prs \
             --force`.{no_upstream_note}",
            repo = own.repo,
            also = if stem == branch {
                String::new()
            } else {
                format!(", nor for its round sibling `{stem}`")
            }
        ),
    ))
}

/// A confirmed MERGED pull request and the base it landed on.
struct Landed {
    /// The full ref to judge this worktree's content against.
    base_ref: String,
    /// The branch whose pull request this is — `branch` itself, or its stem.
    head: String,
    /// Whether that branch is the one this worktree has checked out.
    is_own: bool,
}

/// Read the confirmed pull request's base, or deny when it carries none.
///
/// Why (#7275 round 2, finding 2): the base used to be `origin/HEAD` resolved
/// locally, which judged a branch that merged into a release or stacked base
/// against the default branch instead. A merged row with no `baseRefName` is
/// the ADR-0045 undeterminable case, so it denies rather than falling back.
fn base_of(
    target: &Path,
    head: &str,
    lookup: &MergedPrLookup,
    is_own: bool,
) -> Result<Landed, String> {
    let base = lookup.base_ref.trim();
    if base.is_empty() {
        return Err(recheck_deny(
            CHECK_MERGED_PULL_REQUEST,
            target,
            &format!(
                "GitHub reported a MERGED pull request for `{head}` in `{repo}` but named no \
                 base branch for it, so which base this worktree's content should be judged \
                 against could not be established.",
                repo = lookup.repo
            ),
        ));
    }
    Ok(Landed {
        base_ref: format!("origin/{base}"),
        head: head.to_string(),
        is_own,
    })
}

/// Does this worktree still hold content the merge did not carry?
///
/// Why: with a MERGED pull request in hand, "would landing this change
/// anything" finally means what round 1 assumed it meant — an empty answer is
/// content the merge already carried. Asked here and nowhere else.
/// What: `None` grants; a non-empty merge, or a git that could not be asked,
/// denies and names the base it compared against. The check it denies under
/// follows the actual fault — `unpushed-commits` when the tree is genuinely
/// ahead of a live upstream, `merged-pull-request` when the upstream is gone
/// and only the merge state was ever going to answer.
/// Test: `a_related_merged_pr_with_a_non_empty_merge_tree_denies_with_the_residue`,
/// `denies_worktree_remove_from_version_control_when_commits_are_unpushed`,
/// `a_stale_upstream_still_denies_when_work_is_not_on_the_base`.
fn residue_deny(
    target: &Path,
    branch: &str,
    landed: &Landed,
    upstream: UpstreamComparison,
    probe: &dyn WorktreeRemovalProbe,
) -> Option<String> {
    let via = if landed.is_own {
        format!("`{branch}`'s own MERGED pull request")
    } else {
        format!(
            "the MERGED pull request for its round sibling `{}`",
            landed.head
        )
    };
    let ahead = match upstream {
        UpstreamComparison::Ahead(n) if n > 0 => Some(n),
        _ => None,
    };
    let check = if ahead.is_some() {
        CHECK_UNPUSHED_COMMITS
    } else {
        CHECK_MERGED_PULL_REQUEST
    };
    let base = &landed.base_ref;
    match probe.merge_into_base_is_a_noop(target, base) {
        Ok(true) => None,
        Ok(false) => Some(recheck_deny(
            check,
            target,
            &match ahead {
                Some(n) => format!(
                    "{n} commit(s) on HEAD are not on the upstream branch, and merging \
                     `{branch}` into {base} would still change files — that work is on no \
                     remote, even though {via} landed there. Inspect it with `git -C {dir} \
                     diff --name-only {base}...HEAD`.",
                    dir = target.display()
                ),
                None => format!(
                    "{via} landed on {base}, but merging `{branch}` into {base} would still \
                     change files — this tree holds work that merge did not carry. Inspect \
                     the residue with `git -C {dir} diff --name-only {base}...HEAD`.",
                    dir = target.display()
                ),
            },
        )),
        Err(e) => Some(recheck_deny(
            check,
            target,
            &format!(
                "{via} landed on {base}, and whether this tree's content is already there \
                 could not be established: {e}"
            ),
        )),
    }
}

/// The deny for a merged-PR lookup that did not answer at all.
fn lookup_failed(target: &Path, branch: &str, detail: &str) -> String {
    recheck_deny(
        CHECK_MERGED_PULL_REQUEST,
        target,
        &format!("the MERGED pull request lookup for `{branch}` did not answer: {detail}"),
    )
}
