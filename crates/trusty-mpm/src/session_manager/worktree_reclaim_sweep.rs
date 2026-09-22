//! The merged-PR survey and the reclaim loop that acts on it (#2919).
//!
//! Why: split from `worktree_reclaim` so both files stay under the 500-SLOC
//! production cap, and because the two halves answer different questions. That
//! module decides whether ONE worktree, described by a set of facts, may be
//! reclaimed. This one is responsible for GATHERING those facts — and, on the
//! destructive path, for gathering them AGAIN, freshly, immediately before each
//! delete.
//!
//! THE STALENESS RULE, which is the whole point of this file:
//!
//! A survey verdict is evidence about the moment it was computed, and nothing
//! more. Measured on this repository: byte-walking the 46 registered worktrees
//! does not finish in 600 seconds. A delete loop that consults the survey's
//! captured `in_use` slice is therefore asking a question that was answered
//! more than ten minutes ago — a session that attached in the meantime is
//! invisible, and a `git worktree lock` applied in the meantime is ignored
//! (`Admission` was evaluated at survey time). The first cut of this feature
//! made exactly that mistake, and its three "re-checks" all survived mutation
//! testing because they re-asked stale inputs. Until #4732,
//! `remove_session_worktree`'s `remove_dir_all` fallback then deleted the
//! locked worktree regardless, so this re-check was the ONLY thing honouring
//! the lock; it now refuses too, and this pass is the outer of two defences.
//!
//! So: [`reclaim_with_probes`] re-reads the live session set, git's own
//! worktree registry, the ownership marker, the pull-request state, and the
//! working tree PER CANDIDATE, immediately before that candidate's deletion —
//! mirroring `prune_orphaned_worktrees`' Phase 2, whose comment warns against
//! collapsing exactly this boundary. Liveness is re-read through a closure that
//! can return `None` for "could not be determined", which REFUSES.
//!
//! Test: `worktree_reclaim_sweep_tests` — every re-check has a test that fails
//! if that re-check is deleted, driven by probes that change state BETWEEN the
//! survey and the delete.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// #7889: the bounded per-repository fetch that makes gate 6's landing refs
// current before anything is classified against them, and the landed-content
// admission gate 5 asks when no pull request carries the branch's name.
use super::worktree_landing_refresh::refresh_landing_refs;
use super::worktree_reclaim_landed::{landing_recheck, reclaim_landed_content};

use super::worktree_reclaim::{
    AgentStateProbe, BranchPrState, KeepList, LandedContentProbe, LiveClaims, NOT_INSPECTED_REASON,
    PrIndex, ReclaimCandidate, ReclaimGate, ReclaimMode, ReclaimOutcome, ReclaimSurvey,
    ReclaimVerdict, agent_ownership_blocks, classify_with_landed_content, measure_bytes_until,
    session_ownership_blocks, tm_provisioned, unattributed_nested_blocks,
};
// #7504: the worktree-launched-process gate, applied per candidate immediately
// before its deletion alongside the five `recheck_before_delete` re-asks.
use super::worktree_reclaim_launch::launch_refusal;
// #7267: the merged-pull-request matcher — round stem and head commit, not the
// branch name alone.
use super::worktree_reclaim_pr_match::{GhLandingProbe, resolve_with_index};
use super::worktree_registry::{list_registered_worktrees, scan_registered_worktrees};
use super::worktree_safety::inspect_dirt;

/// How long a survey may spend measuring bytes, and classifying (#2919).
///
/// Why: the two costs differ by orders of magnitude and must be bounded
/// separately. Classification is a handful of `git`/`gh` calls per worktree —
/// seconds for a whole workspace. Byte measurement walks every file under every
/// worktree, including each one's multi-gigabyte `target/`; on this repository
/// it exceeds 600 seconds. Giving them one shared budget forces a choice
/// between an unbounded probe and a survey that classifies almost nothing.
/// What: `measure` bounds the byte walk; `classify` bounds the whole pass. A
/// `None` field is unbounded. Past `classify`, remaining worktrees are still
/// LISTED but marked `Blocked` — an interrupted survey may never widen what is
/// reclaimable.
/// Test: `survey_past_its_classify_deadline_reclaims_nothing`,
/// `survey_past_its_measure_deadline_still_classifies`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SurveyBudget {
    /// How long the MEASUREMENT PHASE may run, starting when it begins.
    ///
    /// A duration rather than an instant, deliberately. Measurement is a second
    /// pass that runs after classification, and classification against the real
    /// store takes ~92 seconds — so an absolute instant computed at survey
    /// start had already expired before the phase began, and NOTHING was
    /// measured (observed: `unmeasured = 269` of 269). A duration is allotted
    /// to the phase itself and cannot be consumed by the phase before it.
    pub measure: Option<Duration>,
    /// Stop classifying after this instant; the rest are `Blocked`.
    pub classify: Option<Instant>,
}

