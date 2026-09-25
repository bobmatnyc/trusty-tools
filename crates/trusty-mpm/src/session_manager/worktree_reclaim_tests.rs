//! Tests for merged-PR worktree reclamation (#2919).
//!
//! Why: this module deletes directories. Every gate in [`super::classify`]
//! therefore gets a test that FAILS if that gate is removed — a guard whose
//! absence no test notices is not a guard. The gates are exercised through the
//! real `classify`, against real git worktrees where the check reads git.
//! What: one refusal test per gate (non-admitted, live session, not
//! tm-provisioned, agent-owned and still live, agent-owned with the registry
//! unable to answer, an unreadable sentinel inside the agent store, open PR,
//! closed-unmerged PR, no PR, indeterminate PR state, dirty tree, unpushed
//! commits), the approval paths, the [`super::PrIndex`]
//! truncation/availability rules that decide when "absent" is allowed to mean
//! "no PR", and the `Report`-mode and re-check behaviour of the removal path.

use std::path::{Path, PathBuf};

use super::*;
// #6867: the runner's own vocabulary — the failure type, and the keychain
// predicate whose answer decides whether a poll can hang in `securityd`.
use crate::session_manager::worktree_reclaim_gh::{
    GhFailure, consults_the_keychain, gh_command, keychain_warning,
};

use crate::session_manager::worktree_git_fixture::{GitWorktreeFixture, deny_all};
use crate::session_manager::worktree_ownership::{AgentDelegationState, AgentWorktreeOwner};
use crate::session_manager::worktree_safety::inspect_dirt;
// #7889: gate 5's admission verdict, now defined outside `worktree_reclaim`.
use crate::core::worktree_carried_by_pr::CarriedByPr;
use crate::core::worktree_landed_content::{LandedContent, LandingAdmission};

/// A dirt probe that always reports CLEAN — used only where the test's subject
/// is a gate ABOVE the dirt gate, so a real probe would add nothing.
fn clean(_: &Path) -> Option<DirtyWorktree> {
    None
}

/// A dirt probe that always reports dirty, standing in for any of the many
/// ways `inspect_dirt` fails toward dirty.
fn dirty(path: &Path) -> Option<DirtyWorktree> {
    Some(DirtyWorktree::new(path, "2 modified files", 2, 0))
}

/// Put the worktree in the state a merged PR leaves behind: one commit, pushed.
///
/// The file write is required rather than incidental — `commit_all_and_push`
/// runs a real `git commit`, which refuses an empty commit, and a fresh
/// `git worktree add` has nothing staged.
fn land(path: &Path) {
    std::fs::write(path.join("landed.rs"), "// landed\n").expect("write landed file");
    GitWorktreeFixture::commit_all_and_push(path, "landed");
}

fn merged(pr: u64) -> BranchPrState {
    BranchPrState::Merged { pr }
}

/// A delegation registry that has never heard of any agent — the REFUSING
/// answer (#5661).
///
/// Used as the default in every pre-#5661 test below, deliberately: those
/// fixtures are session-owned, so the strictest possible agent probe must still
/// leave their verdicts untouched. A test that went green only because the probe
/// was permissive would prove nothing about the gate's scope.
fn no_agents(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Unknown
}

/// A registry that reports the owning agent as still working.
fn agent_live(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Live
}

/// A registry that holds the owning agent's delegation and calls it finished.
fn agent_ended(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Ended
}

/// [`classify`] with the refusing agent probe, for the tests whose subject is a
/// different gate (#5661).
///
/// Since #6806 the gate-2 argument is a [`ClaimState`] rather than a bool;
/// [`claim`] builds the two states the pre-#6806 `live: bool` stood for.
fn classify_no_agent(
    path: &Path,
    admission: Admission,
    claim: &ClaimState,
    pr: &BranchPrState,
    probe_dirt: &dyn Fn(&Path) -> Option<DirtyWorktree>,
) -> ReclaimVerdict {
    classify(
        path,
        admission,
        claim,
        pr,
        probe_dirt,
        &no_agents,
        &SessionOwners::default(),
        &KeepList::default(),
    )
}

/// The gate-2 claim state a `live: bool` used to stand for (#6806).
///
/// `true` becomes a FOREIGN session's claim — the strictest gate-2 answer — so
/// every test aimed at another gate keeps the verdict it always had.
fn claim(live: bool) -> ClaimState {
    if live {
        ClaimState::Foreign {
            session: "tm-other-01".to_string(),
            caller: Some("tm-client-03".to_string()),
        }
    } else {
        ClaimState::Unclaimed
    }
}

/// A real git worktree in the harness agent store, landed and pushed — the
/// exact shape `tm session prune-worktrees --merged-prs --force` deleted three
/// live agents' trees in (#5661).
fn agent_store_worktree(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    let path = fx.add_worktree_at(&fx.repo.join(".claude").join("worktrees"), name);
    land(&path);
    path
}

/// A synthetic path that satisfies the OWNERSHIP gate, so tests aimed at the
/// gates BELOW it are not short-circuited there.
///
/// `is_session_worktree` is a pure path predicate — the `.worktrees/<name>`
/// shape is what it matches, and nothing needs to exist on disk.
fn wt() -> PathBuf {
    PathBuf::from("/tmp/.worktrees/worktree-2919")
}

/// The refusal text of any blocking verdict, whichever gate produced it.
///
/// Both refusal kinds are accepted so that the tests aimed at gates 1-3, 5 and
/// 6 keep asserting on their own gate's wording rather than on which variant
/// carries it (#5829).
fn reason(v: &ReclaimVerdict) -> String {
    match v {
        ReclaimVerdict::Blocked { reason, .. } | ReclaimVerdict::BlockedByAgent { reason, .. } => {
            reason.clone()
        }
        ReclaimVerdict::Reclaimable { pr } => panic!("expected a refusal, got Reclaimable {pr}"),
        // #7889: the second grant kind. Same contract — a refusal test that
        // reached it has found a gate that stopped refusing.
        ReclaimVerdict::ReclaimableLandedContent { base } => {
            panic!("expected a refusal, got landed content on {base}")
        }
    }
}

/// [`classify_with_landed_content`] with the refusing agent probe, for the
/// #7889 gate-5 tests.
///
/// Why: the admission's whole point is that gate 5 stops being the end of the
/// road, so its tests need the ninth argument `classify_no_agent` does not
/// take. Everything else is that helper's fixture verbatim, so a difference in
/// verdict is attributable to the admission alone.
fn classify_landed(
    path: &Path,
    probe_dirt: &dyn Fn(&Path) -> Option<DirtyWorktree>,
    landed: LandedContent,
) -> ReclaimVerdict {
    classify_admission(path, &BranchPrState::NoPr, probe_dirt, Some(landed.into()))
}

/// [`classify_landed`] for any pull-request state and both admission routes,
/// or with no probe offered at all (#7889).
fn classify_admission(
    path: &Path,
    pr: &BranchPrState,
    probe_dirt: &dyn Fn(&Path) -> Option<DirtyWorktree>,
    admission: Option<LandingAdmission>,
) -> ReclaimVerdict {
    let ask = |_: &Path| admission.clone().expect("probe asked only when offered");
    classify_with_landed_content(
        path,
        Admission::Admitted,
        &claim(false),
        pr,
        probe_dirt,
        &no_agents,
        &SessionOwners::default(),
        &KeepList::default(),
        admission
            .as_ref()
            .map(|_| &ask as &dyn Fn(&Path) -> LandingAdmission),
    )
}

