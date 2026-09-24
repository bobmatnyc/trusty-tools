//! One deadline over the ADR-0057 removal re-checks (#7889).
//!
//! Why: the guard runs inside a `PreToolUse` hook that Claude Code kills at 5 s
//! (`crate::core::standalone::hooks::mpm_hook_additions_with_exe` registers the
//! timeout). A killed hook prints no decision, and no decision is not a deny —
//! the removal then proceeds on Claude Code's own permission flow. The re-checks
//! call `git fetch` (3 s bound) and several `gh` lookups (10 s bound each), and
//! #7889 added the landing admission to the refusing path, so a slow network
//! turned a refusal into no decision at all.
//!
//! What: [`removal_recheck_deny`] runs the daemon owner query and then
//! [`evaluate_removal_rechecks_within`], all inside [`REMOVAL_GUARD_BUDGET`]
//! measured from process start. The re-checks run on a detached `std::thread`;
//! the caller waits on a channel with a timeout, so an expired deadline DENIES
//! and names the check still running, and nothing joins the thread — process
//! exit never waits for it.
//! Test: `a_recheck_slower_than_the_deadline_denies_and_names_the_pending_check`,
//! `a_late_start_still_denies_inside_the_remaining_budget`,
//! `a_recheck_inside_the_deadline_returns_its_own_verdict`,
//! `a_recheck_that_panics_denies`,
//! `a_silent_owner_query_denies_inside_the_remaining_budget` (#8082),
//! `the_removal_deadlines_fit_inside_the_registered_hook_timeout` (#8082).

use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;
use trusty_mpm::core::worktree_landed_content::{LANDED_CONTENT_CHECK, LandingAdmission};
use trusty_mpm::core::worktree_removal_facts::{
    GitAndGhProbe, MergedPrLookup, UpstreamComparison, WorktreeRemovalProbe,
};

use super::worktree_remove_rechecks::{
    CHECK_CLEAN_TREE, CHECK_LOCAL_ONLY_COMMITS, CHECK_MERGED_PULL_REQUEST, CHECK_SOLE_OWNER,
    CHECK_UNPUSHED_COMMITS, evaluate_removal_rechecks, recheck_deny,
};
use crate::commands::pm_guard::{audit_denied_tool, build_pm_guard_deny_response};
use crate::commands::pm_guard_dispatch;
use std::io::Write;

/// By when, from PROCESS START, the re-checks must have decided (#7889).
///
/// Why: the hook is killed at 5 s, measured by Claude Code from the spawn, so
/// the budget is measured from the first line of `main`, not from wherever the
/// guard reaches this rule. 3.5 s leaves 1 s for the deny's print, its audit
/// (which must end by [`DECISION_DEADLINE`]) and exit. It still fits the normal
/// grant path: one fetch, one or two `gh` lookups and a local `merge-tree` —
/// see the #7889 latency measurement in the PR.
pub(crate) const REMOVAL_GUARD_BUDGET: Duration = Duration::from_millis(3500);

/// By when, from process start, the deny's audit must have finished (#7889).
///
/// Why: `audit_denied_tool` allows itself 2 s, which could carry the process
/// past the 5 s kill. 4.5 s leaves 0.5 s to exit; the deny itself is already
/// printed and flushed before the audit starts.
pub(crate) const DECISION_DEADLINE: Duration = Duration::from_millis(4500);

/// What is left of `budget` for a process that started at `started` (#7889).
///
/// Test: `a_late_start_still_denies_inside_the_remaining_budget`.
pub(crate) fn remaining(budget: Duration, started: Instant) -> Duration {
    budget.saturating_sub(started.elapsed())
}

