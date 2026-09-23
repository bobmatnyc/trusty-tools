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

use std::time::Instant;

use chrono::Utc;
use tracing::{info, warn};

use super::auth_backoff::{AuthBackoff, is_auth_failure};
use super::driver::{ClaimEnder, Gh, Git, Landing};
use super::registry::{CleanupRegistry, CleanupScope, OpenedPr};
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
///
/// #8301: an entry's recorded [`CleanupScope`] bounds the run — a deferred
/// entry is never read at all, and a head-only one is cleaned head-only. The
/// scope and stamp are re-read from disk immediately before cleaning, so a
/// deferral recorded while `gh` answered is still honoured.
///
/// #8058: every `gh` call passes `backoff` first. An authentication failure is
/// not a per-entry problem the next entry can succeed at — it is host-wide — so
/// without the gate one tick spawned one doomed `gh` per pending entry, every
/// tick, and logged one `warn!` per doomed call. The gate suspends the calls and
/// caps the log at a constant per backoff window; `/health` reads the same
/// gate's [`AuthBackoff::degraded_reason`]. Only an auth failure is gated — a
/// per-entry error still `continue`s untouched, or one stale registry row would
/// stop the whole sweep.
/// Test: `sweep_stamps_only_a_fully_successful_run`,
/// `sweep_leaves_a_dirty_worktree_pending`,
/// `sweep_stops_calling_gh_after_repeated_auth_failures`,
/// `a_non_auth_failure_never_suspends_the_sweep`,
/// `sweep_never_touches_a_deferred_entry`,
/// `sweep_honours_a_recorded_head_only_scope`,
/// `sweep_rereads_a_deferral_written_during_the_pr_read`.
pub async fn run_sweep<G: Gh, T: Git, C: ClaimEnder>(
    gh: &G,
    git: &T,
    claims: &C,
    landing: &dyn Landing,
    probe_dirt: DirtProbe<'_>,
    registry: &CleanupRegistry,
    backoff: &AuthBackoff,
) -> usize {
    let mut cleaned = 0usize;
    for entry in registry.pending() {
        if !backoff.may_poll(Instant::now()) {
            // Suspended: the reason was logged once when the window opened and
            // is published on /health for as long as it stays open.
            break;
        }
        let req = CleanupRequest {
            pr: entry.pr,
            repo: Some(entry.repo.clone()),
            repo_root: entry.repo_root.clone(),
            dry_run: false,
            // #8301: a merge-chained cleanup's head-only scope outlives the
            // merge; a deferred entry never reaches here (`pending` drops it).
            head_only: entry.scope == CleanupScope::HeadOnly,
        };
        let view = match super::view_pr(gh, &req) {
            Ok(v) => {
                backoff.record_answer();
                v
            }
            Err(e) => {
                let detail = format!("{e:#}");
                if is_auth_failure(&detail) {
                    if backoff.record_auth_failure(Instant::now(), &detail) {
                        warn!(
                            pr = entry.pr,
                            repo = %entry.repo,
                            "pr cleanup sweep: `gh` authentication failed: {detail}"
                        );
                    }
                    continue;
                }
                backoff.record_answer();
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
                // #8301: `gh pr view` took time; re-read this entry so a
                // deferral or stamp written meanwhile wins, and so does a
                // newly recorded head-only scope.
                let Some(fresh) = registry
                    .entry(&entry.repo, entry.pr)
                    .filter(OpenedPr::pending)
                else {
                    info!(pr = entry.pr, repo = %entry.repo, "pr cleanup sweep: skipping — deferred or cleaned since it was read");
                    continue;
                };
                let req = CleanupRequest {
                    head_only: fresh.scope == CleanupScope::HeadOnly,
                    ..req
                };
                let report = super::run(gh, git, claims, landing, probe_dirt, &req).await;
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
