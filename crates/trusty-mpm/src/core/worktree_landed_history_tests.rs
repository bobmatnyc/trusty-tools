//! REAL-git coverage for the #8633 squash-then-edited landed-content shape.
//!
//! Why: the defect lives in what `git merge-tree` prints and returns, so a
//! mocked git would assert the mock. Every fixture is a temp repository whose
//! "remote" is a bare repository beside it; nothing reaches the network.
//!
//! What: the grant (a squash-merged branch whose files `main` edited later),
//! the two fail-open checks (a different version of the change on `main`, and
//! a branch only partly landed), and a `merge-tree` git error. Round 3: the
//! same revert and deletion with the squash still `main`'s tip, a clean
//! squash and a rebase-merge landed at a named commit, and an ancestor `HEAD`.
//! Final round: a revert or deletion followed by an unrelated `main` commit,
//! a later landing whose patch leaves an undone file uncovered, and the git
//! error arms of the ancestry check and the branch-path listing.

use std::path::Path;
use std::time::Duration;

use super::{ContentOnBase, branch_touched_paths, content_on_base, head_is_ancestor_of};
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

/// Assert `wt` is NOT landed on `origin/main`: the probe refuses, names the
/// landing commit `at` and the file `HEAD` took back, and the landed-content
/// verdict refuses too. Written against `describe` and `is_landed` so the
/// same assertion compiles — and fails — against the pre-round-3 probe.
fn assert_undone_at(wt: &Path, at: &str, taken_back: &str) {
    let found = content_on_base(wt, "origin/main").expect("the probe must answer");
    assert!(
        !found.is_landed(),
        "undone content read as landed: {found:?}"
    );
    let text = found.describe("origin/main");
    assert!(
        text.contains(&format!("no longer holds all of what landed at `{at}`")),
        "{text}"
    );
    assert!(text.contains(&format!("`{taken_back}`")), "{text}");
    let verdict = landed_content_verdict(wt, BOUND);
    assert!(!verdict.is_landed(), "{verdict:?}");
    assert!(
        verdict.note().contains("no longer holds"),
        "{}",
        verdict.note()
    );
}

/// 🔴 FAIL-OPEN CHECK (#8633 round 3): the partial-revert shape with `main`
/// NOT moved since the squash, so the squash IS the tip. Merging `HEAD` into
/// the tip comes out empty, and the old tip-only admission granted without
/// the reverse check — the unpushed revert would be lost. Not landed.
///
/// Fails against a probe that admits an empty tip merge on its own.
#[test]
fn a_partial_revert_while_main_is_unmoved_is_not_landed() {
    let fx = GitWorktreeFixture::new();
    commit_files(&fx.repo, &[("f.txt", &ten_lines("a", "c"))], "seed");
    git(&fx.repo, &["push", "origin", "main"]);
    let wt = fx.add_worktree("revert-at-tip");
    commit_files(&wt, &[("f.txt", &ten_lines("b", "d"))], "S");
    let squash = squash_onto_main(&fx, "session/revert-at-tip");
    git(&fx.repo, &["push", "origin", "main"]);
    commit_files(&wt, &[("f.txt", &ten_lines("b", "c"))], "T: revert line 10");
    git(&wt, &["fetch", "origin"]);

    assert_undone_at(&wt, &squash, "f.txt");
}

/// 🔴 FAIL-OPEN CHECK (#8633 round 3): an unpushed deletion of a file the
/// squash added, with the squash still `main`'s tip. Not landed.
///
/// Fails against a probe that admits an empty tip merge on its own.
#[test]
fn a_post_squash_deletion_at_the_unmoved_tip_is_not_landed() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("delete-at-tip");
    commit_files(
        &wt,
        &[("README.md", "branch\n"), ("added.txt", "new\n")],
        "S",
    );
    let squash = squash_onto_main(&fx, "session/delete-at-tip");
    git(&fx.repo, &["push", "origin", "main"]);
    git(&wt, &["rm", "-q", "added.txt"]);
    git(&wt, &["commit", "-m", "T: delete added.txt"]);
    git(&wt, &["fetch", "origin"]);

    assert_undone_at(&wt, &squash, "added.txt");
}

