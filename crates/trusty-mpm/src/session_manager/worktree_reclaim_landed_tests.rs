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
//! tree with no pull request and refuses one that stopped being landed. The
//! #7771 (f) route counts a tree landed only when every commit is on some
//! origin ref, and refuses when that count fails.

use std::path::Path;

use super::reclaim_landed_content;
use crate::core::worktree_landed_content::LandedContent;
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
        !verdict.admits(),
        "an uncommitted file must refuse: {verdict:?}"
    );
    assert!(verdict.note().contains("uncommitted"), "{}", verdict.note());

    // A path git cannot answer for fails the dirty-check itself, and refuses.
    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(!reclaim_landed_content(tmp.path()).admits());
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

/// Run `git -C <dir> <args>`, panicking with git's stderr on failure.
fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("fixture: git could not be run");
    assert!(
        out.status.success(),
        "fixture: `git {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 🔴 REGRESSION (#7889, critic CRITICAL): a commit on `session/<leaf>` that
/// HEAD cannot reach is counted by gate 6 as an unpushed commit with no dirty
/// file, so it reached the admission — which judges HEAD only, found HEAD on
/// `origin/main`, and granted. The removal then deletes `session/<leaf>` and
/// with it the commit's last ref.
///
/// Fails at bc9df345a: the verdict there is `ReclaimableLandedContent`.
#[test]
fn worktree_7889_a_session_branch_commit_head_cannot_reach_refuses() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("moved-7889");
    GitWorktreeFixture::commit_unpushed(&wt);
    let orphan = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("rev-parse");
    let orphan = String::from_utf8_lossy(&orphan.stdout).trim().to_string();
    git(&wt, &["switch", "-c", "donor-moved", "origin/main"]);
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
        !found.verdict.is_reclaimable(),
        "a commit only session/moved-7889 holds must refuse: {:?}",
        found.verdict
    );
    // #7889 critic round 2: the refusal names the branch and the commit.
    let shown = format!("{:?}", found.verdict);
    assert!(shown.contains("`session/moved-7889`"), "{shown}");
    assert_eq!(orphan.len(), 40, "premise: a full sha: {orphan}");
    assert!(shown.contains(&orphan), "the first unlanded sha: {shown}");
}

/// 🔴 REGRESSION (#7889, critic MEDIUM): the admission can take 40 s, so a
/// grant is re-checked. A file written, or a commit made, while it ran refuses.
///
/// Fails at bc9df345a, which returned the admission's grant unexamined.
#[test]
fn worktree_7889_dirt_that_appears_during_the_admission_refuses() {
    let landed = || -> crate::core::worktree_landed_content::LandingAdmission {
        LandedContent::Landed {
            base: "origin/main".into(),
            base_sha: "0".repeat(40),
            landed_at: None,
        }
        .into()
    };

    let (_fx, wt) = donor("donor-late-file-7889");
    let late_file = super::reclaim_landed_content_with(&wt, &|p| {
        std::fs::write(p.join("late.txt"), "written mid-admission\n").expect("write");
        landed()
    });
    assert!(!late_file.admits(), "{late_file:?}");
    assert!(
        late_file.note().contains("appeared while"),
        "{}",
        late_file.note()
    );

    let (_fx2, wt2) = donor("donor-late-commit-7889");
    let late_commit = super::reclaim_landed_content_with(&wt2, &|p| {
        GitWorktreeFixture::commit_unpushed(p);
        landed()
    });
    assert!(!late_commit.admits(), "{late_commit:?}");
    assert!(
        late_commit.note().contains("HEAD moved"),
        "{}",
        late_commit.note()
    );
}