/// The landed-content answer the #7889 shape produces.
fn landed_on_main() -> LandedContent {
    LandedContent::Landed {
        base: "origin/main".to_string(),
        base_sha: "7df1c383f0a1b2c3d4e5f60718293a4b5c6d7e8f".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Gate 1 — git's own admission verdict
// ---------------------------------------------------------------------------

#[test]
fn classify_blocks_non_admitted_worktree() {
    // Every non-Admitted verdict must refuse, including the operator's explicit
    // `git worktree lock` and the repository's own main checkout. Deleting the
    // main checkout is the 2026-07-21 incident in miniature.
    for admission in [
        Admission::MainCheckout,
        Admission::Bare,
        Admission::Locked,
        // #6561: the harness's own agent-lifetime lock refuses too — it is
        // reported differently, never more permissively.
        Admission::HarnessAgentLock,
        Admission::Prunable,
        Admission::Unresolvable,
        Admission::OutsideProject,
        Admission::OutsideReposRoot,
    ] {
        let v = classify_no_agent(&wt(), admission, &claim(false), &merged(1), &clean);
        assert!(
            !v.is_reclaimable(),
            "{admission:?} must never be reclaimable, even with a merged PR"
        );
        // #7771: a harness lock appends why its holder still counts.
        assert!(reason(&v).starts_with(admission.reason()), "{}", reason(&v));
    }
}

// ---------------------------------------------------------------------------
// Gate 2 — a live session's workspace
// ---------------------------------------------------------------------------

#[test]
fn classify_blocks_live_session_workspace() {
    // The strongest gate: a merged PR plus a clean tree still loses to a
    // session that claims the path. A live session can sit in a directory whose
    // record reads terminal — measured on this repo 2026-07-28.
    let v = classify_no_agent(
        &wt(),
        Admission::Admitted,
        &claim(true),
        &merged(42),
        &clean,
    );
    assert!(!v.is_reclaimable());
    assert!(
        reason(&v).contains("claims this workspace"),
        "{}",
        reason(&v)
    );
}

/// #6806 closure criterion 2: the refusal names the claiming session and says
/// it is not the caller. Fails on `origin/main`, where gate 2's refusal was the
/// fixed string "a session still claims this workspace".
#[test]
fn classify_names_the_foreign_session_that_blocked_a_candidate() {
    let v = classify_no_agent(
        &wt(),
        Admission::Admitted,
        &claim(true),
        &merged(42),
        &clean,
    );
    let reason = reason(&v);
    assert!(
        reason.contains("tm-other-01"),
        "names the claimant: {reason}"
    );
    assert!(
        reason.contains("tm-client-03"),
        "names the caller: {reason}"
    );
    assert!(
        reason.contains("not the calling session"),
        "says the claim is not the caller's: {reason}"
    );
}

/// #6806 closure criteria 1 and 4: a worktree the CALLER alone claims — the
/// nested `.worktrees/<name>` shape a session creates inside its own workspace
/// — passes gate 2 and reaches the merged-PR and unsaved-work gates. Fails on
/// `origin/main`, where any claim at all blocked.
#[test]
fn classify_allows_a_worktree_claimed_only_by_the_calling_session() {
    let v = classify(
        &wt(),
        Admission::Admitted,
        &ClaimState::CallerNested {
            session: "tm-client-03".to_string(),
        },
        &merged(42),
        &clean,
        &no_agents,
        &SessionOwners::default(),
        &KeepList::default(),
    );
    assert_eq!(
        v,
        ReclaimVerdict::Reclaimable { pr: 42 },
        "the caller's own claim must not block its own nested worktree"
    );
}

/// The caller's own WORKSPACE is not a worktree it may reclaim, however it
/// asked — the guard that keeps #6806 from becoming a self-deletion.
#[test]
fn classify_blocks_the_callers_own_workspace() {
    let v = classify(
        &wt(),
        Admission::Admitted,
        &ClaimState::CallerWorkspace {
            session: "tm-client-03".to_string(),
        },
        &merged(42),
        &clean,
        &no_agents,
        &SessionOwners::default(),
        &KeepList::default(),
    );
    assert!(!v.is_reclaimable());
    assert!(reason(&v).contains("IS the caller"), "{}", reason(&v));
}

// ---------------------------------------------------------------------------
// Gate 3 — landing evidence
// ---------------------------------------------------------------------------

#[test]
fn classify_blocks_open_pr() {
    let v = classify_no_agent(
        &wt(),
        Admission::Admitted,
        &claim(false),
        &BranchPrState::Open { pr: 9 },
        &clean,
    );
    assert!(!v.is_reclaimable());
    assert!(reason(&v).contains("#9"), "{}", reason(&v));
}

#[test]
fn classify_blocks_closed_unmerged_pr() {
    // Closed-without-merging is NOT landing evidence — the branch may hold the
    // only copy of abandoned-but-wanted work.
    let v = classify_no_agent(
        &wt(),
        Admission::Admitted,
        &claim(false),
        &BranchPrState::ClosedUnmerged { pr: 11 },
        &clean,
    );
    assert!(!v.is_reclaimable());
    assert!(reason(&v).contains("without merging"), "{}", reason(&v));
}

#[test]
fn classify_blocks_no_pr() {
    let v = classify_no_agent(
        &wt(),
        Admission::Admitted,
        &claim(false),
        &BranchPrState::NoPr,
        &clean,
    );
    assert!(!v.is_reclaimable());
    assert!(reason(&v).contains("no pull request"), "{}", reason(&v));
}

/// 🔴 REGRESSION (#7889): gate 5's admission. A clean worktree whose every file
/// is already on `origin/main` is reclaimable even though GitHub has no pull
/// request for its branch — the donor-branch shape, where the work landed
/// through a sibling's squash and no pull request will ever carry this name.
///
/// Owner ruling 2026-09-22. Fails against the pre-#7889 gate, which refuses
/// here unconditionally: nineteen such trees were spared across 2026-09-21/22,
/// each holding 10–25 GB.
#[test]
fn worktree_7889_classify_admits_a_landed_tree_with_no_pull_request() {
    let v = classify_landed(&wt(), &clean, landed_on_main());
    assert_eq!(
        v,
        ReclaimVerdict::ReclaimableLandedContent {
            base: "origin/main".to_string()
        },
        "a tree holding no content the remote lacks must be reclaimable"
    );
}

/// 🔴 #7889, the refusing direction: one path the merge would still change is
/// work on no remote. The refusal names the admission and that path.
#[test]
fn worktree_7889_classify_refuses_a_tree_holding_residue() {
    let v = classify_landed(
        &wt(),
        &clean,
        LandedContent::Residual {
            base: "origin/main".to_string(),
            first_path: "crates/trusty-mpm/src/daemon/mod.rs".to_string(),
        },
    );
    assert!(!v.is_reclaimable());
    let r = reason(&v);
    assert!(r.contains("landed-content"), "{r}");
    assert!(r.contains("crates/trusty-mpm/src/daemon/mod.rs"), "{r}");
}

/// 🔴 #7889, ADR-0045: a failed refresh, an unresolvable base and a
/// `merge-tree` error all arrive as `Unavailable`, and none of them grants.
#[test]
fn worktree_7889_classify_refuses_when_the_admission_is_unavailable() {
    let v = classify_landed(
        &wt(),
        &clean,
        LandedContent::unavailable("`origin` could not be refreshed: host unreachable"),
    );
    assert!(!v.is_reclaimable());
    let r = reason(&v);
    assert!(r.contains("landed-content"), "{r}");
    assert!(r.contains("could not be refreshed"), "{r}");
}

/// 🔴 #7889: the admission did not become a bypass — gate 6's unsaved-work
/// check still decides first, so a dirty tree is refused however landed its
/// history is.
#[test]
fn worktree_7889_classify_refuses_a_dirty_tree_whose_content_is_landed() {
    let v = classify_landed(&wt(), &dirty, landed_on_main());
    assert!(!v.is_reclaimable());
    assert!(reason(&v).contains("unsaved work"), "{}", reason(&v));
}

/// 🔴 REGRESSION (#7889): gate 6 counts a donor branch's commits as unpushed.
/// They reach no `origin` ref, and the squash that landed them also carried
/// the continuing agent's work, so no patch id matches. Those commits are what
/// the content comparison judges, so commits-only dirt must reach it.
///
/// Fails before the fix: gate 6 refused ANY dirt ahead of the admission, so
/// the sweep could never admit the shape #7889 is about.
#[test]
fn worktree_7889_classify_admits_commits_only_dirt_when_landed() {
    let commits_only = |p: &Path| Some(DirtyWorktree::new(p, "0 files, 3 unpushed", 0, 3));
    let v = classify_landed(&wt(), &commits_only, landed_on_main());
    assert!(v.is_reclaimable(), "commits the base already holds: {v:?}");
}

/// 🔴 #7889: an uncommitted file beside those commits is outside HEAD, so the
/// comparison cannot vouch for it and gate 6 still refuses.
#[test]
fn worktree_7889_classify_refuses_files_beside_unpushed_commits() {
    let both = |p: &Path| Some(DirtyWorktree::new(p, "1 file, 3 unpushed", 1, 3));
    let v = classify_landed(&wt(), &both, landed_on_main());
    assert!(!v.is_reclaimable());
    assert!(reason(&v).contains("unsaved work"), "{}", reason(&v));
}

/// The (b) refusal and (c) answer a superseded donor produces (#7889).
fn residual_then(carried: CarriedByPr) -> LandingAdmission {
    LandingAdmission {
        content: LandedContent::Residual {
            base: "origin/main".to_string(),
            first_path: "src/superseded.rs".to_string(),
        },
        carried: Some(carried),
    }
}

/// Commits no `origin` ref reaches and no patch id matches — the donor's dirt.
fn donor_commits(p: &Path) -> Option<DirtyWorktree> {
    Some(DirtyWorktree::new(p, "0 files, 2 unpushed", 0, 2))
}

/// 🔴 REGRESSION (#7889, route (c)): with no pull request under this name, a
/// HEAD inside a merged pull request's history is reclaimable under THAT pull
/// request, even where the content comparison found residue.
#[test]
fn worktree_7889_classify_admits_a_tree_a_merged_pr_carried() {
    let carried = residual_then(CarriedByPr::Carried {
        pr: 8328,
        pr_head: "2222222222222222222222222222222222222222".to_string(),
    });
    let v = classify_admission(&wt(), &BranchPrState::NoPr, &clean, Some(carried));
    assert_eq!(v, ReclaimVerdict::Reclaimable { pr: 8328 });
}

/// 🔴 REGRESSION (#7889): gate 5 found the sibling's merged pull request through
/// the #7267 commit search, and gate 6 then counted the donor's commits as
/// unpushed. The admission judges them; its probe refuses any HEAD cannot reach.
///
/// Fails before the fix: gate 6 refused any dirt on a merged pull request, so
/// a donor matched by commit never reached the admission.
#[test]
fn worktree_7889_a_merged_pr_with_commits_only_dirt_reaches_the_admission() {
    let merged = BranchPrState::Merged { pr: 8328 };
    let v = classify_admission(
        &wt(),
        &merged,
        &donor_commits,
        Some(landed_on_main().into()),
    );
    assert_eq!(v, ReclaimVerdict::Reclaimable { pr: 8328 });
    // No probe offered keeps the pre-#7889 refusal.
    let unoffered = classify_admission(&wt(), &merged, &donor_commits, None);
    assert!(
        reason(&unoffered).contains("unsaved work"),
        "{}",
        reason(&unoffered)
    );
}

/// 🔴 #7889: on a merged pull request, commits the admission cannot vouch for
/// still refuse, and the refusal names the dirt and each route that failed.
#[test]
fn worktree_7889_a_merged_pr_with_unlanded_commits_still_refuses() {
    let neither = residual_then(CarriedByPr::Unavailable {
        detail: "gh timed out".to_string(),
    });
    let v = classify_admission(
        &wt(),
        &BranchPrState::Merged { pr: 8328 },
        &donor_commits,
        Some(neither),
    );
    assert!(!v.is_reclaimable());
    let r = reason(&v);
    assert!(r.contains("2 unpushed"), "{r}");
    assert!(r.contains("landed-content"), "{r}");
    assert!(r.contains("src/superseded.rs"), "{r}");
    assert!(r.contains("merged-pr-ancestry"), "{r}");
    assert!(r.contains("gh timed out"), "{r}");
}

#[test]
fn classify_blocks_unknown_pr_state() {
    // THE indeterminate case. An unanswerable probe must be a skip, never a
    // delete — this is the fail-closed property the whole module rests on.
    let v = classify_no_agent(
        &wt(),
        Admission::Admitted,
        &claim(false),
        &BranchPrState::Unknown,
        &clean,
    );
    assert!(!v.is_reclaimable());
    assert!(
        reason(&v).contains("could not be determined"),
        "{}",
        reason(&v)
    );
}

// ---------------------------------------------------------------------------
// Gate 4 — unsaved work
// ---------------------------------------------------------------------------

#[test]
fn classify_blocks_dirty_worktree() {
    // A merged PR does not prove the directory holds nothing novel. The
    // 2026-07-21 salvage found merged-PR worktrees carrying real unpushed
    // source; this gate is why they survive.
    let v = classify_no_agent(
        &wt(),
        Admission::Admitted,
        &claim(false),
        &merged(5),
        &dirty,
    );
    assert!(!v.is_reclaimable());
    assert!(reason(&v).contains("unsaved work"), "{}", reason(&v));
}

#[test]
fn classify_blocks_a_really_dirty_worktree() {
    // Same gate, but against REAL git rather than a stub probe: an uncommitted
    // file in a real worktree must block even with a merged PR.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("dirty-2919");
    std::fs::write(path.join("scratch.rs"), "fn main() {}\n").expect("write untracked file");
    let v = classify_no_agent(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(5),
        &inspect_dirt,
    );
    assert!(
        !v.is_reclaimable(),
        "a real untracked file must block: {v:?}"
    );
}

#[test]
fn classify_blocks_a_worktree_with_unpushed_commits() {
    // The case that LOOKS safe — the work IS committed — but is destroyed
    // anyway, because removal deletes the session branch that held it.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("unpushed-2919");
    GitWorktreeFixture::commit_unpushed(&path);
    let v = classify_no_agent(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(6),
        &inspect_dirt,
    );
    assert!(!v.is_reclaimable(), "an unpushed commit must block: {v:?}");
}

// ---------------------------------------------------------------------------
// The one path that says yes
// ---------------------------------------------------------------------------

#[test]
fn classify_allows_clean_pushed_merged_worktree() {
    // The approval path must actually be reachable, or every refusal test above
    // would pass against a function that refuses unconditionally.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("clean-2919");
    land(&path);
    let v = classify_no_agent(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(77),
        &inspect_dirt,
    );
    assert_eq!(v, ReclaimVerdict::Reclaimable { pr: 77 });
}

// ---------------------------------------------------------------------------
// Gate 4 — a dispatched agent's ownership (#5661)
// ---------------------------------------------------------------------------

#[test]
fn classify_blocks_a_live_agents_worktree() {
    // THE #5661 defect, in one test. The worktree carries an agent sentinel, has
    // no session record (so gate 2's `live` is false), sits under
    // `.claude/worktrees/`, and its branch merged — every condition the three
    // gates above check is satisfied, and before this gate existed `classify`
    // returned `Reclaimable` and the sweep deleted it out from under the agent.
    let fx = GitWorktreeFixture::new();
    let path = agent_store_worktree(&fx, "live-agent-5661");
    GitWorktreeFixture::stamp_agent_sentinel(&path, "agent-a39a3a8181f33e597");
    let v = classify(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(101),
        &inspect_dirt,
        &agent_live,
        &SessionOwners::default(),
        &KeepList::default(),
    );
    assert!(!v.is_reclaimable(), "a live agent's worktree: {v:?}");
    assert!(reason(&v).contains("has not ended"), "{}", reason(&v));
    assert!(
        reason(&v).contains("agent-a39a3a8181f33e597"),
        "the refusal must name the agent it is protecting: {}",
        reason(&v)
    );
}

/// The post-restart shape, with the harness still holding the tree (#5661,
/// retargeted by #6561; was `classify_blocks_an_agent_the_registry_never_heard_of`).
///
/// `DaemonState::delegations` is rebuilt empty at every boot, so "no delegation
/// names this agent" is what the registry says about an agent that is still
/// working, and an empty observation on a destructive path is undeterminable,
/// not absent (ADR-0045). #6561 did not weaken that: it found a SECOND source
/// for the same fact. The harness locks an agent's worktree for the life of that
/// agent, and the lock is a file under `.git/worktrees/<id>/` that no daemon
/// restart clears — so the fixture now states the fact the registry lost, and the
/// refusal must still stand.
#[test]
fn classify_blocks_an_agent_the_harness_still_holds_after_a_restart() {
    let fx = GitWorktreeFixture::new();
    let path = agent_store_worktree(&fx, "forgotten-agent-5661");
    GitWorktreeFixture::stamp_agent_sentinel(&path, "agent-lost-to-a-restart");
    fx.harness_lock_worktree(&path, "agent-lost-to-a-restart");
    let v = classify(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(102),
        &inspect_dirt,
        &no_agents,
        &SessionOwners::default(),
        &KeepList::default(),
    );
    assert!(!v.is_reclaimable(), "{v:?}");
    assert!(
        reason(&v).contains("still reports the harness"),
        "{}",
        reason(&v)
    );
}

#[test]
fn classify_records_an_agent_refusal_as_its_own_verdict_kind() {
    // #5829: the refusal reached the operator as nothing at all. Gate 4 spared
    // the tree — correctly, since #5661 — but returned an anonymous `Blocked`,
    // and a candidate blocked during classification appears in none of the
    // lists the prune route returns. So the survey now has to be able to TELL
    // this refusal from the other five, without matching on its wording.
    //
    // Both registry answers that refuse are pinned, because the post-restart
    // `Unknown` case is the one a fail-open implementation would quietly
    // reclassify as an ordinary block. #6561: the `Unknown` row now also holds
    // the harness lock, which is what carries that refusal since the registry's
    // silence alone stopped being the whole answer.
    let fx = GitWorktreeFixture::new();
    for (name, probe, agent) in [
        (
            "live-agent-5829",
            &agent_live as &dyn Fn(&AgentWorktreeOwner) -> AgentDelegationState,
            "agent-still-writing",
        ),
        (
            "forgotten-agent-5829",
            &no_agents,
            "agent-lost-to-a-restart",
        ),
    ] {
        let path = agent_store_worktree(&fx, name);
        GitWorktreeFixture::stamp_agent_sentinel(&path, agent);
        fx.harness_lock_worktree(&path, agent);
        let v = classify(
            &path,
            Admission::Admitted,
            &claim(false),
            &merged(105),
            &inspect_dirt,
            probe,
            &SessionOwners::default(),
            &KeepList::default(),
        );
        assert!(
            matches!(v, ReclaimVerdict::BlockedByAgent { .. }),
            "an agent-ownership refusal must be its own verdict kind, got {v:?}"
        );
        assert!(reason(&v).contains(agent), "{}", reason(&v));
    }
}

#[test]
fn classify_reserves_the_agent_verdict_for_agent_refusals() {
    // The complement, and the reason this is worth a test: if every refusal
    // collapsed into `BlockedByAgent`, the operator surface would report a dirty
    // tree or an open PR as "a dispatched agent owns it" and send the operator
    // hunting for an agent that does not exist.
    let dirty_tree = classify_no_agent(
        &wt(),
        Admission::Admitted,
        &claim(false),
        &merged(106),
        &dirty as &dyn Fn(&Path) -> Option<DirtyWorktree>,
    );
    assert!(
        matches!(dirty_tree, ReclaimVerdict::Blocked { .. }),
        "a dirty tree is not an agent refusal: {dirty_tree:?}"
    );
    let open_pr = classify_no_agent(
        &wt(),
        Admission::Admitted,
        &claim(false),
        &BranchPrState::Open { pr: 107 },
        &clean,
    );
    assert!(
        matches!(open_pr, ReclaimVerdict::Blocked { .. }),
        "an open PR is not an agent refusal: {open_pr:?}"
    );
}

/// `agent_owned` and `blocked_reasons` PARTITION the blocked set (#6507).
///
/// Why: `blocked_reasons` was folded from every refusal, so a tree an agent
/// holds was disclosed twice — once as the agent skip and once as an ordinary
/// block — and an operator reading the prune reply counts one spared worktree
/// as two separate refusals.
///
/// Both `BlockedByAgent` construction sites are exercised, because the harness
/// lock builds that verdict at GATE 1 and the registry probe builds it at gate
/// 4. An exclusion written against `ReclaimGate::AgentOwnership` alone leaves
/// the harness-locked candidate in both lists and fails here.
#[test]
fn survey_lists_an_agent_held_candidate_in_exactly_one_place() {
    let fx = GitWorktreeFixture::new();

    // Gate 1: the harness holds the tree for the life of the agent.
    let harness_held = agent_store_worktree(&fx, "harness-lock-6507");
    GitWorktreeFixture::stamp_agent_sentinel(&harness_held, "agent-under-a-harness-lock");
    fx.harness_lock_worktree(&harness_held, "agent-under-a-harness-lock");
    let harness_verdict = classify_no_agent(
        &harness_held,
        Admission::HarnessAgentLock,
        &claim(false),
        &merged(6507),
        &clean,
    );
    assert!(
        matches!(
            harness_verdict,
            ReclaimVerdict::BlockedByAgent {
                gate: ReclaimGate::Admission,
                ..
            }
        ),
        "gate 1's harness lock must build the agent verdict: {harness_verdict:?}"
    );

    // Gate 4: the registry reports the sentinel's agent as still working.
    let registry_held = agent_store_worktree(&fx, "live-agent-6507");
    GitWorktreeFixture::stamp_agent_sentinel(&registry_held, "agent-still-writing");
    let registry_verdict = classify(
        &registry_held,
        Admission::Admitted,
        &claim(false),
        &merged(6508),
        &inspect_dirt,
        &agent_live,
        &SessionOwners::default(),
        &KeepList::default(),
    );
    assert!(
        matches!(
            registry_verdict,
            ReclaimVerdict::BlockedByAgent {
                gate: ReclaimGate::AgentOwnership,
                ..
            }
        ),
        "gate 4 must build the agent verdict: {registry_verdict:?}"
    );

    // The control: an ordinary refusal still owes a `blocked_reasons` line.
    let dirty_tree = fx.add_worktree("dirty-6507");
    let dirty_verdict = classify_no_agent(
        &dirty_tree,
        Admission::Admitted,
        &claim(false),
        &merged(6509),
        &dirty,
    );
    assert!(
        matches!(dirty_verdict, ReclaimVerdict::Blocked { .. }),
        "the control must be an ordinary block: {dirty_verdict:?}"
    );

    let candidate = |path: &Path, verdict: ReclaimVerdict| ReclaimCandidate {
        path: path.to_path_buf(),
        branch: None,
        registry_root: fx.repo.clone(),
        bytes: Some(1),
        pr: merged(6507),
        verdict,
    };
    let survey = ReclaimSurvey::from_candidates(vec![
        candidate(&harness_held, harness_verdict),
        candidate(&registry_held, registry_verdict),
        candidate(&dirty_tree, dirty_verdict),
    ]);

    let lines_for = |list: &[String], path: &Path| {
        let prefix = format!("{}: ", path.display());
        list.iter().filter(|l| l.starts_with(&prefix)).count()
    };
    for held in [&harness_held, &registry_held] {
        assert_eq!(
            lines_for(&survey.agent_owned, held),
            1,
            "an agent-held tree owes exactly one agent line; got {:?}",
            survey.agent_owned
        );
        assert_eq!(
            lines_for(&survey.blocked_reasons, held),
            0,
            "and it must not be disclosed a second time as an ordinary block; got {:?}",
            survey.blocked_reasons
        );
    }
    assert_eq!(
        lines_for(&survey.blocked_reasons, &dirty_tree),
        1,
        "an ordinary refusal still owes its line; got {:?}",
        survey.blocked_reasons
    );
    assert_eq!(
        lines_for(&survey.agent_owned, &dirty_tree),
        0,
        "a dirty tree is nobody's agent skip; got {:?}",
        survey.agent_owned
    );
    assert_eq!(
        survey.agent_owned.len() + survey.blocked_reasons.len(),
        survey.blocked,
        "the two lists partition the blocked set: {:?} / {:?}",
        survey.agent_owned,
        survey.blocked_reasons
    );
}

#[test]
fn classify_allows_a_finished_agents_merged_worktree() {
    // The complement, and the whole reason the gate consults the registry
    // instead of refusing every agent-owned tree outright: once the delegation
    // has ended, the tree must become reclaimable or the agent store grows
    // without bound and the sweep stops doing its job.
    let fx = GitWorktreeFixture::new();
    let path = agent_store_worktree(&fx, "finished-agent-5661");
    let owner = GitWorktreeFixture::stamp_agent_sentinel(&path, "agent-that-finished");
    // #7771 (d): the dispatching session must be provably ended too.
    let v = classify(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(103),
        &inspect_dirt,
        &agent_ended,
        &GitWorktreeFixture::parent_ended(&owner),
        &KeepList::default(),
    );
    assert_eq!(v, ReclaimVerdict::Reclaimable { pr: 103 });
}

#[test]
fn classify_blocks_an_agent_store_worktree_with_an_unreadable_sentinel() {
    // #7771, #8511: an owner file that exists but cannot be read or parsed
    // could be a truncated claim, so the strict reader refuses it — with no
    // harness lock and the MOST permissive registry answer, which proves the
    // refusal comes from the owner file alone. Both spellings of unreadable are
    // covered: content that does not parse, and a file the process cannot open.
    //
    // Fails before the strict-reader switch: the tolerant reader folded both
    // into "names nobody", and the unlocked tree was reclaimed.
    let fx = GitWorktreeFixture::new();
    let garbled = agent_store_worktree(&fx, "garbled-sentinel-5661");
    crate::session_manager::worktree_ownership_location::write_sentinel_bytes(
        &garbled,
        b"{not json",
    )
    .expect("write a malformed sentinel");
    let v = classify(
        &garbled,
        Admission::Admitted,
        &claim(false),
        &merged(104),
        &inspect_dirt,
        &agent_ended,
        &SessionOwners::default(),
        &KeepList::default(),
    );
    assert!(!v.is_reclaimable(), "malformed sentinel: {v:?}");
    assert!(reason(&v).contains("does not parse"), "{}", reason(&v));

    let denied = agent_store_worktree(&fx, "denied-sentinel-5661");
    // #8511: the agent marker is written to the git admin dir.
    let sentinel =
        crate::session_manager::worktree_ownership_location::admin_sentinel_path(&denied)
            .expect("an agent-store worktree has a git admin dir");
    GitWorktreeFixture::stamp_agent_sentinel(&denied, "agent-behind-a-locked-door");
    let _restore = deny_all(&sentinel);
    let v = classify(
        &denied,
        Admission::Admitted,
        &claim(false),
        &merged(105),
        &inspect_dirt,
        &agent_ended,
        &SessionOwners::default(),
        &KeepList::default(),
    );
    assert!(!v.is_reclaimable(), "unreadable sentinel: {v:?}");
    assert!(reason(&v).contains("cannot be read"), "{}", reason(&v));
}

#[test]
fn classify_leaves_a_session_owned_worktree_alone() {
    // The over-correction guard. A session-owned `.worktrees/<name>` worktree
    // with a merged PR must still be reclaimable under the STRICTEST agent
    // probe — the #5661 gate is scoped to agent ownership, and a version of it
    // that refused everything would silently retire this whole feature.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("session-owned-5661");
    land(&path);
    GitWorktreeFixture::stamp_reclaimable_sentinel(&path);
    let v = classify(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(106),
        &inspect_dirt,
        &no_agents,
        // #7652: the owner's record is tombstoned and tmux no longer lists it.
        &GitWorktreeFixture::reclaimable_owner_gone(),
        &KeepList::default(),
    );
    assert_eq!(v, ReclaimVerdict::Reclaimable { pr: 106 });
}

// ---------------------------------------------------------------------------
// PrIndex — when "absent" is allowed to mean "no PR"
// ---------------------------------------------------------------------------

const ROWS: &str = r#"[
  {"number": 1, "headRefName": "feat/merged", "state": "MERGED"},
  {"number": 2, "headRefName": "feat/open", "state": "OPEN"},
  {"number": 3, "headRefName": "feat/closed", "state": "CLOSED"}
]"#;

#[test]
fn pr_index_reads_merged_open_and_closed_rows() {
    let idx = PrIndex::from_json(ROWS, 400);
    assert_eq!(
        idx.state_for(Some("feat/merged")),
        BranchPrState::Merged { pr: 1 }
    );
    assert_eq!(
        idx.state_for(Some("feat/open")),
        BranchPrState::Open { pr: 2 }
    );
    assert_eq!(
        idx.state_for(Some("feat/closed")),
        BranchPrState::ClosedUnmerged { pr: 3 }
    );
}

#[test]
fn pr_index_absent_branch_is_no_pr_when_complete() {
    let idx = PrIndex::from_json(ROWS, 400);
    assert_eq!(
        idx.state_for(Some("feat/never-had-one")),
        BranchPrState::NoPr
    );
}

#[test]
fn pr_index_truncated_reply_makes_absent_branches_unknown() {
    // A reply that filled the page may have dropped this branch's merged PR —
    // or its OPEN one. Absent must therefore mean "unknown", which blocks.
    let idx = PrIndex::from_json(ROWS, 3);
    assert_eq!(
        idx.state_for(Some("feat/never-had-one")),
        BranchPrState::Unknown
    );
    // Rows that DID come back are still trustworthy.
    assert_eq!(
        idx.state_for(Some("feat/merged")),
        BranchPrState::Merged { pr: 1 }
    );
}

// ---------------------------------------------------------------------------
// PrIndex — round siblings (#7267)
// ---------------------------------------------------------------------------

/// The rows a review round leaves behind: the pull request keeps the name it
/// was opened from, and the worktree sits on the other spelling.
const ROUND_ROWS: &str = r#"[
  {"number": 11, "headRefName": "fix/7267-thing", "state": "MERGED"},
  {"number": 12, "headRefName": "feat/other-r2", "state": "MERGED"}
]"#;