/// How long the reclaim path's byte-measurement phase may run (#7884).
///
/// Why: measurement walks every file under every worktree, `target/` included,
/// and this module's own note records it exceeding 600 s over 46 worktrees. On
/// the destructive path it ran UNBOUNDED, which is the hang #7884 observed:
/// `tm session prune-worktrees --merged-prs` sat six minutes at 0.01 s CPU
/// under 12 concurrent `rustc` processes — I/O-starved inside that walk — and
/// once dropped the connection with "connection closed before message
/// completed". Nothing that walk produces can change WHAT is reclaimed; it
/// produces the `bytes_freed` figure in the report, and an unmeasured candidate
/// is already modelled as `None` rather than as zero.
/// What: 120 s, against a client bound of
/// [`RECLAIM_SURVEY_REQUEST_TIMEOUT`](crate::client::http_client::RECLAIM_SURVEY_REQUEST_TIMEOUT).
/// Test: `worktree_7884_the_reclaim_pass_bounds_its_measurement_phase`.
pub(crate) const RECLAIM_MEASURE_BUDGET: Duration = Duration::from_secs(120);

impl SurveyBudget {
    /// The operator-invoked reclaim path's budget (#2919, #7884).
    ///
    /// Why: CLASSIFICATION stays unbounded — a human who typed `--merged-prs` is
    /// waiting for a correct answer, not a fast one, and a partial
    /// classification on the DESTRUCTIVE path would silently shrink what gets
    /// reclaimed rather than shrink a report. MEASUREMENT is bounded, because it
    /// decides nothing and is where the #7884 hang lives; degrading it costs one
    /// reported number, already modelled as absent.
    /// Test: `reclaim_leaves_classification_unbounded`,
    /// `worktree_7884_the_reclaim_pass_bounds_its_measurement_phase`.
    pub(crate) fn for_reclaim() -> Self {
        Self {
            measure: Some(RECLAIM_MEASURE_BUDGET),
            classify: None,
        }
    }
}

/// Survey every registered worktree under `repos_root` (#2919).
///
/// Why: the read-only half of this feature, and the only half anything
/// automatic runs. It opens no destructive path.
/// What: enumerates via [`scan_registered_worktrees`] (git-authoritative,
/// ADR-0023), builds ONE [`PrIndex`] per registry root, and runs [`classify`].
/// When `per_branch_fallback` is set, [`resolve_with_index`] adds the targeted
/// per-branch lookup a truncated index cannot answer — without that, no
/// worktree older than the last
/// [`PR_INDEX_LIMIT`](super::worktree_reclaim::PR_INDEX_LIMIT) pull requests
/// could ever be reclaimed, which on this repository is nearly all of them —
/// and the #7267 widening onto the round-stem sibling and the head commit.
/// `index_for` is injectable so the classification paths are testable
/// without `gh`.
/// Test: `survey_reports_a_merged_worktree_as_reclaimable`,
/// `survey_past_its_classify_deadline_reclaims_nothing`.
// #7357 adds `adopted` to an already-wide signature. Grouping the seven into a
// struct would rewrite every one of this module's ~20 call sites for no change
// in what any of them pass.
#[allow(clippy::too_many_arguments)]
pub(crate) fn survey_with_index(
    repos_root: &Path,
    in_use: &LiveClaims,
    index_for: &dyn Fn(&Path) -> PrIndex,
    agent_state: AgentStateProbe<'_>,
    budget: SurveyBudget,
    per_branch_fallback: bool,
    // #6927: the operator keep-list `classify`'s gate 0 applies. An empty list
    // is a no-op gate, so every pre-#6927 caller keeps its exact behaviour.
    keep_list: &KeepList,
    // #7357: adopted anchors reach projects the repos-root walk cannot. They
    // are INJECTED, never resolved here — resolving them internally makes this
    // function's unit tests read the operator's real adoption store and survey
    // whatever real worktrees it names. `&[]` is the pre-#7357 behaviour.
    adopted: &[PathBuf],
) -> ReclaimSurvey {
    // #7889: gate 5's landed-content admission is opt-in — see
    // [`survey_with_landed_content`]. A caller that does not ask for it keeps
    // the pre-#7889 refusal and performs no fetch.
    survey_with_landed_content(
        repos_root,
        in_use,
        index_for,
        agent_state,
        budget,
        per_branch_fallback,
        keep_list,
        adopted,
        None,
    )
}

