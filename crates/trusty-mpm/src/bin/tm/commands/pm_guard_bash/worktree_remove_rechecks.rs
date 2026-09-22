//! The re-checks that gate `version-control`'s `git worktree remove`
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
//! What: [`evaluate_removal_rechecks`] runs the subprocess-backed checks in cost order and
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
//! **A tree that holds no commit of its own needs no pull request (#7914).**
//! Requiring a MERGED pull request left three shapes with no removal route at
//! all: a worktree created off `origin/main` and never committed to, a branch
//! fast-forwarded into a sibling that landed, and a detached-HEAD tree whose
//! `branch` lookup cannot even produce a name to search by. None of them can
//! lose anything to a removal, because every commit they hold is on an `origin`
//! ref already. [`evaluate_removal_rechecks`] therefore asks
//! [`WorktreeRemovalProbe::local_only_commits`] once the two ownership gates
//! have passed, and grants on a literal zero.
//!
//! That admission is a RELAXATION, so it inherits the fail-closed rule in the
//! opposite direction: only `Ok(0)` admits. A non-zero count and an `Err` both
//! fall through to the merged-PR route unchanged, which is why an unanswerable
//! `rev-list` cannot cost a removal that a merged pull request would still have
//! granted. It also does not reopen the #7275 round-2 hole: the never-pushed
//! branch holding one empty commit holds a commit no `origin` ref has, so it
//! counts one and still denies. The probe refreshes `origin` before it counts
//! (critic round 1), so a branch deleted on GitHub behind this worktree's back
//! stops vouching for its own commits.
//!
//! **A detached HEAD is asked about by COMMIT (#7832).** #7914 admitted the
//! detached tree that holds no local-only commit; the one parked on a merged
//! pull request's own head still held one, so it fell through to
//! [`WorktreeRemovalProbe::branch`] and was refused for having no name. That is
//! a refusal about identity, not about work: `.claude/worktrees/review-7751` at
//! `02a83032d` was denied while PR #7794 had already merged it.
//! [`detached_head_verdict`] now asks GitHub which MERGED pull request was
//! opened from that exact commit. The match is exact and one-directional, and
//! every other answer — an unresolvable HEAD, a search that did not answer, a
//! search that found nothing — denies.
//!
//! **An exact head-sha match outranks the merge-tree comparison (#7958).**
//! `gh pr merge` leaves `@{upstream}` stale rather than level, so `Ahead(n)` is
//! what a landed worktree reports and the `landed.is_own && !ahead`
//! short-circuit never fired for one. Evaluation fell through to
//! [`residue_deny`], whose `merge_into_base_is_a_noop` reported residue for a
//! tree holding none — PR #7946's worktree, sitting on head `9c8699fe0`, was
//! refused that way. [`head_is_the_merged_pr_head`] answers first: a worktree
//! whose HEAD IS the commit the pull request merged holds nothing that merge did
//! not carry, whatever a tracking ref says.
//!
//! **Landed content admits where no pull request ever can (#7889).** A donor
//! branch fast-forwarded onto a sibling's head and squash-merged under that
//! name never acquires a MERGED row of its own — not now, not later — so
//! [`landing_evidence`]'s refusal was permanent for nineteen clean trees across
//! 2026-09-21 and 2026-09-22, every file of which was byte-identical on
//! `origin/main`. Owner ruling 2026-09-22: admit them.
//! [`landed_content_admission`] runs after that refusal, refreshes `origin` and
//! asks whether merging HEAD into the landing base would change any file.
//!
//! That supersedes half of #7275 round 2. The never-pushed branch holding one
//! empty or self-reverting commit IS now admitted — not because the guard
//! stopped requiring evidence, but because the ruling makes the evidence
//! CONTENT rather than a pull request, and such a tree demonstrably holds none
//! the remote lacks. What round 2 established still stands everywhere else: the
//! admission is reached only after `clean-tree` and both ownership gates, only
//! when the lookup ANSWERED (an unanswerable `gh` call is
//! [`LandingFailure::Undeterminable`] and stops there), and only on a positive,
//! post-refresh comparison — a failed refresh, an unresolvable base, a
//! `merge-tree` conflict and a residual path all refuse.
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
//! `the_merge_tree_is_judged_against_the_merged_prs_own_base`,
//! `a_clean_tree_whose_commits_are_all_on_origin_needs_no_pull_request`,
//! `a_detached_head_holding_no_local_only_commit_is_reclaimable`,
//! `a_commit_no_origin_ref_has_still_denies_without_a_merged_pr`,
//! `a_dirty_tree_denies_even_when_no_commit_is_local_only`,
//! `a_live_owner_denies_even_when_no_commit_is_local_only`,
//! `an_unanswerable_local_only_count_never_admits`,
//! `a_head_sha_matching_the_merged_prs_own_head_grants_despite_a_stale_upstream`,
//! `a_head_that_is_not_the_merged_prs_head_still_denies_when_ahead`,
//! `an_unanswerable_head_sha_never_grants_an_ahead_worktree`,
//! `a_merged_pr_carrying_no_head_sha_never_grants_an_ahead_worktree`,
//! `a_detached_head_that_is_a_merged_prs_own_head_is_reclaimable`,
//! `a_detached_head_no_merged_pr_carries_still_denies`,
//! `a_detached_head_matched_to_a_pull_request_with_another_head_denies`,
//! `a_detached_head_matched_to_a_pull_request_with_no_head_denies`,
//! `an_unanswerable_commit_search_denies_a_detached_head`,
//! `an_unresolvable_head_sha_denies_a_detached_head`,
//! `worktree_7889_a_landed_tree_with_no_merged_pr_is_reclaimable`,
//! `worktree_7889_a_residual_path_denies_and_names_it`,
//! `worktree_7889_an_unestablished_landed_content_answer_never_grants`,
//! `worktree_7889_a_dirty_tree_denies_even_when_its_content_is_landed`,
//! `worktree_7889_a_live_owner_denies_even_when_its_content_is_landed`,
//! `worktree_7889_an_unanswerable_lookup_never_reaches_the_admission`
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
/// The #7914 admission's name — quoted in the merged-PR deny when it did NOT
/// apply, so a refusal says which of the two routes to landing evidence failed.
/// It is never a `check` slug in its own right: it can only grant.
pub(crate) const CHECK_LOCAL_ONLY_COMMITS: &str = "local-only-commits";
/// The #7889 admission's name, spelled ONCE in
/// [`trusty_mpm::core::worktree_landed_content`] because the reclaim sweep
/// quotes the same slug. Like [`CHECK_LOCAL_ONLY_COMMITS`] it can only grant.
pub(crate) const CHECK_LANDED_CONTENT: &str =
    trusty_mpm::core::worktree_landed_content::LANDED_CONTENT_CHECK;
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