/// A worktree on `<branch>-r2` resolves the merged pull request opened from
/// `<branch>` (#7267).
///
/// Why: `prune-worktrees --merged-prs --force` reclaimed 0 of 11 stale trees
/// because every one of them had been renamed before its pull request opened.
#[test]
fn pr_index_resolves_a_round_sibling_by_stem() {
    let idx = PrIndex::from_json(ROUND_ROWS, 400);
    assert_eq!(
        idx.state_for(Some("fix/7267-thing-r3")),
        BranchPrState::Merged { pr: 11 }
    );
}

/// And the same in reverse: the worktree kept the stem, the pull request was
/// opened from the round-suffixed branch (#7267).
#[test]
fn pr_index_resolves_a_stem_branch_from_its_round_sibling() {
    let idx = PrIndex::from_json(ROUND_ROWS, 400);
    assert_eq!(
        idx.state_for(Some("feat/other")),
        BranchPrState::Merged { pr: 12 }
    );
}

/// A branch that shares no stem is still unrelated — the widening relates round
/// siblings and nothing else (#7267).
#[test]
fn pr_index_does_not_relate_an_unrelated_branch() {
    let idx = PrIndex::from_json(ROUND_ROWS, 400);
    assert_eq!(idx.state_for(Some("fix/7267-other")), BranchPrState::NoPr);
    // A suffix that is not `-r<digits>` is not a round suffix.
    assert_eq!(
        idx.state_for(Some("fix/7267-thing-rework")),
        BranchPrState::NoPr
    );
}