/// [`survey_with_index`], offering gate 5's landed-content admission (#7889).
///
/// Why: the predicate fetches, so it is offered by the operator-invoked reclaim
/// path and withheld from the doctor's unattended survey. A second entry point
/// rather than a ninth argument, so the twenty-odd existing call sites keep
/// their exact shape.
/// What: as [`survey_with_index`], plus the probe handed down to
/// [`classify_with_landed_content`].
/// Test: `worktree_7889_classify_admits_a_landed_tree_with_no_pull_request`,
/// `survey_reports_a_merged_worktree_as_reclaimable`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn survey_with_landed_content(
    repos_root: &Path,
    in_use: &LiveClaims,
    index_for: &dyn Fn(&Path) -> PrIndex,
    agent_state: AgentStateProbe<'_>,
    budget: SurveyBudget,
    per_branch_fallback: bool,
    keep_list: &KeepList,
    adopted: &[PathBuf],
    landed_content: LandedContentProbe<'_>,
) -> ReclaimSurvey {
    let mut indexes: BTreeMap<PathBuf, PrIndex> = BTreeMap::new();
    let mut candidates = Vec::new();
    for scanned in scan_registered_worktrees(repos_root, adopted) {
        if budget.classify.is_some_and(|d| Instant::now() >= d) {
            // #2919: fail closed. A candidate we ran out of time to inspect is
            // reported as blocked, never omitted and never approved.
            candidates.push(ReclaimCandidate {
                path: scanned.path,
                branch: scanned.branch,
                registry_root: scanned.registry_root,
                bytes: None,
                pr: BranchPrState::Unknown,
                verdict: ReclaimVerdict::blocked(ReclaimGate::Deadline, NOT_INSPECTED_REASON),
            });
            continue;
        }
        // #7889: the landing-ref refresh gate 6 depends on runs in
        // `reclaim_with_probes`, on the destructive path only — a survey never
        // mutates refs (#7652 critic round).
        let index = indexes
            .entry(scanned.registry_root.clone())
            .or_insert_with(|| index_for(&scanned.registry_root));
        // #6561 (the per-branch retry) and #7267 (the round-stem and head-commit
        // widening) both live in `resolve_with_index`, so this call site and the
        // pre-delete re-check below cannot drift apart.
        let pr = resolve_with_index(
            &scanned.path,
            &scanned.registry_root,
            scanned.branch.as_deref(),
            index,
            per_branch_fallback,
            &GhLandingProbe,
        );
        // #6806: WHOSE claim, not merely whether one exists.
        let claim = in_use.claim_state(&scanned.path);
        // #7232: a claim that stopped blocking has to be visible, or the change
        // reads as a regression to whoever saw yesterday's refusal.
        if let Some(note) = claim.note() {
            tracing::info!(path = %scanned.path.display(), "{note}");
        }
        let verdict = classify_with_landed_content(
            &scanned.path,
            scanned.admission,
            &claim,
            &pr,
            &inspect_dirt,
            agent_state,
            // #7652: gate 4b's owner map rides in the claim snapshot.
            &in_use.owners,
            keep_list,
            landed_content,
        );
        candidates.push(ReclaimCandidate {
            // Measured in a SECOND pass — see below.
            bytes: None,
            path: scanned.path,
            branch: scanned.branch,
            registry_root: scanned.registry_root,
            pr,
            verdict,
        });
    }
    // The measurement deadline starts HERE, when the phase does — not when the
    // survey did. See `SurveyBudget::measure`.
    let measure_deadline = budget.measure.map(|d| Instant::now() + d);
    measure_reclaimable_first(&mut candidates, measure_deadline);
    ReclaimSurvey::from_candidates(candidates)
}

/// Measure bytes, spending the budget on the RECLAIMABLE worktrees first
/// (#2919).
///
/// Why: measurement is the expensive half and the budget usually runs out. When
/// it ran out in scan order, the reclaimable worktrees — the only ones an
/// operator can act on — were routinely the ones left unmeasured. Observed
/// end-to-end against the real store: 5 reclaimable worktrees, and
/// `reclaimable_bytes = 0`, because the budget was consumed measuring 264
/// blocked ones first. "0 bytes reclaimable" alongside "5 worktrees
/// reclaimable" is not a partial answer, it is a WRONG one, and the 2026-07-21
/// post-mortem's eighth constraint is specifically that this feature report
/// real numbers rather than a claim.
/// What: two ordered passes over the same slice — reclaimable candidates first,
/// then the rest — both bounded by the same `deadline`.
///
/// This ORDERS the degradation; it does not remove it, and an earlier draft of
/// this comment claimed otherwise. Measured against the real store: the first
/// reclaimable candidate is 17.8 GiB and consumed the entire 20-second budget
/// on its own, so four of the five reclaimable worktrees still came back
/// unmeasured — `reclaimable_bytes` was one worktree's size wearing the whole
/// set's label, and because every other measurement had also been starved it
/// happened to equal `total_bytes`, which reads as "all of it is reclaimable".
/// The `unmeasured`/UNDERCOUNT disclosure covers `total_bytes` only. So
/// [`ReclaimSurvey::reclaimable_measured`] carries the measured-vs-total count
/// for the reclaimable set specifically, and every surface that prints
/// `reclaimable_bytes` must print that qualifier beside it.
/// Test: `survey_measures_reclaimable_worktrees_before_blocked_ones`,
/// `survey_discloses_a_partially_measured_reclaimable_set`.
fn measure_reclaimable_first(candidates: &mut [ReclaimCandidate], deadline: Option<Instant>) {
    for want_reclaimable in [true, false] {
        for c in candidates
            .iter_mut()
            .filter(|c| c.verdict.is_reclaimable() == want_reclaimable)
        {
            c.bytes = measure_bytes_until(&c.path, deadline);
        }
    }
}

