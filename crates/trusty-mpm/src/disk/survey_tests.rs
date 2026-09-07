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

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{ReasonCode, WorktreeFacts, WorktreeTier, classify_tier};
use crate::disk::size_index::{DirSizeIndex, IndexPolicy};
use crate::disk::survey_run::{DiskProbes, run};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_keep_list::KeepList;
use crate::session_manager::worktree_ownership::{AgentDelegationState, AgentWorktreeOwner};
use crate::session_manager::worktree_reclaim::{
    BranchPrState, LiveClaims, ReclaimGate, ReclaimVerdict, WorkspaceClaim,
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
#[test]
fn a_reclaimable_verdict_is_always_shown_stale() {
    let pr = BranchPrState::Merged { pr: 7 };
    let claim = ClaimState::Unclaimed;
    let verdict = reclaimable();
    let wt = existing();
    let c = classify_tier(
        &facts(wt.path(), Some("feat/x"), &pr, &claim, &verdict),
        &no_keeps(),
        &clean,
    );
    assert!(verdict.is_reclaimable());
    assert_eq!(c.tier, WorktreeTier::Stale, "{c:?}");
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
    let probes = DiskProbes {
        pr_state: &pr_state,
        claims: &fixed.claims,
        agent_state: &agent_state,
        dirt: &inspect_dirt,
    };
    let mut index = test_index();
    run(
        &fx.repos_root,
        &keep_list,
        keep_patterns,
        &probes,
        &mut index,
        deadline,
        None,
    )
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
    let probes = DiskProbes {
        pr_state: &pr_state,
        claims: &claims,
        agent_state: &agent_state,
        dirt: &inspect_dirt,
    };
    let mut index = test_index();
    let miss = run(
        &fx.repos_root,
        &keep_list,
        &[],
        &probes,
        &mut index,
        None,
        Some("someone-else/repo"),
    );
    assert!(miss.root.projects.is_empty(), "{:#?}", miss.root.projects);

    let hit = run(
        &fx.repos_root,
        &keep_list,
        &[],
        &probes,
        &mut index,
        None,
        Some("owner/repo"),
    );
    assert_eq!(hit.root.projects.len(), 1, "{:#?}", hit.root.projects);
}