/// Run the subprocess-backed re-checks against `target`.
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
/// cannot. #7914: between the daemon answer and that network call sits the
/// second route to landing evidence — a tree holding no commit any `origin` ref
/// lacks grants there and never reaches GitHub at all. #7832: a branch lookup
/// that fails no longer ends the evaluation; a detached HEAD is asked about by
/// commit in [`detached_head_verdict`]. #7958: a branch whose own pull request
/// merged THIS commit grants before the upstream comparison is weighed at all.
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

    // #7914: the admission for a tree GitHub never saw a pull request for.
    // Asked AFTER both ownership gates — a live owner and unsaved work still
    // decide first — and BEFORE the branch lookup, so a detached HEAD, which
    // cannot produce a name to search GitHub by, reaches it at all.
    let local_only = probe.local_only_commits(target);
    if local_only.as_ref().is_ok_and(|n| *n == 0) {
        return None;
    }
    let local_only_note = local_only_note(&local_only);

    let branch = match probe.branch(target) {
        Ok(b) => b,
        // #7832: a detached checkout has no name to search GitHub by, which is
        // not the same as having no landing evidence. Ask by COMMIT before
        // giving up on it.
        Err(e) => return detached_head_verdict(target, probe, &e, &local_only_note),
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
    // #7914: both notes ride on the same deny — "no pull request" is only half
    // the refusal once a second route to landing evidence exists.
    let landed = match landing_evidence(
        target,
        &branch,
        probe,
        &format!("{no_upstream_note}{local_only_note}"),
    ) {
        Ok(l) => l,
        // #7889: GitHub ANSWERED, and has no merged pull request for this
        // branch or its stem. That is the donor-branch shape — the content
        // landed under a sibling's name — so the third route to landing
        // evidence is asked before the refusal stands.
        Err(LandingFailure::NoMergedPr(deny)) => {
            return landed_content_admission(target, probe, deny);
        }
        // A lookup that did not answer establishes nothing, and an admission
        // is never reached from an unestablished fact (ADR-0045).
        Err(LandingFailure::Undeterminable(deny)) => return Some(deny),
    };
    // #7232, unchanged: a branch whose OWN pull request merged has DIRECT
    // evidence its commits reached GitHub, so a clean tree grants whether or
    // not the merge deleted its upstream. The merge-tree question is a
    // RELAXATION, needed only where that direct evidence is missing — a branch
    // sitting ahead of an upstream that still resolves, or a round sibling
    // whose own name no pull request ever carried.
    // #7958: an exact head-sha match settles it outright. The pull request
    // merged THIS commit, so nothing here is off a remote and nothing the merge
    // did not carry can remain. The upstream comparison cannot weaken that:
    // `gh pr merge` leaves the tracking ref stale rather than level, so `Ahead`
    // is what a landed worktree reports, and reading it as unfinished work sent
    // every such tree to `merge_into_base_is_a_noop` — which answered "residue"
    // for a tree holding none.
    if landed.is_own && head_is_the_merged_pr_head(target, &landed, probe) {
        return None;
    }
    let ahead = matches!(upstream, UpstreamComparison::Ahead(n) if n > 0);
    if landed.is_own && !ahead {
        return None;
    }
    residue_deny(target, &branch, &landed, upstream, probe)
}