/// #8633 round 3, the conservative direction of the branch-scoped reverse
/// check: the squash carried a sibling's edit to a file THIS branch also
/// edited. `HEAD` never had that line, but the file is one its commits
/// touched, so the probe cannot tell it from a revert and refuses.
#[test]
fn extra_content_on_a_touched_path_is_not_landed() {
    let fx = GitWorktreeFixture::new();
    commit_files(&fx.repo, &[("f.txt", &ten_lines("a", "c"))], "seed");
    git(&fx.repo, &["push", "origin", "main"]);
    let wt = fx.add_worktree("touched-extra");
    commit_files(&wt, &[("f.txt", &ten_lines("b", "c"))], "line 1");
    commit_files(&fx.repo, &[("f.txt", &ten_lines("b", "d"))], "squash (#1)");
    let squash = git(&fx.repo, &["rev-parse", "HEAD"]);
    git(&fx.repo, &["push", "origin", "main"]);
    git(&wt, &["fetch", "origin"]);

    assert_undone_at(&wt, &squash, "f.txt");
}

/// #8633 round 3: a clean squash-merge with `main` unmoved since is landed,
/// and the tip — the squash — is named as the landing commit.
#[test]
fn a_clean_squash_merge_with_main_unmoved_is_landed_at_the_tip() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("clean-squash");
    commit_files(&wt, &[("README.md", "one\n")], "one");
    commit_files(&wt, &[("two.txt", "two\n")], "two");
    let squash = squash_onto_main(&fx, "session/clean-squash");
    git(&fx.repo, &["push", "origin", "main"]);
    git(&wt, &["fetch", "origin"]);

    assert_eq!(
        content_on_base(&wt, "origin/main"),
        Ok(ContentOnBase::Landed { at: Some(squash) })
    );
    assert!(landed_content_verdict(&wt, BOUND).is_landed());
}

/// Rebase-merge `branch` onto `main` in the owning checkout, as
/// `gh pr merge --rebase` does — each branch commit replayed with a new id
/// after other work on `main` — push it, and return the last replayed commit.
fn rebase_merge_onto_main(fx: &GitWorktreeFixture, branch: &str) -> String {
    commit_files(&fx.repo, &[("main.txt", "main work\n")], "main work first");
    let range = format!("main..{branch}");
    git(&fx.repo, &["cherry-pick", &range]);
    git(&fx.repo, &["push", "origin", "main"]);
    git(&fx.repo, &["rev-parse", "HEAD"])
}

/// A branch of three commits over one ten-line file and one new file.
fn three_commit_branch(fx: &GitWorktreeFixture, name: &str) -> std::path::PathBuf {
    commit_files(&fx.repo, &[("f.txt", &ten_lines("a", "c"))], "seed");
    git(&fx.repo, &["push", "origin", "main"]);
    let wt = fx.add_worktree(name);
    commit_files(&wt, &[("f.txt", &ten_lines("b", "c"))], "one");
    commit_files(&wt, &[("new.txt", "new\n")], "two");
    commit_files(&wt, &[("f.txt", &ten_lines("b", "d"))], "three");
    wt
}