// #7259: the `survey` wrapper that bound `survey_with_index` to
// `PrIndex::from_gh` lived here for one caller — `doctor_worktree_disk`. That
// probe now owns the same wrapper itself
// (`check_worktree_disk` over `check_worktree_disk_with_index`), so the index
// seam reaches the doctor's own tests rather than stopping one layer below
// them, and this indirection had no callers left.

/// Re-ask git, RIGHT NOW, whether this path may still be removed (#2919).
///
/// Why: `Admission` was decided during the survey. Ten minutes later the
/// operator may have run `git worktree lock` — an explicit "do not remove
/// this" — and the survey's verdict knows nothing about it. Git itself honours
/// the lock (`git worktree remove --force` exits 128); until #4732
/// `remove_session_worktree` read that as a git failure and fell back to
/// `remove_dir_all`, so this re-check was the only thing honouring it. The
/// remover now refuses as well — this stays the OUTER defence, because a
/// candidate proposed here and refused there is a reported near-miss.
/// What: one `git -C <candidate> worktree list --porcelain`, resolved against
/// the candidate's own repository. Refuses when git cannot be asked, when git
/// no longer lists the path, and when the record is the main checkout, bare,
/// locked, or already prunable. Strictly stronger than the presence-only
/// `git_worktree_list_agrees` it replaces on this path.
/// Test: `recheck_refuses_a_worktree_locked_after_the_survey`,
/// `recheck_refuses_a_path_git_no_longer_lists`.
fn git_still_permits(path: &Path) -> Result<(), String> {
    let Some(registered) = list_registered_worktrees(path) else {
        return Err("git could not be queried for this path".into());
    };
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let found = registered
        .into_iter()
        .find(|w| std::fs::canonicalize(&w.path).unwrap_or_else(|_| w.path.clone()) == canonical);
    let Some(record) = found else {
        return Err("git no longer lists this path as a worktree".into());
    };
    if record.is_main {
        return Err("git now reports this as the repository's main checkout".into());
    }
    if record.bare {
        return Err("git now reports this as a bare repository".into());
    }
    if record.locked {
        return Err("git-locked by the operator since the survey".into());
    }
    if record.prunable {
        return Err("git reports the directory is already gone".into());
    }
    Ok(())
}

