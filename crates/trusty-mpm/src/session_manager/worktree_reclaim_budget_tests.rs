//! Tests for the merged-PR preview's wall-clock bound (#8301).
//!
//! Why: the preview never finished on a repository with ~240 worktrees, and
//! every call it made already had its own timeout. These tests drive the shape
//! that broke — a call that hangs for its whole per-call budget — through the
//! real survey over a real-git fixture, and assert the survey ends at its
//! deadline with that worktree kept.
//! What: the hung-call bound, the preview-only budget, and the Fail-Open Check
//! for a `gh` failure: a failed or cut-short lookup is never a removable verdict.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::{CUT_SHORT_REASON, PREVIEW_CLASSIFY_BUDGET};
use crate::core::bounded_proc::{run_bounded, with_deadline};
use crate::core::worktree_landed_content::LandingAdmission;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_landing_refresh::FETCH_TIMEOUT;
use crate::session_manager::worktree_ownership::{AgentDelegationState, AgentWorktreeOwner};
use crate::session_manager::worktree_reclaim::{
    BranchPrState, KeepList, LiveClaims, PrIndex, ReclaimGate, ReclaimMode, ReclaimVerdict,
};
use crate::session_manager::worktree_reclaim_gh::{GH_TIMEOUT, run_with_timeout};
use crate::session_manager::worktree_reclaim_landed::reclaim_landed_content;
use crate::session_manager::worktree_reclaim_sweep::{SurveyBudget, survey_with_landed_content};

/// The strictest agent probe, as the sweep tests use it.
fn no_agents(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Unknown
}

/// A complete index naming only an unrelated branch, so the donor branch
/// resolves to `NoPr` without a `gh` call and reaches the landing admission.
fn unrelated_index(_: &Path) -> PrIndex {
    PrIndex::from_json(
        r#"[{"number": 72, "headRefName": "session/somebody-else", "state": "MERGED"}]"#,
        400,
    )
}

/// A worktree whose content is already on `origin/main` — the #7889 donor
/// shape, which the real admission grants.
fn donor(name: &str) -> (GitWorktreeFixture, PathBuf) {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree(name);
    fx.land_with_sibling_work(&wt, &format!("{name}.txt"), &format!("{name}-r2.txt"));
    (fx, wt)
}

/// A child that never answers, run for its full per-call budget — the shape of
/// a `git fetch` or `gh` call wedged on the network or the keychain.
fn stall(budget: Duration) {
    let mut cmd = Command::new("sleep");
    cmd.arg("30");
    let _ = run_bounded(cmd, budget);
}

/// 🔴 REGRESSION (#8301): a call that hangs for its whole per-call budget ends
/// at the survey deadline, and the worktree it was inspecting is kept.
///
/// The landing admission here takes the real grant for a landed tree, then
/// stalls in a child for the 30 s fetch budget before returning it — a slow
/// probe whose late answer is "removable". Fails on `origin/main`: the survey
/// checked its deadline only before each candidate, so it waited the full 30 s
/// and then reported the tree reclaimable.
#[test]
fn worktree_8301_a_hung_call_ends_at_the_deadline_and_is_kept() {
    let (fx, wt) = donor("donor-8301");
    let hung_then_grant = |p: &Path| -> LandingAdmission {
        let grant = reclaim_landed_content(p);
        stall(FETCH_TIMEOUT);
        grant
    };
    let started = Instant::now();
    let s = survey_with_landed_content(
        &fx.repos_root,
        &LiveClaims::default(),
        &unrelated_index,
        &no_agents,
        SurveyBudget {
            measure: Some(Duration::ZERO),
            classify: Some(started + Duration::from_secs(3)),
        },
        false,
        &KeepList::default(),
        &[],
        Some(&hung_then_grant),
    );
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(20),
        "the survey must end near its 3 s deadline, not after the 30 s call: took {took:?}"
    );
    let found = s
        .candidates
        .iter()
        .find(|c| c.path == wt)
        .unwrap_or_else(|| panic!("survey missed {}", wt.display()));
    match &found.verdict {
        ReclaimVerdict::Blocked {
            gate: ReclaimGate::Deadline,
            reason,
        } => assert_eq!(reason, CUT_SHORT_REASON),
        other => panic!("an interrupted inspection must be kept at the deadline gate: {other:?}"),
    }
    assert_eq!(s.reclaimable, 0, "an interrupted survey approves nothing");
    assert_eq!(s.not_inspected, 1);
}

/// #8301: only a preview is bounded. A `Remove` pass keeps the unbounded
/// classification `for_reclaim` gives it — it is scoped to its preview's paths.
#[test]
fn worktree_8301_a_preview_budget_bounds_classification() {
    let before = Instant::now();
    let preview = SurveyBudget::for_mode(ReclaimMode::Report);
    let deadline = preview
        .classify
        .expect("a preview must stop classifying at a deadline");
    assert!(deadline >= before + PREVIEW_CLASSIFY_BUDGET);
    assert!(deadline <= Instant::now() + PREVIEW_CLASSIFY_BUDGET);
    assert_eq!(preview.measure, SurveyBudget::for_reclaim().measure);
    assert!(
        SurveyBudget::for_mode(ReclaimMode::Remove)
            .classify
            .is_none()
    );
}

/// 🔴 Fail-Open Check (#8301): a `gh` call the deadline cut short is an ERROR,
/// never an answer, and it does not read as `gh` hanging.
#[test]
fn worktree_8301_a_gh_call_cut_short_is_an_error_never_an_answer() {
    let started = Instant::now();
    let (during, after) = with_deadline(started + Duration::from_millis(300), || {
        let mut hung = Command::new("sleep");
        hung.arg("30");
        let during = run_with_timeout(hung, GH_TIMEOUT);
        let after = run_with_timeout(Command::new("true"), GH_TIMEOUT);
        (during, after)
    });
    for (label, result) in [("during", during), ("after", after)] {
        let failure = result.expect_err("a cut-short `gh` call must be an error");
        assert!(failure.was_cut_short(), "{label}: {failure}");
        assert!(!failure.timed_out(), "{label}: must not count as a gh hang");
    }
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// 🔴 Fail-Open Check (#8301): a failed `gh` lookup never yields a removable
/// verdict, even when the landing admission would grant.
#[test]
fn worktree_8301_a_failed_gh_lookup_never_yields_a_removable_verdict() {
    let (fx, wt) = donor("donor-8301-gh-down");
    let failed = |_: &Path| PrIndex::unavailable_because("`gh` exited 1: boom".to_string());
    let grant = |p: &Path| reclaim_landed_content(p);
    let s = survey_with_landed_content(
        &fx.repos_root,
        &LiveClaims::default(),
        &failed,
        &no_agents,
        SurveyBudget {
            measure: Some(Duration::ZERO),
            classify: Some(Instant::now() + Duration::from_secs(60)),
        },
        true,
        &KeepList::default(),
        &[],
        Some(&grant),
    );
    let found = s
        .candidates
        .iter()
        .find(|c| c.path == wt)
        .unwrap_or_else(|| panic!("survey missed {}", wt.display()));
    assert!(
        !found.verdict.is_reclaimable(),
        "a failed lookup must keep the tree: {:?}",
        found.verdict
    );
    assert!(
        matches!(found.pr, BranchPrState::LookupFailed { .. }),
        "{:?}",
        found.pr
    );
    assert_eq!(s.reclaimable, 0);
}
