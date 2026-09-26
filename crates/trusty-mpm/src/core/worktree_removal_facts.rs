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
//! What: [`WorktreeRemovalProbe`] is the questions the guard asks —
//! working-tree cleanliness, unpushed commits, the checked-out branch, whether
//! any commit here is on no `origin` ref (#7914), which commit HEAD points at
//! (#7832, #7958), and whether GitHub has a MERGED pull request for the branch
//! or for that commit. [`GitAndGhProbe`] answers them for real; a test
//! substitutes its own implementation and reaches no network.
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
//! comes from the target worktree's own remote
//! ([`crate::session_manager::worktree_repo_slug`]); a worktree whose
//! repository cannot be established denies rather than asking `gh` to guess,
//! and the deny message names the repository that WAS searched so a
//! wrong-repository answer is visible instead of reading as "no pull request".
//!
//! **Which REMOTE is the one the branch was pushed to (#7850).** `origin` is
//! not always that remote. In `breezeblue-ai/breeze-tts` local `main` tracks
//! `fork` (`bobmatnyc/breeze-tts`) because `origin` 403s for the operator's
//! account; `fix/matsuoka-respelling` merged as `bobmatnyc/breeze-tts#5`, and
//! the removal gate refused the worktree because it searched `origin` for a
//! pull request that had never been opened there. The lookup now resolves the
//! branch's push remote through git's own three keys
//! ([`crate::session_manager::worktree_repo_slug::push_remote_for_branch`]) and
//! searches THAT repository. It corrects which repository is asked and relaxes
//! nothing: a MERGED pull request is still required, and every way the
//! resolution can fail — no key set, git unaskable, a URL that will not parse —
//! lands on `origin` or on an `Err`, both of which are the pre-#7850 verdict.
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
//! **A missing upstream is a FACT, not a failure (#7232).** `gh pr merge
//! --delete-branch` — the sanctioned merge flow — deletes the remote branch, so
//! `@{upstream}` stops resolving on every squash-merged worktree. Reporting
//! that as `Err` made the guard deny exactly the trees ADR-0057 exists to let
//! `version-control` reclaim. [`UpstreamComparison::NoUpstream`] names the
//! condition instead, and the policy decides: it is not evidence the commits
//! are safe, so the merged-PR re-check still has to supply that evidence.
//!
//! **Content-equivalence is never landing evidence on its own (#7275 round
//! 2).** [`WorktreeRemovalProbe::content_on_base`] answers "would
//! landing this branch change anything", and a branch that was never pushed —
//! holding one empty or self-reverting commit — answers it exactly the way a
//! merged branch does. The policy therefore asks it only once a MERGED pull
//! request is in hand, and judges the content against THAT pull request's own
//! `baseRefName` rather than against `origin/HEAD`.
//!
//! **A commit no remote has is the fact the pull request stands in for
//! (#7914).** ADR-0057 gate 5 accepted exactly one proof that a worktree's
//! commits reached GitHub — a MERGED pull request — so a tree whose branch
//! never carried one had no removal route at all, however empty it was: a
//! worktree created off `origin/main` and never committed to, a branch
//! fast-forwarded into a sibling that landed, a detached-HEAD install tree.
//! [`WorktreeRemovalProbe::local_only_commits`] asks the underlying question
//! directly. It is a RELAXATION, so the policy admits only on a literal zero;
//! an `Err` and a non-zero count both leave the merged-PR route to decide.
//!
//! **A remote-tracking ref is a cache, and a cache is refreshed before it is
//! trusted (#7914, critic round 1).** `--remotes=origin` reads local refs, and
//! a branch deleted on GitHub by any route other than a fetch in this worktree
//! — `gh pr close --delete-branch`, the web UI, a second clone — leaves
//! `refs/remotes/origin/<branch>` naming a commit the remote no longer has.
//! The stale ref then vouched for the only surviving copy of that work, and
//! the admission destroyed it. A bounded `git fetch --prune origin` now runs
//! immediately before the count, under a timeout below the `PreToolUse` hook's
//! own, and a refresh that fails makes the count unanswerable.
//!
//! **A detached HEAD still has a commit, and a commit still has a pull request
//! (#7832).** [`WorktreeRemovalProbe::branch`] fails on a detached checkout by
//! design, so the merged-PR route could not run for a review worktree parked on
//! a merged pull request's head — `.claude/worktrees/review-7751` at `02a83032d`
//! was refused with "HEAD is detached" even though PR #7794 had merged it.
//! [`WorktreeRemovalProbe::head_sha`] and
//! [`WorktreeRemovalProbe::merged_pull_request_for_commit`] answer by COMMIT
//! instead, reusing `worktree_reclaim_pr_match`'s existing commit search. The
//! match is exact — this tree's HEAD must BE the pull request's `headRefOid` —
//! because nothing re-inspects the residue after this gate the way the sweep's
//! gate 6 does after its own ancestry-tolerant ladder.
//!
//! **The merged pull request's own head is a fact worth carrying (#7958).**
//! `MERGED_PR_ARGS` asks for `headRefOid` alongside the base, so the policy can
//! see that a worktree is sitting on exactly the commit that merged. Without it
//! a stale `@{upstream}` reporting `Ahead(4)` pushed the decision onto a
//! merge-tree comparison that reported residue for a tree holding none.
//!
//! **Landed content is landing evidence of its own (#7889).** A donor branch
//! fast-forwarded onto a sibling's head and squash-merged under THAT name
//! leaves GitHub with no pull request carrying the donor's own name, forever.
//! [`WorktreeRemovalProbe::landing_admission`] answers the two questions that
//! ruling (owner, 2026-09-22) turns on instead — (b) would merging this HEAD
//! into the landing base change any file, and (c) is HEAD inside the history
//! of a merged pull request's head — through
//! [`crate::core::worktree_landed_content`], which the reclaim sweep's gate 5
//! also calls. It is a RELAXATION, so only an affirmative answer admits. The
//! only ancestry consulted is (c)'s one direction; ancestry against the squash
//! commit is never evidence.
//!
//! Test: `merged_pull_request_argv_asks_github_for_the_branch`,
//! `detached_head_is_not_a_branch`,
//! `local_only_commits_counts_only_what_no_origin_ref_has`,
//! `local_only_commits_reprunes_a_branch_deleted_behind_this_worktrees_back`,
//! `local_only_commits_cannot_be_answered_when_origin_is_unreachable` in
//! `crate::session_manager::worktree_safety_tests`,
//! `the_harness_ownership_marker_alone_leaves_the_tree_clean`,
//! `a_real_untracked_file_beside_the_marker_still_counts`,
//! `a_branch_with_no_upstream_reports_no_upstream_not_an_error`,
//! `a_branch_with_an_upstream_counts_the_commits_it_is_ahead_by`,
//! `only_a_non_fork_row_whose_own_head_is_this_commit_matches`,
//! `head_sha_reads_the_full_oid_of_a_detached_checkout`,
//! `an_unoverridden_probe_establishes_neither_new_fact` below; the
//! policy that consumes these answers is tested in
//! `bin/tm/commands/pm_guard_bash/worktree_remove`.

