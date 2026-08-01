//! Tests for the survey and the fresh-recheck delete loop (#2919).
//!
//! Why: the first cut of this feature had three pre-delete "re-checks" that ALL
//! survived mutation testing — deleting every one of them left 31/31 tests
//! green. They re-asked the survey's own captured inputs, so re-asking changed
//! nothing, and the one test that claimed to cover them fed the same `in_use`
//! to the survey, which blocked the candidate during classification so the
//! delete loop never executed at all. Its name asserted the opposite of what it
//! did.
//!
//! What: every re-check gets (a) a direct unit test of its branch in
//! [`recheck_before_delete`], and (b) a loop test whose probes CHANGE the
//! workspace between the survey and the delete — a session attaching, a
//! `git worktree lock`, a file appearing, a pull request reopening. That second
//! shape is the only one that exercises a re-check, and each of these tests
//! fails if its re-check is removed.

use std::cell::RefCell;
use std::path::Path;

use super::*;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// Put the worktree in the state a merged PR leaves behind: one commit, pushed.
///
/// The file write is required rather than incidental — `commit_all_and_push`
/// runs a real `git commit`, which refuses an empty commit.
fn land(path: &Path) {
    std::fs::write(path.join("landed.rs"), "// landed\n").expect("write landed file");
    GitWorktreeFixture::commit_all_and_push(path, "landed");
}

fn merged(pr: u64) -> BranchPrState {
    BranchPrState::Merged { pr }
}