/// The removal re-checks' deny reason, or `None` to allow, decided inside
/// [`REMOVAL_GUARD_BUDGET`] of process start (#7889, ADR-0057).
///
/// Why: see the module doc. The owner query is inside the budget because it
/// is a network call too (2 s client timeout). It uses `_or_deny`, not the
/// HEAD-move rule's fail-open reader: an unreachable daemon establishes nothing
/// about who holds this tree. Keyed on the TARGET, not the caller's cwd — the
/// tree being deleted is the one whose owner matters.
/// What: the owner query, itself bounded by the budget left at process start
/// (#8082), then [`evaluate_removal_rechecks_within`] with whatever budget
/// startup and the query left.
/// Test: as the module doc; the owner arm in
/// `denies_worktree_remove_from_version_control_when_the_owner_query_fails`;
/// the bounded query in `a_silent_owner_query_denies_inside_the_remaining_budget`.
pub(crate) async fn removal_recheck_deny(
    url: &str,
    session_id: &str,
    target: &Path,
    payload: &Value,
    started: Instant,
) -> Option<String> {
    // #8082: the owner query is bounded by what is left of the budget, not only
    // by its own 2.5 s client timeouts — a late start plus a silent daemon
    // otherwise decided after the deadline. Expiry denies, naming `sole-owner`.
    let query =
        pm_guard_dispatch::live_shared_tree_writers_or_deny(url, session_id, target, payload);
    let owner_budget = remaining(REMOVAL_GUARD_BUDGET, started);
    let Ok(live) = tokio::time::timeout(owner_budget, query).await else {
        return Some(out_of_time(CHECK_SOLE_OWNER, target, owner_budget));
    };
    let left = remaining(REMOVAL_GUARD_BUDGET, started);
    evaluate_removal_rechecks_within(target, live, GitAndGhProbe, left)
}

/// Print and flush the deny, then audit it inside [`DECISION_DEADLINE`] (#7889).
///
/// Why: a deny still sitting in a buffer when the hook is killed decides
/// nothing, and the audit is best-effort, so the decision goes out first.
/// What: prints the `PreToolUse` deny, flushes stdout, then runs the audit
/// under whatever is left before [`DECISION_DEADLINE`], skipping it when
/// nothing is.
/// Test: `a_late_start_still_denies_inside_the_remaining_budget` (the budget
/// arithmetic); the print path in `tests/tm_hook_pm_guard.rs`.
pub(crate) async fn print_deny_then_audit(
    url: &str,
    session_id: &str,
    tool_name: &str,
    reason: &str,
    started: Instant,
) {
    println!("{}", build_pm_guard_deny_response(reason));
    // Nothing useful can be done with a flush error; the deny is written.
    let _ = std::io::stdout().flush();
    let left = remaining(DECISION_DEADLINE, started);
    if left.is_zero() {
        return;
    }
    let audit = audit_denied_tool(url, session_id, tool_name, reason);
    let _ = tokio::time::timeout(left, audit).await;
}

/// [`evaluate_removal_rechecks`] under a deadline; expiry denies (#7889).
///
/// Why: a deadline that allowed on expiry would be the fail-open this module
/// exists to close, and one that blocked process exit would be killed by the
/// hook timeout anyway.
/// What: runs the re-checks on a detached thread through [`PendingProbe`],
/// which records the check each probe call belongs to. The verdict is returned
/// when it arrives within `deadline`. On expiry the deny names that pending
/// check; a thread that died without answering (a panic) or could not be
/// spawned also denies.
/// Test: `a_recheck_slower_than_the_deadline_denies_and_names_the_pending_check`,
/// `a_recheck_inside_the_deadline_returns_its_own_verdict`,
/// `a_recheck_that_panics_denies`.
pub(crate) fn evaluate_removal_rechecks_within<P>(
    target: &Path,
    live_owners: Result<Vec<String>, String>,
    probe: P,
    deadline: Duration,
) -> Option<String>
where
    P: WorktreeRemovalProbe + Send + 'static,
{
    // #7889 critic round 2: no time left means no check can finish, so deny
    // without starting one — and deterministically, not by racing a thread.
    if deadline.is_zero() {
        return Some(out_of_time(CHECK_CLEAN_TREE, target, deadline));
    }
    // The owner answer is already in hand, so the first probe-backed check
    // is the one pending until the thread reports otherwise.
    let pending = Arc::new(Mutex::new(CHECK_CLEAN_TREE));
    let recording = PendingProbe {
        inner: probe,
        pending: Arc::clone(&pending),
    };
    let (tx, rx) = mpsc::channel();
    let owned = target.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("worktree-remove-rechecks".into())
        .spawn(move || {
            let owners = live_owners.as_deref().map_err(String::as_str);
            // A closed receiver means the deadline already denied.
            let _ = tx.send(evaluate_removal_rechecks(&owned, owners, &recording));
        });
    if let Err(e) = spawned {
        return Some(recheck_deny(
            CHECK_SOLE_OWNER,
            target,
            &format!("the re-check thread could not be started: {e}."),
        ));
    }
    let pending_check = || pending.lock().map(|g| *g).unwrap_or(CHECK_CLEAN_TREE);
    match rx.recv_timeout(deadline) {
        Ok(verdict) => verdict,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Some(out_of_time(pending_check(), target, deadline))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Some(recheck_deny(
            pending_check(),
            target,
            "the re-check thread stopped without an answer.",
        )),
    }
}