use std::path::Path;

use crate::core::worktree_landed_content::{
    LandedContent, LandingAdmission, landing_admission, landing_admission_on_fetched_refs,
};
use crate::core::worktree_landed_history::ContentOnBase;
use crate::session_manager::worktree_landing_refresh::refresh_landing_refs_within;
use crate::session_manager::worktree_reclaim_gh::{
    GH_TIMEOUT, gh_pr_list_command, resolve_daemon_gh_env,
};
use crate::session_manager::worktree_repo_slug::{
    first_merged, merged_pr_search_repos, repo_slug_for,
};
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
/// #7275 round 2: `baseRefName` rides along, because the base a worktree's
/// content is judged against must be the one its pull request actually merged
/// into. Asking `origin/HEAD` instead judged every branch against the default
/// branch, which is the wrong answer for a stacked or release-branch PR.
/// #7958: `headRefOid` rides along too. A worktree sitting on exactly the commit
/// the pull request merged has the strongest landing evidence there is, and the
/// policy could not see it because nothing carried the pull request's own head.
const MERGED_PR_ARGS: &[&str] = &[
    "--state",
    "merged",
    "--json",
    "number,baseRefName,headRefOid",
    "--limit",
    "1",
];

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
    /// The `baseRefName` of the first MERGED pull request found, if any
    /// (#7275 round 2).
    ///
    /// Empty when `count` is 0, and — fail-closed — also when GitHub reported a
    /// pull request without one. The policy denies rather than substituting a
    /// base of its own, because "which base did this land on" is exactly the
    /// question a guess gets wrong for a stacked or release-branch PR.
    pub base_ref: String,
    /// The `headRefOid` of the first MERGED pull request found, if any (#7958).
    ///
    /// Why: a worktree whose HEAD IS this commit holds nothing the merge did
    /// not carry, which is stronger evidence than the merge-tree comparison a
    /// stale `@{upstream}` can trip. Empty when `count` is 0, and — fail-closed
    /// — also when GitHub reported a pull request without one, in which case
    /// the policy never grants on this route.
    pub head_sha: String,
}

impl MergedPrLookup {
    /// The answer `count` merged pull requests in `repo`, landing on `base_ref`.
    ///
    /// Why: the struct is `#[non_exhaustive]`, so the `tm` binary's fake probe
    /// — a different crate — cannot build one with a struct expression. The
    /// pull request's own head commit is added by
    /// [`with_head_sha`](Self::with_head_sha) rather than by a fourth parameter,
    /// so no existing caller has to restate a fact it does not care about.
    #[must_use]
    pub fn new(count: usize, repo: impl Into<String>, base_ref: impl Into<String>) -> Self {
        Self {
            count,
            repo: repo.into(),
            base_ref: base_ref.into(),
            head_sha: String::new(),
        }
    }

    /// The same answer, carrying the merged pull request's own head commit
    /// (#7958).
    ///
    /// Test: `a_head_sha_matching_the_merged_prs_own_head_grants_despite_a_stale_upstream`,
    /// `a_detached_head_matched_to_a_pull_request_with_another_head_denies` in
    /// `bin/tm/commands/pm_guard_bash/worktree_remove`.
    #[must_use]
    pub fn with_head_sha(mut self, head_sha: impl Into<String>) -> Self {
        self.head_sha = head_sha.into();
        self
    }
}

/// What `git rev-parse --abbrev-ref HEAD` prints for a detached HEAD.
const DETACHED_HEAD: &str = "HEAD";

/// The fail-closed default for the #7832/#7958 probe methods.
///
/// Why: they were added to an already-`pub` trait, so they carry default bodies
/// rather than breaking every outside implementor. The body has to be the
/// ADR-0045 undeterminable answer — an implementor that has not overridden it
/// establishes nothing, and nothing is never a grant.
const NOT_IMPLEMENTED: &str =
    "this `WorktreeRemovalProbe` implementation does not answer that question";

/// The rev-list argv behind [`WorktreeRemovalProbe::local_only_commits`] (#7914).
///
/// Why: named so the scope cannot drift. `--remotes=origin` is deliberately
/// narrower than the bare `--remotes`
/// [`crate::session_manager::worktree_safety`] uses for the sweep — ADR-0057's
/// guarantee is that the commits reached GITHUB, and `origin` is the remote
/// this module already resolves the repository slug from, so a local-path
/// remote someone added cannot vouch for a tree the guard is about to delete.
/// A repository whose GitHub remote is not named `origin` simply counts every
/// commit and is never admitted here, which is the fail-closed direction.
/// What: `git rev-list --count HEAD --not --remotes=origin`.
/// Test: `local_only_commits_counts_only_what_no_origin_ref_has` in
/// `crate::session_manager::worktree_safety_tests`.
pub(crate) const LOCAL_ONLY_COMMITS_ARGS: &[&str] =
    &["rev-list", "--count", "HEAD", "--not", "--remotes=origin"];