/// An OPEN sibling outranks a MERGED one, so a workstream with work still in
/// flight is refused rather than reclaimed (#7267).
#[test]
fn pr_index_open_round_sibling_beats_a_merged_one() {
    const ROWS_WITH_OPEN: &str = r#"[
  {"number": 21, "headRefName": "fix/x", "state": "MERGED"},
  {"number": 22, "headRefName": "fix/x-r2", "state": "OPEN"}
]"#;
    let idx = PrIndex::from_json(ROWS_WITH_OPEN, 400);
    assert_eq!(
        idx.state_for(Some("fix/x-r5")),
        BranchPrState::Open { pr: 22 }
    );
}

/// A `gh` call that FAILED reports its reason for every branch it cannot
/// answer, rather than a cause-free `Unknown` (#6561).
///
/// Why: the live failure this issue is about. `gh pr list` exited 4 for all 261
/// registered worktrees and the survey said only "0 reclaimable" — the reason
/// existed in `gh`'s stderr and was thrown away.
#[test]
fn pr_index_reports_a_failed_lookup_with_its_reason() {
    let idx = PrIndex::unavailable_because("`gh` exited 4: gh auth login".to_string());
    assert_eq!(
        idx.state_for(Some("anything")),
        BranchPrState::LookupFailed {
            reason: "`gh` exited 4: gh auth login".to_string()
        }
    );
}

#[test]
fn pr_index_malformed_json_is_unavailable() {
    // A `gh` that printed a warning, an auth prompt, or nothing at all must not
    // parse into an empty-but-complete index (which would read as "no PRs
    // anywhere" and make every branch NoPr).
    for junk in ["", "not json", "{}", "gh: not authenticated"] {
        let idx = PrIndex::from_json(junk, 400);
        // #6561: unparsable output is a FAILED lookup with a reason, no longer
        // an unexplained `Unknown`.
        assert!(
            matches!(
                idx.state_for(Some("feat/x")),
                BranchPrState::LookupFailed { .. }
            ),
            "{junk:?} must not yield a usable index"
        );
    }
}

#[test]
fn pr_index_detached_worktree_is_unknown() {
    // No branch means no pull request can prove the work landed.
    let idx = PrIndex::from_json(ROWS, 400);
    assert_eq!(idx.state_for(None), BranchPrState::Unknown);
}

#[test]
fn pr_index_open_pr_beats_a_merged_one_on_the_same_branch() {
    // Branch reuse: PR #1 merged, then the branch was pushed again and PR #2
    // opened. Reclaiming on #1 would delete live work.
    let rows = r#"[
      {"number": 1, "headRefName": "feat/reused", "state": "MERGED"},
      {"number": 2, "headRefName": "feat/reused", "state": "OPEN"}
    ]"#;
    assert_eq!(
        PrIndex::from_json(rows, 400).state_for(Some("feat/reused")),
        BranchPrState::Open { pr: 2 }
    );
    // Order must not matter.
    let flipped = r#"[
      {"number": 2, "headRefName": "feat/reused", "state": "OPEN"},
      {"number": 1, "headRefName": "feat/reused", "state": "MERGED"}
    ]"#;
    assert_eq!(
        PrIndex::from_json(flipped, 400).state_for(Some("feat/reused")),
        BranchPrState::Open { pr: 2 }
    );
}

#[test]
fn pr_index_unrecognised_state_is_not_treated_as_merged() {
    let rows = r#"[{"number": 1, "headRefName": "feat/x", "state": "DRAFT_SOMETHING"}]"#;
    let idx = PrIndex::from_json(rows, 400);
    assert_ne!(
        idx.state_for(Some("feat/x")),
        BranchPrState::Merged { pr: 1 }
    );
}

