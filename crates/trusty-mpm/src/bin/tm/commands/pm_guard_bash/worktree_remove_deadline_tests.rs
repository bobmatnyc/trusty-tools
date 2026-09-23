//! The #7889 deadline over the ADR-0057 removal re-checks.
//!
//! Why: an expired deadline must deny — never allow, never hang. A probe that
//! sleeps in one named check stands in for a slow `git fetch` or `gh` call.

use std::path::Path;
use std::time::{Duration, Instant};

use trusty_mpm::core::worktree_removal_facts::{
    MergedPrLookup, UpstreamComparison, WorktreeRemovalProbe,
};

use super::{DECISION_DEADLINE, REMOVAL_GUARD_BUDGET, evaluate_removal_rechecks_within, remaining};
use crate::commands::pm_guard_bash::worktree_remove_rechecks::{
    CHECK_CLEAN_TREE, CHECK_LOCAL_ONLY_COMMITS,
};

const WT: &str = "/repo/.claude/worktrees/agent-slow";

/// A clean tree whose `local-only-commits` check takes `slow`, then reports
/// zero local-only commits — which GRANTS if the evaluation is allowed to
/// finish. `dirty` makes `clean-tree` deny immediately instead.
struct SlowProbe {
    slow: Duration,
    dirty: usize,
    panics: bool,
}

impl WorktreeRemovalProbe for SlowProbe {
    fn dirty_entries(&self, _dir: &Path) -> Result<usize, String> {
        Ok(self.dirty)
    }
    fn unpushed_commits(&self, _dir: &Path) -> Result<UpstreamComparison, String> {
        Ok(UpstreamComparison::NoUpstream)
    }
    fn branch(&self, _dir: &Path) -> Result<String, String> {
        Ok("fix/slow".to_string())
    }
    fn local_only_commits(&self, _dir: &Path) -> Result<usize, String> {
        assert!(!self.panics, "probe blew up");
        std::thread::sleep(self.slow);
        Ok(0)
    }
    fn merged_pull_requests(&self, _dir: &Path, _b: &str) -> Result<MergedPrLookup, String> {
        Ok(MergedPrLookup::new(0, "o/r", ""))
    }
    fn merge_into_base_is_a_noop(&self, _dir: &Path, _base: &str) -> Result<bool, String> {
        Ok(false)
    }
    fn nested_dirt(&self, _dir: &Path) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// 🔴 REGRESSION (#7889): a re-check still running at the deadline DENIES,
/// promptly, and names the check that was pending. The probe would have
/// GRANTED had it been waited for, so an allow here means the deadline is
/// fail-open and a hang means it does not bound anything.
///
/// Fails against 72ae2eba5, which had no deadline: the evaluation ran to
/// completion however long it took, and granted.
#[test]
fn a_recheck_slower_than_the_deadline_denies_and_names_the_pending_check() {
    let probe = SlowProbe {
        slow: Duration::from_secs(5),
        dirty: 0,
        panics: false,
    };
    let started = Instant::now();
    let reason = evaluate_removal_rechecks_within(
        Path::new(WT),
        Ok(Vec::new()),
        probe,
        Duration::from_millis(200),
    )
    .expect("an expired deadline must deny, never allow");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the deadline must not wait for the slow check: {:?}",
        started.elapsed()
    );
    assert!(reason.contains("ran out of time"), "{reason}");
    assert!(
        reason.contains(&format!("`{CHECK_LOCAL_ONLY_COMMITS}` was still running")),
        "the deny must name the pending check: {reason}"
    );
}