/// How long the admission's `git fetch --prune` may run (#7914, critic round 1).
///
/// Why: the fetch happens inside the `PreToolUse` hook, which
/// [`crate::core::standalone::hooks::mpm_hook_additions_with_exe`] registers
/// with a 5-second timeout. A hook Claude Code kills emits no decision at all,
/// and no decision is not a deny — so this bound must leave the guard time to
/// print one, rather than matching the reclaim sweep's 30 s. Three seconds is
/// roughly twice a warm incremental fetch over the network (measured 1.28 s
/// against github.com on this repository, 2026-09-15), and expiring costs only
/// the admission: the count becomes unanswerable, which falls through to the
/// merged-PR route.
/// Test: `local_only_commits_cannot_be_answered_when_origin_is_unreachable` in
/// `crate::session_manager::worktree_safety_tests`.
const ADMISSION_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// How HEAD compares to its upstream branch, when it still has one (#7232).
///
/// Why: "2 commits are unpushed" and "there is no upstream to compare against"
/// are different facts, and collapsing the second into `Err` denied every
/// worktree the sanctioned merge flow produces — `gh pr merge --delete-branch`
/// removes the remote branch, so `@{upstream}` stops resolving the moment the
/// pull request lands. `Err` keeps its meaning: git could not answer at all.
/// What: `Ahead(n)` is `git rev-list --count @{upstream}..HEAD`; `NoUpstream`
/// means the upstream ref does not resolve while git and the repository do.
/// Test: `a_branch_with_no_upstream_reports_no_upstream_not_an_error`,
/// `a_branch_with_an_upstream_counts_the_commits_it_is_ahead_by`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UpstreamComparison {
    /// Commits on HEAD that the upstream branch does not have.
    Ahead(usize),
    /// `@{upstream}` does not resolve — the branch tracks nothing, or the
    /// remote branch it tracked has been deleted.
    NoUpstream,
}

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

    /// How `HEAD` compares to its upstream branch.
    ///
    /// #7232: a worktree with no upstream reports
    /// [`UpstreamComparison::NoUpstream`], not `Err`. That is not a pass —
    /// nothing there proves the commits reached a remote — but it is a fact the
    /// policy can weigh against the merged-PR answer, which the `Err` it used
    /// to be could not be.
    fn unpushed_commits(&self, dir: &Path) -> Result<UpstreamComparison, String>;

    /// The branch `dir` has checked out. A detached HEAD is an `Err`.
    fn branch(&self, dir: &Path) -> Result<String, String>;

    /// The full object id `dir`'s HEAD points at (#7832, #7958).
    ///
    /// Why: the one fact a detached checkout still has, and the one that
    /// settles "did this tree's content land" outright when it equals a merged
    /// pull request's own head. Both routes compare it, neither infers from it.
    /// What: `git rev-parse HEAD`. `Err` when git could not be asked, which
    /// never grants — see the module doc.
    ///
    /// **Defaulted, not required.** The two #7832/#7958 methods arrived after
    /// this trait was already `pub`, and making them required would break every
    /// outside implementor's build. The default is the ADR-0045 answer — the
    /// fact cannot be established — so an implementor that has not overridden it
    /// is refused both new grant routes and keeps exactly its pre-#7832
    /// behaviour. It can never grant anything, which is why defaulting is safe
    /// here and would not be for a method whose `Ok` relaxes the gate.
    /// Test: `a_detached_head_that_is_a_merged_prs_own_head_is_reclaimable`,
    /// `an_unresolvable_head_sha_denies_a_detached_head`,
    /// `an_unanswerable_head_sha_never_grants_an_ahead_worktree` in
    /// `bin/tm/commands/pm_guard_bash/worktree_remove`;
    /// `an_unoverridden_probe_establishes_neither_new_fact`.
    fn head_sha(&self, _dir: &Path) -> Result<String, String> {
        Err(NOT_IMPLEMENTED.to_string())
    }

    /// The MERGED pull request whose OWN head commit is `sha`, and in WHICH
    /// repository the question was asked (#7832).
    ///
    /// Why: `merged_pull_requests` searches by branch NAME, and a detached
    /// checkout has none — so a review worktree parked on a merged pull
    /// request's head had no landing evidence the guard could reach, however
    /// clean it was. GitHub's own commit search relates the two.
    /// What: `count` is 1 when a MERGED pull request in THIS repository was
    /// opened from exactly `sha`, and 0 otherwise; `repo` is the
    /// `[host/]owner/repo` that was searched, as `merged_pull_requests`
    /// reports it. `base_ref` is not read on this route and is left empty: an
    /// exact head match needs no merge-tree comparison to judge, so no base is
    /// required. A fork's pull request is never a match, for the reason
    /// `worktree_reclaim_pr_match` skips one. `Err` denies.
    ///
    /// Defaulted for the reason [`head_sha`](Self::head_sha) is, and with the
    /// same fail-closed default.
    /// Test: `a_detached_head_that_is_a_merged_prs_own_head_is_reclaimable`,
    /// `a_detached_head_no_merged_pr_carries_still_denies`,
    /// `an_unanswerable_commit_search_denies_a_detached_head` in
    /// `bin/tm/commands/pm_guard_bash/worktree_remove`;
    /// `an_unoverridden_probe_establishes_neither_new_fact`.
    fn merged_pull_request_for_commit(
        &self,
        _dir: &Path,
        _sha: &str,
    ) -> Result<MergedPrLookup, String> {
        Err(NOT_IMPLEMENTED.to_string())
    }

    /// Commits reachable from `HEAD` that no `origin` remote-tracking ref has
    /// (#7914).
    ///
    /// Why: "did this tree's commits reach GitHub" is the question the
    /// merged-PR re-check stands in for, and a worktree can answer it without
    /// any pull request ever existing — a tree created off `origin/main` and
    /// never committed to holds zero such commits, as does one whose branch was
    /// fast-forwarded into a sibling that landed. Those trees had no removal
    /// route at all under gate 5's original wording.
    /// What: `Ok(0)` means every commit here is also on an `origin` ref, so
    /// removing the directory can destroy no history; any higher count means
    /// this worktree is the only place some commit exists. `Err` means git
    /// could not be asked, which never admits — see the module doc.
    ///
    /// **The refs are refreshed before they are trusted (#7914, critic round
    /// 1).** `refs/remotes/origin/*` is a local cache, and a branch deleted on
    /// GitHub by any route other than a fetch in this worktree leaves it
    /// naming a commit the remote no longer has — the implementation therefore
    /// runs a bounded `git fetch --prune origin` first, and a refresh that
    /// fails makes the count unanswerable rather than trusted.
    /// Test: `local_only_commits_counts_only_what_no_origin_ref_has`,
    /// `local_only_commits_reprunes_a_branch_deleted_behind_this_worktrees_back`,
    /// `local_only_commits_cannot_be_answered_when_origin_is_unreachable` in
    /// `crate::session_manager::worktree_safety_tests`;
    /// `a_clean_tree_whose_commits_are_all_on_origin_needs_no_pull_request` in
    /// `bin/tm/commands/pm_guard_bash/worktree_remove`.
    fn local_only_commits(&self, dir: &Path) -> Result<usize, String>;

    /// How many MERGED pull requests GitHub has for `branch`, and in WHICH
    /// repository the question was asked (#7057).
    ///
    /// #7850: the repository is the one behind the branch's PUSH remote, not
    /// unconditionally `origin` — see the module doc.
    /// Test: `a_fork_branch_is_searched_in_the_repository_it_was_pushed_to`.
    fn merged_pull_requests(&self, dir: &Path, branch: &str) -> Result<MergedPrLookup, String>;

    /// Would merging this worktree's HEAD into `base_ref` change anything
    /// (#7275)?
    ///
    /// Why: a review round lands on `<branch>-r2` and `version-control` pushes
    /// it onto the PR's own head name, so the PR merges under `<branch>` and no
    /// MERGED pull request ever carries the sibling's name — the guard refused
    /// two such trees on 2026-09-09. Their content IS on the base, which is the
    /// fact the merged-PR question was standing in for.
    ///
    /// **This answers ownership, never landing (#7275 round 2).** An empty
    /// merge is what a branch holding no NEW content looks like, and a branch
    /// that was never pushed at all looks exactly the same. So the caller must
    /// already hold a MERGED pull request before it asks — see
    /// `worktree_remove_rechecks::landing_evidence`.
    /// What: `base_ref` is the full ref the pull request merged into
    /// (`origin/<baseRefName>`), supplied by the caller rather than guessed
    /// here. `Ok` carries the verdict — landed at the tip or at an earlier
    /// base commit (#8633), residue, or a conflict — and its description;
    /// `Err` when git could not be asked, which denies, like every other
    /// undeterminable answer here.
    fn content_on_base(&self, dir: &Path, base_ref: &str) -> Result<ContentOnBase, String>;

    /// Is this tree's content landed, or its HEAD inside a merged pull
    /// request's history (#7889)?
    ///
    /// Why: the admission for the one shape no pull request can ever vouch
    /// for by name — a parked branch fast-forwarded onto a sibling `-r2` head
    /// that squash-merged under THAT name. Nineteen such trees were refused
    /// across 2026-09-21 and 2026-09-22 while holding nothing `origin/main`
    /// lacked. Owner ruling 2026-09-22 admits routes (b) and (c).
    /// What: delegates to
    /// [`landing_admission`](crate::core::worktree_landed_content::landing_admission)
    /// under [`ADMISSION_FETCH_TIMEOUT`], so this ladder and the reclaim
    /// sweep's gate 5 run one predicate rather than two. The default
    /// implementation establishes nothing, which never grants.
    /// Test: `worktree_7889_a_landed_tree_with_no_merged_pr_is_reclaimable`,
    /// `worktree_7889_a_residual_path_denies_and_names_it`,
    /// `worktree_7889_an_unestablished_landed_content_answer_never_grants`,
    /// `worktree_7889_a_head_carried_by_a_merged_pr_is_reclaimable` in
    /// `bin/tm/commands/pm_guard_bash/worktree_remove`;
    /// `an_unoverridden_probe_establishes_neither_new_fact`.
    fn landing_admission(&self, _dir: &Path) -> LandingAdmission {
        LandedContent::unavailable(NOT_IMPLEMENTED).into()
    }

    /// [`landing_admission`](Self::landing_admission), trusting a refresh
    /// [`local_only_commits`](Self::local_only_commits) completed in this same
    /// evaluation (#7889).
    ///
    /// Why: the guard runs inside a 5 s hook, and a second `git fetch` of up
    /// to 3 s is the difference between a decision and none. The policy calls
    /// this only after `local_only_commits` answered `Ok`, which the production
    /// probe returns only once its fetch succeeded.
    /// What: the default delegates to the fetching method, so an implementor
    /// that does not override it loses time, never safety.
    /// Test: `worktree_7889_the_admission_reuses_the_local_only_fetch_only_when_it_succeeded`
    /// in `bin/tm/commands/pm_guard_bash/worktree_remove`.
    fn landing_admission_on_fetched_refs(&self, dir: &Path) -> LandingAdmission {
        self.landing_admission(dir)
    }

    /// Work `git status` cannot see: a nested repository holding unsaved work,
    /// or a high-value gitignored file (#7889 critic round 2).
    ///
    /// Why: `git worktree remove --force` deletes ignored content, so a clone
    /// with unpushed commits under an ignored directory dies with the tree.
    /// The sweep already refuses it; every guard grant now asks the same scan.
    /// What: `Ok(None)` when there is none, `Ok(Some(reason))` naming the first
    /// nested path, `Err` when the scan could not run. The default is `Err`,
    /// which refuses: an implementor that has not overridden it establishes
    /// nothing.
    /// Test: `worktree_7889_nested_dirt_denies_a_landed_grant`,
    /// `worktree_7889_nested_dirt_denies_a_merged_pr_grant`,
    /// `worktree_7889_an_unanswerable_nested_scan_denies` in
    /// `bin/tm/commands/pm_guard_bash/worktree_remove`;
    /// `nested_dirt_finds_an_ignored_clone_with_an_unpushed_commit`.
    fn nested_dirt(&self, _dir: &Path) -> Result<Option<String>, String> {
        Err(NOT_IMPLEMENTED.to_string())
    }
}