/// A PR index naming one branch as merged.
fn merged_index(branch: &str, pr: u64) -> PrIndex {
    PrIndex::from_json(
        &format!(r#"[{{"number": {pr}, "headRefName": "{branch}", "state": "MERGED"}}]"#),
        400,
    )
}

/// A PR index naming one branch as having an OPEN pull request.
fn open_index(branch: &str, pr: u64) -> PrIndex {
    PrIndex::from_json(
        &format!(r#"[{{"number": {pr}, "headRefName": "{branch}", "state": "OPEN"}}]"#),
        400,
    )
}

// ---------------------------------------------------------------------------
// recheck_before_delete — one test per refusal branch
// ---------------------------------------------------------------------------

#[test]
fn recheck_refuses_when_the_live_set_cannot_be_read() {
    // THE fail-closed case. An unanswerable liveness question must never
    // resolve to "nothing claims it" — that is the fail-open shape that would
    // delete a live session's worktree during a store outage.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("unreadable-2919");
    land(&path);
    let reason =
        recheck_before_delete(&path, None, &merged(1)).expect("an unreadable live set must refuse");
    assert!(reason.contains("could not be re-read"), "{reason}");
}

#[test]
fn recheck_refuses_a_worktree_a_session_claims_now() {
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("claimed-now-2919");
    land(&path);
    let reason = recheck_before_delete(&path, Some(std::slice::from_ref(&path)), &merged(1))
        .expect("a claimed worktree must refuse");
    assert!(reason.contains("claims this workspace now"), "{reason}");
}

#[test]
fn recheck_refuses_a_worktree_locked_after_the_survey() {
    // `git worktree lock` is the operator's explicit "do not remove this". git
    // honours it (`worktree remove --force` exits 128) but
    // `remove_session_worktree` treats that as a git failure and falls back to
    // `remove_dir_all`, which deletes the directory anyway — so the lock can
    // ONLY be honoured by this re-check. The survey's `Admission` was computed
    // before the lock existed.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("locked-2919");
    land(&path);
    assert!(
        recheck_before_delete(&path, Some(&[]), &merged(1)).is_none(),
        "precondition: the worktree is reclaimable before the lock"
    );
    fx.lock_worktree(&path);
    let reason =
        recheck_before_delete(&path, Some(&[]), &merged(1)).expect("a locked worktree must refuse");
    assert!(reason.contains("git-locked"), "{reason}");
}

#[test]
fn recheck_refuses_a_path_git_no_longer_lists() {
    // A plain subdirectory of the checkout: git resolves the repository but
    // does not list this path as a worktree.
    let fx = GitWorktreeFixture::new();
    let plain = fx.repo.join("not-a-worktree");
    std::fs::create_dir_all(&plain).expect("mkdir");
    let reason =
        recheck_before_delete(&plain, Some(&[]), &merged(1)).expect("an unlisted path must refuse");
    assert!(reason.contains("no longer lists"), "{reason}");
}

#[test]
fn recheck_refuses_when_git_cannot_be_queried() {
    // Outside any repository, git answers nothing — which must refuse, not
    // pass for lack of a contradiction.
    let tmp = tempfile::tempdir().expect("tempdir");
    let reason = recheck_before_delete(tmp.path(), Some(&[]), &merged(1))
        .expect("an unqueryable path must refuse");
    assert!(reason.contains("could not be queried"), "{reason}");
}

#[test]
fn recheck_refuses_a_worktree_that_lost_its_ownership_marker() {
    // Parked outside `.worktrees/` and carrying no sentinel: this is the
    // harness `.claude/worktrees/` shape, which `remove_session_worktree`
    // refuses. Approving it would report a removal that never happened.
    let fx = GitWorktreeFixture::new();
    let parent = fx.repo.join(".claude").join("worktrees");
    let path = fx.add_worktree_at(&parent, "unowned-2919");
    let reason = recheck_before_delete(&path, Some(&[]), &merged(1))
        .expect("an unowned worktree must refuse");
    assert!(reason.contains("ownership marker"), "{reason}");
}

#[test]
fn recheck_refuses_when_the_pr_is_no_longer_merged() {
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("reopened-2919");
    land(&path);
    for state in [
        BranchPrState::Open { pr: 2 },
        BranchPrState::ClosedUnmerged { pr: 2 },
        BranchPrState::NoPr,
        BranchPrState::Unknown,
    ] {
        let reason = recheck_before_delete(&path, Some(&[]), &state)
            .unwrap_or_else(|| panic!("{state:?} must refuse"));
        assert!(reason.contains("no longer a merge"), "{reason}");
    }
}

#[test]
fn recheck_refuses_a_worktree_dirtied_after_the_survey() {
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("dirtied-2919");
    land(&path);
    assert!(
        recheck_before_delete(&path, Some(&[]), &merged(1)).is_none(),
        "precondition: clean before the write"
    );
    std::fs::write(path.join("appeared.rs"), "fn main() {}\n").expect("write");
    let reason = recheck_before_delete(&path, Some(&[]), &merged(1))
        .expect("a dirtied worktree must refuse");
    assert!(reason.contains("unsaved work"), "{reason}");
}

#[test]
fn recheck_permits_a_clean_merged_owned_worktree() {
    // The permit path must be reachable, or every refusal test above would pass
    // against a function that refuses unconditionally.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("permitted-2919");
    land(&path);
    assert_eq!(recheck_before_delete(&path, Some(&[]), &merged(1)), None);
}

// ---------------------------------------------------------------------------
// Survey
// ---------------------------------------------------------------------------

#[test]
fn survey_reports_a_merged_worktree_as_reclaimable() {
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("survey-2919");
    land(&path);
    let s = survey_with_index(
        &fx.repos_root,
        &[],
        &|_: &Path| merged_index("session/survey-2919", 21),
        SurveyBudget::default(),
        false,
    );
    let found = s
        .candidates
        .iter()
        .find(|c| c.path == path)
        .unwrap_or_else(|| panic!("survey missed {}", path.display()));
    assert_eq!(found.verdict, ReclaimVerdict::Reclaimable { pr: 21 });
    assert_eq!(s.reclaimable, 1);
    assert!(s.total_bytes > 0);
}

#[test]
fn survey_excludes_a_worktree_trusty_mpm_cannot_remove() {
    // #2919 HIGH: `.claude/worktrees/` entries used to be classified
    // `Reclaimable` and counted into `reclaimable_bytes`, so `tm doctor`
    // advertised a command that then failed and left them on disk. On the
    // machine this was measured on that is most of the 1.1 TiB.
    let fx = GitWorktreeFixture::new();
    let parent = fx.repo.join(".claude").join("worktrees");
    let path = fx.add_worktree_at(&parent, "harness-2919");
    let s = survey_with_index(
        &fx.repos_root,
        &[],
        &|_: &Path| merged_index("wt/harness-2919", 55),
        SurveyBudget::default(),
        false,
    );
    let found = s
        .candidates
        .iter()
        .find(|c| c.path == path)
        .expect("listed");
    assert!(
        !found.verdict.is_reclaimable(),
        "a worktree the remover would refuse must not be advertised: {:?}",
        found.verdict
    );
    assert_eq!(
        s.reclaimable_bytes, 0,
        "its bytes must not be counted as reclaimable"
    );
}

#[test]
fn survey_past_its_classify_deadline_reclaims_nothing() {
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("deadline-2919");
    land(&path);
    let expired = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let s = survey_with_index(
        &fx.repos_root,
        &[],
        &|_: &Path| merged_index("session/deadline-2919", 40),
        SurveyBudget {
            measure: Some(expired),
            classify: Some(expired),
        },
        false,
    );
    assert!(!s.candidates.is_empty(), "candidates must still be listed");
    assert_eq!(s.reclaimable, 0, "an out-of-time survey approves nothing");
    assert_eq!(s.blocked, s.candidates.len());
}

#[test]
fn survey_past_its_measure_deadline_still_classifies() {
    // The two budgets are separate on purpose: byte measurement costs minutes
    // and classification costs seconds, so an expired MEASURE deadline must
    // still leave a usable verdict rather than blocking everything.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("measure-2919");
    land(&path);
    let expired = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let s = survey_with_index(
        &fx.repos_root,
        &[],
        &|_: &Path| merged_index("session/measure-2919", 41),
        SurveyBudget {
            measure: Some(expired),
            classify: None,
        },
        false,
    );
    let found = s
        .candidates
        .iter()
        .find(|c| c.path == path)
        .expect("listed");
    assert!(found.verdict.is_reclaimable(), "{:?}", found.verdict);
    assert_eq!(found.bytes, None, "bytes must be unmeasured");
    assert!(s.unmeasured > 0, "and disclosed as unmeasured");
}

// ---------------------------------------------------------------------------
// The reclaim loop — probes that change state BETWEEN survey and delete
// ---------------------------------------------------------------------------

#[test]
fn reclaim_report_mode_removes_nothing() {
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("report-2919");
    land(&path);
    let out = reclaim_with_probes(
        &fx.repos_root,
        &FreshProbes {
            in_use_now: &|| Some(Vec::new()),
            index_for: &|_: &Path| merged_index("session/report-2919", 30),
        },
        ReclaimMode::Report,
    );
    assert_eq!(out.survey.reclaimable, 1, "it IS reclaimable…");
    assert!(out.removed.is_empty(), "…but Report mode removed it");
    assert!(path.exists());
}

#[test]
fn reclaim_remove_mode_refuses_a_worktree_claimed_after_the_survey() {
    // The candidate passes classification (nothing claimed it at survey time),
    // then a session attaches before the delete. Only the fresh re-read can
    // catch that — this fails if the liveness re-check is removed.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("claim-race-2919");
    land(&path);
    let calls = RefCell::new(0usize);
    let claimed = path.clone();
    let in_use_now = || {
        let mut n = calls.borrow_mut();
        *n += 1;
        // Call 1 is the survey's snapshot; every later call is a per-candidate
        // re-read inside the delete loop.
        if *n == 1 {
            Some(Vec::new())
        } else {
            Some(vec![claimed.clone()])
        }
    };
    let out = reclaim_with_probes(
        &fx.repos_root,
        &FreshProbes {
            in_use_now: &in_use_now,
            index_for: &|_: &Path| merged_index("session/claim-race-2919", 31),
        },
        ReclaimMode::Remove,
    );
    assert_eq!(out.survey.reclaimable, 1, "must reach the delete loop");
    assert!(
        out.removed.is_empty(),
        "removed a claimed worktree: {out:?}"
    );
    assert_eq!(out.refused_at_recheck.len(), 1);
    assert!(path.exists());
}

#[test]
fn reclaim_remove_mode_refuses_a_worktree_dirtied_after_the_survey() {
    // Work appears in the worktree between the survey and the delete. This
    // fails if the dirt re-check is removed.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("dirt-race-2919");
    land(&path);
    let calls = RefCell::new(0usize);
    let to_dirty = path.clone();
    let in_use_now = || {
        let mut n = calls.borrow_mut();
        *n += 1;
        if *n > 1 {
            std::fs::write(to_dirty.join("rescued.rs"), "fn main() {}\n").expect("write");
        }
        Some(Vec::new())
    };
    let out = reclaim_with_probes(
        &fx.repos_root,
        &FreshProbes {
            in_use_now: &in_use_now,
            index_for: &|_: &Path| merged_index("session/dirt-race-2919", 32),
        },
        ReclaimMode::Remove,
    );
    assert_eq!(out.survey.reclaimable, 1, "must reach the delete loop");
    assert!(out.removed.is_empty(), "destroyed new work: {out:?}");
    assert!(path.join("rescued.rs").exists(), "the work must survive");
}

#[test]
fn reclaim_remove_mode_refuses_a_worktree_locked_after_the_survey() {
    // The operator runs `git worktree lock` mid-sweep. This fails if the git
    // re-check is removed — and note `remove_session_worktree` would delete it
    // regardless via its `remove_dir_all` fallback, so this re-check is the
    // ONLY thing honouring the lock.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("lock-race-2919");
    land(&path);
    let calls = RefCell::new(0usize);
    let locked_path = path.clone();
    let repo = fx.repo.clone();
    let in_use_now = || {
        let mut n = calls.borrow_mut();
        *n += 1;
        if *n > 1 {
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["worktree", "lock"])
                .arg(&locked_path)
                .output();
        }
        Some(Vec::new())
    };
    let out = reclaim_with_probes(
        &fx.repos_root,
        &FreshProbes {
            in_use_now: &in_use_now,
            index_for: &|_: &Path| merged_index("session/lock-race-2919", 33),
        },
        ReclaimMode::Remove,
    );
    assert_eq!(out.survey.reclaimable, 1, "must reach the delete loop");
    assert!(out.removed.is_empty(), "deleted a locked worktree: {out:?}");
    assert!(path.exists());
}

#[test]
fn reclaim_remove_mode_refuses_when_the_pr_reopens_after_the_survey() {
    // The branch's PR is merged at survey time and open by delete time — a
    // reused branch. This fails if the delete loop reuses the survey's index
    // instead of rebuilding it.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("pr-race-2919");
    land(&path);
    let index_calls = RefCell::new(0usize);
    let index = |_: &Path| {
        let mut n = index_calls.borrow_mut();
        *n += 1;
        if *n == 1 {
            merged_index("session/pr-race-2919", 34)
        } else {
            open_index("session/pr-race-2919", 35)
        }
    };
    let out = reclaim_with_probes(
        &fx.repos_root,
        &FreshProbes {
            in_use_now: &|| Some(Vec::new()),
            index_for: &index,
        },
        ReclaimMode::Remove,
    );
    assert_eq!(out.survey.reclaimable, 1, "must reach the delete loop");
    assert!(out.removed.is_empty(), "deleted a reopened branch: {out:?}");
    assert!(path.exists());
}

#[test]
fn reclaim_remove_mode_refuses_when_the_live_set_cannot_be_read() {
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("unreadable-race-2919");
    land(&path);
    let calls = RefCell::new(0usize);
    let in_use_now = || {
        let mut n = calls.borrow_mut();
        *n += 1;
        if *n == 1 { Some(Vec::new()) } else { None }
    };
    let out = reclaim_with_probes(
        &fx.repos_root,
        &FreshProbes {
            in_use_now: &in_use_now,
            index_for: &|_: &Path| merged_index("session/unreadable-race-2919", 36),
        },
        ReclaimMode::Remove,
    );
    assert_eq!(out.survey.reclaimable, 1, "must reach the delete loop");
    assert!(out.removed.is_empty(), "deleted on an unknown live set");
    assert!(path.exists());
}

#[test]
fn reclaim_remove_mode_reclaims_a_clean_merged_worktree() {
    // The destructive path must actually work, or every refusal test above
    // would pass against a loop that never deletes.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("reclaim-2919");
    land(&path);
    let out = reclaim_with_probes(
        &fx.repos_root,
        &FreshProbes {
            in_use_now: &|| Some(Vec::new()),
            index_for: &|_: &Path| merged_index("session/reclaim-2919", 37),
        },
        ReclaimMode::Remove,
    );
    assert_eq!(out.removed, vec![path.clone()], "outcome: {out:?}");
    assert!(!path.exists(), "the directory must be gone");
    assert!(out.removed_bytes > 0);
    assert!(out.removal_failed.is_empty());
}

#[test]
fn reclaim_uses_an_unbounded_budget() {
    // A human waiting on `--merged-prs` wants a correct answer, not a fast one.
    // A partial classification on the DESTRUCTIVE path silently shrinks what
    // gets reclaimed.
    let b = SurveyBudget::unbounded();
    assert!(b.measure.is_none() && b.classify.is_none());
}