/// Is this worktree parked on exactly the commit its pull request merged
/// (#7958)?
///
/// Why: the strongest landing evidence the guard can hold, and the only one a
/// moved base or a stale `@{upstream}` cannot distort. It is a RELAXATION, so
/// it inherits #7914's rule in the same direction: only a positive, exact match
/// grants.
/// What: true when GitHub named a head commit for the merged pull request AND
/// git resolved this worktree's HEAD to the same object id. A pull request that
/// carried no `headRefOid`, a HEAD git could not resolve, and a mismatch all
/// answer false, which leaves the pre-#7958 decision — the `!ahead`
/// short-circuit, then [`residue_deny`] — exactly as it was.
/// Test: `a_head_sha_matching_the_merged_prs_own_head_grants_despite_a_stale_upstream`,
/// `a_head_that_is_not_the_merged_prs_head_still_denies_when_ahead`,
/// `an_unanswerable_head_sha_never_grants_an_ahead_worktree`,
/// `a_merged_pr_carrying_no_head_sha_never_grants_an_ahead_worktree`.
fn head_is_the_merged_pr_head(
    target: &Path,
    landed: &Landed,
    probe: &dyn WorktreeRemovalProbe,
) -> bool {
    let pr_head = landed.head_sha.trim();
    if pr_head.is_empty() {
        return false;
    }
    match probe.head_sha(target) {
        Ok(head) => {
            let head = head.trim();
            !head.is_empty() && head.eq_ignore_ascii_case(pr_head)
        }
        // A HEAD git could not name proves nothing, and a relaxation that
        // cannot be established never grants (ADR-0045).
        Err(_) => false,
    }
}

