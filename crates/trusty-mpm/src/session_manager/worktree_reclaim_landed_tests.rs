//! REAL-git coverage for the sweep's #7889 landed-content admission.
//!
//! Why: the gate-ladder tests in `worktree_reclaim_tests` inject a clean dirt
//! probe, so they cannot see what `inspect_dirt` really reports for a donor
//! branch. These build that branch — its commit reaches no `origin` ref, and
//! the squash that landed it also carries a sibling's work — and run the real
//! probes over it. Every fixture lives in a `tempfile::TempDir`.
//!
//! What: the survey admits the donor tree; the sweep probe refuses a donor
//! tree holding an uncommitted file; the pre-delete re-check admits a landed
//! tree with no pull request and refuses one that stopped being landed.

use std::path::Path;

use super::reclaim_landed_content;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_ownership::{AgentDelegationState, AgentWorktreeOwner};
use crate::session_manager::worktree_reclaim::{
    BranchPrState, KeepList, LiveClaims, PrIndex, ReclaimVerdict,
};
use crate::session_manager::worktree_reclaim_sweep::{
    SurveyBudget, recheck_before_delete, survey_with_landed_content,
};

/// The strictest agent probe, as `worktree_reclaim_sweep_tests` uses it.
fn no_agents(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Unknown
}

/// A complete PR index that carries only an unrelated branch, so the donor
/// branch resolves to `NoPr` without a per-branch `gh` call.
fn unrelated_index(_: &Path) -> PrIndex {
    PrIndex::from_json(
        r#"[{"number": 72, "headRefName": "session/somebody-else", "state": "MERGED"}]"#,
        400,
    )
}

/// A worktree in the real #7889 donor shape, returned with its fixture.
fn donor(name: &str) -> (GitWorktreeFixture, std::path::PathBuf) {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree(name);
    fx.land_with_sibling_work(&wt, &format!("{name}.txt"), &format!("{name}-r2.txt"));
    (fx, wt)
}

/// 🔴 REGRESSION (#7889): the survey admits a real donor branch. Gate 6's
/// `inspect_dirt` counts its commit as unpushed, because no patch id on
/// `origin/main` matches it; the content comparison finds nothing to land.
///
/// Fails before the fix: `no_pr_verdict` refused on that commit count, so the
/// verdict was `Blocked { gate: UnsavedWork, .. }` and the admission never ran.
#[test]
fn worktree_7889_the_sweep_admits_a_real_donor_branch() {
    let (fx, wt) = donor("donor-7889");
    let s = survey_with_landed_content(
        &fx.repos_root,
        &LiveClaims::default(),
        &unrelated_index,
        &no_agents,
        SurveyBudget::default(),
        false,
        &KeepList::default(),
        &[],
        Some(&reclaim_landed_content),
    );
    let found = s
        .candidates
        .iter()
        .find(|c| c.path == wt)
        .unwrap_or_else(|| panic!("survey missed {}", wt.display()));
    assert!(
        matches!(
            found.verdict,
            ReclaimVerdict::ReclaimableLandedContent { .. }
        ),
        "a donor tree whose content is on origin/main must be reclaimable: {:?}",
        found.verdict
    );
}

/// 🔴 #7889: the probe runs the dirt checks `inspect_dirt` skipped. An
/// untracked file is not in HEAD, so no comparison of HEAD can vouch for it.
///
/// Fails without the probe's own dirty-file check: the content comparison
/// alone reports `Landed` for this tree.
#[test]
fn worktree_7889_the_sweep_probe_refuses_an_uncommitted_file() {
    let (_fx, wt) = donor("donor-dirty-7889");
    std::fs::write(wt.join("notes.md"), "never committed\n").expect("write untracked file");
    let verdict = reclaim_landed_content(&wt);
    assert!(
        !verdict.is_landed(),
        "an uncommitted file must refuse: {verdict:?}"
    );
    assert!(verdict.note().contains("uncommitted"), "{}", verdict.note());

    // A path git cannot answer for fails the dirty-check itself, and refuses.
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(!reclaim_landed_content(tmp.path()).is_landed());
}

/// 🔴 REGRESSION (#7889): the pre-delete re-check admits a landed tree with no
/// pull request, so an approved candidate can actually be removed.
///
/// Fails before the fix: the re-check refused every state but `Merged`, so
/// every candidate the survey admitted on content was refused here.
#[test]
fn worktree_7889_the_recheck_admits_a_landed_tree_with_no_pull_request() {
    let (_fx, wt) = donor("donor-recheck-7889");
    assert_eq!(
        recheck_before_delete(
            &wt,
            &KeepList::default(),
            Some(&LiveClaims::default()),
            &BranchPrState::NoPr,
            &no_agents,
            Some(&reclaim_landed_content),
        ),
        None
    );
}

/// 🔴 #7889: the re-check re-asks the evidence. A commit made after the survey
/// is residue, so the re-check refuses and names the admission. Offering no
/// probe keeps the pre-#7889 refusal.
#[test]
fn worktree_7889_the_recheck_refuses_a_tree_no_longer_landed() {
    let (_fx, wt) = donor("donor-residue-7889");
    GitWorktreeFixture::commit_unpushed(&wt);
    let reason = recheck_before_delete(
        &wt,
        &KeepList::default(),
        Some(&LiveClaims::default()),
        &BranchPrState::NoPr,
        &no_agents,
        Some(&reclaim_landed_content),
    )
    .expect("residue must refuse");
    assert!(reason.contains("landed-content"), "{reason}");
    assert!(reason.contains("unpushed.txt"), "{reason}");

    let unoffered = recheck_before_delete(
        &wt,
        &KeepList::default(),
        Some(&LiveClaims::default()),
        &BranchPrState::NoPr,
        &no_agents,
        None,
    )
    .expect("no probe offered must refuse");
    assert!(unoffered.contains("no longer a merge"), "{unoffered}");
}