/// #8633 round 3: a REBASE-merged branch — three commits replayed onto
/// `main` after other work there — is landed at its last replayed commit,
/// which carries all of its content and whose own patch `HEAD` holds. Once
/// with `main` unmoved since (the tip is that commit), once with `main`
/// having edited the same file afterwards (found in history).
#[test]
fn a_rebase_merged_branch_is_landed_at_its_last_replayed_commit() {
    let fx = GitWorktreeFixture::new();
    let wt = three_commit_branch(&fx, "rebased");
    let last = rebase_merge_onto_main(&fx, "session/rebased");
    git(&wt, &["fetch", "origin"]);
    assert_eq!(
        content_on_base(&wt, "origin/main"),
        Ok(ContentOnBase::Landed {
            at: Some(last.clone())
        }),
        "main unmoved since the rebase-merge"
    );

    commit_files(&fx.repo, &[("f.txt", &ten_lines("e", "d"))], "later edit");
    git(&fx.repo, &["push", "origin", "main"]);
    git(&wt, &["fetch", "origin"]);
    assert_eq!(
        content_on_base(&wt, "origin/main"),
        Ok(ContentOnBase::Landed { at: Some(last) }),
        "main moved on after the rebase-merge"
    );
    assert!(landed_content_verdict(&wt, BOUND).is_landed());
}

/// KNOWN RESIDUAL (#8633, predates it — the tip-only forward merge admitted
/// it too): the branch is rebase-merged as one', two', three', and an
/// UNPUSHED commit then reverts `f.txt` line 1 to `a`. The probe reads it as
/// Landed at three': three' changes `f.txt`, so it covers the file, and its
/// own patch touches only line 10, which `HEAD` holds. The revert is on no
/// remote. This pins today's behaviour so that a fix is a deliberate change
/// to this assertion; see the residual in ADR-0057.
#[test]
fn known_residual_rebase_merge_then_unpushed_undo_on_a_file_the_last_replay_touches_reads_landed() {
    let fx = GitWorktreeFixture::new();
    let wt = three_commit_branch(&fx, "rebased-undo");
    let last = rebase_merge_onto_main(&fx, "session/rebased-undo");
    commit_files(&wt, &[("f.txt", &ten_lines("a", "d"))], "T: undo line 1");
    git(&wt, &["fetch", "origin"]);

    assert_eq!(
        content_on_base(&wt, "origin/main"),
        Ok(ContentOnBase::Landed { at: Some(last) }),
        "known residual: the unpushed undo of line 1 is not seen"
    );
    assert!(landed_content_verdict(&wt, BOUND).is_landed());
}

/// #8633: a branch behind `main` is squash-merged after `main` edited the
/// same file first. The fork holds `f.txt` = (a, z); `main` commits z→Z
/// before the squash; the branch changes a→A; the squash gives (A, Z) and
/// `HEAD` holds (A, z). The squash's own patch is line 1 only, which `HEAD`
/// holds, and `HEAD`'s older line 10 is `main`'s earlier history, not an
/// undo. Landed at the squash.
#[test]
fn a_squash_of_a_behind_branch_whose_file_main_edited_first_is_landed() {
    let fx = GitWorktreeFixture::new();
    commit_files(&fx.repo, &[("f.txt", &ten_lines("a", "z"))], "seed");
    git(&fx.repo, &["push", "origin", "main"]);
    let wt = fx.add_worktree("behind");
    commit_files(
        &fx.repo,
        &[("f.txt", &ten_lines("a", "Z"))],
        "main edits line 10 first",
    );
    commit_files(&wt, &[("f.txt", &ten_lines("A", "z"))], "branch line 1");
    let squash = squash_onto_main(&fx, "session/behind");
    git(&fx.repo, &["push", "origin", "main"]);
    git(&wt, &["fetch", "origin"]);

    assert_eq!(
        git(&fx.repo, &["show", "HEAD:f.txt"]),
        ten_lines("A", "Z").trim_end(),
        "the squash holds both edits"
    );
    assert_eq!(
        content_on_base(&wt, "origin/main"),
        Ok(ContentOnBase::Landed { at: Some(squash) })
    );
    assert!(landed_content_verdict(&wt, BOUND).is_landed());
}

