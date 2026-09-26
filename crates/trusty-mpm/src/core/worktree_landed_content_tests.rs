//! REAL-git coverage for the #7889 landed-content admission.
//!
//! Why: the whole admission rests on what `git merge-tree` and `git fetch`
//! actually do to a checkout whose `origin/main` is stale. A mocked git would
//! assert the mock. Every fixture here lives inside a `tempfile::TempDir` and
//! its "remote" is a bare repository in the same temp tree, so nothing reaches
//! the network and nothing touches a real worktree.
//!
//! What: the grant arm (the donor-branch shape #7889 is about), the residual
//! arm, and the two undeterminable arms — an unresolvable base and a refresh
//! that cannot run.

use std::time::Duration;

use super::{LANDED_CONTENT_CHECK, LandedContent, LandingAdmission, landed_content_verdict};
use crate::core::worktree_carried_by_pr::{CarriedByPr, MERGED_PR_ANCESTRY_CHECK};
use crate::core::worktree_landed_history::{ContentOnBase, content_on_base};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// The bound every test here runs the refresh under.
///
/// Why: the remote is a bare repository on the same filesystem, so a healthy
/// fetch is milliseconds; 30 s only has to be above the pathological case on a
/// loaded CI box.
const BOUND: Duration = Duration::from_secs(30);

/// 🔴 REGRESSION (#7889): the donor-branch shape. A worktree whose commit
/// reached `origin/main` through a SIBLING branch's squash — so no pull request
/// carries its own name and its `origin/main` is stale until it fetches — holds
/// nothing, and the admission says so.
///
/// Fails against any implementation that skips the refresh: before the fetch
/// this checkout's `origin/main` predates the landing, and the merge reports
/// the file as residue.
#[test]
fn a_divergence_landed_by_another_route_reports_landed() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("donor");
    fx.land_on_the_remote_only(&wt, "donor.txt");

    match landed_content_verdict(&wt, BOUND) {
        LandedContent::Landed { base, base_sha, .. } => {
            assert!(base.contains("main"), "the base must be named: {base}");
            assert_eq!(base_sha.len(), 40, "a full object id: {base_sha}");
        }
        other => panic!("a tree whose content is on origin/main must be landed: {other:?}"),
    }
}

/// 🔴 #7889, the refusing direction: a commit that reached no remote is
/// residue, and the verdict names the FIRST path so the refusal is actionable.
#[test]
fn an_unlanded_commit_reports_its_residual_path() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("unlanded");
    GitWorktreeFixture::commit_unpushed(&wt);

    match landed_content_verdict(&wt, BOUND) {
        LandedContent::Residual { first_path, .. } => {
            assert_eq!(first_path, "unpushed.txt", "the residue must be named");
        }
        other => panic!("work on no remote must not be reported landed: {other:?}"),
    }
}

/// 🔴 #7889, ADR-0045: a directory git cannot answer for establishes nothing.
/// A path that is not a repository at all resolves no landing base, and an
/// unresolved base is never a grant.
#[test]
fn a_worktree_with_no_resolvable_landing_base_is_unavailable() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let verdict = landed_content_verdict(tmp.path(), BOUND);
    assert!(
        !verdict.is_landed(),
        "a non-repository must never report landed: {verdict:?}"
    );
}

/// 🔴 #7889, ADR-0045 again, at the gate that matters most: a refresh that
/// cannot run leaves the remote-tracking refs stale, and a comparison against
/// stale refs is exactly the grant this admission must never make.
///
/// The tree's content IS on the remote, but this checkout's `origin/main` is
/// stale, so a comparison made anyway after a swallowed refresh error reports
/// `Residual`, which also refuses. Only the `Unavailable` variant proves the
/// failed refresh itself was what decided, so that is what is asserted.
#[test]
fn a_refresh_that_fails_never_reports_landed() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("stale");
    fx.land_on_the_remote_only(&wt, "stale.txt");
    // Point `origin` at a directory that does not exist, so the fetch fails
    // deterministically rather than depending on a timeout landing.
    let broken = fx.repos_root.join("no-such-remote.git");
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["remote", "set-url", "origin"])
        .arg(&broken)
        .output()
        .expect("fixture: git remote set-url");
    assert!(out.status.success(), "fixture: could not break the remote");

    let verdict = landed_content_verdict(&wt, BOUND);
    assert!(
        matches!(verdict, LandedContent::Unavailable { .. }),
        "a failed refresh must decide, not a comparison against stale refs: {verdict:?}"
    );
    assert!(
        verdict.note().contains("refreshed"),
        "the refusal must name the refresh: {}",
        verdict.note()
    );
}

