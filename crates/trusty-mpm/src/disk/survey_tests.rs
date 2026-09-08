//! Tests for the Disk dashboard's worktree classification and survey (#6927).
//!
//! Why: the owner's staleness ruling is a conjunction, and a conjunction is
//! only trustworthy when each conjunct has a test that fails when that conjunct
//! is dropped. Every KEEP reason below is one such test, and
//! `a_keep_listed_merged_worktree_is_never_stale` is the one that fails if the
//! keep-list gate is deleted.
//!
//! HERMETIC: no test here reads the operator's workspace, config, or GitHub.
//! Every worktree is a scratch git repository in a tempdir; the pull-request
//! state, the live-session set, and the agent-liveness answer are injected.
//! Test target: `super::survey`, `super::survey_run`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{ReasonCode, WorktreeFacts, WorktreeTier, classify_tier};
use crate::disk::size_index::{DirSizeIndex, IndexPolicy};
use crate::disk::survey_run::{self, DiskProbes, run};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_keep_list::KeepList;
use crate::session_manager::worktree_ownership::{AgentDelegationState, AgentWorktreeOwner};
use crate::session_manager::worktree_reclaim::{
    BranchPrState, LiveClaims, ReclaimGate, ReclaimVerdict, WorkspaceClaim, classify,
};
use crate::session_manager::worktree_reclaim_claim::ClaimState;
use crate::session_manager::worktree_registry::{Admission, ScannedWorktree};
use crate::session_manager::worktree_safety::{DirtyWorktree, inspect_dirt};

// ── helpers ──────────────────────────────────────────────────────────────────

/// An empty keep-list — the default, and a no-op gate.
fn no_keeps() -> KeepList {
    KeepList::from_patterns::<String>(&[])
}

/// A dirt probe that reports a provably clean, fully pushed tree.
fn clean(_: &Path) -> Option<DirtyWorktree> {
    None
}

/// A dirt probe reporting `files` uncommitted entries and `unpushed` commits.
fn dirt_with(files: usize, unpushed: usize) -> impl Fn(&Path) -> Option<DirtyWorktree> {
    move |path: &Path| {
        Some(DirtyWorktree {
            path: path.to_path_buf(),
            reason: "fixture".to_string(),
            dirty_files: files,
            unpushed_commits: unpushed,
        })
    }
}

/// The reclaim verdict a clean, merged, unclaimed worktree reaches.
fn reclaimable() -> ReclaimVerdict {
    ReclaimVerdict::Reclaimable { pr: 1 }
}

/// Facts for a worktree in the default happy state, overridden per test.
fn facts<'a>(
    path: &'a Path,
    branch: Option<&'a str>,
    pr: &'a BranchPrState,
    claim: &'a ClaimState,
    verdict: &'a ReclaimVerdict,
) -> WorktreeFacts<'a> {
    WorktreeFacts {
        path,
        branch,
        admission: Admission::Admitted,
        claim,
        pr,
        verdict,
    }
}

/// A real directory to classify.
///
/// Why: `classify_tier` reports a path that is not on disk as `missing` before
/// it looks at anything else — a registration whose directory is gone holds no
/// bytes to clear. Every unit test below therefore needs a directory that
/// exists, or it would be testing the missing branch by accident.
fn existing() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// An index that will walk a tempdir even when `TMPDIR` sits under `$HOME`.
///
/// Why: the default policy forbids `$HOME` and every ancestor of it, which is
/// right for production and would make these tests' byte figures `None` on a
/// host whose temp directory is home-relative.
fn test_index() -> DirSizeIndex {
    DirSizeIndex::with_policy(IndexPolicy {
        forbidden_roots: vec![PathBuf::from("/")],
        ..IndexPolicy::default()
    })
}

/// Probes that answer `pr` for every worktree, with nothing claimed and no
/// agent registered.
struct Fixed {
    pr: BranchPrState,
    claims: LiveClaims,
}

impl Fixed {
    fn new(pr: BranchPrState) -> Self {
        Self {
            pr,
            claims: LiveClaims::default(),
        }
    }
}

// ── classify_tier: one test per conjunct of the owner's ruling ───────────────

#[test]
fn a_merged_clean_worktree_is_stale() {
    let pr = BranchPrState::Merged { pr: 42 };
    let claim = ClaimState::Unclaimed;
    let verdict = reclaimable();
    let wt = existing();
    let c = classify_tier(
        &facts(wt.path(), Some("feat/x"), &pr, &claim, &verdict),
        &no_keeps(),
        &clean,
    );
    assert_eq!(c.tier, WorktreeTier::Stale, "{c:?}");
    assert_eq!(c.reasons[0].code, ReasonCode::MergedPr);
}

