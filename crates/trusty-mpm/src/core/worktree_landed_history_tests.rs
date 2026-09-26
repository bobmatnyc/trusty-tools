//! REAL-git coverage for the #8633 squash-then-edited landed-content shape.
//!
//! Why: the defect lives in what `git merge-tree` prints and returns, so a
//! mocked git would assert the mock. Every fixture is a temp repository whose
//! "remote" is a bare repository beside it; nothing reaches the network.
//!
//! What: the grant (a squash-merged branch whose files `main` edited later),
//! the two fail-open checks (a different version of the change on `main`, and
//! a branch only partly landed), and a `merge-tree` git error.

use std::path::Path;
use std::time::Duration;

use super::{ContentOnBase, content_on_base};
use crate::core::worktree_landed_content::{LandedContent, landed_content_verdict};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// The refresh bound; the remote is a local bare repository.
const BOUND: Duration = Duration::from_secs(30);

/// Run `git -C <dir> <args>`, panicking on failure; returns trimmed stdout.
fn git(dir: &Path, args: &[&str]) -> String {
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

/// Write each `(file, content)` in `dir` and commit them as `msg`.
fn commit_files(dir: &Path, files: &[(&str, &str)], msg: &str) {
    for (file, content) in files {
        std::fs::write(dir.join(file), content).expect("fixture: write file");
        git(dir, &["add", file]);
    }
    git(dir, &["commit", "-m", msg]);
}

/// Squash-merge `branch` onto `main` in the owning checkout, as
/// `gh pr merge --squash` does, and return the squash commit.
fn squash_onto_main(fx: &GitWorktreeFixture, branch: &str) -> String {
    git(&fx.repo, &["merge", "--squash", branch]);
    git(&fx.repo, &["commit", "-m", "squash (#1)"]);
    git(&fx.repo, &["rev-parse", "HEAD"])
}

/// Later work on `main` that rewrites the same lines, then push it.
fn edit_over_on_main_and_push(fx: &GitWorktreeFixture, files: &[&str]) {
    let later: Vec<(&str, &str)> = files.iter().map(|f| (*f, "later main edit\n")).collect();
    commit_files(&fx.repo, &later, "later work on main");
    git(&fx.repo, &["push", "origin", "main"]);
}

/// 🔴 REGRESSION (#8633, #8602): a branch squash-merged into `main`, whose
/// files `main` edited afterwards, is LANDED. The tip merge conflicts in
/// every file — exit 1 with an EMPTY stderr, the "no output" refusal seen on
/// PR #8655's worktree — but the squash commit holds the content.
///
/// Fails against a probe that reads any non-zero `merge-tree` exit as a
/// failure (`Unavailable`) or that only compares against the base's tip.
#[test]
fn a_squash_merged_branch_edited_over_on_main_is_landed() {
    let fx = GitWorktreeFixture::new();
    commit_files(
        &fx.repo,
        &[
            ("a.txt", "base\n"),
            ("b.txt", "base\n"),
            ("c.txt", "base\n"),
        ],
        "seed",
    );
    git(&fx.repo, &["push", "origin", "main"]);
    let wt = fx.add_worktree("squashed");
    let files = ["a.txt", "b.txt", "c.txt"];
    commit_files(
        &wt,
        &[
            ("a.txt", "branch\n"),
            ("b.txt", "branch\n"),
            ("c.txt", "branch\n"),
        ],
        "branch work",
    );
    let squash = squash_onto_main(&fx, "session/squashed");
    edit_over_on_main_and_push(&fx, &files);
    git(&wt, &["fetch", "origin"]);

    // The exit-1-no-output shape, observed directly: a conflict is reported
    // on stdout with exit status 1 and nothing on stderr.
    let raw = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["merge-tree", "--write-tree", "origin/main", "HEAD"])
        .output()
        .expect("git merge-tree");
    assert_eq!(raw.status.code(), Some(1), "the tip merge must conflict");
    assert!(raw.stderr.is_empty(), "a conflict writes nothing to stderr");

    assert_eq!(
        content_on_base(&wt, "origin/main"),
        Ok(ContentOnBase::Landed {
            at: Some(squash.clone())
        })
    );
    match landed_content_verdict(&wt, BOUND) {
        v @ LandedContent::Landed { .. } => {
            assert!(v.note().contains(&squash), "names the squash: {}", v.note());
        }
        other => panic!("squash-merged content must read as landed: {other:?}"),
    }
}