/// Every gate, re-asked against CURRENT state, immediately before one delete
/// (#2919).
///
/// Why: this is the TOCTOU defense, and it is only a defense if its inputs are
/// fresh. The first cut re-ran three checks against the survey's own captured
/// slice; deleting all three left every test passing, because re-asking a stale
/// question yields the same stale answer. Each input here is re-read by the
/// caller for THIS candidate, immediately before THIS deletion.
/// What: `keep_list_now` is the operator's keep-list as it reads RIGHT NOW, so
/// an entry added while a sweep that has exceeded 600 s on 46 worktrees is
/// still running stops every candidate it has not yet deleted (#6927). It is
/// asked first, as gate 0 is, and costs an in-process path compare — no
/// subprocess. Then `in_use_now`, which is `None` when the live session set
/// could not be re-read at all — that REFUSES, because an unanswerable liveness
/// question must never resolve to "nothing claims it". Then git's current
/// verdict ([`git_still_permits`], which honours a lock applied since the
/// survey), the ownership marker, and finally [`landing_recheck`]: a merged
/// pull request re-runs [`inspect_dirt`], which fails toward dirty, and #7889's
/// `landed_content` probe, when offered, re-asks the admission for a branch no
/// pull request carries. `Some(reason)` refuses; `None` permits.
/// Test: `worktree_7889_the_recheck_admits_a_landed_tree_with_no_pull_request`,
/// `worktree_7889_the_recheck_refuses_a_tree_no_longer_landed`; and one test
/// per branch —
/// `recheck_refuses_a_worktree_keep_listed_after_the_survey`,
/// `recheck_refuses_when_the_live_set_cannot_be_read`,
/// `recheck_refuses_a_worktree_a_session_claims_now`,
/// `recheck_refuses_a_worktree_locked_after_the_survey`,
/// `recheck_refuses_a_path_git_no_longer_lists`,
/// `recheck_refuses_a_worktree_that_lost_its_ownership_marker`,
/// `recheck_refuses_a_worktree_an_agent_claimed_after_the_survey`,
/// `worktree_7652_the_recheck_refuses_an_owner_that_came_back`,
/// `recheck_refuses_when_the_pr_is_no_longer_merged`,
/// `recheck_refuses_a_worktree_dirtied_after_the_survey`,
/// `recheck_permits_a_clean_merged_owned_worktree`.
pub(crate) fn recheck_before_delete(
    path: &Path,
    keep_list_now: &KeepList,
    in_use_now: Option<&LiveClaims>,
    pr_now: &BranchPrState,
    agent_state: AgentStateProbe<'_>,
    landed_content: LandedContentProbe<'_>,
) -> Option<String> {
    // #6927: gate 0, re-asked. The survey read the keep-list once, minutes ago;
    // this reads what the operator has written since — including the
    // fail-closed state a config that stopped parsing produces.
    if let Some(kept) = keep_list_now.keeps(path) {
        return Some(kept.detail());
    }
    let Some(in_use_now) = in_use_now else {
        return Some("the live session set could not be re-read — refusing to delete".into());
    };
    // #6806: the same owner-aware resolution the survey's gate 2 applies, so
    // the re-check can neither refuse a candidate gate 2 admitted nor admit one
    // it refused.
    let claim_now = in_use_now.claim_state(path);
    if let Some(reason) = claim_now.refusal(true) {
        return Some(reason);
    }
    if let Err(reason) = git_still_permits(path) {
        return Some(reason);
    }
    if !tm_provisioned(path) {
        return Some("no longer carries a trusty-mpm ownership marker".into());
    }
    // #5661: re-read the ownership sentinel adjacent to the deletion, for the
    // reason #4118 established — an agent can be dispatched into a tree while a
    // survey that takes minutes is still running, and the survey's verdict knows
    // nothing about it.
    if let Some(reason) = agent_ownership_blocks(path, agent_state) {
        return Some(reason);
    }
    // #7652: gate 4b, re-asked against the FRESH owner map — a session that
    // resumed in this tree during a minutes-long survey is invisible to the
    // survey's verdict, exactly as a freshly dispatched agent is.
    if let Some(reason) = session_ownership_blocks(path, &in_use_now.owners) {
        return Some(reason);
    }
    // #7652 critic round: gate 4c, against the same fresh claim.
    if let Some(reason) = unattributed_nested_blocks(path, &claim_now) {
        return Some(reason);
    }
    // #7889: a merge still re-runs `inspect_dirt`; no pull request re-asks the
    // landed-content admission when the caller offered it.
    landing_recheck(path, pr_now, landed_content)
}

/// Probes the reclaim loop uses to re-read state per candidate (#2919).
///
/// Why: making freshness an injected capability rather than a captured value is
/// what makes it testable. A test supplies probes that CHANGE the workspace
/// between the survey and the delete — locking a worktree, dirtying it,
/// reopening its pull request — which is the only shape that exercises a
/// re-check at all. The previous test claimed to do this and did not: it fed
/// the same `in_use` to the survey, so the candidate was blocked during
/// classification and the delete loop never ran.
/// What: `in_use_now` re-reads the workspace claims sessions hold — WITH the
/// invoking session's id, since #6806 — returning `None` when that cannot be
/// determined (which refuses). `index_for` rebuilds a repository's
/// pull-request index.
/// Test: used by every `reclaim_remove_mode_*` test.
pub(crate) struct FreshProbes<'a> {
    /// Re-read the claimed workspace paths; `None` means "could not determine".
    pub in_use_now: &'a dyn Fn() -> Option<LiveClaims>,
    /// Rebuild a repository's pull-request index.
    pub index_for: &'a dyn Fn(&Path) -> PrIndex,
    /// Ask the delegation registry about the agent a sentinel names (#5661).
    ///
    /// Read at classify time AND again per candidate immediately before its
    /// deletion, so an agent dispatched during a minutes-long survey still
    /// protects its tree.
    pub agent_state: AgentStateProbe<'a>,
    /// Read the operator's keep-list, for `classify`'s gate 0 (#6927).
    ///
    /// Why a probe and not a value: the answer CHANGES under a running sweep.
    /// This pass is unbounded and has exceeded 600 s over 46 worktrees, and an
    /// operator who adds an entry while it runs means it for the candidates
    /// still queued, not only for the next invocation. Read once for the
    /// survey and again per candidate immediately before its deletion, exactly
    /// as `in_use_now` is. Reading it is a small local file, not a subprocess,
    /// so the per-candidate cost is nil.
    pub keep_list: &'a dyn Fn() -> KeepList,
    /// The directories a live process was launched from (#7504).
    ///
    /// Why a VALUE and not a probe, unlike its three neighbours: a process's
    /// working directory and executable path are fixed for that process's
    /// lifetime, so re-reading them per candidate would re-derive the same
    /// answer at a cost. The staleness this module guards against is state
    /// OTHERS change during a minutes-long sweep; this input is the sweep's own.
    /// An empty slice is the pre-#7504 behaviour — a no-op gate.
    pub launched_from: &'a [PathBuf],
}