#[test]
fn a_branchless_contained_worktree_is_stale() {
    // The owner's ruling: no branch counts as landed when the tree is clean and
    // holds nothing unpushed — which `inspect_dirt`'s no-upstream arm proves by
    // finding no commit outside the remotes.
    let pr = BranchPrState::Unknown;
    let claim = ClaimState::Unclaimed;
    let verdict = ReclaimVerdict::blocked(ReclaimGate::PrState, "no branch");
    let wt = existing();
    let c = classify_tier(
        &facts(wt.path(), None, &pr, &claim, &verdict),
        &no_keeps(),
        &clean,
    );
    assert_eq!(c.tier, WorktreeTier::Stale, "{c:?}");
    assert_eq!(c.reasons[0].code, ReasonCode::ContainedInMainline);
}

#[test]
fn a_dirty_worktree_is_kept_and_says_dirty() {
    let pr = BranchPrState::Merged { pr: 42 };
    let claim = ClaimState::Unclaimed;
    let verdict = ReclaimVerdict::blocked(ReclaimGate::UnsavedWork, "holds unsaved work");
    let wt = existing();
    let c = classify_tier(
        &facts(wt.path(), Some("feat/x"), &pr, &claim, &verdict),
        &no_keeps(),
        &dirt_with(3, 0),
    );
    assert_eq!(c.tier, WorktreeTier::Keep, "{c:?}");
    assert!(
        c.reasons.iter().any(|r| r.code == ReasonCode::Dirty),
        "{c:?}"
    );
}

#[test]
fn an_unpushed_worktree_is_kept_and_says_unpushed() {
    let pr = BranchPrState::Merged { pr: 42 };
    let claim = ClaimState::Unclaimed;
    let verdict = ReclaimVerdict::blocked(ReclaimGate::UnsavedWork, "holds unsaved work");
    let wt = existing();
    let c = classify_tier(
        &facts(wt.path(), Some("feat/x"), &pr, &claim, &verdict),
        &no_keeps(),
        &dirt_with(0, 2),
    );
    assert_eq!(c.tier, WorktreeTier::Keep, "{c:?}");
    assert!(
        c.reasons.iter().any(|r| r.code == ReasonCode::Unpushed),
        "{c:?}"
    );
}

#[test]
fn a_live_sessions_worktree_is_kept() {
    let pr = BranchPrState::Merged { pr: 42 };
    let claim = ClaimState::Foreign {
        session: "sess-1".to_string(),
        caller: None,
    };
    let verdict = ReclaimVerdict::blocked(ReclaimGate::Liveness, "claimed");
    let wt = existing();
    let c = classify_tier(
        &facts(wt.path(), Some("feat/x"), &pr, &claim, &verdict),
        &no_keeps(),
        &clean,
    );
    assert_eq!(c.tier, WorktreeTier::Keep, "{c:?}");
    let live = c
        .reasons
        .iter()
        .find(|r| r.code == ReasonCode::LiveSession)
        .expect("a live-session reason");
    assert!(live.detail.contains("sess-1"), "{live:?}");
}

#[test]
fn a_callers_own_nested_worktree_is_not_kept_for_liveness() {
    // #6806: a session pruning worktrees it created inside its own workspace is
    // not blocked by its own claim, and the view must agree with the deleter.
    let pr = BranchPrState::Merged { pr: 42 };
    let claim = ClaimState::CallerNested {
        session: "sess-1".to_string(),
    };
    let verdict = reclaimable();
    let wt = existing();
    let c = classify_tier(
        &facts(wt.path(), Some("feat/x"), &pr, &claim, &verdict),
        &no_keeps(),
        &clean,
    );
    assert_eq!(c.tier, WorktreeTier::Stale, "{c:?}");
}

#[test]
fn an_agent_owned_worktree_is_kept() {
    let pr = BranchPrState::Merged { pr: 42 };
    let claim = ClaimState::Unclaimed;
    let verdict = ReclaimVerdict::blocked_by_agent(
        ReclaimGate::AgentOwnership,
        "owned by dispatched agent agent-7",
    );
    let wt = existing();
    let c = classify_tier(
        &facts(wt.path(), Some("feat/x"), &pr, &claim, &verdict),
        &no_keeps(),
        &clean,
    );
    assert_eq!(c.tier, WorktreeTier::Keep, "{c:?}");
    assert!(
        c.reasons.iter().any(|r| r.code == ReasonCode::AgentOwned),
        "{c:?}"
    );
}