/// 🔴 FAIL-OPEN CHECK (#8633): `main` squash-merged a DIFFERENT version of the
/// same change. Every commit on `main` conflicts with this branch, so it is
/// not landed, and the refusal names the conflicted file.
#[test]
fn a_different_version_of_the_change_on_main_is_not_landed() {
    let fx = GitWorktreeFixture::new();
    let other = fx.add_worktree("other-version");
    commit_files(&other, &[("README.md", "their version\n")], "theirs");
    let wt = fx.add_worktree("mine");
    commit_files(&wt, &[("README.md", "my version\n")], "mine");
    squash_onto_main(&fx, "session/other-version");
    edit_over_on_main_and_push(&fx, &["README.md"]);

    let verdict = landed_content_verdict(&wt, BOUND);
    assert!(
        !verdict.is_landed(),
        "unlanded work read as landed: {verdict:?}"
    );
    assert!(
        matches!(verdict, LandedContent::Conflicted { .. }),
        "a conflict is a verdict, not a failure: {verdict:?}"
    );
    let note = verdict.note();
    assert!(note.contains("conflicts in 1 file(s)"), "{note}");
    assert!(note.contains("`README.md`"), "{note}");
}

/// 🔴 FAIL-OPEN CHECK (#8633): the squash carried an EARLIER state of this
/// branch; a later commit here adds a file no commit on `main` holds. The
/// squash commit's merge is clean but leaves that file, so it is not landed.
#[test]
fn a_branch_only_partly_landed_is_not_landed() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("partial");
    commit_files(&wt, &[("README.md", "landed part\n")], "landed part");
    squash_onto_main(&fx, "session/partial");
    edit_over_on_main_and_push(&fx, &["README.md"]);
    commit_files(&wt, &[("unlanded.txt", "never pushed\n")], "unlanded part");

    match content_on_base(&wt, "origin/main") {
        Ok(ContentOnBase::Conflicted {
            paths, searched, ..
        }) => {
            assert!(paths.contains(&"README.md".to_string()), "{paths:?}");
            assert!(searched >= 1, "the squash commit must have been tried");
        }
        other => panic!("partly landed work must not read as landed: {other:?}"),
    }
    assert!(!landed_content_verdict(&wt, BOUND).is_landed());
}

/// 🔴 #8633: `merge-tree` exits 1 for a ref it cannot merge too, so the exit
/// code alone cannot mean "conflict". A git error is undeterminable, is never
/// read as landed or as a conflict, and quotes git's stderr.
#[test]
fn a_merge_tree_git_error_is_undeterminable_and_quotes_stderr() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("bad-ref");
    let err = content_on_base(&wt, "origin/does-not-exist")
        .expect_err("an unmergeable ref must not produce a verdict");
    assert!(err.contains("not a merge conflict"), "{err}");
    assert!(
        err.contains("origin/does-not-exist - not something we can merge"),
        "git's stderr must be quoted: {err}"
    );
    assert!(
        content_on_base(&wt, "  ").is_err(),
        "an empty base is an error"
    );
}

/// #8633: every verdict names what the probe found, so a refusal is
/// actionable.
#[test]
fn the_description_names_the_probe_result() {
    let conflicted = ContentOnBase::Conflicted {
        paths: (1..=7).map(|i| format!("f{i}.rs")).collect(),
        searched: 3,
        candidates: 3,
    };
    let text = conflicted.describe("origin/main");
    assert!(text.contains("conflicts in 7 file(s)"), "{text}");
    assert!(
        text.contains("`f1.rs`") && text.contains("and 2 more"),
        "{text}"
    );
    assert!(text.contains("none of the 3 commit(s)"), "{text}");
    // #8633 critic round: a capped walk must not read as exhaustive.
    let capped = ContentOnBase::Residual {
        paths: vec!["f.rs".into()],
        searched: 24,
        candidates: 30,
    };
    let text = capped.describe("origin/main");
    assert!(text.contains("the oldest 24 of 30 commit(s)"), "{text}");
    assert!(text.contains("the newer 6 were not searched"), "{text}");
    let landed = ContentOnBase::Landed {
        at: Some("abc123".into()),
    };
    assert!(landed.describe("origin/main").contains("`abc123`"));
}

/// A ten-line file whose first and last lines are `first` and `last`; the
/// eight lines between never change, so edits to the two ends merge apart.
fn ten_lines(first: &str, last: &str) -> String {
    format!("{first}\n2\n3\n4\n5\n6\n7\n8\n9\n{last}\n")
}