/// Gates 2, 4b and 4c re-asked against a claim set read immediately before the
/// removal (#7652 critic round).
///
/// Why: [`recheck_before_delete`] reads the claims before git, the harness-lock
/// probe and [`inspect_dirt`], which takes seconds on a large tree, and
/// `git worktree remove --force` follows. The three gates that read the claim
/// set are the ones a session arriving in that window changes.
/// What: `None` (the set could not be read) refuses; otherwise the fresh
/// claim's gate-2 refusal, then [`session_ownership_blocks`], then
/// [`unattributed_nested_blocks`].
///
/// Gate 4a ([`agent_ownership_blocks`]) is deliberately NOT re-asked here
/// (#7652 critic round 2). It is asked twice already — at classification and in
/// [`recheck_before_delete`] — and the window this guard covers is the one
/// between that second read and `git worktree remove --force`. An agent
/// dispatched into the tree inside that window holds git's harness lock, and
/// `git worktree remove` refuses a locked worktree with exit 128, so the
/// removal fails on git's own check rather than on a third sentinel read. The
/// three gates re-asked here have no such backstop: nothing in git knows about
/// a session's workspace claim.
/// Test: `worktree_7652_an_owner_back_after_the_dirt_check_is_refused`.
fn last_moment_refusal(path: &Path, claims: Option<&LiveClaims>) -> Option<String> {
    let Some(claims) = claims else {
        return Some(
            "the live session set could not be re-read immediately before removal — refusing \
             to delete"
                .into(),
        );
    };
    let claim = claims.claim_state(path);
    claim
        .refusal(true)
        .or_else(|| session_ownership_blocks(path, &claims.owners))
        .or_else(|| unattributed_nested_blocks(path, &claim))
}

/// Refresh the landing refs of every repository a destructive sweep will judge,
/// once each (#7889).
///
/// Why: gate 6 reads `refs/remotes/*/main` to decide what has already landed,
/// and nothing else in this path updates it — so a branch whose pull request
/// squash-merged on GitHub still counted its commits as unpushed. Only the
/// destructive path calls this, so a report mutates no refs (#7652 critic
/// round). A FAILED refresh leaves the stale refs, which count MORE unpushed
/// commits, which refuses.
/// Test: `a_refresh_updates_the_stale_landing_ref`,
/// `a_refresh_against_a_missing_remote_fails_without_touching_the_refs`.
fn refresh_repositories(repos_root: &Path, adopted: &[PathBuf]) {
    let roots: BTreeSet<PathBuf> = scan_registered_worktrees(repos_root, adopted)
        .into_iter()
        .map(|scanned| scanned.registry_root)
        .collect();
    for root in roots {
        if let Err(e) = refresh_landing_refs(&root) {
            tracing::warn!(
                root = %root.display(),
                "worktree-reclaim: could not refresh this repository's landing refs, so \
                 gate 6 judges every candidate under it against possibly stale \
                 remote-tracking refs — the refusing direction (#7889): {e}"
            );
        }
    }
}

