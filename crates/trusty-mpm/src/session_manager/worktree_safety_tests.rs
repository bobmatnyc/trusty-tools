//! Unit tests for the #4091 dirty-worktree guard in `worktree_safety.rs`.
//!
//! Why: these exercise [`super::inspect_dirt`] in isolation against REAL git
//! repositories built under `tempfile::tempdir()` — never a live worktree —
//! so each "dirty" definition (modified tracked, untracked-not-ignored,
//! committed-but-unpushed) and each fail-safe error path is pinned
//! independently of the reclaim path that consumes them. The wired-up
//! behaviour is covered separately by the `#4091` tests in
//! `prune_orphan_tests`.
//! What: the clean/pushed baseline, the three dirty definitions, the
//! ignored-files carve-out, the non-git-directory classification, and the
//! error-means-dirty invariant.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::*;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// The default policy must be the SAFE one. If this ever flips, every caller
/// that relies on `..Default::default()` silently gains the power to destroy
/// uncommitted work.
#[test]
fn dirty_policy_defaults_to_skip() {
    assert_eq!(
        DirtyWorktreePolicy::default(),
        DirtyWorktreePolicy::Skip,
        "the default dirty policy must never be ForceDiscard"
    );
}

/// Baseline: a worktree with no local edits, whose HEAD is already on a
/// remote-tracking ref, is PROVABLY clean — the guard must return `None` so
/// reclamation still works. Without this the guard would be a permanent leak.
#[test]
fn inspect_dirt_clean_pushed_worktree_is_none() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("clean");
    assert!(
        inspect_dirt(&wt).is_none(),
        "a clean, fully-pushed worktree must be reclaimable; got {:?}",
        inspect_dirt(&wt)
    );
}

/// Definition 1a of dirty: a MODIFIED TRACKED file.
#[test]
fn inspect_dirt_reports_modified_tracked_file() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("modified");
    std::fs::write(wt.join("README.md"), "edited but never committed\n").unwrap();

    let dirt = inspect_dirt(&wt).expect("a modified tracked file must read as dirty");
    assert_eq!(dirt.dirty_files, 1, "reason was: {}", dirt.reason);
    assert_eq!(dirt.unpushed_commits, 0);
    assert_eq!(dirt.path, wt);
}

/// Definition 1b of dirty: an UNTRACKED, non-ignored file. Untracked work is
/// the most easily lost kind — it exists nowhere but that directory.
#[test]
fn inspect_dirt_reports_untracked_file() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("untracked");
    std::fs::write(wt.join("notes.md"), "hand-written, never added\n").unwrap();

    let dirt = inspect_dirt(&wt).expect("an untracked file must read as dirty");
    assert_eq!(dirt.dirty_files, 1, "reason was: {}", dirt.reason);
}

/// The carve-out that keeps the guard usable: IGNORED files (build artefacts)
/// are not work, and must not pin a worktree as dirty forever. A `target/`
/// directory in every checkout would otherwise make the guard refuse
/// everything, which is indistinguishable from having no reclamation at all.
#[test]
fn inspect_dirt_ignores_gitignored_files() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("ignored");
    std::fs::write(wt.join(".gitignore"), "build-output/\n").unwrap();
    std::fs::create_dir_all(wt.join("build-output")).unwrap();
    std::fs::write(wt.join("build-output").join("artifact.bin"), "junk\n").unwrap();
    // Commit the .gitignore itself so only the IGNORED path remains on disk.
    let commit = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["add", ".gitignore"])
        .output()
        .unwrap();
    assert!(commit.status.success());
    let commit = std::process::Command::new("git")
        .arg("-C")
        .arg(&wt)
        .args(["commit", "-m", "ignore build output"])
        .output()
        .unwrap();
    assert!(commit.status.success());

    // The commit itself is unpushed, so the worktree IS dirty — but for the
    // commit, not for the ignored artefact. Assert on the file count.
    let dirt = inspect_dirt(&wt).expect("the new commit is unpushed, so this is dirty");
    assert_eq!(
        dirt.dirty_files, 0,
        "ignored build output must not count as uncommitted work; reason: {}",
        dirt.reason
    );
    assert_eq!(dirt.unpushed_commits, 1, "reason: {}", dirt.reason);
}

/// trusty-mpm's OWN untracked ownership sentinel must not count as work.
///
/// Why this test exists: it caught a real self-inflicted bug. Every managed
/// worktree carries a `.trusty-mpm-worktree` sentinel, and most host projects
/// do not gitignore it — so counting it marked EVERY managed worktree
/// permanently dirty, which would have shipped a guard that silently disabled
/// all reclamation.
#[test]
fn inspect_dirt_excludes_own_sentinel() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("sentinel-only");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);

    assert!(
        inspect_dirt(&wt).is_none(),
        "trusty-mpm's own sentinel must not make a worktree dirty; got {:?}",
        inspect_dirt(&wt)
    );
}