/// Run `git -C <dir> <args>` and return trimmed stdout, panicking on failure.
fn git(dir: &std::path::Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// 🔴 REGRESSION (#7889, critic HIGH): a donor that only bumps a gitlink holds
/// work the remote lacks. `git diff` honours `diff.ignoreSubmodules=all` and
/// `submodule.<name>.ignore=all`, which hid the bump and read the tree as
/// landed. Both are set here, where a user's own config could set them.
///
/// Fails without `--ignore-submodules=none` on the residue diff.
#[test]
fn a_gitlink_bump_is_residue_even_when_submodule_diffs_are_ignored() {
    let fx = GitWorktreeFixture::new();
    let base = git(&fx.repo, &["rev-parse", "HEAD"]);
    git(
        &fx.repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{base},sub"),
        ],
    );
    git(&fx.repo, &["commit", "-m", "add gitlink sub"]);
    git(&fx.repo, &["push", "origin", "main"]);
    git(&fx.repo, &["fetch", "origin"]);

    let wt = fx.add_worktree("gitlink-bump");
    let bumped = "1111111111111111111111111111111111111111";
    git(
        &wt,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{bumped},sub"),
        ],
    );
    git(&wt, &["commit", "-m", "bump sub"]);
    git(&wt, &["config", "diff.ignoreSubmodules", "all"]);
    git(&wt, &["config", "submodule.sub.ignore", "all"]);

    match content_on_base(&wt, "origin/main") {
        Ok(ContentOnBase::Residual { paths, .. }) => {
            assert_eq!(
                paths,
                vec!["sub".to_string()],
                "the gitlink bump is residue"
            );
        }
        other => panic!("the merge must be answerable and leave residue: {other:?}"),
    }
    match landed_content_verdict(&wt, BOUND) {
        LandedContent::Residual { first_path, .. } => assert_eq!(first_path, "sub"),
        other => panic!("a gitlink bump must not read as landed: {other:?}"),
    }
}

/// #7889: `merge-tree` against a ref that does not resolve is an `Err`, not an
/// empty residue — an empty residue would read as "everything landed".
#[test]
fn merge_residue_against_an_unresolvable_base_is_an_error() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("no-base");
    assert!(
        content_on_base(&wt, "origin/does-not-exist").is_err(),
        "an unresolvable base must be an error"
    );
    assert!(
        content_on_base(&wt, "   ").is_err(),
        "an empty base must be an error"
    );
}

/// #7889: every arm's sentence names the admission, so an operator grepping one
/// ladder's refusal finds the other's.
#[test]
fn the_note_names_the_admission_in_every_arm() {
    let arms = [
        LandedContent::Landed {
            base: "origin/main".into(),
            base_sha: "abc123".into(),
            landed_at: None,
        },
        LandedContent::Landed {
            base: "origin/main".into(),
            base_sha: "abc123".into(),
            landed_at: Some("def456".into()),
        },
        LandedContent::Conflicted {
            base: "origin/main".into(),
            detail: "merging HEAD into origin/main conflicts in 1 file(s)".into(),
        },
        LandedContent::Residual {
            base: "origin/main".into(),
            first_path: "src/lib.rs".into(),
        },
        LandedContent::unavailable("git said no"),
    ];
    for arm in arms {
        assert!(
            arm.note().contains(LANDED_CONTENT_CHECK),
            "every arm names the admission: {}",
            arm.note()
        );
    }
    let residual = LandedContent::Residual {
        base: "origin/main".into(),
        first_path: "src/lib.rs".into(),
    };
    assert!(
        residual.note().contains("src/lib.rs"),
        "the residual arm names the path: {}",
        residual.note()
    );
}

/// 🔴 #7889: routes (b) and (c) combine — either admits, and a refusal names
/// BOTH predicates and (b)'s first residual path, so an operator sees every
/// route that was tried.
#[test]
fn admission_admits_on_either_route_and_names_both_refusals() {
    let residual = LandedContent::Residual {
        base: "origin/main".into(),
        first_path: "src/lib.rs".into(),
    };
    let not_carried = CarriedByPr::NotCarried {
        head: "1111111".into(),
    };

    // (b) alone: a fake that states only the content answer.
    let only_b = LandingAdmission::from(residual.clone());
    assert!(!only_b.admits());

    // (c) admits where (b) found residue.
    let carried = LandingAdmission {
        content: residual.clone(),
        carried: Some(CarriedByPr::Carried {
            pr: 8328,
            pr_head: "2222222".into(),
        }),
    };
    assert!(carried.admits());
    assert_eq!(carried.carried_pr(), Some(8328));
    assert!(
        carried.note().contains(MERGED_PR_ANCESTRY_CHECK),
        "{}",
        carried.note()
    );

    // Neither admits: the refusal names both routes and the residual path.
    let neither = LandingAdmission {
        content: residual,
        carried: Some(not_carried),
    };
    assert!(!neither.admits());
    assert_eq!(neither.carried_pr(), None);
    let note = neither.note();
    assert!(note.contains(LANDED_CONTENT_CHECK), "{note}");
    assert!(note.contains(MERGED_PR_ANCESTRY_CHECK), "{note}");
    assert!(note.contains("src/lib.rs"), "{note}");

    // (c) carrying a pull request with no head commit proves nothing.
    let headless = LandingAdmission {
        content: LandedContent::unavailable("x"),
        carried: Some(CarriedByPr::Carried {
            pr: 1,
            pr_head: "  ".into(),
        }),
    };
    assert!(!headless.admits());
}