#[test]
fn gh_command_strips_repository_redirecting_env() {
    // `gh` resolves the repository through git, so an inherited GIT_DIR would
    // aim the PR query at a different repository entirely.
    let cmd = gh_command(
        Path::new("/tmp"),
        &crate::core::gh_identity::GhEnv::default(),
    );
    let removed: Vec<&str> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .filter_map(|(k, _)| k.to_str())
        .collect();
    // #8510: GH_HOST too — an inherited host must not retarget the lookup.
    for key in ["GIT_DIR", "GIT_WORK_TREE", "GH_REPO", "GH_HOST"] {
        assert!(
            removed.contains(&key),
            "{key} must be stripped: {removed:?}"
        );
    }
}

/// Why (#6623): the resolved daemon `gh` identity must reach the actual
/// `Command` — this is what makes a `GH_CONFIG_DIR` resolved from a project's
/// config take effect instead of leaving the daemon's bare launchd
/// environment in place.
#[test]
fn gh_command_applies_the_resolved_gh_env() {
    let env = crate::core::gh_identity::resolve_gh_env(Some(
        &crate::core::trusty_tools_config::GithubConfig {
            config_dir: Some(PathBuf::from("/cfg/daemon")),
            ..Default::default()
        },
    ))
    .expect("ok");
    let cmd = gh_command(Path::new("/tmp"), &env);
    let value = cmd
        .get_envs()
        .find(|(k, _)| *k == "GH_CONFIG_DIR")
        .and_then(|(_, v)| v)
        .expect("GH_CONFIG_DIR must be set");
    assert_eq!(value, "/cfg/daemon");
}

/// Why (#6668): `resolve_gh_env` set `GH_CONFIG_DIR` and nothing else, so a
/// `gh` spawned from a shell that exports a DIFFERENT account's `GH_TOKEN`
/// authenticated as that account — an env token outranks a scoped config dir
/// in gh's own resolution order. The per-project binding lost to the shell,
/// and `tm session prune-worktrees --merged-prs` reported "Could not resolve
/// to a Repository" for every repo the shell token could not see. A resolved
/// binding must therefore REMOVE the inherited identity vars from the child.
/// Test: itself.
#[test]
fn gh_command_removes_an_inherited_token_when_a_config_dir_binds() {
    let env = crate::core::gh_identity::resolve_gh_env(Some(
        &crate::core::trusty_tools_config::GithubConfig {
            config_dir: Some(PathBuf::from("/cfg/duetto")),
            ..Default::default()
        },
    ))
    .expect("ok");
    let cmd = gh_command(Path::new("/tmp"), &env);
    let removed: Vec<&str> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .filter_map(|(k, _)| k.to_str())
        .collect();
    for key in [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        "GH_USER",
    ] {
        assert!(
            removed.contains(&key),
            "{key} must be removed when a config_dir binds: {removed:?}"
        );
    }
    let value = cmd
        .get_envs()
        .find(|(k, _)| *k == "GH_CONFIG_DIR")
        .and_then(|(_, v)| v)
        .expect("GH_CONFIG_DIR must still be set");
    assert_eq!(value, "/cfg/duetto");
}

/// Why (#6668): with no binding the spawn must stay exactly as ambient as it
/// was — removing an inherited token from an UNBOUND `gh` call would break
/// every operator whose only credential is `GH_TOKEN`.
/// Test: itself.
#[test]
fn gh_command_keeps_an_inherited_token_when_nothing_binds() {
    let cmd = gh_command(
        Path::new("/tmp"),
        &crate::core::gh_identity::GhEnv::default(),
    );
    let removed: Vec<&str> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .filter_map(|(k, _)| k.to_str())
        .collect();
    assert!(
        !removed.contains(&"GH_TOKEN"),
        "an unbound spawn must inherit GH_TOKEN: {removed:?}"
    );
}

// ---------------------------------------------------------------------------
// Byte measurement
// ---------------------------------------------------------------------------

#[test]
fn measure_bytes_counts_file_contents() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("a"), vec![0u8; 100]).expect("write a");
    std::fs::create_dir(tmp.path().join("sub")).expect("mkdir");
    std::fs::write(tmp.path().join("sub").join("b"), vec![0u8; 50]).expect("write b");
    assert_eq!(measure_bytes_until(tmp.path(), None), Some(150));
}

#[test]
fn measure_bytes_of_missing_path_is_none() {
    assert_eq!(
        measure_bytes_until(Path::new("/nonexistent-2919"), None),
        None
    );
}

#[test]
fn measure_bytes_stops_at_an_expired_deadline() {
    // The walk must bound ITSELF. `tokio::time::timeout` cannot cancel a
    // `spawn_blocking` task, so an outer timeout leaves the walk running and
    // runtime shutdown waits for it — measured as a >20-minute hang against a
    // real ~1 TiB worktree store while building this change.
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("a"), vec![0u8; 100]).expect("write a");
    let expired = std::time::Instant::now() - std::time::Duration::from_secs(1);
    assert_eq!(
        measure_bytes_until(tmp.path(), Some(expired)),
        None,
        "an expired deadline must abandon the walk, not measure it"
    );
    // A deadline comfortably in the future still measures normally.
    let ample = std::time::Instant::now() + std::time::Duration::from_secs(60);
    assert_eq!(measure_bytes_until(tmp.path(), Some(ample)), Some(100));
}

#[test]
fn classify_blocks_a_worktree_trusty_mpm_does_not_own() {
    // #2919 HIGH: `remove_session_worktree` refuses any path carrying none of
    // the ownership marks, so classifying one as `Reclaimable` made `tm doctor`
    // advertise a command that then failed and left the directory on disk. The
    // classifier applies exactly the predicate the remover applies.
    //
    // #6561 moved the harness `.claude/worktrees/` store OUT of this case — it
    // is now tier 3 of `removal_permitted` — so the unowned shape this asserts
    // on is a worktree parked somewhere with no sentinel at all.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree_at(&fx.repo.join("elsewhere"), "unowned-2919");
    let v = classify_no_agent(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(9),
        &clean,
    );
    assert!(!v.is_reclaimable(), "{v:?}");
    assert!(
        reason(&v).contains("not a trusty-mpm-removable worktree"),
        "{}",
        reason(&v)
    );
}

#[test]
fn the_remover_really_refuses_what_tm_provisioned_rejects() {
    // Ties the classifier's predicate to the REMOVER's actual behaviour rather
    // than to a copy of its rules. Without this the two can drift silently and
    // the classifier goes back to advertising worktrees that cannot be removed.
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree_at(&fx.repo.join("elsewhere"), "remover-refuses-2919");
    assert!(
        !tm_provisioned(&path),
        "precondition: the classifier rejects this shape"
    );
    let removed = crate::session_manager::decommission::remove_session_worktree(&path, "test");
    assert!(!removed.removed(), "the remover must refuse it too");
    assert!(
        path.exists(),
        "and must leave the directory on disk — which is exactly why \
         classifying it Reclaimable produced a `removal_failed` report"
    );
}

#[test]
fn tm_provisioned_matches_the_removers_own_predicate() {
    // Every tier `remove_session_worktree` accepts must be accepted here, or
    // the classifier and the remover disagree in the OTHER direction and real
    // reclaimable worktrees are silently skipped forever.
    let fx = GitWorktreeFixture::new();
    let under_worktrees = fx.add_worktree("convention-2919");
    assert!(
        tm_provisioned(&under_worktrees),
        "the `.worktrees/<name>` convention must be accepted"
    );

    let parked = fx.add_worktree_at(&fx.repo.join("elsewhere"), "sentinel-2919");
    assert!(
        !tm_provisioned(&parked),
        "no sentinel, not under .worktrees/, not in the harness store"
    );
    GitWorktreeFixture::stamp_reclaimable_sentinel(&parked);
    assert!(
        tm_provisioned(&parked),
        "an ownership sentinel must be accepted wherever the worktree is parked"
    );

    // #6561 tier 3: the harness's own `isolation: "worktree"` store, which
    // `agent_worktree_reap` already removes from on every agent exit.
    let agent_store = fx.add_worktree_at(
        &fx.repo.join(".claude").join("worktrees"),
        "agent-aa049179cd3e4e1bb",
    );
    assert!(
        tm_provisioned(&agent_store),
        "a `.claude/worktrees/agent-*` leaf must be accepted — it is the dominant \
         creation path in a tm-orchestrated session"
    );
}

/// The #6556 critic round, HIGH 3. `removal_permitted`'s third tier lets an
/// agent-store path reach gate 4, and the first cut of #6561 then let a
/// SENTINEL-LESS one straight through it — leaving merged-PR and clean-tree as
/// the only gates in front of a delete. That is the routine window the critic
/// named: a `version-control` agent squash-merges while the dispatched agent is
/// still finishing, so the tree is merged and clean and the agent is still in
/// it. The sentinel is written only after `PostToolUse` teaches an `agent_id`,
/// so #6556's own lost-`PostToolUse` population lands here with no attribution
/// at all.
///
/// #7771 superseded the #6561 refusal: a hand-made tree (`git worktree add`,
/// no owner file) that is merged, clean and unlocked, with no process in it,
/// is reclaimed — the shape of `semver-accept-trusty-mpm-1.7.2`.
///
/// Fails before #7771: refused with "names no owner inside the harness
/// agent-worktree store".
#[test]
fn worktree_7771_a_hand_made_tree_is_reclaimed() {
    let fx = GitWorktreeFixture::new();
    let path = agent_store_worktree(&fx, "semver-accept-7771");
    let v = classify_no_agent(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(759),
        &inspect_dirt,
    );
    assert_eq!(v, ReclaimVerdict::Reclaimable { pr: 759 });
}

/// #7771: the same hand-made tree holding a commit no remote has is kept.
#[test]
fn worktree_7771_a_hand_made_tree_with_an_unpushed_commit_is_kept() {
    let fx = GitWorktreeFixture::new();
    let path = agent_store_worktree(&fx, "handmade-7771unpushed");
    std::fs::write(path.join("novel.txt"), "unsaved\n").expect("write");
    for args in [
        &["add", "novel.txt"][..],
        &["commit", "-q", "-m", "novel"][..],
    ] {
        let ok = std::process::Command::new("git")
            .current_dir(&path)
            .args(args)
            .status()
            .expect("git");
        assert!(ok.success());
    }
    let v = classify_no_agent(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(759),
        &inspect_dirt,
    );
    assert!(!v.is_reclaimable(), "{v:?}");
    assert!(reason(&v).contains("unpushed"), "{}", reason(&v));
}

