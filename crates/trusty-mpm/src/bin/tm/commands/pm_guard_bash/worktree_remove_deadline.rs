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
//! measured from its own start. The re-checks run on a detached `std::thread`;
//! the caller waits on a channel with a timeout, so an expired deadline DENIES
//! and names the check still running, and nothing joins the thread — process
//! exit never waits for it.
//! Test: `a_recheck_slower_than_the_deadline_denies_and_names_the_pending_check`,
//! `a_recheck_inside_the_deadline_returns_its_own_verdict`,
//! `a_recheck_that_panics_denies`.

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
use crate::commands::pm_guard_dispatch;

/// How long the owner query plus every re-check may take (#7889).
///
/// Why: the hook is killed at 5 s. After the re-checks a deny still has to be
/// printed and audited (the audit is bounded by [`RECHECK_AUDIT_BUDGET`]), and
/// the process has already spent time starting and parsing the payload before
/// this runs. 3.5 s + 0.75 s leaves ~0.75 s for startup and output. It still
/// fits the normal grant path: one fetch (~1.3 s measured 2026-09-15), one or
/// two `gh` lookups (~1 s each) and a local `merge-tree`.
pub(crate) const REMOVAL_GUARD_BUDGET: Duration = Duration::from_millis(3500);

/// How long the daemon audit of a re-check deny may take (#7889).
///
/// Why: `audit_denied_tool` allows itself 2 s, which on top of
/// [`REMOVAL_GUARD_BUDGET`] would pass the hook's 5 s. The deny is printed
/// before the audit, and the audit is best-effort.
pub(crate) const RECHECK_AUDIT_BUDGET: Duration = Duration::from_millis(750);

/// The removal re-checks' deny reason, or `None` to allow, decided inside
/// [`REMOVAL_GUARD_BUDGET`] (#7889, ADR-0057).
///
/// Why: see the module doc. The owner query is inside the budget because it
/// is a network call too (2 s client timeout). It uses `_or_deny`, not the
/// HEAD-move rule's fail-open reader: an unreachable daemon establishes nothing
/// about who holds this tree. Keyed on the TARGET, not the caller's cwd — the
/// tree being deleted is the one whose owner matters.
/// What: the owner query, then [`evaluate_removal_rechecks_within`] with
/// whatever budget the query left.
/// Test: as the module doc; the owner arm in
/// `denies_worktree_remove_from_version_control_when_the_owner_query_fails`.
pub(crate) async fn removal_recheck_deny(
    url: &str,
    session_id: &str,
    target: &Path,
    payload: &Value,
) -> Option<String> {
    let started = Instant::now();
    let live =
        pm_guard_dispatch::live_shared_tree_writers_or_deny(url, session_id, target, payload).await;
    let remaining = REMOVAL_GUARD_BUDGET.saturating_sub(started.elapsed());
    evaluate_removal_rechecks_within(target, live, GitAndGhProbe, remaining)
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
            let check = pending_check();
            Some(recheck_deny(
                check,
                target,
                &format!(
                    "the ADR-0057 re-checks ran out of time — they did not finish within \
                     {} ms, and `{check}` was still running. The hook is killed at 5 s and a \
                     killed hook decides nothing, so an unfinished check denies.",
                    deadline.as_millis()
                ),
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Some(recheck_deny(
            pending_check(),
            target,
            "the re-check thread stopped without an answer.",
        )),
    }
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
}

#[cfg(test)]
#[path = "worktree_remove_deadline_tests.rs"]
mod worktree_remove_deadline_tests;