#[test]
fn an_open_pr_is_review() {
    let pr = BranchPrState::Open { pr: 9 };
    let claim = ClaimState::Unclaimed;
    let verdict = ReclaimVerdict::blocked(ReclaimGate::PrState, "PR #9 is still open");
    let wt = existing();
    let c = classify_tier(
        &facts(wt.path(), Some("feat/x"), &pr, &claim, &verdict),
        &no_keeps(),
        &clean,
    );
    assert_eq!(c.tier, WorktreeTier::Review, "{c:?}");
    assert_eq!(c.reasons[0].code, ReasonCode::UnknownBranchState);
}

#[test]
fn a_missing_pointer_is_missing() {
    let pr = BranchPrState::Merged { pr: 42 };
    let claim = ClaimState::Unclaimed;
    let verdict = ReclaimVerdict::blocked(ReclaimGate::Admission, "prunable");
    let mut f = facts(
        Path::new("/nowhere/gone"),
        Some("feat/x"),
        &pr,
        &claim,
        &verdict,
    );
    f.admission = Admission::Prunable;
    let c = classify_tier(&f, &no_keeps(), &clean);
    assert_eq!(c.tier, WorktreeTier::Missing, "{c:?}");
    assert_eq!(c.reasons[0].code, ReasonCode::Missing);
}

/// #6927 KEEP-LIST GATE: delete the gate and this test fails.
///
/// Why this shape: the worktree is merged, clean, unclaimed and admitted —
/// every other gate votes STALE. The keep-list is the only thing standing
/// between it and a green "safe to clear" arc.
#[test]
fn a_keep_listed_merged_worktree_is_never_stale() {
    let pr = BranchPrState::Merged { pr: 42 };
    let claim = ClaimState::Unclaimed;
    let verdict = reclaimable();
    // A REAL directory, so removing the gate fails this test with
    // `Stale != Keep` — the thing under test — rather than with `Missing`.
    let workspace = existing();
    let wt = workspace.path().join("wt");
    std::fs::create_dir_all(&wt).expect("mkdir");
    let pattern = workspace.path().to_string_lossy().into_owned();
    let keeps = KeepList::from_patterns(std::slice::from_ref(&pattern));
    let c = classify_tier(
        &facts(&wt, Some("feat/x"), &pr, &claim, &verdict),
        &keeps,
        &clean,
    );
    assert_eq!(
        c.tier,
        WorktreeTier::Keep,
        "a keep-listed worktree must never render stale: {c:?}"
    );
    assert_eq!(c.reasons[0].code, ReasonCode::KeepList);
    assert!(c.reasons[0].detail.contains(&pattern), "{c:?}");
}