/// #7771 (f): every commit on an origin ref, no pull request, clean, no owner
/// file — reclaimed. A detached HEAD at a published commit is the same case.
///
/// Fails against 576135c5a: refused with "no pull request found" and, for the
/// detached tree, "could not be determined".
#[test]
fn worktree_7771_a_published_tree_with_no_pr_is_reclaimed() {
    let fx = GitWorktreeFixture::new();
    let path = agent_store_worktree(&fx, "published-7771");
    let v = classify_no_agent(
        &path,
        Admission::Admitted,
        &claim(false),
        &BranchPrState::NoPr,
        &inspect_dirt,
    );
    assert!(v.is_reclaimable(), "{v:?}");
    let detached = agent_store_worktree(&fx, "published-7771-detached");
    detach(&detached);
    let v = classify_no_agent(
        &detached,
        Admission::Admitted,
        &claim(false),
        &BranchPrState::Unknown,
        &inspect_dirt,
    );
    assert!(v.is_reclaimable(), "{v:?}");
}

/// `git checkout --detach` in `path`.
fn detach(path: &Path) {
    let ok = std::process::Command::new("git")
        .current_dir(path)
        .args(["checkout", "-q", "--detach"])
        .status()
        .expect("git");
    assert!(ok.success());
}

/// #7771 (f): one commit on no origin ref keeps the tree.
///
/// Fails against 576135c5a: its published first half is refused there.
#[test]
fn worktree_7771_a_commit_on_no_origin_ref_is_kept() {
    let fx = GitWorktreeFixture::new();
    let path = agent_store_worktree(&fx, "unpublished-7771");
    // The published half first, so this test fails where (f) is absent.
    let before = classify_no_agent(
        &path,
        Admission::Admitted,
        &claim(false),
        &BranchPrState::NoPr,
        &inspect_dirt,
    );
    assert!(before.is_reclaimable(), "{before:?}");
    GitWorktreeFixture::commit_unpushed(&path);
    let v = classify_no_agent(
        &path,
        Admission::Admitted,
        &claim(false),
        &BranchPrState::NoPr,
        &inspect_dirt,
    );
    assert!(!v.is_reclaimable(), "{v:?}");
}

/// #7771 (f) never outranks (d): a published tree whose owner file names
/// another LIVE session is kept; the same tree once that session ended is
/// reclaimed.
///
/// Fails against 576135c5a: the ended-owner half is refused, no PR found.
#[test]
fn worktree_7771_a_published_tree_of_a_live_foreign_session_is_kept() {
    use crate::session_manager::worktree_reclaim_claim::ClaimLiveness;
    let fx = GitWorktreeFixture::new();
    let path = agent_store_worktree(&fx, "published-foreign-7771");
    let owner = GitWorktreeFixture::stamp_agent_sentinel(&path, "published-foreign-7771");
    let parent = owner.parent_session_id.0.to_string();
    let verdict = |liveness| {
        classify(
            &path,
            Admission::Admitted,
            &claim(false),
            &BranchPrState::NoPr,
            &inspect_dirt,
            &no_agents,
            &SessionOwners::observed([(parent.clone(), liveness)]),
            &KeepList::default(),
        )
    };
    let live = verdict(ClaimLiveness::Live);
    assert!(!live.is_reclaimable(), "{live:?}");
    assert!(reason(&live).contains("is live"), "{}", reason(&live));
    assert!(verdict(ClaimLiveness::SessionGone).is_reclaimable());
}

/// #7771 item 3: a detached HEAD holding unpublished work is refused without
/// blaming `gh`, and the refusal names the detached HEAD.
///
/// Fails against 1ed820971 (the text was fixed in 576135c5a).
#[test]
fn worktree_7771_a_detached_unknown_state_does_not_blame_gh() {
    let fx = GitWorktreeFixture::new();
    let path = agent_store_worktree(&fx, "detached-unpublished-7771");
    detach(&path);
    GitWorktreeFixture::commit_unpushed(&path);
    let v = classify_no_agent(
        &path,
        Admission::Admitted,
        &claim(false),
        &BranchPrState::Unknown,
        &clean,
    );
    assert!(!v.is_reclaimable(), "{v:?}");
    assert!(reason(&v).contains("detached HEAD"), "{}", reason(&v));
    assert!(!reason(&v).contains("gh"), "{}", reason(&v));
}

/// #7771 (a): a harness lock whose pid is gone, or reused, is released at
/// gate 1; a lock naming this running process is still held.
///
/// Fails before #7771: every harness lock refused at gate 1.
#[test]
fn worktree_7771_a_stale_harness_lock_is_released() {
    let fx = GitWorktreeFixture::new();
    let dead = agent_store_worktree(&fx, "agent-7771deadlock");
    let mut child = std::process::Command::new("true").spawn().expect("spawn");
    let gone = child.id();
    child.wait().expect("reap");
    fx.harness_lock_worktree_with_pid(&dead, "agent-7771deadlock", gone);
    let reused = agent_store_worktree(&fx, "agent-7771reused");
    fx.harness_lock_worktree_with_reason(
        &reused,
        &format!(
            "claude agent agent-7771reused (pid {} start Mon Sep  1 20:33:51 2025)",
            std::process::id()
        ),
    );
    for path in [&dead, &reused] {
        let v = classify_no_agent(
            path,
            Admission::HarnessAgentLock,
            &claim(false),
            &merged(761),
            &clean,
        );
        assert_eq!(
            v,
            ReclaimVerdict::Reclaimable { pr: 761 },
            "{}",
            path.display()
        );
    }
    let live = agent_store_worktree(&fx, "agent-7771livelock");
    fx.harness_lock_worktree(&live, "agent-7771livelock");
    let v = classify_no_agent(
        &live,
        Admission::HarnessAgentLock,
        &claim(false),
        &merged(761),
        &clean,
    );
    assert!(matches!(v, ReclaimVerdict::BlockedByAgent { .. }), "{v:?}");
}

/// #7771 (d), the cross-session scenario: an agent tree whose owner file names
/// ANOTHER live session is kept by prune; the caller's own and an ended
/// session's are reclaimed.
///
/// Fails before #7771: the released lock alone reclaimed all three.
#[test]
fn worktree_7771_the_owner_files_session_decides() {
    use crate::session_manager::worktree_reclaim_claim::ClaimLiveness;
    let fx = GitWorktreeFixture::new();
    let mut verdicts = Vec::new();
    for (name, liveness, caller) in [
        ("agent-7771other", ClaimLiveness::Live, false),
        ("agent-7771mine", ClaimLiveness::Live, true),
        ("agent-7771ended", ClaimLiveness::SessionGone, false),
    ] {
        let path = agent_store_worktree(&fx, name);
        let owner = GitWorktreeFixture::stamp_agent_sentinel(&path, name);
        let claude = owner.parent_session_id.0.to_string();
        let owners = SessionOwners::observed([("managed-x".to_string(), liveness)])
            .with_aliases([(claude, "managed-x".to_string())])
            .with_caller(caller.then(|| "managed-x".to_string()));
        verdicts.push(classify(
            &path,
            Admission::Admitted,
            &claim(false),
            &merged(762),
            &clean,
            &no_agents,
            &owners,
            &KeepList::default(),
        ));
    }
    assert!(!verdicts[0].is_reclaimable(), "{:?}", verdicts[0]);
    assert!(
        reason(&verdicts[0]).contains("is live"),
        "{}",
        reason(&verdicts[0])
    );
    assert_eq!(verdicts[1], ReclaimVerdict::Reclaimable { pr: 762 });
    assert_eq!(verdicts[2], ReclaimVerdict::Reclaimable { pr: 762 });
}

/// A merged, clean agent tree the HARNESS HAS RELEASED is reclaimable even
/// though the delegation registry never heard of its agent (#6561).
///
/// This is the issue's headline case. The registry is a `DashMap` rebuilt empty
/// at every daemon boot, so on a restarted daemon it answers `Unknown` for every
/// agent — which refused every `.claude/worktrees/agent-*` tree in the store and
/// produced the reported `0 of 0 measured`. Git's lock is the durable second
/// source: the harness holds it for the agent's life and releases it when the
/// agent ends, and no daemon writes or clears it.
///
/// Fails before #6561: `classify` refuses with "the delegation registry holds no
/// record of that agent".
#[test]
fn classify_allows_a_merged_agent_tree_the_harness_released() {
    let fx = GitWorktreeFixture::new();
    let path = agent_store_worktree(&fx, "agent-6561released");
    let owner = GitWorktreeFixture::stamp_agent_sentinel(&path, "agent-6561released");
    // No `harness_lock_worktree` call: the harness released this tree when its
    // agent ended, which is what `git worktree list` now reports. #7771 (d):
    // its dispatching session is provably ended.
    let v = classify(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(759),
        &clean,
        &no_agents,
        &GitWorktreeFixture::parent_ended(&owner),
        &KeepList::default(),
    );
    assert_eq!(
        v,
        ReclaimVerdict::Reclaimable { pr: 759 },
        "a released, attributed, merged, clean agent tree must be reclaimable"
    );
}

/// Git being unaskable is still undeterminable, never released (#6561).
///
/// The permit above rests entirely on `Released` being POSITIVE evidence. Its
/// `Held` complement is `classify_blocks_an_agent_the_harness_still_holds_after_a_restart`;
/// this is the third arm, where git answers nothing at all.
#[test]
fn classify_blocks_an_agent_tree_git_cannot_be_asked_about() {
    let fx = GitWorktreeFixture::new();
    let path = agent_store_worktree(&fx, "agent-6561unaskable");
    GitWorktreeFixture::stamp_agent_sentinel(&path, "agent-6561unaskable");
    let _restore = deny_all(&path.join(".git"));
    let v = classify_no_agent(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(759),
        &clean,
    );
    assert!(!v.is_reclaimable(), "two silences are not an answer: {v:?}");
}

/// A harness-locked agent tree reaches gate 1 as its own verdict KIND, so the
/// survey can tell the operator it spared an agent (#6561).
///
/// Fails before #6561: the verdict is a plain `Blocked` naming the operator's
/// `git worktree lock`, so `ReclaimSurvey::agent_owned` stays empty and the run
/// prints `0 of 0` with no mention of the agent it protected.
#[test]
fn classify_discloses_a_harness_locked_agent_tree_as_agent_owned() {
    let v = classify_no_agent(
        &wt(),
        Admission::HarnessAgentLock,
        &claim(false),
        &merged(760),
        &clean,
    );
    assert!(
        matches!(v, ReclaimVerdict::BlockedByAgent { .. }),
        "a harness lock must be disclosed as an agent refusal, not an operator one: {v:?}"
    );
    // #7771: the lock's own verdict rides after the admission reason.
    assert!(reason(&v).starts_with(Admission::HarnessAgentLock.reason()));
}

/// The #4091 dirty-work guard, asserted on the agent store specifically: unsaved
/// work outranks a merged PR there as anywhere. This assertion must never be
/// relaxed.
#[test]
fn a_dirty_agent_store_worktree_is_still_refused() {
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree_at(
        &fx.repo.join(".claude").join("worktrees"),
        "agent-6561dirty",
    );
    let v = classify_no_agent(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(759),
        &dirty,
    );
    assert!(
        !v.is_reclaimable(),
        "unsaved work outranks a merged PR, in the harness store as anywhere: {v:?}"
    );
}