/// #8633 round 3, unchanged behaviour: a `HEAD` that is an ancestor of `main`
/// — fast-forwarded, or merged by a merge commit — is landed with no landing
/// commit named, even after `main` edited the same file.
#[test]
fn a_head_that_is_an_ancestor_of_main_is_landed() {
    let fx = GitWorktreeFixture::new();
    let ff = fx.add_worktree("fast-forward");
    commit_files(&ff, &[("ff.txt", "ff\n")], "ff work");
    git(&fx.repo, &["merge", "--ff-only", "session/fast-forward"]);
    git(&fx.repo, &["push", "origin", "main"]);
    git(&ff, &["fetch", "origin"]);
    assert_eq!(
        content_on_base(&ff, "origin/main"),
        Ok(ContentOnBase::Landed { at: None })
    );

    let merged = fx.add_worktree("merge-commit");
    commit_files(&merged, &[("m.txt", "branch\n")], "branch work");
    commit_files(&fx.repo, &[("other.txt", "main\n")], "main work");
    git(
        &fx.repo,
        &["merge", "--no-ff", "-m", "merge", "session/merge-commit"],
    );
    commit_files(&fx.repo, &[("m.txt", "edited on main\n")], "later edit");
    git(&fx.repo, &["push", "origin", "main"]);
    git(&merged, &["fetch", "origin"]);
    assert_eq!(
        content_on_base(&merged, "origin/main"),
        Ok(ContentOnBase::Landed { at: None })
    );
    assert!(landed_content_verdict(&merged, BOUND).is_landed());
}

/// Commit an unrelated `u.txt` on `main` and push everything `main` holds.
fn unrelated_main_commit_and_push(fx: &GitWorktreeFixture) {
    commit_files(&fx.repo, &[("u.txt", "unrelated\n")], "unrelated main work");
    git(&fx.repo, &["push", "origin", "main"]);
}

/// 🔴 FAIL-OPEN CHECK (#8633 final round): after the squash, an UNPUSHED
/// branch commit reverts line 10, and `main` then pushes an unrelated
/// `u.txt`. The tip merge is empty and the tip's own patch touches only
/// `u.txt`, so its reverse check sees nothing of `f.txt`. The tip must not be
/// the landing commit; the squash is tried and shows the revert. Not landed.
///
/// Fails at 15f2e3450, which filtered the tip's `u.txt` residue away and
/// admitted `Landed { at: tip }`.
#[test]
fn an_unpushed_revert_with_an_unrelated_main_commit_after_the_squash_is_not_landed() {
    let fx = GitWorktreeFixture::new();
    commit_files(&fx.repo, &[("f.txt", &ten_lines("a", "c"))], "seed");
    git(&fx.repo, &["push", "origin", "main"]);
    let wt = fx.add_worktree("revert-unrelated");
    commit_files(&wt, &[("f.txt", &ten_lines("b", "d"))], "S");
    let squash = squash_onto_main(&fx, "session/revert-unrelated");
    commit_files(&wt, &[("f.txt", &ten_lines("b", "c"))], "T: revert line 10");
    unrelated_main_commit_and_push(&fx);
    git(&wt, &["fetch", "origin"]);

    assert_undone_at(&wt, &squash, "f.txt");
}

/// 🔴 FAIL-OPEN CHECK (#8633 final round): an UNPUSHED deletion of a file the
/// squash added, then an unrelated `u.txt` on `main`. Not landed — once with
/// another squashed file keeping the squash a candidate, once with the
/// deleted file the branch's only change, where no commit is eligible.
///
/// Fails at 15f2e3450, which admitted both at the tip.
#[test]
fn an_unpushed_deletion_with_an_unrelated_main_commit_after_the_squash_is_not_landed() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("delete-unrelated");
    commit_files(
        &wt,
        &[("README.md", "branch\n"), ("added.txt", "new\n")],
        "S",
    );
    let squash = squash_onto_main(&fx, "session/delete-unrelated");
    git(&wt, &["rm", "-q", "added.txt"]);
    git(&wt, &["commit", "-m", "T: delete added.txt"]);
    unrelated_main_commit_and_push(&fx);
    git(&wt, &["fetch", "origin"]);
    assert_undone_at(&wt, &squash, "added.txt");

    let fx = GitWorktreeFixture::new();
    let only = fx.add_worktree("delete-only");
    commit_files(&only, &[("only.txt", "new\n")], "S");
    squash_onto_main(&fx, "session/delete-only");
    git(&only, &["rm", "-q", "only.txt"]);
    git(&only, &["commit", "-m", "T: delete only.txt"]);
    unrelated_main_commit_and_push(&fx);
    git(&only, &["fetch", "origin"]);
    let found = content_on_base(&only, "origin/main");
    assert!(
        !matches!(&found, Ok(c) if c.is_landed()),
        "a deletion no commit covers read as landed: {found:?}"
    );
    assert!(!landed_content_verdict(&only, BOUND).is_landed());
}