/// The deny for a deadline that expired while `check` was pending (#7889).
fn out_of_time(check: &str, target: &Path, deadline: Duration) -> String {
    recheck_deny(
        check,
        target,
        &format!(
            "the ADR-0057 re-checks ran out of time — they did not finish within {} ms, \
             and `{check}` was still running. The hook is killed at 5 s and a killed hook \
             decides nothing, so an unfinished check denies.",
            deadline.as_millis()
        ),
    )
}

/// A probe that records which re-check each call belongs to (#7889).
///
/// Why: an expired deadline's deny must name the check that did not finish.
/// What: every trait method — the defaulted ones included, so the wrapped
/// probe's own overrides are never bypassed — sets `pending`, then delegates.
struct PendingProbe<P> {
    inner: P,
    pending: Arc<Mutex<&'static str>>,
}

impl<P> PendingProbe<P> {
    fn mark(&self, check: &'static str) {
        if let Ok(mut slot) = self.pending.lock() {
            *slot = check;
        }
    }
}

impl<P: WorktreeRemovalProbe> WorktreeRemovalProbe for PendingProbe<P> {
    fn dirty_entries(&self, dir: &Path) -> Result<usize, String> {
        self.mark(CHECK_CLEAN_TREE);
        self.inner.dirty_entries(dir)
    }
    fn unpushed_commits(&self, dir: &Path) -> Result<UpstreamComparison, String> {
        self.mark(CHECK_UNPUSHED_COMMITS);
        self.inner.unpushed_commits(dir)
    }
    fn branch(&self, dir: &Path) -> Result<String, String> {
        self.mark(CHECK_MERGED_PULL_REQUEST);
        self.inner.branch(dir)
    }
    fn head_sha(&self, dir: &Path) -> Result<String, String> {
        self.mark(CHECK_MERGED_PULL_REQUEST);
        self.inner.head_sha(dir)
    }
    fn merged_pull_request_for_commit(
        &self,
        dir: &Path,
        sha: &str,
    ) -> Result<MergedPrLookup, String> {
        self.mark(CHECK_MERGED_PULL_REQUEST);
        self.inner.merged_pull_request_for_commit(dir, sha)
    }
    fn local_only_commits(&self, dir: &Path) -> Result<usize, String> {
        self.mark(CHECK_LOCAL_ONLY_COMMITS);
        self.inner.local_only_commits(dir)
    }
    fn merged_pull_requests(&self, dir: &Path, branch: &str) -> Result<MergedPrLookup, String> {
        self.mark(CHECK_MERGED_PULL_REQUEST);
        self.inner.merged_pull_requests(dir, branch)
    }
    fn merge_into_base_is_a_noop(&self, dir: &Path, base_ref: &str) -> Result<bool, String> {
        self.mark(CHECK_MERGED_PULL_REQUEST);
        self.inner.merge_into_base_is_a_noop(dir, base_ref)
    }
    fn landing_admission(&self, dir: &Path) -> LandingAdmission {
        self.mark(LANDED_CONTENT_CHECK);
        self.inner.landing_admission(dir)
    }
    fn landing_admission_on_fetched_refs(&self, dir: &Path) -> LandingAdmission {
        self.mark(LANDED_CONTENT_CHECK);
        self.inner.landing_admission_on_fetched_refs(dir)
    }
    fn nested_dirt(&self, dir: &Path) -> Result<Option<String>, String> {
        self.mark(CHECK_CLEAN_TREE);
        self.inner.nested_dirt(dir)
    }
}

#[cfg(test)]
#[path = "worktree_remove_deadline_tests.rs"]
mod worktree_remove_deadline_tests;