/// 🔴 FAIL-OPEN CHECK (#8633 critic round): after the squash, an UNPUSHED
/// branch commit reverts part of what the squash carried, and `main` then
/// edits another line so the tip merge conflicts. Merging `HEAD` into the
/// squash comes out empty — relative to the fork the revert changes nothing —
/// but the revert is on no remote. Not landed.
///
/// Fails against a history walk that trusts the forward merge alone.
#[test]
fn a_post_squash_partial_revert_is_not_landed() {
    let fx = GitWorktreeFixture::new();
    commit_files(&fx.repo, &[("f.txt", &ten_lines("a", "c"))], "seed");
    git(&fx.repo, &["push", "origin", "main"]);
    let wt = fx.add_worktree("revert");
    commit_files(&wt, &[("f.txt", &ten_lines("b", "d"))], "S");
    squash_onto_main(&fx, "session/revert");
    commit_files(&wt, &[("f.txt", &ten_lines("b", "c"))], "T: revert line 10");
    commit_files(
        &fx.repo,
        &[("f.txt", &ten_lines("e", "d"))],
        "main edits line 1",
    );
    git(&fx.repo, &["push", "origin", "main"]);
    git(&wt, &["fetch", "origin"]);

    match content_on_base(&wt, "origin/main") {
        Ok(ContentOnBase::Conflicted {
            paths, searched, ..
        }) => {
            assert_eq!(paths, vec!["f.txt".to_string()]);
            assert_eq!(searched, 2, "the squash and the later edit were tried");
        }
        other => panic!("a reverted squash change must not read as landed: {other:?}"),
    }
    assert!(!landed_content_verdict(&wt, BOUND).is_landed());
}

/// 🔴 FAIL-OPEN CHECK (#8633 critic round): after the squash, an UNPUSHED
/// branch commit deletes a file the squash added — the same blind spot as the
/// partial revert, since relative to the fork the deletion is a no-op.
#[test]
fn a_post_squash_deletion_of_a_squashed_file_is_not_landed() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("delete");
    commit_files(
        &wt,
        &[("README.md", "branch\n"), ("added.txt", "new\n")],
        "S",
    );
    squash_onto_main(&fx, "session/delete");
    git(&wt, &["rm", "-q", "added.txt"]);
    git(&wt, &["commit", "-m", "T: delete added.txt"]);
    edit_over_on_main_and_push(&fx, &["README.md"]);
    git(&wt, &["fetch", "origin"]);

    match content_on_base(&wt, "origin/main") {
        Ok(ContentOnBase::Conflicted { paths, .. }) => {
            assert_eq!(paths, vec!["README.md".to_string()]);
        }
        other => panic!("a deleted squash file must not read as landed: {other:?}"),
    }
    assert!(!landed_content_verdict(&wt, BOUND).is_landed());
}

/// #8633 critic round: the reverse check reads the landing commit's patch
/// against its FIRST parent, so content landed by a MERGE commit — of a
/// rebased copy of this branch, after other work on `main`, and edited over
/// afterwards — is still landed at that merge commit.
#[test]
fn content_landed_by_a_merge_commit_edited_over_on_main_is_landed() {
    let fx = GitWorktreeFixture::new();
    commit_files(&fx.repo, &[("f.txt", &ten_lines("a", "c"))], "seed");
    git(&fx.repo, &["push", "origin", "main"]);
    let wt = fx.add_worktree("merged");
    commit_files(&wt, &[("f.txt", &ten_lines("a", "d"))], "branch work");
    git(&fx.repo, &["switch", "-q", "-c", "pr"]);
    git(&fx.repo, &["cherry-pick", "session/merged"]);
    git(&fx.repo, &["commit", "--amend", "-m", "rebased copy"]);
    git(&fx.repo, &["switch", "-q", "main"]);
    commit_files(&fx.repo, &[("other.txt", "main work\n")], "main work");
    git(&fx.repo, &["merge", "--no-ff", "-m", "merge (#2)", "pr"]);
    let merge = git(&fx.repo, &["rev-parse", "HEAD"]);
    commit_files(&fx.repo, &[("f.txt", &ten_lines("a", "z"))], "later edit");
    git(&fx.repo, &["push", "origin", "main"]);
    git(&wt, &["fetch", "origin"]);

    assert_eq!(
        content_on_base(&wt, "origin/main"),
        Ok(ContentOnBase::Landed { at: Some(merge) })
    );
}