/// The display may never be narrower than the deleter: anything
/// `worktree_reclaim::classify` would delete has to show as clearable.
///
/// Why a TABLE over the real `classify`, and not a hand-built verdict: the
/// first cut of this test constructed `Reclaimable` itself and hand-picked
/// facts that already resolved to `Stale`, so it asserted a tautology.
/// `classify_tier` never branches on `Reclaimable`, which is exactly why the
/// two functions can drift — the only test that catches that drift is one where
/// the verdict comes out of `classify` over the SAME facts the tier is computed
/// from, the way `survey_run::inspect` runs them.
/// Test target: the implication `classify(..).is_reclaimable() => Stale`.
/// The display may never be narrower than the deleter: anything
/// `worktree_reclaim::classify` would delete has to show as clearable, and
/// nothing it refuses may be advertised as clearable.
///
/// Why a TABLE over the real `classify`, and not a hand-built verdict: the
/// first cut of this test constructed `Reclaimable` itself and hand-picked
/// facts that already resolved to `Stale`, so it asserted a tautology.
/// `classify_tier` never branches on `Reclaimable`, which is exactly why the
/// two can drift — the only test that catches that drift is one where the
/// verdict comes out of `classify` over the SAME facts the tier is computed
/// from, the way `survey_run::inspect` runs them.
#[test]
fn a_reclaimable_verdict_is_always_shown_stale() {
    // Two directories, because gate 4 keys on which OWNER the sentinel names:
    // a session-owned tree for every row but the last, and an agent-owned one
    // for the row that asks what a live agent does.
    let session_wt = existing();
    GitWorktreeFixture::stamp_reclaimable_sentinel(session_wt.path());
    let agent_wt = existing();
    let owner = GitWorktreeFixture::stamp_agent_sentinel(agent_wt.path(), "agent-6927");
    let _ = &owner;

    let claimed = ClaimState::Foreign {
        session: "tm-other-01".to_string(),
        caller: None,
    };
    let dirty = dirt_with(1, 0);
    let unclaimed = ClaimState::Unclaimed;
    let merged = BranchPrState::Merged { pr: 7 };

    // Every axis `classify` gates on, at a value that admits and at one that
    // refuses: (label, path, admission, claim, pr, dirt probe, agent state).
    #[allow(clippy::type_complexity)]
    let cases: Vec<(
        &str,
        &Path,
        Admission,
        &ClaimState,
        &BranchPrState,
        &dyn Fn(&Path) -> Option<DirtyWorktree>,
        AgentDelegationState,
    )> = vec![
        (
            "merged, clean, unclaimed, no live agent",
            session_wt.path(),
            Admission::Admitted,
            &unclaimed,
            &merged,
            &clean,
            AgentDelegationState::Ended,
        ),
        (
            "a foreign session claims it",
            session_wt.path(),
            Admission::Admitted,
            &claimed,
            &merged,
            &clean,
            AgentDelegationState::Ended,
        ),
        (
            "git does not admit it",
            session_wt.path(),
            Admission::Locked,
            &unclaimed,
            &merged,
            &clean,
            AgentDelegationState::Ended,
        ),
        (
            "the pull request is still open",
            session_wt.path(),
            Admission::Admitted,
            &unclaimed,
            &BranchPrState::Open { pr: 7 },
            &clean,
            AgentDelegationState::Ended,
        ),
        (
            "no pull request at all",
            session_wt.path(),
            Admission::Admitted,
            &unclaimed,
            &BranchPrState::NoPr,
            &clean,
            AgentDelegationState::Ended,
        ),
        (
            "the tree holds unsaved work",
            session_wt.path(),
            Admission::Admitted,
            &unclaimed,
            &merged,
            &dirty,
            AgentDelegationState::Ended,
        ),
        (
            "a finished agent owned it",
            agent_wt.path(),
            Admission::Admitted,
            &unclaimed,
            &merged,
            &clean,
            AgentDelegationState::Ended,
        ),
        (
            "a live agent owns it",
            agent_wt.path(),
            Admission::Admitted,
            &unclaimed,
            &merged,
            &clean,
            AgentDelegationState::Live,
        ),
    ];

    let mut ever_reclaimable = false;
    for (label, path, admission, claim, pr, dirt, agent) in cases {
        let agent_state = |_: &AgentWorktreeOwner| agent;
        let verdict = classify(path, admission, claim, pr, dirt, &agent_state, &no_keeps());
        let facts = WorktreeFacts {
            path,
            branch: Some("feat/x"),
            admission,
            claim,
            pr,
            verdict: &verdict,
        };
        let c = classify_tier(&facts, &no_keeps(), dirt);
        if verdict.is_reclaimable() {
            ever_reclaimable = true;
            assert_eq!(
                c.tier,
                WorktreeTier::Stale,
                "{label}: the deleter would remove this, so the view must show it \
                 clearable — verdict {verdict:?}, classification {c:?}"
            );
        } else {
            assert_ne!(
                c.tier,
                WorktreeTier::Stale,
                "{label}: the deleter refuses this, so the view must not advertise it \
                 as clearable — verdict {verdict:?}, classification {c:?}"
            );
        }
    }
    assert!(
        ever_reclaimable,
        "at least one row must reach Reclaimable, or the implication is vacuous"
    );
}

// ── the whole survey, over scratch git repositories ──────────────────────────

/// Run a survey over the fixture's repos root with injected probes.
fn survey_fixture(
    fx: &GitWorktreeFixture,
    fixed: &Fixed,
    keep_patterns: &[String],
    deadline: Option<Instant>,
) -> super::DiskSurvey {
    let keep_list = KeepList::from_patterns(keep_patterns);
    let pr_state = |_: &ScannedWorktree| fixed.pr.clone();
    let agent_state = |_: &AgentWorktreeOwner| AgentDelegationState::Ended;
    let index = RefCell::new(test_index());
    let measure = |path: &Path, budget: Option<Duration>| {
        survey_run::measure(&mut index.borrow_mut(), path, budget)
    };
    let probes = DiskProbes {
        pr_state: &pr_state,
        claims: &fixed.claims,
        agent_state: &agent_state,
        dirt: &inspect_dirt,
        measure: &measure,
    };
    run(&fx.repos_root, &keep_list, &probes, deadline, None)
}

/// Every worktree row in the survey, flattened out of its project.
fn rows(survey: &super::DiskSurvey) -> Vec<&super::DiskWorktree> {
    survey
        .root
        .projects
        .iter()
        .flat_map(|p| p.worktrees.iter())
        .collect()
}