/// Survey, and in [`ReclaimMode::Remove`] reclaim, merged-PR worktrees (#2919).
///
/// Why: see this module's staleness rule. The survey establishes candidates;
/// nothing it computed is trusted at deletion time.
/// What: in `Remove` mode refreshes each repository's landing refs first
/// ([`refresh_repositories`]). Surveys with an initial snapshot, then — in
/// `Remove` mode only —
/// re-reads liveness and the pull-request index and re-runs
/// [`recheck_before_delete`] for each candidate immediately before that
/// candidate's own deletion. Deletion itself delegates to
/// `remove_session_worktree`, which applies its own ownership gate.
/// Test: `reclaim_report_mode_removes_nothing`,
/// `reclaim_remove_mode_refuses_a_worktree_claimed_after_the_survey`,
/// `reclaim_remove_mode_refuses_a_worktree_dirtied_after_the_survey`,
/// `reclaim_remove_mode_refuses_a_worktree_locked_after_the_survey`,
/// `reclaim_remove_mode_refuses_when_the_pr_reopens_after_the_survey`,
/// `reclaim_remove_mode_reclaims_a_clean_merged_worktree`.
pub(crate) fn reclaim_with_probes(
    repos_root: &Path,
    probes: &FreshProbes<'_>,
    mode: ReclaimMode,
    // #7357: the caller's adopted anchors, passed straight through.
    adopted: &[PathBuf],
) -> ReclaimOutcome {
    // 🔴 #7965: `unwrap_or_default()` stood here, and an empty `LiveClaims` reads
    // as "nothing claims anything" — the exact downgrade the fail-closed contract
    // forbids, applied to EVERY candidate at once. A probe that could not answer
    // now demotes the whole pass to a REPORT: candidates are still surveyed and
    // listed, and nothing is deleted.
    let (initial, mode) = match (probes.in_use_now)() {
        Some(claims) => (claims, mode),
        None => {
            tracing::warn!(
                "worktree-reclaim: the claim probe could not be read; surveying only, \
                 deleting nothing (#7965)"
            );
            (LiveClaims::default(), ReclaimMode::Report)
        }
    };
    // #7889: only a DESTRUCTIVE pass refreshes the landing refs. Read after
    // #7965's demotion rather than before it, so a pass demoted to `Report` by
    // an unanswerable claim probe mutates no refs either.
    if mode == ReclaimMode::Remove {
        refresh_repositories(repos_root, adopted);
    }
    let survey = survey_with_landed_content(
        repos_root,
        &initial,
        probes.index_for,
        probes.agent_state,
        SurveyBudget::for_reclaim(),
        true,
        &(probes.keep_list)(),
        adopted,
        // #7889: the operator typed `prune-worktrees --merged-prs`, so the
        // admission is offered in BOTH modes — a report that hid a candidate
        // `--force` would then reclaim is a report of the wrong thing. It is
        // the only fetch a report performs, it is bounded, and it runs only for
        // a candidate that reached gate 5 with no pull request.
        Some(&reclaim_landed_content),
    );
    let mut out = ReclaimOutcome {
        removed: Vec::new(),
        removed_bytes: 0,
        refused_at_recheck: Vec::new(),
        removal_failed: Vec::new(),
        survey,
    };
    if mode == ReclaimMode::Report {
        return out;
    }
    let approved: Vec<ReclaimCandidate> = out
        .survey
        .candidates
        .iter()
        .filter(|c| c.verdict.is_reclaimable())
        .cloned()
        .collect();
    // Rebuilt fresh for the delete phase — the survey's index is as old as the
    // survey, which is minutes.
    let mut fresh_indexes: BTreeMap<PathBuf, PrIndex> = BTreeMap::new();
    for candidate in approved {
        let path = candidate.path;
        // #7504: ahead of the pull-request lookups, because it is the only gate
        // here that costs nothing — two in-process path compares — and a
        // candidate holding the running daemon's own footing must not first buy
        // a `gh` call. It is also the only input that cannot go stale under this
        // sweep, so evaluating it early costs no freshness (see
        // `FreshProbes::launched_from`).
        if let Some(reason) = launch_refusal(&path, probes.launched_from) {
            tracing::warn!(
                path = %path.display(),
                "worktree-reclaim: spared a surveyed candidate — {reason}"
            );
            out.refused_at_recheck
                .push(format!("{}: {reason}", path.display()));
            continue;
        }
        // The pull-request lookups go FIRST because they are the slow part —
        // measured 322-366 ms for a per-branch call and 1220 ms for the bulk
        // one, against a 10 s ceiling. Reading liveness before them would date
        // the re-check's snapshot by that whole interval for no reason (#2919).
        // That snapshot is still read BEFORE the re-check's git, harness-lock
        // and dirt probes — seconds on a large tree — so the removal below
        // re-reads it once more, after all of them (#7652 critic round).
        let index = fresh_indexes
            .entry(candidate.registry_root.clone())
            .or_insert_with(|| (probes.index_for)(&candidate.registry_root));
        // #6561, #7267: the same resolution the survey ran, re-run FRESH — the
        // survey's answer is minutes old, and a widening applied only there
        // would propose a candidate this re-check could not confirm.
        let pr_now = resolve_with_index(
            &path,
            &candidate.registry_root,
            candidate.branch.as_deref(),
            index,
            true,
            &GhLandingProbe,
        );
        // FRESH, per candidate, and now genuinely immediately before the
        // re-check that judges it.
        let in_use_now = (probes.in_use_now)();
        // #6927: re-read, not reused — see `FreshProbes::keep_list`.
        let keep_list_now = (probes.keep_list)();
        // #7889: the survey offered gate 5's landed-content admission, so the
        // re-check re-asks it for a candidate no pull request carries.
        if let Some(reason) = recheck_before_delete(
            &path,
            &keep_list_now,
            in_use_now.as_ref(),
            &pr_now,
            probes.agent_state,
            Some(&reclaim_landed_content),
        ) {
            tracing::warn!(
                path = %path.display(),
                "worktree-reclaim: re-check refused a surveyed candidate — {reason} (#2919)"
            );
            out.refused_at_recheck
                .push(format!("{}: {reason}", path.display()));
            continue;
        }
        // #7885: the route names itself in the audit line the remover emits
        // before it deletes — an operator reading the log after the fact could
        // not otherwise tell this pass from the orphan sweep.
        // #7652 critic round: the guard runs after that line, immediately before
        // `git worktree remove --force`, against a claim set read right then.
        let late_refusal: std::cell::Cell<Option<String>> = std::cell::Cell::new(None);
        let outcome = super::decommission::remove_session_worktree_guarded(
            &path,
            &format!(
                "merged-PR reclaim: every gate passed and the pre-delete re-check agreed \
                 ({pr_now:?})"
            ),
            &|| {
                let refusal = last_moment_refusal(&path, (probes.in_use_now)().as_ref());
                late_refusal.set(refusal.clone());
                refusal
            },
        );
        if let Some(reason) = late_refusal.take() {
            tracing::warn!(
                path = %path.display(),
                "worktree-reclaim: the last claim re-read refused a surveyed candidate — \
                 {reason} (#7652)"
            );
            out.refused_at_recheck
                .push(format!("{}: {reason}", path.display()));
            continue;
        }
        if outcome.removed() && !path.exists() {
            // #7504: the AUDIT LINE. It carries path, branch, pull request and
            // bytes freed because the reclaim is now automatic: nobody typed the
            // command, so this line is the only record of what the daemon
            // decided and why it was allowed to. `bytes` is `None` when the
            // survey's measurement phase ran out of budget before reaching this
            // candidate — reported as absent rather than as zero, which would
            // read as "freed nothing".
            tracing::info!(
                path = %path.display(),
                branch = candidate.branch.as_deref().unwrap_or("(detached)"),
                pr = match &pr_now {
                    BranchPrState::Merged { pr } => Some(*pr),
                    // #7889: `NoPr` reaches here on landed content, recorded
                    // by `evidence` below. Rendered as absent rather than as a
                    // fabricated number.
                    _ => None,
                },
                evidence = ?candidate.verdict,
                bytes_freed = candidate.bytes,
                "worktree-reclaim: reclaimed a merged-PR worktree (#2919, #7504)"
            );
            out.removed_bytes = out
                .removed_bytes
                .saturating_add(candidate.bytes.unwrap_or(0));
            out.removed.push(path);
        } else {
            // #4732: report WHY it is still on disk. "removal failed" with no
            // reason reads as a transient error, but the commonest reason is
            // now a deliberate refusal — a `git worktree lock` the operator set
            // precisely so this pass would leave the worktree alone.
            let reason = outcome
                .reason()
                .unwrap_or("the directory is still present after a reported removal");
            tracing::warn!(
                path = %path.display(),
                "worktree-reclaim: worktree kept — {reason}"
            );
            out.removal_failed
                .push(format!("{}: {reason}", path.display()));
        }
    }
    out
}