/// The verdict for a worktree whose HEAD is detached (#7832).
///
/// Why: `branch` fails by design on a detached checkout, and the merged-PR
/// route used to end there — so a review worktree parked on a merged pull
/// request's head was refused for having no NAME, not for holding anything.
/// `.claude/worktrees/review-7751` at `02a83032d`, whose PR #7794 had merged,
/// is the observed shape. The commit is the identity a detached checkout still
/// has, and GitHub resolves it.
///
/// This is reached only AFTER `clean-tree`, `sole-owner` and the #7914
/// admission, so the tree is already known clean, unowned, and holding at least
/// one commit no `origin` ref has — which is what the merged pull request has
/// to vouch for.
/// What: `None` grants when the search reported a MERGED pull request AND that
/// pull request's own head commit is the one this worktree is sitting on — the
/// count alone never grants, so the exact-sha invariant holds even if the probe
/// behind it were to relax. Everything else denies under `merged-pull-request`:
/// an unresolvable HEAD, a commit search that did not answer, a search that
/// answered with no such pull request, and a pull request whose head is some
/// other commit. `branch_error` is git's own words for why there is no branch,
/// quoted so a deny on a NON-detached failure (a corrupt HEAD, an unreadable
/// repository) still says what git said.
/// Test: `a_detached_head_that_is_a_merged_prs_own_head_is_reclaimable`,
/// `a_detached_head_no_merged_pr_carries_still_denies`,
/// `a_detached_head_matched_to_a_pull_request_with_another_head_denies`,
/// `an_unanswerable_commit_search_denies_a_detached_head`,
/// `an_unresolvable_head_sha_denies_a_detached_head`.
fn detached_head_verdict(
    target: &Path,
    probe: &dyn WorktreeRemovalProbe,
    branch_error: &str,
    local_only_note: &str,
) -> Option<String> {
    let head = match probe.head_sha(target) {
        Ok(head) => head,
        Err(e) => {
            return Some(recheck_deny(
                CHECK_MERGED_PULL_REQUEST,
                target,
                &format!(
                    "{branch_error}, and the commit it points at could not be resolved either \
                     — {e} — so there is nothing left to look a pull request up \
                     by.{local_only_note}"
                ),
            ));
        }
    };
    let found = match probe.merged_pull_request_for_commit(target, &head) {
        Ok(found) => found,
        Err(e) => {
            return Some(recheck_deny(
                CHECK_MERGED_PULL_REQUEST,
                target,
                &format!(
                    "{branch_error}, and the MERGED pull request search for its commit \
                     `{head}` did not answer: {e}{local_only_note}"
                ),
            ));
        }
    };
    // #7832, critic round: the exact-sha invariant is enforced HERE, not only
    // inside the probe that produced the count. A `count` the policy trusts on
    // its own makes the whole grant rest on one unobserved line of a different
    // module, and a search hit that merely MENTIONS this commit would then
    // delete the tree holding it. The answer has to name the commit back.
    let pr_head = found.head_sha.trim();
    if found.count > 0 && pr_head.eq_ignore_ascii_case(head.trim()) && !pr_head.is_empty() {
        return None;
    }
    let mismatch = match (found.count, pr_head.is_empty()) {
        (0, _) => String::new(),
        (_, true) => " GitHub did report a MERGED pull request for that search, but named no \
                       head commit for it, so it cannot be shown to be this tree's."
            .to_string(),
        (_, false) => format!(
            " GitHub did report a MERGED pull request for that search, but its own head commit \
             is `{pr_head}` — a pull request that merely mentions a commit, or was opened from \
             a descendant of it, is not evidence that THIS tree's content landed."
        ),
    };
    Some(recheck_deny(
        CHECK_MERGED_PULL_REQUEST,
        target,
        &format!(
            "{branch_error}, and no MERGED pull request in `{repo}` was opened from its commit \
             `{head}` either (resolved from this worktree's `origin` remote).{mismatch} Check \
             the commit out on a branch and open a pull request for it, or reclaim the tree \
             with `tm session prune-worktrees --merged-prs --force`.{local_only_note}",
            repo = found.repo
        ),
    ))
}

/// Say why the #7914 admission did not apply, for the deny that follows it.
///
/// Why: with two routes to landing evidence, a refusal that names only the
/// missing pull request tells the agent half the story — and the other half is
/// the actionable one, because it names the commits that have to reach a remote
/// before this tree can go. Called only after `Ok(0)` has been ruled out, so
/// neither arm can describe a tree that was admitted.
/// What: a leading-space sentence appended to the merged-PR deny. The `Err` arm
/// quotes git's own failure, since an unanswerable count and a real local commit
/// call for different next steps.
/// Test: `a_commit_no_origin_ref_has_still_denies_without_a_merged_pr`,
/// `an_unanswerable_local_only_count_never_admits`.
fn local_only_note(local_only: &Result<usize, String>) -> String {
    match local_only {
        Ok(n) => format!(
            " ADR-0057's `{CHECK_LOCAL_ONLY_COMMITS}` admission does not apply either: {n} \
             commit(s) reachable from HEAD are on no `origin` remote-tracking ref, so this \
             worktree is the only place they exist. Push the branch, or open and land a pull \
             request for it."
        ),
        Err(e) => format!(
            " ADR-0057's `{CHECK_LOCAL_ONLY_COMMITS}` admission could not be established \
             either — {e} — and a fact the guard cannot establish never grants."
        ),
    }
}

/// Why [`landing_evidence`] could not produce a pull request (#7889).
///
/// Why: the two reasons lead to different next steps. GitHub answering "there
/// is no such pull request" is a FACT, and the #7889 admission is allowed to
/// ask a further question after it. A lookup that did not answer at all
/// establishes nothing, so nothing may be built on top of it (ADR-0045).
/// Collapsing both into one `String`, as this did before #7889, would have let
/// an unanswerable `gh` call reach the admission.
/// What: each variant carries the deny text, ready to hand back.
/// Test: `worktree_7889_a_landed_tree_with_no_merged_pr_is_reclaimable`,
/// `no_upstream_and_an_unanswerable_merged_pr_lookup_denies`.
enum LandingFailure {
    /// GitHub answered: no MERGED pull request for the branch or its stem.
    NoMergedPr(String),
    /// The lookup failed, or the merged row carried no base branch.
    Undeterminable(String),
}