/// The row for one worktree path.
fn row_for<'a>(survey: &'a super::DiskSurvey, wt: &Path) -> &'a super::DiskWorktree {
    rows(survey)
        .into_iter()
        .find(|r| r.path == wt)
        .unwrap_or_else(|| panic!("no row for {}", wt.display()))
}

#[test]
fn a_survey_groups_worktrees_under_their_project_and_measures_bytes() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("one");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    std::fs::write(wt.join("payload.bin"), vec![7u8; 4096]).expect("write payload");
    GitWorktreeFixture::commit_all_and_push(&wt, "payload");

    let survey = survey_fixture(&fx, &Fixed::new(BranchPrState::Merged { pr: 1 }), &[], None);

    assert_eq!(survey.root.projects.len(), 1, "{:#?}", survey.root.projects);
    let project = &survey.root.projects[0];
    assert_eq!(project.name, "owner/repo", "{project:#?}");
    let row = row_for(&survey, &wt);
    assert_eq!(row.tier, WorktreeTier::Stale, "{row:#?}");
    assert!(
        row.bytes.unwrap_or(0) >= 4096,
        "the worktree's payload must be counted: {row:#?}"
    );
    assert!(row.size.is_some(), "the byte figure must carry provenance");
    assert_eq!(survey.root.counts.stale, 1, "{:#?}", survey.root.counts);
    assert!(survey.root.stale_bytes >= 4096);
    assert_eq!(survey.root.stale_measured, 1);
}

#[test]
fn a_survey_reports_a_dirty_worktree_as_kept_with_the_dirty_reason() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("dirty");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    std::fs::write(wt.join("scratch.txt"), "uncommitted\n").expect("write");

    let survey = survey_fixture(&fx, &Fixed::new(BranchPrState::Merged { pr: 1 }), &[], None);
    let row = row_for(&survey, &wt);
    assert_eq!(row.tier, WorktreeTier::Keep, "{row:#?}");
    assert!(
        row.reasons.iter().any(|r| r.code == ReasonCode::Dirty),
        "{row:#?}"
    );
}

#[test]
fn a_survey_reports_an_unpushed_worktree_as_kept() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("unpushed");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    GitWorktreeFixture::commit_unpushed(&wt);

    let survey = survey_fixture(&fx, &Fixed::new(BranchPrState::Merged { pr: 1 }), &[], None);
    let row = row_for(&survey, &wt);
    assert_eq!(row.tier, WorktreeTier::Keep, "{row:#?}");
    assert!(
        row.reasons.iter().any(|r| r.code == ReasonCode::Unpushed),
        "{row:#?}"
    );
}

/// The owner's no-upstream conjunct, against real git: a detached worktree
/// whose HEAD is already on `origin/main` holds nothing to lose.
#[test]
fn a_branchless_worktree_contained_in_a_remote_is_stale() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("detached");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    // Detach onto the pushed mainline commit: no branch, nothing unpushed.
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["checkout", "--detach", "origin/main"])
        .output()
        .expect("git checkout --detach");
    assert!(
        out.status.success(),
        "detach failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The pull-request index cannot answer for a worktree with no branch.
    let survey = survey_fixture(&fx, &Fixed::new(BranchPrState::Unknown), &[], None);
    let row = row_for(&survey, &wt);
    assert_eq!(row.branch, None, "{row:#?}");
    assert_eq!(row.tier, WorktreeTier::Stale, "{row:#?}");
    assert_eq!(row.reasons[0].code, ReasonCode::ContainedInMainline);
    assert!(
        !row.reclaimable,
        "the DELETE path still refuses a branchless worktree — only the view is wider"
    );
}

/// #6927 KEEP-LIST GATE, end to end: the same merged, clean worktree that reads
/// `stale` without the keep-list reads `keep` with it, and the reclaim verdict
/// records gate 0.
#[test]
fn a_keep_listed_worktree_is_kept_across_the_whole_survey() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("kept");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    GitWorktreeFixture::commit_all_and_push(&wt, "pushed");

    // CONTROL: with no keep-list this worktree IS stale, so the assertion below
    // fails for the keep-list's absence and never for an unrelated reason.
    let control = survey_fixture(&fx, &Fixed::new(BranchPrState::Merged { pr: 1 }), &[], None);
    assert_eq!(
        row_for(&control, &wt).tier,
        WorktreeTier::Stale,
        "CONTROL: without the keep-list this worktree must be stale"
    );

    let patterns = vec![fx.repos_root.to_string_lossy().into_owned()];
    let kept = survey_fixture(
        &fx,
        &Fixed::new(BranchPrState::Merged { pr: 1 }),
        &patterns,
        None,
    );
    let row = row_for(&kept, &wt);
    assert_eq!(row.tier, WorktreeTier::Keep, "{row:#?}");
    assert_eq!(row.reasons[0].code, ReasonCode::KeepList);
    assert_eq!(
        row.gate,
        Some(ReclaimGate::KeepList),
        "the reclaim verdict must record gate 0, not omit the worktree: {row:#?}"
    );
    assert!(!row.reclaimable, "{row:#?}");
    assert_eq!(kept.keep_list.patterns, patterns);
}

