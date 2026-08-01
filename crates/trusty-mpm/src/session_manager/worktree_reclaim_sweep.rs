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
//! (`Admission` was evaluated at survey time, and
//! `remove_session_worktree`'s `remove_dir_all` fallback deletes a locked
//! worktree regardless). The first cut of this feature made exactly that
//! mistake, and its three "re-checks" all survived mutation testing because
//! they re-asked stale inputs.
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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::worktree_reclaim::{
    BranchPrState, PrIndex, ReclaimCandidate, ReclaimMode, ReclaimOutcome, ReclaimSurvey,
    ReclaimVerdict, classify, is_live, measure_bytes_until, pr_state_for_branch, tm_provisioned,
};
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
    /// Stop measuring bytes after this instant; the rest report `unmeasured`.
    pub measure: Option<Instant>,
    /// Stop classifying after this instant; the rest are `Blocked`.
    pub classify: Option<Instant>,
}

impl SurveyBudget {
    /// A budget that never expires — the operator-invoked reclaim path.
    ///
    /// Why: a human who typed `--merged-prs` is waiting for a correct answer,
    /// not a fast one, and a partial classification on the DESTRUCTIVE path
    /// would silently shrink what gets reclaimed rather than shrink a report.
    /// Test: `reclaim_uses_an_unbounded_budget`.
    pub(crate) fn unbounded() -> Self {
        Self {
            measure: None,
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
/// When the bulk index was truncated and `per_branch_fallback` is set, a branch
/// the index cannot answer is resolved with a targeted
/// [`pr_state_for_branch`] call — without that, no worktree older than the last
/// [`PR_INDEX_LIMIT`](super::worktree_reclaim::PR_INDEX_LIMIT) pull requests
/// could ever be reclaimed, which on this repository is nearly all of them.
/// `index_for` is injectable so the classification paths are testable
/// without `gh`.
/// Test: `survey_reports_a_merged_worktree_as_reclaimable`,
/// `survey_past_its_classify_deadline_reclaims_nothing`.
pub(crate) fn survey_with_index(
    repos_root: &Path,
    in_use: &[PathBuf],
    index_for: &dyn Fn(&Path) -> PrIndex,
    budget: SurveyBudget,
    per_branch_fallback: bool,
) -> ReclaimSurvey {
    let mut indexes: BTreeMap<PathBuf, PrIndex> = BTreeMap::new();
    let mut candidates = Vec::new();
    for scanned in scan_registered_worktrees(repos_root) {
        if budget.classify.is_some_and(|d| Instant::now() >= d) {
            // #2919: fail closed. A candidate we ran out of time to inspect is
            // reported as blocked, never omitted and never approved.
            candidates.push(ReclaimCandidate {
                path: scanned.path,
                branch: scanned.branch,
                registry_root: scanned.registry_root,
                bytes: None,
                pr: BranchPrState::Unknown,
                verdict: ReclaimVerdict::blocked("survey deadline reached before inspection"),
            });
            continue;
        }
        let index = indexes
            .entry(scanned.registry_root.clone())
            .or_insert_with(|| index_for(&scanned.registry_root));
        let mut pr = index.state_for(scanned.branch.as_deref());
        if per_branch_fallback
            && pr == BranchPrState::Unknown
            && !index.is_complete()
            && let Some(branch) = scanned.branch.as_deref()
        {
            pr = pr_state_for_branch(&scanned.registry_root, branch);
        }
        let verdict = classify(
            &scanned.path,
            scanned.admission,
            is_live(&scanned.path, in_use),
            &pr,
            &inspect_dirt,
        );
        candidates.push(ReclaimCandidate {
            bytes: measure_bytes_until(&scanned.path, budget.measure),
            path: scanned.path,
            branch: scanned.branch,
            registry_root: scanned.registry_root,
            pr,
            verdict,
        });
    }
    ReclaimSurvey::from_candidates(candidates)
}

/// [`survey_with_index`] against the real `gh`-backed index.
///
/// Why: the production entry point; the injectable form exists for tests.
/// Test: covered through [`survey_with_index`].
pub(crate) fn survey(
    repos_root: &Path,
    in_use: &[PathBuf],
    budget: SurveyBudget,
    per_branch_fallback: bool,
) -> ReclaimSurvey {
    survey_with_index(
        repos_root,
        in_use,
        &PrIndex::from_gh,
        budget,
        per_branch_fallback,
    )
}

/// Re-ask git, RIGHT NOW, whether this path may still be removed (#2919).
///
/// Why: `Admission` was decided during the survey. Ten minutes later the
/// operator may have run `git worktree lock` — an explicit "do not remove
/// this" — and the survey's verdict knows nothing about it. This matters more
/// than it looks: git itself honours the lock (`git worktree remove --force`
/// exits 128), but `remove_session_worktree` treats that as a git failure and
/// falls back to `remove_dir_all`, which deletes the directory anyway. The lock
/// can therefore only be honoured by re-checking it here.
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
/// What: `in_use_now` is `None` when the live session set could not be re-read
/// at all — which REFUSES, because an unanswerable liveness question must never
/// resolve to "nothing claims it". Then git's current verdict
/// ([`git_still_permits`], which honours a lock applied since the survey), the
/// ownership marker, the current pull-request state, and finally
/// [`inspect_dirt`], which fails toward dirty. `Some(reason)` refuses;
/// `None` permits.
/// Test: one test per branch —
/// `recheck_refuses_when_the_live_set_cannot_be_read`,
/// `recheck_refuses_a_worktree_a_session_claims_now`,
/// `recheck_refuses_a_worktree_locked_after_the_survey`,
/// `recheck_refuses_a_path_git_no_longer_lists`,
/// `recheck_refuses_a_worktree_that_lost_its_ownership_marker`,
/// `recheck_refuses_when_the_pr_is_no_longer_merged`,
/// `recheck_refuses_a_worktree_dirtied_after_the_survey`,
/// `recheck_permits_a_clean_merged_owned_worktree`.
pub(crate) fn recheck_before_delete(
    path: &Path,
    in_use_now: Option<&[PathBuf]>,
    pr_now: &BranchPrState,
) -> Option<String> {
    let Some(in_use_now) = in_use_now else {
        return Some("the live session set could not be re-read — refusing to delete".into());
    };
    if is_live(path, in_use_now) {
        return Some("a session claims this workspace now".into());
    }
    if let Err(reason) = git_still_permits(path) {
        return Some(reason);
    }
    if !tm_provisioned(path) {
        return Some("no longer carries a trusty-mpm ownership marker".into());
    }
    if !matches!(pr_now, BranchPrState::Merged { .. }) {
        return Some(format!(
            "pull-request state is no longer a merge ({pr_now:?})"
        ));
    }
    if let Some(dirt) = inspect_dirt(path) {
        return Some(format!("holds unsaved work: {}", dirt.reason));
    }
    None
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
/// What: `in_use_now` re-reads the workspace paths sessions claim, returning
/// `None` when that cannot be determined (which refuses). `index_for` rebuilds
/// a repository's pull-request index.
/// Test: used by every `reclaim_remove_mode_*` test.
pub(crate) struct FreshProbes<'a> {
    /// Re-read the claimed workspace paths; `None` means "could not determine".
    pub in_use_now: &'a dyn Fn() -> Option<Vec<PathBuf>>,
    /// Rebuild a repository's pull-request index.
    pub index_for: &'a dyn Fn(&Path) -> PrIndex,
}

/// Survey, and in [`ReclaimMode::Remove`] reclaim, merged-PR worktrees (#2919).
///
/// Why: see this module's staleness rule. The survey establishes candidates;
/// nothing it computed is trusted at deletion time.
/// What: surveys with an initial snapshot, then — in `Remove` mode only —
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
) -> ReclaimOutcome {
    let initial = (probes.in_use_now)().unwrap_or_default();
    let survey = survey_with_index(
        repos_root,
        &initial,
        probes.index_for,
        SurveyBudget::unbounded(),
        true,
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
        // FRESH, per candidate, immediately before this candidate's delete.
        let in_use_now = (probes.in_use_now)();
        let index = fresh_indexes
            .entry(candidate.registry_root.clone())
            .or_insert_with(|| (probes.index_for)(&candidate.registry_root));
        let mut pr_now = index.state_for(candidate.branch.as_deref());
        if pr_now == BranchPrState::Unknown
            && !index.is_complete()
            && let Some(branch) = candidate.branch.as_deref()
        {
            pr_now = pr_state_for_branch(&candidate.registry_root, branch);
        }
        if let Some(reason) = recheck_before_delete(&path, in_use_now.as_deref(), &pr_now) {
            tracing::warn!(
                path = %path.display(),
                "worktree-reclaim: re-check refused a surveyed candidate — {reason} (#2919)"
            );
            out.refused_at_recheck
                .push(format!("{}: {reason}", path.display()));
            continue;
        }
        if super::decommission::remove_session_worktree(&path) && !path.exists() {
            tracing::info!(
                path = %path.display(),
                "worktree-reclaim: reclaimed a merged-PR worktree (#2919)"
            );
            out.removed_bytes = out
                .removed_bytes
                .saturating_add(candidate.bytes.unwrap_or(0));
            out.removed.push(path);
        } else {
            out.removal_failed.push(path);
        }
    }
    out
}

/// [`reclaim_with_probes`] against the real `gh`-backed index and a live
/// re-read of the session store (#2919).
///
/// Why: the production entry point, reached only from the explicit
/// `merged_prs` opt-in on `tm session prune-worktrees`.
/// What: `in_use_paths` is re-invoked per candidate by the loop, so the caller
/// must supply a closure that genuinely RE-READS rather than one that closes
/// over a captured snapshot. It returns `None` when the set cannot be read,
/// which refuses.
/// Test: exercised through `reclaim_with_probes`' tests.
pub(crate) fn reclaim_merged_pr_worktrees(
    repos_root: &Path,
    in_use_paths: &dyn Fn() -> Option<Vec<PathBuf>>,
    mode: ReclaimMode,
) -> ReclaimOutcome {
    reclaim_with_probes(
        repos_root,
        &FreshProbes {
            in_use_now: in_use_paths,
            index_for: &PrIndex::from_gh,
        },
        mode,
    )
}

#[cfg(test)]
#[path = "worktree_reclaim_sweep_tests.rs"]
mod worktree_reclaim_sweep_tests;