/// The #7889 admission: is this tree's content already on its landing base?
///
/// Why: a donor branch fast-forwarded onto a sibling's head and squash-merged
/// under that name can never acquire a pull request of its own, so the
/// merged-PR refusal is permanent for a tree that holds nothing. Owner ruling
/// 2026-09-22 admits it. Reached ONLY after `clean-tree`, `sole-owner` and the
/// branch lookup have passed, and only for a genuine `version-control`
/// dispatch — [`super::worktree_remove`] settles the identity and scope halves
/// before this module runs at all.
/// What: `None` grants when
/// [`WorktreeRemovalProbe::landed_content`] reports the merge would change no
/// file. Everything else appends that verdict's own sentence — the first
/// residual path, or what could not be established — to the merged-PR deny and
/// refuses, so the refusal names both routes that failed.
/// Test: `worktree_7889_a_landed_tree_with_no_merged_pr_is_reclaimable`,
/// `worktree_7889_a_residual_path_denies_and_names_it`,
/// `worktree_7889_an_unestablished_landed_content_answer_never_grants`,
/// `worktree_7889_a_dirty_tree_denies_even_when_its_content_is_landed`,
/// `worktree_7889_a_live_owner_denies_even_when_its_content_is_landed`.
fn landed_content_admission(
    target: &Path,
    probe: &dyn WorktreeRemovalProbe,
    no_pr_deny: String,
) -> Option<String> {
    let verdict = probe.landed_content(target);
    if verdict.is_landed() {
        return None;
    }
    Some(format!("{no_pr_deny} {}", verdict.note()))
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
    // #7914: no longer the #7232 upstream sentence alone — the caller appends
    // why the local-only-commits admission did not apply too.
    notes: &str,
) -> Result<Landed, LandingFailure> {
    // #7232: the branch is named in every failure here. A failed lookup used to
    // quote only the probe's error, so a deny an operator had to act on did not
    // say which branch had been asked about.
    let own = probe
        .merged_pull_requests(target, branch)
        .map_err(|e| LandingFailure::Undeterminable(lookup_failed(target, branch, &e)))?;
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
            .map_err(|e| LandingFailure::Undeterminable(lookup_failed(target, stem, &e)))?;
        if related.count > 0 {
            return base_of(target, stem, &related, false);
        }
    }
    Err(LandingFailure::NoMergedPr(recheck_deny(
        CHECK_MERGED_PULL_REQUEST,
        target,
        &format!(
            "GitHub has no MERGED pull request for `{branch}` in `{repo}` (resolved from this \
             worktree's `origin` remote){also}. Ancestry is not an acceptable substitute: a \
             squash merge leaves the branch tip no ancestry relationship to the squash commit, \
             so `git merge-base --is-ancestor` and `git cherry` both answer \"not merged\" for \
             a tree that is safe to reclaim. Open a pull request for this branch, or reclaim \
             the tree with `tm session prune-worktrees --merged-prs --force`.{notes}",
            repo = own.repo,
            also = if stem == branch {
                String::new()
            } else {
                format!(", nor for its round sibling `{stem}`")
            }
        ),
    )))
}

/// A confirmed MERGED pull request and the base it landed on.
struct Landed {
    /// The full ref to judge this worktree's content against.
    base_ref: String,
    /// The branch whose pull request this is — `branch` itself, or its stem.
    head: String,
    /// Whether that branch is the one this worktree has checked out.
    is_own: bool,
    /// The commit that pull request's head branch pointed at (#7958). Empty
    /// when GitHub named none, which never grants — see
    /// [`head_is_the_merged_pr_head`].
    head_sha: String,
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
) -> Result<Landed, LandingFailure> {
    let base = lookup.base_ref.trim();
    if base.is_empty() {
        // #7889: UNDETERMINABLE, not "no pull request" — GitHub found one and
        // could not say where it landed, so no further admission may be built
        // on the answer.
        return Err(LandingFailure::Undeterminable(recheck_deny(
            CHECK_MERGED_PULL_REQUEST,
            target,
            &format!(
                "GitHub reported a MERGED pull request for `{head}` in `{repo}` but named no \
                 base branch for it, so which base this worktree's content should be judged \
                 against could not be established.",
                repo = lookup.repo
            ),
        )));
    }
    Ok(Landed {
        base_ref: format!("origin/{base}"),
        head: head.to_string(),
        is_own,
        // #7958: carried, never required — a row without one simply cannot
        // grant on the head-sha route.
        head_sha: lookup.head_sha.trim().to_string(),
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