/// [`reclaim_with_probes`] against the real `gh`-backed index and a live
/// re-read of the session store (#2919).
///
/// Why: the production entry point for the merged-PR reclaim — reached from the
/// `merged_prs` opt-in on `tm session prune-worktrees` AND, since #7504, from the
/// daemon's automatic post-merge sweep. Both arrive through
/// [`crate::daemon::services::merged_pr_reclaim::reclaim`], the one place the
/// production probes are assembled, so an operator-typed reclaim and an automatic
/// one cannot apply different gates.
/// What: `in_use_paths` and `keep_list` are both re-invoked per candidate by
/// the loop, so the caller must supply closures that genuinely RE-READ rather
/// than ones closing over a captured snapshot. `in_use_paths` returns `None`
/// when the set cannot be read, which refuses; `keep_list` returns
/// `KeepList::unreadable` when the config cannot be parsed, which keeps
/// everything (#6927).
/// Test: exercised through `reclaim_with_probes`' tests.
pub(crate) fn reclaim_merged_pr_worktrees(
    repos_root: &Path,
    in_use_paths: &dyn Fn() -> Option<LiveClaims>,
    agent_state: AgentStateProbe<'_>,
    mode: ReclaimMode,
    keep_list: &dyn Fn() -> KeepList,
    // #7357: resolved by the route that invokes this, never here.
    adopted: &[PathBuf],
    // #7504: the caller's own launch directories, resolved by the entry point
    // that assembles the probes — never here, so a test can hand in a scratch
    // path without the real process's cwd leaking into the comparison.
    launched_from: &[PathBuf],
) -> ReclaimOutcome {
    reclaim_with_probes(
        repos_root,
        &FreshProbes {
            in_use_now: in_use_paths,
            index_for: &PrIndex::from_gh,
            agent_state,
            keep_list,
            launched_from,
        },
        mode,
        adopted,
    )
}

#[cfg(test)]
#[path = "worktree_reclaim_sweep_tests.rs"]
mod worktree_reclaim_sweep_tests;