/// 🔴 REGRESSION (#7889, critic MEDIUM): the budget is measured from PROCESS
/// START. A guard that reached this rule 2 s after `main` began has 1.5 s left,
/// not 3.5 s, and a slow check still denies inside it. A start 4 s back leaves
/// nothing, which denies at once; the audit's own deadline shrinks the same way.
///
/// This checks the budget arithmetic and the deny it bounds. That the two
/// async entry points pass their own `started` through is proved by
/// `removal_recheck_deny_measures_from_the_started_it_is_given` and
/// `print_deny_then_audit_measures_from_the_started_it_is_given`.
#[test]
fn a_late_start_still_denies_inside_the_remaining_budget() {
    let two_s_ago = Instant::now() - Duration::from_secs(2);
    let left = remaining(REMOVAL_GUARD_BUDGET, two_s_ago);
    assert!(left <= Duration::from_millis(1500), "{left:?}");
    assert!(left > Duration::from_millis(1000), "{left:?}");
    let slow = SlowProbe {
        slow: Duration::from_secs(5),
        dirty: 0,
        panics: false,
    };
    let reason = evaluate_removal_rechecks_within(Path::new(WT), Ok(Vec::new()), slow, left)
        .expect("a late start must still deny, never allow");
    assert!(
        two_s_ago.elapsed() < REMOVAL_GUARD_BUDGET + Duration::from_millis(300),
        "the deny must land inside the process-start budget: {:?}",
        two_s_ago.elapsed()
    );
    assert!(reason.contains("ran out of time"), "{reason}");

    let four_s_ago = Instant::now() - Duration::from_secs(4);
    assert_eq!(remaining(REMOVAL_GUARD_BUDGET, four_s_ago), Duration::ZERO);
    assert!(remaining(DECISION_DEADLINE, four_s_ago) <= Duration::from_millis(500));
    assert_eq!(
        remaining(DECISION_DEADLINE, Instant::now() - Duration::from_secs(5)),
        Duration::ZERO
    );
}

/// 🔴 REGRESSION (#7889 critic round 2): `removal_recheck_deny` measures its
/// budget from the `started` it is GIVEN. With a start 10 s in the past there
/// is no budget left, so it denies as out of time without running a check.
/// Had it used `Instant::now()`, it would have run the checks against this
/// nonexistent path and denied on `clean-tree` instead, with no timeout named.
#[tokio::test]
async fn removal_recheck_deny_measures_from_the_started_it_is_given() {
    let long_ago = Instant::now() - Duration::from_secs(10);
    let reason = super::removal_recheck_deny(
        "http://127.0.0.1:9",
        "11111111-1111-1111-1111-111111111111",
        Path::new("/nonexistent/.claude/worktrees/agent-late"),
        &serde_json::json!({}),
        long_ago,
    )
    .await
    .expect("no budget left must deny");
    assert!(reason.contains("ran out of time"), "{reason}");
}

/// 🔴 REGRESSION (#7889 critic round 2): `print_deny_then_audit` bounds the
/// audit by the `started` it is GIVEN. The daemon here accepts the connection
/// and never answers. With a start 10 s in the past the audit is skipped and
/// the call returns at once; with `Instant::now()` it would wait ~2 s.
#[tokio::test]
async fn print_deny_then_audit_measures_from_the_started_it_is_given() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", silent.local_addr().expect("addr"));
    let long_ago = Instant::now() - Duration::from_secs(10);
    let t = Instant::now();
    super::print_deny_then_audit(
        &url,
        "11111111-1111-1111-1111-111111111111",
        "Bash",
        "test deny",
        long_ago,
    )
    .await;
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "no budget left must skip the audit: {:?}",
        t.elapsed()
    );
    drop(silent);
}

/// #7889: a re-check that finishes in time returns its own verdict — the
/// deadline adds nothing to a fast answer, grant or deny.
#[test]
fn a_recheck_inside_the_deadline_returns_its_own_verdict() {
    let fast = |dirty| SlowProbe {
        slow: Duration::ZERO,
        dirty,
        panics: false,
    };
    let deadline = Duration::from_secs(10);
    assert_eq!(
        evaluate_removal_rechecks_within(Path::new(WT), Ok(Vec::new()), fast(0), deadline),
        None,
        "a clean tree with no local-only commits grants"
    );
    let reason = evaluate_removal_rechecks_within(Path::new(WT), Ok(Vec::new()), fast(2), deadline)
        .expect("a dirty tree denies");
    assert!(reason.contains(CHECK_CLEAN_TREE), "{reason}");
    assert!(!reason.contains("ran out of time"), "{reason}");
}

/// 🔴 #7889: a re-check thread that dies without answering denies.
#[test]
fn a_recheck_that_panics_denies() {
    let probe = SlowProbe {
        slow: Duration::ZERO,
        dirty: 0,
        panics: true,
    };
    let reason = evaluate_removal_rechecks_within(
        Path::new(WT),
        Ok(Vec::new()),
        probe,
        Duration::from_secs(10),
    )
    .expect("a thread that stopped without an answer must deny");
    assert!(reason.contains("stopped without an answer"), "{reason}");
}