/// The row, if any, whose pull request was opened from exactly `sha` (#7832).
///
/// Why: the commit search returns every MERGED pull request GitHub relates to a
/// commit, which includes ones that merely MENTION it in a body or a comment.
/// Only one relationship is landing evidence for a worktree the guard is about
/// to delete: the pull request's own head branch pointed at this commit, so
/// everything the tree holds is what merged. The match is therefore EXACT and
/// ONE-DIRECTIONAL. `worktree_reclaim_pr_match` accepts ancestry either way
/// round because its gate 6 re-inspects whatever the merge did not carry; this
/// gate has no such gate after it, so an ancestor relationship — which can
/// leave commits here the merge never saw — is refused.
/// What: `None` for no rows, for a fork's row (a fork's pull request says
/// nothing about this repository's checkout, the reason
/// `worktree_reclaim_pr_match` skips one), for a row carrying no `headRefOid`,
/// and for any row whose oid differs. Comparison is
/// [`str::eq_ignore_ascii_case`] on the trimmed ids, because `gh` and `git`
/// agree on the object id but not reliably on its case.
/// Test: `only_a_non_fork_row_whose_own_head_is_this_commit_matches`.
fn pr_opened_from_commit<'a>(
    rows: &'a [crate::session_manager::worktree_reclaim_pr_match::MergedPrHead],
    sha: &str,
) -> Option<&'a crate::session_manager::worktree_reclaim_pr_match::MergedPrHead> {
    let sha = sha.trim();
    if sha.is_empty() {
        return None;
    }
    rows.iter()
        .find(|row| !row.is_cross_repository && row.head_ref_oid.trim().eq_ignore_ascii_case(sha))
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

    fn unpushed_commits(&self, dir: &Path) -> Result<UpstreamComparison, String> {
        // #7232: ask whether there IS an upstream before asking how far ahead
        // of it HEAD is. `gh pr merge --delete-branch` deletes the remote
        // branch, so every squash-merged worktree reaches this with an
        // unresolvable `@{upstream}` — a fact, not a probe failure.
        if !upstream_resolves(dir)? {
            return Ok(UpstreamComparison::NoUpstream);
        }
        let out = git_stdout(dir, &["rev-list", "--count", "@{upstream}..HEAD"])?;
        out.trim()
            .parse::<usize>()
            .map(UpstreamComparison::Ahead)
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

    fn head_sha(&self, dir: &Path) -> Result<String, String> {
        // #7832, #7958: the full object id, never `--short`. Both comparisons
        // are against a `headRefOid` GitHub reports in full, and an abbreviated
        // id would make every match a prefix question nobody asked.
        let sha = git_stdout(dir, &["rev-parse", "HEAD"])?.trim().to_string();
        if sha.is_empty() {
            return Err("`git rev-parse HEAD` named no commit".to_string());
        }
        Ok(sha)
    }

    fn merged_pull_request_for_commit(
        &self,
        dir: &Path,
        sha: &str,
    ) -> Result<MergedPrLookup, String> {
        // #7832: the commit search `worktree_reclaim_pr_match` already owns —
        // the same hardened `gh` spawn and #6867 hang gate. The repository is
        // resolved ONCE here and handed down, so this call costs one
        // `git config` spawn rather than two inside the hook's 5 s budget
        // (critic round). Which rows count is decided by
        // [`pr_opened_from_commit`], not by that module's ancestry ladder.
        let repo = repo_slug_for(dir)?;
        let rows = crate::session_manager::worktree_reclaim_pr_match::merged_prs_containing_in(
            dir, &repo, sha,
        )?;
        let matched = pr_opened_from_commit(&rows, sha);
        Ok(
            MergedPrLookup::new(usize::from(matched.is_some()), repo, "")
                .with_head_sha(matched.map(|row| row.head_ref_oid.trim()).unwrap_or("")),
        )
    }

    fn local_only_commits(&self, dir: &Path) -> Result<usize, String> {
        // #7914 critic round 1: `--remotes=origin` reads the LOCAL
        // `refs/remotes/origin/*` cache, which goes stale the moment a branch
        // is deleted on GitHub by any route that is not a fetch in THIS
        // worktree — `gh pr close --delete-branch`, the web UI, another clone.
        // The stale ref kept vouching for commits the remote no longer had, so
        // the admission granted and destroyed the only surviving copy. Refresh
        // first, and let a failed refresh make the count unanswerable.
        refresh_landing_refs_within(dir, ADMISSION_FETCH_TIMEOUT).map_err(|e| {
            format!(
                "`origin` could not be refreshed, so the remote-tracking refs cannot be trusted \
                 to vouch for this worktree's commits: {e}"
            )
        })?;
        // An unparsable count is an `Err` rather than a zero, because zero is
        // the only answer that admits.
        let out = git_stdout(dir, LOCAL_ONLY_COMMITS_ARGS)?;
        out.trim().parse::<usize>().map_err(|e| {
            format!(
                "`git {}` printed {:?}: {e}",
                LOCAL_ONLY_COMMITS_ARGS.join(" "),
                out.trim()
            )
        })
    }

    fn merged_pull_requests(&self, dir: &Path, branch: &str) -> Result<MergedPrLookup, String> {
        // #7057: the repository — and its host, when that is not github.com —
        // comes from THIS worktree's own remote, not from whatever `gh` would
        // infer at this working directory. A remote that cannot be read or
        // parsed is an `Err`, which denies — never a lookup aimed at a guess.
        // #7850: WHICH remote is the one this branch was pushed to, read from
        // git's own three push keys. A fork workflow pushes to `fork` and opens
        // the pull request there, so asking `origin` searched a repository the
        // branch had never reached and refused a worktree whose pull request had
        // merged. `None` — nothing configured, or git could not be asked —
        // means `origin`, so every failure branch here lands on the pre-#7850
        // verdict rather than on a guess.
        // #8403: `origin` first — repository identity comes from its URL —
        // then the push remote as a second place to look, never instead.
        // Either URL unreadable denies; see `merged_pr_search_repos_with`.
        let repos = merged_pr_search_repos(dir, branch)?;
        let pin_origin = repos.first().cloned().unwrap_or_default();
        first_merged(
            &repos,
            |repo| merged_pull_requests_in(dir, branch, repo, &pin_origin),
            |lookup: &MergedPrLookup| lookup.count > 0,
        )
    }

    fn content_on_base(&self, dir: &Path, base_ref: &str) -> Result<ContentOnBase, String> {
        // #7275, owner correction 2026-09-09: NOT `git cherry`. Every merge here
        // is a squash, so its per-commit patch-id comparison reports `+` for
        // content that IS on the base — observed on #7258. Merging into the base
        // and diffing the result is the question that actually gets answered.
        // #7275 round 2: the base is the merged pull request's own, passed in.
        // Resolving `origin/HEAD` here judged every branch against the default
        // branch, which is wrong for anything that merged elsewhere.
        // #7889: the two commands now live in `core::worktree_landed_content`,
        // so this check and the landed-content admission cannot drift apart.
        // #8633: a squash whose files `main` later edited is found in the
        // base's history instead of being read as a failed merge.
        crate::core::worktree_landed_history::content_on_base(dir, base_ref)
    }

    fn landing_admission(&self, dir: &Path) -> LandingAdmission {
        // #7889: the same 3 s bound the #7914 admission's fetch runs under —
        // this also executes inside the `PreToolUse` hook, whose own 5 s
        // timeout kills a decision that has not been printed yet. Route (c)
        // reuses the sweep's `gh` commit search and `git` ancestry probe.
        landing_admission(
            dir,
            ADMISSION_FETCH_TIMEOUT,
            &crate::session_manager::worktree_reclaim_pr_match::GhLandingProbe,
        )
    }

    fn nested_dirt(&self, dir: &Path) -> Result<Option<String>, String> {
        // #7889 critic round 2: the sweep's own scan. It fails toward dirty —
        // an unreadable candidate or a failed walk is reported as dirt.
        Ok(crate::session_manager::worktree_nested::nested_dirt(dir).map(|d| d.reason))
    }

    fn landing_admission_on_fetched_refs(&self, dir: &Path) -> LandingAdmission {
        // #7889: `local_only_commits` fetched `origin` in this evaluation and
        // answered `Ok` only because that fetch succeeded — see the trait doc.
        landing_admission_on_fetched_refs(
            dir,
            &crate::session_manager::worktree_reclaim_pr_match::GhLandingProbe,
        )
    }
}