#[test]
fn a_stale_pointer_is_reported_missing_not_stale() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("gone");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    GitWorktreeFixture::commit_all_and_push(&wt, "pushed");
    std::fs::remove_dir_all(&wt).expect("remove the worktree directory");

    let survey = survey_fixture(&fx, &Fixed::new(BranchPrState::Merged { pr: 1 }), &[], None);
    let row = row_for(&survey, &wt);
    assert_eq!(row.tier, WorktreeTier::Missing, "{row:#?}");
    assert_eq!(row.bytes, None, "a directory that is gone holds no bytes");
    assert_eq!(survey.root.counts.missing, 1, "{:#?}", survey.root.counts);
    assert_eq!(survey.root.counts.stale, 0);
}

#[test]
fn a_live_sessions_claim_keeps_a_worktree_in_the_whole_survey() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("claimed");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    GitWorktreeFixture::commit_all_and_push(&wt, "pushed");

    let mut fixed = Fixed::new(BranchPrState::Merged { pr: 1 });
    fixed.claims = LiveClaims::foreign(vec![WorkspaceClaim::new("sess-9", wt.clone())]);
    let survey = survey_fixture(&fx, &fixed, &[], None);
    let row = row_for(&survey, &wt);
    assert_eq!(row.tier, WorktreeTier::Keep, "{row:#?}");
    assert!(
        row.reasons
            .iter()
            .any(|r| r.code == ReasonCode::LiveSession),
        "{row:#?}"
    );
    assert_eq!(row.session.as_deref(), Some("sess-9"), "{row:#?}");
}

#[test]
fn a_survey_past_its_deadline_still_lists_every_worktree() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("late");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);

    let expired = Instant::now() - Duration::from_secs(1);
    let survey = survey_fixture(
        &fx,
        &Fixed::new(BranchPrState::Merged { pr: 1 }),
        &[],
        Some(expired),
    );
    let row = row_for(&survey, &wt);
    assert_eq!(
        row.tier,
        WorktreeTier::Review,
        "a worktree we ran out of time to inspect is never stale: {row:#?}"
    );
    assert_eq!(row.gate, Some(ReclaimGate::Deadline), "{row:#?}");
    assert!(!row.reclaimable);
}

/// A deadline that crosses DURING one inspection invalidates that row's tier,
/// not just its byte figure (#6929 review).
///
/// Why: `classify` and `classify_tier` shell out to `git` and `gh`, so the
/// clock can cross after the loop admitted this worktree and before the
/// measurement. The row that produced kept the tier those subprocesses
/// established — `stale`, `reclaimable: true` — with `bytes: null`, which the
/// console renders as a clearable worktree of unknown size. The loop's own
/// check produces `not_inspected` when the clock crosses one instant earlier,
/// and this row must be indistinguishable from that one.
#[test]
fn a_deadline_that_crosses_mid_inspection_yields_a_not_inspected_row() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("mid-inspection");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    GitWorktreeFixture::commit_all_and_push(&wt, "pushed");

    // Live when the loop checks THIS worktree, spent by the time `inspect`
    // reaches the measurement: the injected pull-request probe stands in for
    // the `gh` call the daemon makes, and sleeps past the deadline. The sleep
    // is scoped to this one worktree so the fixture's other registration — the
    // `<repos_root>/owner/repo` checkout itself — cannot burn the deadline
    // first and send this row down the LOOP's not-inspected path instead.
    let deadline = Instant::now() + Duration::from_millis(1_500);
    let inspected: RefCell<Vec<PathBuf>> = RefCell::new(Vec::new());
    let pr_state = |scanned: &ScannedWorktree| {
        if scanned.path == wt {
            inspected.borrow_mut().push(scanned.path.clone());
            std::thread::sleep(Duration::from_millis(2_000));
        }
        BranchPrState::Merged { pr: 7 }
    };
    let agent_state = |_: &AgentWorktreeOwner| AgentDelegationState::Ended;
    let keep_list = no_keeps();
    let claims = LiveClaims::default();
    let index = RefCell::new(test_index());
    let measure = |path: &Path, budget: Option<Duration>| {
        survey_run::measure(&mut index.borrow_mut(), path, budget)
    };
    let probes = DiskProbes {
        pr_state: &pr_state,
        claims: &claims,
        agent_state: &agent_state,
        dirt: &inspect_dirt,
        measure: &measure,
    };
    let survey = run(&fx.repos_root, &keep_list, &probes, Some(deadline), None);

    assert_eq!(
        inspected.borrow().as_slice(),
        std::slice::from_ref(&wt),
        "the loop's own deadline check must have PASSED for THIS worktree and \
         `inspect` must have run on it, or this proves nothing about a crossing \
         INSIDE one inspection"
    );
    let row = row_for(&survey, &wt);
    assert_eq!(
        row.tier,
        WorktreeTier::Review,
        "a merged, clean worktree classifies as stale — but the deadline crossed \
         before it could be measured, so the tier goes with the bytes: {row:#?}"
    );
    assert_eq!(row.gate, Some(ReclaimGate::Deadline), "{row:#?}");
    assert!(
        !row.reclaimable,
        "a row the survey ran out of time on is never advertised as clearable: {row:#?}"
    );
    assert_eq!(row.bytes, None, "{row:#?}");
    assert_eq!(
        row.reasons[0].code,
        ReasonCode::UnknownBranchState,
        "{row:#?}"
    );
}

