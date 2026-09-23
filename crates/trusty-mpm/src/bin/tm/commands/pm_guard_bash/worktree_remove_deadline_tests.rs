//! The #7889 deadline over the ADR-0057 removal re-checks.
//!
//! Why: an expired deadline must deny — never allow, never hang. A probe that
//! sleeps in one named check stands in for a slow `git fetch` or `gh` call.

use std::path::Path;
use std::time::{Duration, Instant};

use trusty_mpm::core::worktree_removal_facts::{
    MergedPrLookup, UpstreamComparison, WorktreeRemovalProbe,
};

use super::evaluate_removal_rechecks_within;
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