/// One repository's MERGED pull requests for `branch` (#8403: split out so
/// the caller can search `origin` and the push remote in turn).
fn merged_pull_requests_in(
    dir: &Path,
    branch: &str,
    repo: &str,
    pin_origin: &str,
) -> Result<MergedPrLookup, String> {
    let repo = repo.to_string();
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
            // #5850: a registry pin this process cannot honour is a REFUSAL,
            // not a licence to ask GitHub as the machine's global account.
            let gh_env = resolve_daemon_gh_env(dir, pin_origin)?;
            let mut cmd = gh_pr_list_command(dir, &gh_env, &repo);
            cmd.arg("--head").arg(branch);
            cmd.args(MERGED_PR_ARGS);
            crate::session_manager::worktree_reclaim_gh::run_with_timeout(cmd, GH_TIMEOUT)
        })
        .map_err(|f| format!("{f} (repository searched: {repo})"))?;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).map_err(|e| {
        format!("`gh pr list --repo {repo} --head {branch}` JSON did not parse: {e}")
    })?;
    // #7275 round 2: the first row's base is the one the policy judges this
    // worktree's content against. A row without one leaves it empty, which
    // denies at the call site rather than falling back to a guess.
    let base_ref = rows
        .first()
        .and_then(|r| r.get("baseRefName"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    // #7958: the same row's own head commit. Absent leaves it empty, which
    // denies on that route rather than matching a HEAD against nothing.
    let head_sha = rows
        .first()
        .and_then(|r| r.get("headRefOid"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok(MergedPrLookup {
        count: rows.len(),
        repo,
        base_ref,
        head_sha,
    })
}

// #7275 round 2: `base_ref_for` is gone. It resolved `origin/HEAD` (falling back
// to `origin/main`) with no reference to the pull request whose merge was the
// thing being checked, so a branch that merged into a release or stacked base
// was judged against the default branch instead. The base now travels with the
// merged pull request that supplies it — see `MergedPrLookup::base_ref`.

/// Whether `@{upstream}` resolves in `dir` (#7232).
///
/// Why: reading a failed `@{upstream}` lookup as "no upstream" would be
/// fail-OPEN if the reason were a broken git or an unreadable repository — the
/// policy treats `NoUpstream` as a condition it can still grant under, given a
/// merged pull request. So the negative answer is only returned once git has
/// PROVED it works here: `rev-parse --verify HEAD` succeeding in the same
/// directory leaves "the upstream ref does not resolve" as the only remaining
/// explanation.
/// What: `Ok(true)` the ref resolved, `Ok(false)` it did not and HEAD did,
/// `Err` git could not answer either question — which denies at the call site.
/// Test: `a_branch_with_no_upstream_reports_no_upstream_not_an_error`.
fn upstream_resolves(dir: &Path) -> Result<bool, String> {
    if git_stdout(dir, &["rev-parse", "--symbolic-full-name", "@{upstream}"]).is_ok() {
        return Ok(true);
    }
    git_stdout(dir, &["rev-parse", "--verify", "HEAD"]).map_err(|e| {
        format!(
            "`@{{upstream}}` did not resolve and neither did HEAD, so whether this \
                 worktree's commits reached a remote could not be established: {e}"
        )
    })?;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::session_manager::decommission::WORKTREE_SENTINEL_FILE;
    use crate::session_manager::worktree_reclaim_pr_match::MergedPrHead;

    #[test]
    fn merged_pull_request_argv_asks_github_for_the_branch() {
        // The two halves that must never drift: only MERGED pull requests
        // count, and the answer is a JSON array the caller can measure.
        assert!(MERGED_PR_ARGS.contains(&"merged"));
        assert!(MERGED_PR_ARGS.contains(&"--json"));
        assert!(!MERGED_PR_ARGS.contains(&"all"));
        // #7958: the pull request's own head commit is part of the question.
        // Without it the policy cannot see that a worktree is sitting on
        // exactly what merged, and a stale `@{upstream}` decides instead.
        assert!(
            MERGED_PR_ARGS
                .iter()
                .any(|a| a.contains("headRefOid") && a.contains("baseRefName")),
            "{MERGED_PR_ARGS:?}"
        );
    }

    #[test]
    fn detached_head_is_not_a_branch() {
        // A detached HEAD prints the literal `HEAD`, which is not a branch a
        // pull request can be looked up by — so it must not become one.
        assert_eq!(DETACHED_HEAD, "HEAD");
    }

    /// One commit-search row, as `gh` would deserialize it.
    fn row(oid: &str, fork: bool) -> MergedPrHead {
        MergedPrHead {
            number: 7794,
            head_ref_oid: oid.to_string(),
            is_cross_repository: fork,
        }
    }

    /// 🔴 #7832: the whole safety case for the commit route in one table. Only
    /// a row from THIS repository whose own head IS this commit is landing
    /// evidence; a fork's row, an oid-less row, a mention-only row and an empty
    /// result are all refusals, and case alone never decides.
    #[test]
    fn only_a_non_fork_row_whose_own_head_is_this_commit_matches() {
        let head = "02a83032d1f0b4c9e7a6d5c4b3a291807f6e5d4c";
        let other = "9c8699fe0a1b2c3d4e5f60718293a4b5c6d7e8f9";
        for (label, rows, expected) in [
            ("no rows at all", vec![], false),
            ("this repository's own head", vec![row(head, false)], true),
            (
                "the same oid upper-cased",
                vec![row(&head.to_uppercase(), false)],
                true,
            ),
            ("a fork's row carrying it", vec![row(head, true)], false),
            ("a row with no head oid", vec![row("", false)], false),
            ("a mention-only row", vec![row(other, false)], false),
            (
                "a fork's row before the real one",
                vec![row(head, true), row(head, false)],
                true,
            ),
        ] {
            assert_eq!(
                pr_opened_from_commit(&rows, head).is_some(),
                expected,
                "{label}"
            );
        }
        // A HEAD git could not name matches nothing, however many rows there
        // are — otherwise an empty oid would pair with an empty HEAD.
        assert!(pr_opened_from_commit(&[row("", false)], "  ").is_none());
    }

    /// 🔴 #7832: `head_sha` reads the FULL object id. A `--short` answer would
    /// turn every comparison against GitHub's `headRefOid` into a prefix
    /// question nobody asked, and detaching must not change the answer.
    #[test]
    fn head_sha_reads_the_full_oid_of_a_detached_checkout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = checkout(tmp.path());
        let on_branch = GitAndGhProbe
            .head_sha(&repo)
            .expect("HEAD resolvable on a branch");
        assert_eq!(on_branch.len(), 40, "{on_branch}");
        assert!(
            on_branch.chars().all(|c| c.is_ascii_hexdigit()),
            "{on_branch}"
        );

        git_ok(&repo, &["checkout", "--detach", "HEAD"]);
        assert!(
            GitAndGhProbe.branch(&repo).is_err(),
            "the fixture must actually be detached for this to mean anything"
        );
        assert_eq!(
            GitAndGhProbe
                .head_sha(&repo)
                .expect("HEAD still resolvable"),
            on_branch,
            "detaching moves no commit, so the object id is unchanged"
        );
    }

    /// A probe that overrides only the pre-#7832 methods, standing in for an
    /// outside implementor that has not been recompiled against them.
    struct UnoverriddenProbe;

    impl WorktreeRemovalProbe for UnoverriddenProbe {
        fn dirty_entries(&self, _dir: &Path) -> Result<usize, String> {
            Ok(0)
        }
        fn unpushed_commits(&self, _dir: &Path) -> Result<UpstreamComparison, String> {
            Ok(UpstreamComparison::NoUpstream)
        }
        fn branch(&self, _dir: &Path) -> Result<String, String> {
            Ok("main".to_string())
        }
        fn local_only_commits(&self, _dir: &Path) -> Result<usize, String> {
            Ok(0)
        }
        fn merged_pull_requests(&self, _dir: &Path, _b: &str) -> Result<MergedPrLookup, String> {
            Ok(MergedPrLookup::new(0, "o/r", ""))
        }
        fn content_on_base(&self, _dir: &Path, _base: &str) -> Result<ContentOnBase, String> {
            Ok(ContentOnBase::Residual {
                paths: vec!["x".into()],
                searched: 0,
                candidates: 0,
            })
        }
    }

    /// 🔴 #7832/#7958, critic round: the two new methods carry DEFAULT bodies so
    /// adding them to an already-`pub` trait breaks no outside implementor. The
    /// default must be the ADR-0045 undeterminable answer — an implementor that
    /// has not overridden them establishes nothing and can grant nothing.
    #[test]
    fn an_unoverridden_probe_establishes_neither_new_fact() {
        let dir = Path::new("/nonexistent");
        assert!(UnoverriddenProbe.head_sha(dir).is_err());
        assert!(
            UnoverriddenProbe
                .merged_pull_request_for_commit(dir, "deadbeef")
                .is_err()
        );
        // #7889: the admission's default is the same — establishing nothing,
        // which never grants.
        assert!(!UnoverriddenProbe.landing_admission(dir).admits());
        // #7889 critic round 2: an unoverridden nested scan is unanswerable.
        assert!(UnoverriddenProbe.nested_dirt(dir).is_err());
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

    /// 🔴 #7232: `gh pr merge --delete-branch` deletes the remote branch, so
    /// every squash-merged worktree arrives here with no upstream. On
    /// `ad64460e8` this returned `Err`, the guard denied at `unpushed-commits`,
    /// and the merged-PR re-check that would have cleared the tree was never
    /// reached — so `version-control` could not reclaim a single tree the
    /// sanctioned merge flow produced.
    #[test]
    fn a_branch_with_no_upstream_reports_no_upstream_not_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = checkout(tmp.path());

        assert_eq!(
            GitAndGhProbe
                .unpushed_commits(&repo)
                .expect("a missing upstream is a fact, not a probe failure"),
            UpstreamComparison::NoUpstream
        );
    }

    /// The counting arm still counts: `NoUpstream` must not have swallowed the
    /// case the re-check exists for, or an unpushed commit would be deletable.
    #[test]
    fn a_branch_with_an_upstream_counts_the_commits_it_is_ahead_by() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = checkout(tmp.path());
        // A local bare repository is enough: `@{upstream}` only has to resolve.
        let remote = tmp.path().join("remote.git");
        std::fs::create_dir_all(&remote).expect("fixture: create remote dir");
        git_ok(&remote, &["init", "--bare", "--initial-branch=main"]);
        git_ok(
            &repo,
            &["remote", "add", "origin", &remote.to_string_lossy()],
        );
        git_ok(&repo, &["push", "-u", "origin", "main"]);
        assert_eq!(
            GitAndGhProbe.unpushed_commits(&repo).expect("upstream set"),
            UpstreamComparison::Ahead(0)
        );

        std::fs::write(repo.join("later.txt"), "more\n").expect("fixture: write");
        git_ok(&repo, &["add", "later.txt"]);
        git_ok(&repo, &["commit", "-m", "later"]);
        assert_eq!(
            GitAndGhProbe.unpushed_commits(&repo).expect("upstream set"),
            UpstreamComparison::Ahead(1),
            "an unpushed commit must still be counted"
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

    /// 🔴 REGRESSION (#7889 critic round 2): a clone under an IGNORED directory,
    /// holding a commit on no remote, is invisible to `git status` and would be
    /// deleted by `git worktree remove --force`. The guard's probe finds it and
    /// names its path. The ignore rule lives in `info/exclude`, so HEAD — and
    /// with it any landed-content answer — is untouched.
    #[test]
    fn nested_dirt_finds_an_ignored_clone_with_an_unpushed_commit() {
        let fx = crate::session_manager::worktree_git_fixture::GitWorktreeFixture::new();
        let wt = fx.add_worktree("nested-clone-7889");
        assert_eq!(
            GitAndGhProbe.nested_dirt(&wt).expect("scan runs"),
            None,
            "premise: a fresh tree holds no nested work"
        );
        let exclude = fx.repo.join(".git").join("info").join("exclude");
        std::fs::create_dir_all(exclude.parent().expect("parent")).expect("mkdir info");
        std::fs::write(&exclude, "scratch/\n").expect("write exclude");
        let inner = wt.join("scratch").join("side-project");
        std::fs::create_dir_all(&inner).expect("mkdir inner");
        git_ok(&inner, &["init", "--initial-branch=main"]);
        git_ok(&inner, &["config", "user.email", "ci@test.invalid"]);
        git_ok(&inner, &["config", "user.name", "CI"]);
        git_ok(&inner, &["config", "commit.gpgsign", "false"]);
        std::fs::write(inner.join("work.rs"), "// only copy\n").expect("write work");
        git_ok(&inner, &["add", "work.rs"]);
        git_ok(&inner, &["commit", "-m", "only copy"]);
        assert_eq!(
            GitAndGhProbe.dirty_entries(&wt).expect("status readable"),
            0,
            "premise: git status cannot see the ignored clone"
        );
        let reason = GitAndGhProbe
            .nested_dirt(&wt)
            .expect("scan runs")
            .expect("the ignored clone's unpushed commit must be reported");
        assert!(reason.contains("scratch/side-project"), "{reason}");
    }
}