/// 🔴 FAIL-OPEN CHECK (#8633 final round), the partial-overlap shape: the
/// branch lands `g.txt` in one squash and `f.txt` in a second, whose own
/// patch touches `f.txt` only; then an UNPUSHED commit reverts line 10 of
/// `g.txt`. The second squash is the tip, the merge into it is empty, and its
/// reverse check is clean on `f.txt` — but it never reaches `g.txt`, whose
/// `HEAD` version is on no remote. Coverage requires every branch-touched
/// path to be in the landing commit's patch or identical in `HEAD` and that
/// commit, so it is not landed and the refusal names `g.txt`.
///
/// Fails against a rule that only asks the landing commit's patch to touch
/// ONE of the branch's paths, and at 15f2e3450.
#[test]
fn a_later_landing_that_leaves_an_undone_file_uncovered_is_not_landed() {
    let fx = GitWorktreeFixture::new();
    commit_files(
        &fx.repo,
        &[
            ("f.txt", &ten_lines("a", "c")),
            ("g.txt", &ten_lines("a", "c")),
        ],
        "seed",
    );
    git(&fx.repo, &["push", "origin", "main"]);
    let wt = fx.add_worktree("overlap");
    commit_files(&wt, &[("g.txt", &ten_lines("b", "d"))], "g work");
    squash_onto_main(&fx, "session/overlap");
    git(&fx.repo, &["push", "origin", "main"]);
    commit_files(&wt, &[("f.txt", &ten_lines("b", "c"))], "f work");
    let second = squash_onto_main(&fx, "session/overlap");
    git(&fx.repo, &["push", "origin", "main"]);
    let second_patch = git(&fx.repo, &["diff", "--name-only", "HEAD^", "HEAD"]);
    assert_eq!(
        second_patch, "f.txt",
        "the second squash carries f.txt only"
    );
    commit_files(
        &wt,
        &[("g.txt", &ten_lines("b", "c"))],
        "T: revert g line 10",
    );
    git(&wt, &["fetch", "origin"]);

    assert_undone_at(&wt, &second, "g.txt");
}

/// #8633 final round: `git merge-base --is-ancestor` exiting other than 0 or
/// 1 — a directory that is no repository, a base that names no commit — is
/// `Err`, never read as "not an ancestor" or "an ancestor".
#[test]
fn an_ancestry_check_git_cannot_answer_refuses() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let err = head_is_ancestor_of(tmp.path(), "main").expect_err("no repository must refuse");
    assert!(err.contains("could not answer"), "{err}");

    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("ancestry");
    let err = head_is_ancestor_of(&wt, "refs/heads/no-such-branch")
        .expect_err("a base naming no commit must refuse");
    assert!(err.contains("could not answer"), "{err}");
}

/// #8633 final round: a `git log` that fails while listing the paths the
/// branch touched is `Err`, never an empty set — an empty set would drop every
/// reverse-check residue and admit.
#[test]
fn a_failed_git_log_of_the_branch_paths_refuses() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("broken-log");
    let missing = "0123456789abcdef0123456789abcdef01234567";
    let err = branch_touched_paths(&wt, missing).expect_err("an unknown fork must refuse");
    assert!(err.contains("could not be listed"), "{err}");

    let tmp = tempfile::tempdir().expect("tempdir");
    assert!(branch_touched_paths(tmp.path(), "HEAD").is_err());
}