/// No single measurement may outlive the survey that asked for it (#6929).
///
/// Why: the index's own walk budget is a fixed 30 seconds, so a cold refresh
/// starting at 24 seconds into a 30-second survey ran to ~50 and the console's
/// stdio transport returned a 502 with no survey at all. The fix is that the
/// survey hands each measurement the time it actually has left, which is what
/// this pins — a budget bounded by the deadline on every call, and `None` only
/// when the survey has no deadline to bound it with.
#[test]
fn the_survey_hands_each_measurement_only_the_time_left() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("budgeted");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);

    let window = Duration::from_secs(5);
    let deadline = Instant::now() + window;
    let seen: RefCell<Vec<Option<Duration>>> = RefCell::new(Vec::new());
    let pr = BranchPrState::Merged { pr: 1 };
    let pr_state = |_: &ScannedWorktree| pr.clone();
    let agent_state = |_: &AgentWorktreeOwner| AgentDelegationState::Ended;
    let keep_list = no_keeps();
    let claims = LiveClaims::default();
    let index = RefCell::new(test_index());
    let measure = |path: &Path, budget: Option<Duration>| {
        seen.borrow_mut().push(budget);
        survey_run::measure(&mut index.borrow_mut(), path, budget)
    };
    let probes = DiskProbes {
        pr_state: &pr_state,
        claims: &claims,
        agent_state: &agent_state,
        dirt: &inspect_dirt,
        measure: &measure,
    };

    run(&fx.repos_root, &keep_list, &probes, Some(deadline), None);
    let budgets = seen.borrow().clone();
    assert!(
        budgets.len() >= 3,
        "the worktree, its project and the root are each measured: {budgets:?}"
    );
    for budget in &budgets {
        let budget = budget.expect("a deadlined survey budgets every measurement");
        assert!(
            budget <= window,
            "a walk may not be given more time than the survey has: {budget:?} > {window:?}"
        );
        assert!(
            !budget.is_zero(),
            "a spent deadline skips the walk entirely"
        );
    }

    seen.borrow_mut().clear();
    run(&fx.repos_root, &keep_list, &probes, None, None);
    let unbounded = seen.borrow().clone();
    assert!(!unbounded.is_empty());
    assert!(
        unbounded.iter().all(Option::is_none),
        "with no deadline the index's own ceiling stands: {unbounded:?}"
    );
}