/// The #5661 refusal for the spelling it was written against — a sentinel FILE
/// that exists but does not parse. #7771 reclaims a tree with NO owner file,
/// so this pins the other spelling to a refusal: the strict reader (#8511)
/// keeps it with no harness lock in play.
///
/// Fails before the strict-reader switch: reclaimed as `Reclaimable { pr: 759 }`.
#[test]
fn an_unreadable_agent_sentinel_still_blocks_an_agent_store_worktree() {
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree_at(
        &fx.repo.join(".claude").join("worktrees"),
        "agent-6561garbage",
    );
    crate::session_manager::worktree_ownership_location::write_sentinel_bytes(&path, b"{not json")
        .expect("write garbage sentinel");
    let v = classify_no_agent(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(759),
        &clean,
    );
    assert!(!v.is_reclaimable(), "{v:?}");
    assert!(
        reason(&v).contains("does not parse"),
        "the refusal names the unparsable owner file: {}",
        reason(&v)
    );
}

#[test]
fn pr_index_skips_fork_pull_requests() {
    // `headRefName` is not an identity. A fork's `fix/foo` is a different
    // branch from this repository's `fix/foo`; attributing the fork's merge to
    // a local worktree would authorise deleting work that never landed here.
    let rows = r#"[{"number": 1, "headRefName": "feat/x", "state": "MERGED",
                    "isCrossRepository": true}]"#;
    let idx = PrIndex::from_json(rows, 400);
    assert_eq!(
        idx.state_for(Some("feat/x")),
        BranchPrState::NoPr,
        "a fork PR must not make a local branch look merged"
    );
    assert!(
        !classify_no_agent(
            &wt(),
            Admission::Admitted,
            &claim(false),
            &idx.state_for(Some("feat/x")),
            &clean
        )
        .is_reclaimable(),
        "and it must not authorise a delete"
    );
}

#[test]
fn pr_index_keeps_a_same_repo_pull_request() {
    // The complement: the skip must be scoped to fork rows only, or every PR
    // is discarded and nothing is ever reclaimable.
    let rows = r#"[{"number": 1, "headRefName": "feat/x", "state": "MERGED",
                    "isCrossRepository": false}]"#;
    assert_eq!(
        PrIndex::from_json(rows, 400).state_for(Some("feat/x")),
        BranchPrState::Merged { pr: 1 }
    );
}

#[test]
fn run_with_timeout_captures_output() {
    let mut cmd = std::process::Command::new("echo");
    cmd.arg("hello");
    let out = run_with_timeout(cmd, std::time::Duration::from_secs(5))
        .expect("a fast command must complete");
    assert_eq!(out.trim(), "hello");
}

/// 🔴 #6561 REGRESSION: a failing `gh` must hand back its exit code and its own
/// first stderr line, not a bare "it did not work".
///
/// Why: that is the fact which identifies the fix. `exited 4: … gh auth login`
/// sends the operator to the daemon's credentials; `gh: command not found`
/// sends them to PATH. Before this both were the same `None`.
#[test]
fn run_with_timeout_reports_the_exit_code_and_stderr() {
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", "echo 'gh auth login' >&2; exit 4"]);
    let err = run_with_timeout(cmd, std::time::Duration::from_secs(5))
        .expect_err("a non-zero exit must be an error")
        .to_string();
    assert!(err.contains("exited 4"), "{err}");
    assert!(err.contains("gh auth login"), "{err}");
}

/// A binary that is not on PATH is its own reason, not a silent unknown (#6561).
#[test]
fn run_with_timeout_reports_a_spawn_failure() {
    let cmd = std::process::Command::new("definitely-not-a-real-binary-6561");
    let err = run_with_timeout(cmd, std::time::Duration::from_secs(5))
        .expect_err("an unspawnable command must be an error");
    assert!(err.to_string().contains("could not be run"), "{err}");
    assert!(
        !err.timed_out(),
        "a spawn failure is not a hang and must not count toward the backoff (#6867)"
    );
}

#[test]
fn run_with_timeout_kills_a_hung_child() {
    // `Command::output()` has no timeout, so a wedged `gh` would hang the
    // blocking task forever — the same uncancellable-`spawn_blocking` shape
    // already fixed for the byte walk.
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("30");
    let started = std::time::Instant::now();
    let result = run_with_timeout(cmd, std::time::Duration::from_millis(300));
    assert!(result.is_err(), "a timed-out child must report failure");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the timeout must actually fire; took {:?}",
        started.elapsed()
    );
}

/// 🔴 #6867 REGRESSION: a timeout is FLAGGED as one, so the backoff can count
/// it apart from an exit that answered instantly.
///
/// Why: the alternative — matching the reason string — makes a reworded
/// message silently disable the backoff, which is the guard that stops the
/// leak.
#[test]
fn run_with_timeout_marks_a_timeout_as_timed_out() {
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("30");
    let err = run_with_timeout(cmd, std::time::Duration::from_millis(200))
        .expect_err("a hung child must fail");
    assert!(err.timed_out(), "{err}");
    assert!(err.to_string().contains("process group"), "{err}");
}

/// 🔴 #6867 REGRESSION: killing the child must reap its GRANDCHILDREN too.
///
/// Why: this is the leak. `gh` runs `/usr/bin/security find-generic-password`
/// for its token; with `securityd` wedged that grandchild never returns, and
/// `child.kill()` — which signals one pid — left it running, reparented to
/// launchd. Roughly 200 orphan pairs accumulated on the reporting host in a
/// couple of hours.
///
/// The stand-in is a shell that backgrounds a long `sleep` and then blocks,
/// which is the same shape: one process the runner knows about, one it does
/// not. On `origin/main` the backgrounded `sleep` is still alive after the
/// timeout and this assertion fails.
#[test]
fn run_with_timeout_kills_the_whole_process_group() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pidfile = tmp.path().join("grandchild.pid");
    // The grandchild's fds are redirected so it does not hold the runner's
    // stdout pipe open after its parent dies.
    let script = format!(
        "sleep 300 >/dev/null 2>&1 & echo $! > {}; sleep 300",
        pidfile.display()
    );
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", &script]);
    let err = run_with_timeout(cmd, std::time::Duration::from_millis(500))
        .expect_err("a hung child must time out");
    assert!(err.timed_out(), "{err}");

    let pid: i32 = std::fs::read_to_string(&pidfile)
        .expect("the shell must have recorded its background child's pid")
        .trim()
        .parse()
        .expect("a pid");

    // The grandchild is reparented to launchd/init on its parent's death, so a
    // killed one is reaped promptly and `kill(pid, 0)` starts reporting ESRCH.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while alive(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let still_there = alive(pid);
    if still_there {
        // Never leave a 300-second sleep behind, whatever the verdict.
        // SAFETY: `pid` was read from a child this test itself started.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    assert!(
        !still_there,
        "the grandchild (pid {pid}) survived its parent's timeout — the process group \
         was not killed (#6867)"
    );
}

/// Is `pid` still a live process?
///
/// SAFETY: `kill(pid, 0)` sends no signal; it only probes for the process's
/// existence and permission to signal it.
#[cfg(unix)]
fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A `gh` failure renders the identity it resolved, so "used the wrong config
/// dir" reads differently from "used none at all" (#6623, #6867).
#[test]
fn gh_failure_displays_the_resolved_identity() {
    let bare = GhFailure::new("`gh` exited 4: auth");
    assert_eq!(bare.to_string(), "`gh` exited 4: auth");
    assert!(
        bare.identity().contains("never spawned"),
        "{}",
        bare.identity()
    );

    let attributed = GhFailure::new("`gh` exited 4: auth").with_identity("GH_CONFIG_DIR=/cfg");
    assert_eq!(
        attributed.to_string(),
        "`gh` exited 4: auth (resolved gh identity: GH_CONFIG_DIR=/cfg)"
    );
    assert_eq!(attributed.identity(), "GH_CONFIG_DIR=/cfg");
}

/// 🔴 #6867: an unbound daemon `gh` is the one that consults the keychain on
/// every poll, and the operator has to be told which case they are in.
#[test]
fn ambient_gh_env_is_reported_as_keychain_bound() {
    assert!(consults_the_keychain(
        &crate::core::gh_identity::GhEnv::default()
    ));
}

#[test]
fn a_config_dir_binding_avoids_the_keychain() {
    let env = crate::core::gh_identity::resolve_gh_env(Some(
        &crate::core::trusty_tools_config::GithubConfig {
            config_dir: Some(PathBuf::from("/cfg/daemon")),
            ..Default::default()
        },
    ))
    .expect("ok");
    assert!(!consults_the_keychain(&env));
}

/// The warning has to name what to configure, or it is just a complaint.
#[test]
fn the_keychain_warning_names_both_bindings() {
    let w = keychain_warning();
    assert!(w.contains("token_env"), "{w}");
    assert!(w.contains("config_dir"), "{w}");
    assert!(w.contains("securityd"), "{w}");
}

/// 🔴 #6561 REGRESSION: a failed per-branch lookup reports the FAILURE, not a
/// cause-free `Unknown`.
///
/// This is the assertion that fails on `origin/main`, where every failure mapped
/// to `BranchPrState::Unknown` and the survey then counted it in
/// `pr_state_unknown` alongside genuine detached HEADs.
#[test]
fn pr_state_for_branch_reports_a_failed_call_as_lookup_failed() {
    // Any `gh` failure must block, not read as "this branch has no PR".
    //
    // NOTE on what this does and does not prove: an earlier version of this
    // comment claimed the failure came from `gh` being unable to resolve a
    // repository here. That was wrong, and the wrongness mattered — while the
    // module passed a bogus `-C` flag, `gh` died at FLAG PARSING and this test
    // was green for that reason instead, so it would have passed identically
    // against a perfectly valid repository. It pins the failure-to-`Unknown`
    // mapping only. `gh_command_passes_no_dash_c_flag` pins the argv, and
    // `pr_index_from_gh_reads_this_repository` proves a real call succeeds.
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = pr_state_for_branch(tmp.path(), "feat/whatever");
    assert!(
        matches!(state, BranchPrState::LookupFailed { .. }),
        "a broken lookup must be distinguishable from an unanswerable one; got {state:?}"
    );
}