/// 🔴 REGRESSION (#7889): a donor matched to its sibling's merged pull request
/// by the #7267 commit search still carries commits `inspect_dirt` counts as
/// unpushed. The re-check asks the admission about them rather than refusing.
///
/// Fails before the fix: a merge re-ran `inspect_dirt` alone, which refused
/// this tree on its donor commit, so the candidate was never removed.
#[test]
fn worktree_7889_the_recheck_admits_a_merged_donor_with_commits_only_dirt() {
    let (_fx, wt) = donor("donor-merged-7889");
    let merged = BranchPrState::Merged { pr: 8328 };
    assert_eq!(
        recheck_before_delete(
            &wt,
            &KeepList::default(),
            Some(&LiveClaims::default()),
            &merged,
            &no_agents,
            Some(&reclaim_landed_content),
        ),
        None
    );
    let unoffered = recheck_before_delete(
        &wt,
        &KeepList::default(),
        Some(&LiveClaims::default()),
        &merged,
        &no_agents,
        None,
    )
    .expect("no probe offered keeps the pre-#7889 refusal");
    assert!(unoffered.contains("unpushed"), "{unoffered}");
}

/// A tree under `.claude/worktrees/` holding one commit that `origin` carries
/// only under a DIFFERENT branch name (#7771).
fn published_elsewhere(fx: &GitWorktreeFixture, name: &str) -> std::path::PathBuf {
    let wt = fx.add_worktree_at(&fx.repo.join(".claude").join("worktrees"), name);
    std::fs::write(wt.join("published.txt"), "on origin\n").expect("write");
    git(&wt, &["add", "published.txt"]);
    git(&wt, &["commit", "-q", "-m", "published elsewhere"]);
    let refspec = format!("HEAD:refs/heads/vc/{name}");
    git(&wt, &["push", "-q", "origin", &refspec]);
    git(&wt, &["fetch", "-q", "origin"]);
    wt
}

/// 🔴 REGRESSION (#7771 f): a commit on ANY origin ref counts, and one commit
/// on no origin ref beside it makes the tree unlanded.
///
/// The second half uses a clean dirt probe, so only the origin count can
/// refuse: an implementation asking "is any commit on origin" passes the first
/// half and fails here.
#[test]
fn worktree_7771_published_needs_every_commit_on_some_origin_ref() {
    use super::{landing_recheck, published_verdict};
    use crate::session_manager::worktree_safety::inspect_dirt;
    let fx = GitWorktreeFixture::new();
    let wt = published_elsewhere(&fx, "published-elsewhere-7771");
    assert!(
        published_verdict(&wt, &inspect_dirt).is_some(),
        "every commit is on origin/vc/published-elsewhere-7771"
    );
    assert_eq!(landing_recheck(&wt, &BranchPrState::NoPr, None), None);

    GitWorktreeFixture::commit_unpushed(&wt);
    assert!(
        published_verdict(&wt, &|_| None).is_none(),
        "one commit on origin and one local-only is not landed"
    );
    assert!(landing_recheck(&wt, &BranchPrState::NoPr, None).is_some());
}

/// #7771 (f) error arm: when the origin count cannot run, the tree is not
/// published, so the caller's refusal stands (ADR-0045).
///
/// The tree's `HEAD` names an unborn branch, so `git rev-list HEAD` fails
/// inside a real worktree; a plain directory fails the same count outright.
#[test]
fn worktree_7771_a_failed_origin_count_refuses() {
    use super::{landing_recheck, published_verdict};
    use crate::core::worktree_removal_facts::LOCAL_ONLY_COMMITS_ARGS;
    use crate::session_manager::worktree_safety::git_stdout;
    let fx = GitWorktreeFixture::new();
    let wt = published_elsewhere(&fx, "count-fails-7771");
    assert!(published_verdict(&wt, &|_| None).is_some(), "precondition");
    git(&wt, &["symbolic-ref", "HEAD", "refs/heads/unborn-7771"]);
    assert!(
        git_stdout(&wt, LOCAL_ONLY_COMMITS_ARGS).is_err(),
        "the count must fail for this test to reach the error arm"
    );
    assert!(published_verdict(&wt, &|_| None).is_none());
    assert!(landing_recheck(&wt, &BranchPrState::Unknown, None).is_some());

    let plain = tempfile::tempdir().expect("tempdir");
    assert!(published_verdict(plain.path(), &|_| None).is_none());
}