/// The §16.5 payload shape, pinned so a rename cannot silently break the
/// console's renderer.
#[test]
fn a_survey_serializes_the_documented_shape() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("shape");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    GitWorktreeFixture::commit_all_and_push(&wt, "pushed");

    let survey = survey_fixture(&fx, &Fixed::new(BranchPrState::Merged { pr: 4 }), &[], None);
    let json = serde_json::to_value(&survey).expect("the survey must serialize");

    assert!(json["generated_at"].is_string(), "{json:#}");
    assert!(json["keep_list"]["patterns"].is_array(), "{json:#}");
    assert!(json["keep_list"]["invalid"].is_array(), "{json:#}");
    assert!(
        json["keep_list"]["error"].is_null(),
        "a readable keep-list carries no error: {json:#}"
    );
    assert!(json["root"]["path"].is_string(), "{json:#}");
    assert!(json["root"]["projects"].is_array(), "{json:#}");
    assert!(json["root"]["counts"]["stale"].is_number(), "{json:#}");

    let project = &json["root"]["projects"][0];
    assert_eq!(project["name"], "owner/repo", "{json:#}");
    // The scan lists the main checkout too — it holds real bytes and the
    // sunburst renders it — so the row is selected by path, not by position.
    let row = project["worktrees"]
        .as_array()
        .expect("worktrees array")
        .iter()
        .find(|r| r["path"] == wt.to_string_lossy().as_ref())
        .unwrap_or_else(|| panic!("no row for {}: {json:#}", wt.display()));
    assert_eq!(row["id"], row["path"], "the path IS the id");
    assert_eq!(row["tier"], "stale", "{row:#}");
    assert_eq!(row["reasons"][0]["code"], "merged-pr", "{row:#}");
    assert_eq!(row["pr"]["number"], 4, "{row:#}");
    assert_eq!(row["pr"]["state"], "merged", "{row:#}");
    assert!(row["branch"].is_string(), "{row:#}");
    assert!(row["size"]["from_cache"].is_boolean(), "{row:#}");
    assert!(row["size"]["measured_at"].is_string(), "{row:#}");
    assert_eq!(row["gate"], serde_json::Value::Null, "{row:#}");
    assert_eq!(row["session"], serde_json::Value::Null, "{row:#}");
}

/// An unreadable keep-list is DISCLOSED, and shows every worktree as kept
/// (#6927).
///
/// Why the console must be able to see this: the fail-closed state renders an
/// all-`keep` view, which is byte-for-byte what a workspace with nothing to
/// clear also renders. Only `keep_list.error` tells them apart, and without it
/// an operator would read a broken config as a tidy workspace.
#[test]
fn an_unreadable_keep_list_is_reported_and_keeps_every_row() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("unreadable-keeps");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    GitWorktreeFixture::commit_all_and_push(&wt, "pushed");

    let keep_list = KeepList::unreadable("config YAML error at /x/config.yaml: bad");
    let fixed = Fixed::new(BranchPrState::Merged { pr: 4 });
    let pr_state = |_: &ScannedWorktree| fixed.pr.clone();
    let agent_state = |_: &AgentWorktreeOwner| AgentDelegationState::Ended;
    let index = RefCell::new(test_index());
    let measure = |path: &Path, budget: Option<Duration>| {
        survey_run::measure(&mut index.borrow_mut(), path, budget)
    };
    let probes = DiskProbes {
        pr_state: &pr_state,
        claims: &fixed.claims,
        agent_state: &agent_state,
        dirt: &inspect_dirt,
        measure: &measure,
    };
    let survey = run(&fx.repos_root, &keep_list, &probes, None, None);

    assert_eq!(survey.root.counts.stale, 0, "{:#?}", survey.root);
    let row = rows(&survey)
        .into_iter()
        .find(|r| r.path == wt)
        .unwrap_or_else(|| panic!("no row for {}", wt.display()));
    assert_eq!(row.tier, WorktreeTier::Keep, "{row:#?}");
    assert_eq!(row.reasons[0].code, ReasonCode::KeepList, "{row:#?}");
    assert!(
        row.reasons[0].detail.contains("could not be read"),
        "{row:#?}"
    );
    assert!(!row.reclaimable, "{row:#?}");

    let json = serde_json::to_value(&survey).expect("serialize");
    assert_eq!(
        json["keep_list"]["error"], "config YAML error at /x/config.yaml: bad",
        "the console has to be able to say WHY everything is kept: {json:#}"
    );
}

/// A `project` filter selects one managed project and nothing else.
#[test]
fn a_project_filter_selects_only_that_project() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("filtered");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);

    let keep_list = no_keeps();
    let pr = BranchPrState::Merged { pr: 1 };
    let pr_state = |_: &ScannedWorktree| pr.clone();
    let agent_state = |_: &AgentWorktreeOwner| AgentDelegationState::Ended;
    let claims = LiveClaims::default();
    let index = RefCell::new(test_index());
    let measure = |path: &Path, budget: Option<Duration>| {
        survey_run::measure(&mut index.borrow_mut(), path, budget)
    };
    let probes = DiskProbes {
        pr_state: &pr_state,
        claims: &claims,
        agent_state: &agent_state,
        dirt: &inspect_dirt,
        measure: &measure,
    };
    let miss = run(
        &fx.repos_root,
        &keep_list,
        &probes,
        None,
        Some("someone-else/repo"),
    );
    assert!(miss.root.projects.is_empty(), "{:#?}", miss.root.projects);

    let hit = run(
        &fx.repos_root,
        &keep_list,
        &probes,
        None,
        Some("owner/repo"),
    );
    assert_eq!(hit.root.projects.len(), 1, "{:#?}", hit.root.projects);
}