/// 🔴 #6561: a SQUASH-MERGED pull request whose head branch was deleted at merge
/// still classifies as merged.
///
/// Why: the three worktrees the issue names (PRs #6604, #6588, #6573) all sit on
/// branches deleted from the remote by `--delete-branch`. The pull request keeps
/// the `headRefName` it was opened from, so `gh pr list --head <branch>
/// --state all` still answers `MERGED` — the deleted branch was never why they
/// read unknown. This pins that, so a later change to the `--json` field set
/// cannot quietly drop `headRefName` and break the only path that resolves them.
#[test]
fn pr_index_resolves_a_squash_merged_pr_whose_head_branch_was_deleted() {
    // The literal reply `gh pr list --head fix/6550-index-id-guard --state all`
    // returns today, head branch already deleted on the remote.
    let reply = r#"[{"headRefName":"fix/6550-index-id-guard","isCrossRepository":false,
"number":6573,"state":"MERGED"}]"#;
    let idx = PrIndex::from_json(reply, 50);
    assert_eq!(
        idx.state_for(Some("fix/6550-index-id-guard")),
        BranchPrState::Merged { pr: 6573 }
    );
}

/// 🔴 #6561: gate 5 refuses a failed lookup and NAMES the reason.
///
/// The refusal itself is unchanged (ADR-0045: an undeterminable tree stays
/// unreclaimable). What changed is that the operator can read why.
#[test]
fn classify_blocks_a_failed_lookup_and_names_the_reason() {
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("lookup-failed-6561");
    land(&path);
    let verdict = classify(
        &path,
        Admission::Admitted,
        &claim(false),
        &BranchPrState::LookupFailed {
            reason: "`gh` exited 4: gh auth login".to_string(),
        },
        &clean,
        &no_agents,
        &SessionOwners::default(),
        &KeepList::default(),
    );
    assert!(!verdict.is_reclaimable());
    let ReclaimVerdict::Blocked { reason, .. } = verdict else {
        panic!("a failed lookup is an ordinary block, not an agent refusal");
    };
    assert!(reason.contains("lookup failed"), "{reason}");
    assert!(reason.contains("gh auth login"), "{reason}");
}

/// `gh` has no `-C` flag, and passing one made every call in this module fail
/// at flag parsing — silently, because a failed call blocks rather than errors.
///
/// Why this is an argv test and not a behaviour test: the behaviour of the bug
/// was "nothing is ever reclaimable", which is indistinguishable from a correct
/// run against a workspace with no merged-PR worktrees. Only the argv
/// distinguishes them.
#[test]
fn gh_command_passes_no_dash_c_flag() {
    let cmd = gh_command(
        Path::new("/tmp"),
        &crate::core::gh_identity::GhEnv::default(),
    );
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        !args.iter().any(|a| a == "-C"),
        "`gh` has no -C flag (verified against gh 2.96.0: `unknown shorthand \
         flag: 'C' in -C`); argv was {args:?}"
    );
    assert!(
        args.is_empty(),
        "gh_command must contribute no positional args of its own; got {args:?}"
    );
}

#[test]
fn gh_command_runs_in_the_requested_directory() {
    // #7057: the working directory is no longer the repository SELECTION —
    // `gh_pr_list_command` states that with `--repo` — but it still decides
    // which `github:` binding `resolve_daemon_gh_env` picks, so an unset cwd
    // still authenticates as whoever the daemon happens to be.
    let cmd = gh_command(
        Path::new("/tmp"),
        &crate::core::gh_identity::GhEnv::default(),
    );
    assert_eq!(cmd.get_current_dir(), Some(Path::new("/tmp")));
}

/// Prove a real `gh` call SUCCEEDS and parses — the coverage whose absence let
/// the `-C` bug ship.
///
/// Skips (rather than fails) unless `gh` resolves this exact upstream
/// repository. A contributor working on a FORK resolves their own repo, which
/// legitimately has zero pull requests, and an assertion there would go red for
/// a reason that has nothing to do with this module — and a test that is red on
/// a fork gets deleted by whoever hits it. Gating on the repository identity,
/// rather than on "did we get rows", keeps the assertion meaningful wherever it
/// DOES run.
///
/// #7038: the identity probe is not the only live call here — `from_gh` makes a
/// second one, and a transient failure THERE returns an empty index, which the
/// row assertion read as the `-C` bug. `PrIndex` records why a lookup failed, so
/// the failed call is now skipped on the same terms as the probe. What still
/// fails is a call that ANSWERED and answered nothing, which is the bug shape.
#[test]
fn pr_index_from_gh_reads_this_repository() {
    const UPSTREAM: &str = "bobmatnyc/trusty-tools";
    if which::which("gh").is_err() {
        eprintln!("skipping: `gh` not on PATH");
        return;
    }
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let mut probe = std::process::Command::new("gh");
    probe.current_dir(repo_root).args([
        "repo",
        "view",
        "--json",
        "nameWithOwner",
        "-q",
        ".nameWithOwner",
    ]);
    let resolved = match run_with_timeout(probe, std::time::Duration::from_secs(10)) {
        Ok(out) => out.trim().to_string(),
        Err(reason) => {
            eprintln!("skipping: `gh` cannot resolve a repository here — {reason}");
            return;
        }
    };
    if resolved != UPSTREAM {
        eprintln!("skipping: resolved {resolved:?}, not the upstream {UPSTREAM:?}");
        return;
    }
    let index = PrIndex::from_gh(repo_root);
    // #7038: a branch this repository cannot have resolves to `LookupFailed`
    // only when the `gh pr list` inside `from_gh` itself failed — a call that
    // answered reports `NoPr` or `Unknown` instead.
    const ABSENT: &str = "trusty-tools-7038/no-such-branch";
    if let BranchPrState::LookupFailed { reason } = index.state_for(Some(ABSENT)) {
        eprintln!("skipping: `gh pr list` failed — {reason}");
        return;
    }
    assert!(
        index.branch_count() > 0,
        "a successful `gh pr list` against {UPSTREAM} must yield branches; an empty \
         index here means the call failed and every branch will block — which is \
         exactly what the `gh -C` argv bug produced"
    );
}

/// #6927 GATE 0: the operator's keep-list refuses a worktree every other gate
/// would pass, and the refusal names gate 0 plus the operator's own entry.
///
/// Why this shape: a keep-listed worktree must stay VISIBLE in the survey as
/// `Blocked`, not be filtered out of it — DOC-73 §16.2. Deleting the gate makes
/// this test fail with `Reclaimable`.
#[test]
fn classify_blocks_a_keep_listed_worktree() {
    let keeps = KeepList::from_patterns(&["/tmp/.worktrees".to_string()]);
    let v = classify(
        &wt(),
        Admission::Admitted,
        &ClaimState::Unclaimed,
        &merged(42),
        &clean,
        &no_agents,
        &SessionOwners::default(),
        &keeps,
    );
    assert!(
        matches!(
            &v,
            ReclaimVerdict::Blocked {
                gate: ReclaimGate::KeepList,
                ..
            }
        ),
        "a keep-listed worktree must be blocked at gate 0: {v:?}"
    );
    let reason = reason(&v);
    assert!(
        reason.contains("/tmp/.worktrees"),
        "the refusal must name the operator's own entry: {reason}"
    );
}

/// #6927: gate 0 outranks every gate below it, so the merged, clean, unclaimed
/// worktree that IS reclaimable without a keep-list stops being reclaimable
/// with one. The CONTROL half is what proves the gate, not the fixture.
#[test]
fn classify_keep_list_outranks_a_merged_clean_worktree() {
    let control = classify(
        &wt(),
        Admission::Admitted,
        &ClaimState::Unclaimed,
        &merged(42),
        &clean,
        &no_agents,
        &SessionOwners::default(),
        &KeepList::default(),
    );
    assert_eq!(
        control,
        ReclaimVerdict::Reclaimable { pr: 42 },
        "CONTROL: without a keep-list this worktree must be reclaimable"
    );

    let kept = classify(
        &wt(),
        Admission::Admitted,
        &ClaimState::Unclaimed,
        &merged(42),
        &clean,
        &no_agents,
        &SessionOwners::default(),
        &KeepList::from_patterns(&["**/worktree-2919".to_string()]),
    );
    assert!(
        !kept.is_reclaimable(),
        "a keep-list glob must outrank a merged, clean, unclaimed worktree: {kept:?}"
    );
}

/// 🔴 #7652: an owner map nobody read refuses a session-owned worktree.
///
/// Why: the survey falls back to an empty claim set when the store cannot be
/// read. That fallback has to refuse at gate 4b, the same tree the test above
/// reclaims with a read map. Fails against a version where an unread map
/// permits.
#[test]
fn worktree_7652_an_unread_owner_map_refuses() {
    let fx = GitWorktreeFixture::new();
    let path = fx.add_worktree("session-owned-unread-7652");
    land(&path);
    GitWorktreeFixture::stamp_reclaimable_sentinel(&path);
    let v = classify(
        &path,
        Admission::Admitted,
        &claim(false),
        &merged(107),
        &inspect_dirt,
        &no_agents,
        &SessionOwners::default(),
        &KeepList::default(),
    );
    assert!(
        matches!(
            v,
            ReclaimVerdict::Blocked {
                gate: ReclaimGate::SessionOwnership,
                ..
            }
        ),
        "{v:?}"
    );
    assert!(reason(&v).contains("was not read"), "{}", reason(&v));
}

/// 🔴 #7652 critic round: a harness agent-store tree whose sentinel names an
/// ENDED session is refused while git still reports the harness's agent-lifetime
/// lock on it.
///
/// Why: agent-store sentinels can still name the parent session (#7958), so the
/// session's death says nothing about the agent working in the tree. The lock
/// does. Gate 1 reads the lock at scan time only, so gate 4b must read it too.
/// Fails on `e21a6ba0b`, where gate 4b judged the ended session alone.
#[test]
fn worktree_7652_a_held_harness_lock_outranks_an_ended_sentinel_session() {
    let fx = GitWorktreeFixture::new();
    let owners = GitWorktreeFixture::reclaimable_owner_gone();

    let held = agent_store_worktree(&fx, "agent-7652held");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&held);
    fx.harness_lock_worktree(&held, "agent-7652held");
    // #7771: gate 4 now owns every agent-store tree, the lock included.
    let refusal = agent_ownership_blocks(&held, &no_agents, &owners)
        .expect("a held harness lock must refuse although the sentinel's session ended");
    assert!(refusal.contains("agent-lifetime lock"), "{refusal}");
    // Through `classify` too, with gate 1 admitting — a lock taken after the scan.
    let v = classify(
        &held,
        Admission::Admitted,
        &claim(false),
        &merged(7652),
        &clean,
        &agent_ended,
        &owners,
        &KeepList::default(),
    );
    // #7771: refused at gate 4, the agent-store owner, as an agent refusal.
    assert!(
        matches!(
            v,
            ReclaimVerdict::BlockedByAgent {
                gate: ReclaimGate::AgentOwnership,
                ..
            }
        ),
        "{v:?}"
    );

    // The over-correction guard: once the harness releases the tree, the ended
    // session is the answer again.
    let released = agent_store_worktree(&fx, "agent-7652released");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&released);
    assert_eq!(
        agent_ownership_blocks(&released, &agent_ended, &owners),
        None
    );
}
