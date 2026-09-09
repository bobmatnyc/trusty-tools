//! The periodic trigger: clean up each pull request that has newly MERGED
//! (#7275, owner scope amendment 2026-09-09).
//!
//! Why: `tm pr merge` runs cleanup as its own final step, but a PR merged any
//! other way — the GitHub UI, `--auto` firing hours later, another
//! contributor — leaves its worktree, branches and session claim behind with
//! nothing scheduled to notice. The owner's ruling is that a merge is "the most
//! likely determiner of the obsolescence of those items", so the daemon watches
//! the PRs this harness opened and acts on the transition.
//!
//! What: [`sweep_decision`] is the whole trigger as a pure function — given a
//! registry entry and what `gh` says about it, decide whether to run cleanup.
//! [`run_sweep`] is the loop around it: read the pending entries, ask `gh`
//! once each, run cleanup on the merged ones, and stamp each success so no
//! later sweep repeats it.
//!
//! IDEMPOTENCE has two independent guards, deliberately. The registry stamp
//! stops a second sweep from considering an entry at all, and
//! [`SweepDecision::AlreadyCleaned`] stops one that somehow still arrives. A
//! destructive sequence re-run against a repository it already cleaned reports
//! failures for work that succeeded, which is worse than not running.
//!
//! Test: the sibling `sweep_tests.rs`.

use chrono::Utc;
use tracing::{info, warn};

use super::driver::{ClaimEnder, Gh, Git};
use super::registry::{CleanupRegistry, OpenedPr};
use super::{CleanupRequest, DirtProbe, plan};

/// What the periodic trigger decided about one registry entry.
///
/// Test: `sweep_decision_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepDecision {
    /// Run cleanup: the PR is MERGED and has not been cleaned.
    Clean,
    /// The entry carries a `cleaned_at` stamp; never re-run it.
    AlreadyCleaned,
    /// The PR has not merged yet — the reason, for the log.
    NotMerged(String),
}

/// Decide whether the periodic trigger should clean up one entry.
///
/// Why: the whole trigger is this judgment, and keeping it pure is what makes
/// "newly merged fires once, an already-cleaned PR never fires" testable
/// without a daemon, a `gh`, or a repository to delete from.
/// What: an entry already stamped answers [`AlreadyCleaned`](SweepDecision::AlreadyCleaned)
/// BEFORE the state is even consulted — the stamp outranks whatever GitHub says
/// now. Otherwise the same [`plan::merge_refusal`] the executor applies decides,
/// so the trigger and `tm pr cleanup <n>` can never disagree about what
/// "merged" means.
/// Test: `sweep_decision_cleans_a_newly_merged_pr`,
/// `sweep_decision_never_reruns_a_cleaned_pr`,
/// `sweep_decision_leaves_an_open_pr_alone`.
pub fn sweep_decision(entry: &OpenedPr, view: &plan::PrView) -> SweepDecision {
    if !entry.pending() {
        return SweepDecision::AlreadyCleaned;
    }
    match plan::merge_refusal(view, entry.pr) {
        Some(reason) => SweepDecision::NotMerged(reason),
        None => SweepDecision::Clean,
    }
}

/// One periodic sweep: clean up every pending PR that has merged.
///
/// Why: the loop is separated from [`sweep_decision`] so the judgment can be
/// tested exhaustively while this stays a thin, readable driver — read, ask,
/// decide, execute, stamp.
/// What: for each pending registry entry, reads the PR once, applies
/// [`sweep_decision`], and on [`SweepDecision::Clean`] runs [`super::run`] in
/// the entry's own checkout. A run whose every step succeeded is stamped
/// `cleaned_at`; a run with ANY failed step is left pending and logged at
/// `warn`, so a refused dirty worktree is retried after the operator saves
/// their work rather than being silently forgotten.
/// Returns the number of entries cleaned this sweep.
/// Test: `sweep_stamps_only_a_fully_successful_run`,
/// `sweep_leaves_a_dirty_worktree_pending`.
pub async fn run_sweep<G: Gh, T: Git, C: ClaimEnder>(
    gh: &G,
    git: &T,
    claims: &C,
    probe_dirt: DirtProbe<'_>,
    registry: &CleanupRegistry,
) -> usize {
    let mut cleaned = 0usize;
    for entry in registry.pending() {
        let req = CleanupRequest {
            pr: entry.pr,
            repo: Some(entry.repo.clone()),
            repo_root: entry.repo_root.clone(),
            dry_run: false,
        };
        let view = match super::view_pr(gh, &req) {
            Ok(v) => v,
            Err(e) => {
                warn!(pr = entry.pr, repo = %entry.repo, "pr cleanup sweep: cannot read PR: {e:#}");
                continue;
            }
        };
        match sweep_decision(&entry, &view) {
            SweepDecision::AlreadyCleaned => continue,
            SweepDecision::NotMerged(reason) => {
                info!(pr = entry.pr, repo = %entry.repo, "pr cleanup sweep: skipping — {reason}");
            }
            SweepDecision::Clean => {
                let report = super::run(gh, git, claims, probe_dirt, &req).await;
                if report.failed() {
                    warn!(
                        pr = entry.pr,
                        repo = %entry.repo,
                        "pr cleanup sweep: cleanup did not complete; leaving it pending:\n{}",
                        report.render()
                    );
                    continue;
                }
                if let Err(e) = registry.mark_cleaned(&entry.repo, entry.pr, Utc::now()) {
                    // Not stamping is the dangerous half: the next sweep would
                    // re-run a destructive sequence against an already-clean
                    // repository. Say so loudly rather than counting a success.
                    warn!(
                        pr = entry.pr,
                        repo = %entry.repo,
                        "pr cleanup sweep: cleaned #{} but could not stamp the registry: {e:#}; \
                         the next sweep will retry it",
                        entry.pr
                    );
                    continue;
                }
                info!(pr = entry.pr, repo = %entry.repo, "pr cleanup sweep: cleaned up after merge");
                cleaned += 1;
            }
        }
    }
    cleaned
}

#[cfg(test)]
#[path = "sweep_tests.rs"]
mod sweep_tests;