/// The untracked `.trusty-mpm/` scrollback-snapshot directory the daemon
/// writes into every managed workspace must not count as work either.
#[test]
fn inspect_dirt_excludes_untracked_trusty_mpm_dir() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("scrollback");
    std::fs::create_dir_all(wt.join(".trusty-mpm")).unwrap();
    std::fs::write(wt.join(".trusty-mpm").join("scrollback.txt"), "pane dump\n").unwrap();

    assert!(
        inspect_dirt(&wt).is_none(),
        "the daemon's own scrollback dir must not make a worktree dirty; got {:?}",
        inspect_dirt(&wt)
    );
}

/// The counterweight: a TRACKED file under `.trusty-mpm/` that someone edited
/// IS real work, and the bookkeeping exclusion must not swallow it. The
/// exclusion is scoped to untracked entries precisely so this stays protected.
#[test]
fn inspect_dirt_counts_tracked_trusty_mpm_edit() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("tracked-config");
    std::fs::create_dir_all(wt.join(".trusty-mpm")).unwrap();
    std::fs::write(wt.join(".trusty-mpm").join("INSTRUCTIONS.md"), "v1\n").unwrap();
    for args in [
        vec!["add", ".trusty-mpm/INSTRUCTIONS.md"],
        vec!["commit", "-m", "track instructions"],
    ] {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {:?}", out);
    }
    std::fs::write(
        wt.join(".trusty-mpm").join("INSTRUCTIONS.md"),
        "v2 edited\n",
    )
    .unwrap();

    let dirt = inspect_dirt(&wt).expect("a tracked edit is real work, wherever it lives");
    assert_eq!(
        dirt.dirty_files, 1,
        "a tracked .trusty-mpm/ edit must still count; reason: {}",
        dirt.reason
    );
}

/// Definition 2 of dirty: a COMMITTED but UNPUSHED commit. This looks safe —
/// the work is committed — but `remove_session_worktree` deletes the
/// `session/<leaf>` branch after removal, so the commit loses its last
/// reachable ref.
#[test]
fn inspect_dirt_reports_unpushed_commit() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("unpushed");
    GitWorktreeFixture::commit_unpushed(&wt);

    let dirt = inspect_dirt(&wt).expect("an unpushed commit must read as dirty");
    assert_eq!(dirt.dirty_files, 0, "reason: {}", dirt.reason);
    assert_eq!(dirt.unpushed_commits, 1, "reason: {}", dirt.reason);
}

/// FAIL-SAFE: a path git cannot examine at all reads as DIRTY, never as clean.
/// An error on the check must never become a green light to delete.
#[test]
fn inspect_dirt_treats_missing_path_as_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does").join("not").join("exist");
    let dirt = inspect_dirt(&missing).expect("an unexaminable path must read as dirty");
    assert!(
        dirt.reason.contains("unreadable") || dirt.reason.contains("not a git worktree"),
        "unexpected reason: {}",
        dirt.reason
    );
}

/// A plain directory that is NOT a git worktree root but holds files cannot be
/// assessed by git at all — and the fail-safe answer is DIRTY. This also pins
/// the identity check: the directory sits INSIDE a real checkout, so a naive
/// `git status` would have reported the ENCLOSING repo's (clean) status and
/// wrongly approved deletion.
#[test]
fn inspect_dirt_treats_non_worktree_with_files_as_dirty() {
    let fx = GitWorktreeFixture::new();
    let plain = fx.repo.join(".worktrees").join("not-a-worktree");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("leftover.txt"), "who knows\n").unwrap();

    let dirt = inspect_dirt(&plain).expect("a non-worktree dir holding files must read as dirty");
    assert_eq!(dirt.dirty_files, 1, "reason: {}", dirt.reason);
    assert!(
        dirt.reason.contains("not a git worktree root"),
        "unexpected reason: {}",
        dirt.reason
    );
}

/// The counterweight to the test above: an EMPTY leftover shell (nothing but
/// the ownership sentinel) is provably free of work and stays reclaimable.
/// This is the #1838 case the orphan sweep exists for — one project grew 94 of
/// these — so the guard must not turn it into a permanent leak.
#[test]
fn inspect_dirt_allows_empty_non_git_leftover() {
    let tmp = tempfile::tempdir().unwrap();
    let shell = tmp.path().join(".worktrees").join("empty-shell");
    std::fs::create_dir_all(&shell).unwrap();
    std::fs::write(
        shell.join(super::super::decommission::WORKTREE_SENTINEL_FILE),
        "{}",
    )
    .unwrap();

    assert!(
        inspect_dirt(&shell).is_none(),
        "an empty leftover shell must stay reclaimable; got {:?}",
        inspect_dirt(&shell)
    );
}
