//! Unit tests for the nested-repository loss-model split (#4118 round 3).
//!
//! Why: the round-3 CRITICAL was not "the scan misses something" — the scan
//! found the clone. It was that the clone was then assessed with the WRONG loss
//! model, which is worse than not looking, because the sweep deletes with a
//! clean bill of health. These tests pin the discriminator and both models.
//! What: the `--git-common-dir` discriminator on both real shapes, the
//! self-contained model (other-branch commits, stash, remote-less clones), and
//! the counterweights that keep reclamation alive.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::{Path, PathBuf};

use super::*;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_safety::inspect_dirt;

/// Run `git -C <dir> <args>`, panicking with git's own stderr on failure.
///
/// Why: a fixture step that silently no-ops produces a test that passes for the
/// wrong reason — precisely the failure this round is about.
fn git_ok(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("fixture: `git {}` could not be run: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "fixture: `git {}` failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Build a SELF-CONTAINED clone (its own `.git`, its own remote) inside
/// `parent`, at a path `parent` gitignores.
///
/// Why: this is the shape the round-3 CRITICAL is about — `git clone` into a
/// scratch directory. Its object store lives inside the candidate, so removing
/// the candidate destroys every ref it holds.
/// What: a bare remote beside the fixture plus a clone at `<parent>/scratch/work`
/// whose `main` is pushed and tracking. Returns the clone path.
fn seed_self_contained_clone(fx: &GitWorktreeFixture, parent: &Path) -> PathBuf {
    let remote = fx.repos_root.join("inner-remote.git");
    std::fs::create_dir_all(&remote).unwrap();
    git_ok(&remote, &["init", "--bare", "--initial-branch=main"]);

    let clone = parent.join("scratch").join("work");
    std::fs::create_dir_all(&clone).unwrap();
    git_ok(&clone, &["init", "--initial-branch=main"]);
    git_ok(&clone, &["config", "user.email", "ci@test.invalid"]);
    git_ok(&clone, &["config", "user.name", "CI"]);
    git_ok(&clone, &["config", "commit.gpgsign", "false"]);
    std::fs::write(clone.join("a.txt"), "base\n").unwrap();
    git_ok(&clone, &["add", "a.txt"]);
    git_ok(&clone, &["commit", "-m", "base"]);
    git_ok(
        &clone,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git_ok(&clone, &["push", "origin", "main"]);
    git_ok(&clone, &["fetch", "origin"]);
    git_ok(&clone, &["branch", "--set-upstream-to=origin/main", "main"]);
    clone
}

/// A parent worktree that is clean, pushed, and gitignores `scratch/`.
fn seed_parent(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    let parent = fx.add_worktree(name);
    std::fs::write(parent.join(".gitignore"), "scratch/\n.claude/worktrees/\n").unwrap();
    GitWorktreeFixture::commit_all_and_push(&parent, "ignore scratch and agent worktrees");
    parent
}

/// THE ROUND-3 CRITICAL: a self-contained clone whose HEAD is fully pushed but
/// which carries a commit on another local branch must make the parent DIRTY.
///
/// Why: every question the top-level model asks answers "clean" here — `status`
/// is empty, `origin/main..HEAD` is 0, and there is no `session/<leaf>` branch.
/// The commit on `feature` exists nowhere but inside the candidate, so removing
/// the candidate destroys it. Finding the clone and then assessing it with the
/// shared-store model manufactured confidence rather than safety.
#[test]
fn inspect_dirt_reports_self_contained_clone_with_work_on_another_branch() {
    let fx = GitWorktreeFixture::new();
    let parent = seed_parent(&fx, "clone-other-branch");
    let clone = seed_self_contained_clone(&fx, &parent);
    git_ok(&clone, &["checkout", "-b", "feature"]);
    std::fs::write(clone.join("only-here.txt"), "the only copy\n").unwrap();
    git_ok(&clone, &["add", "only-here.txt"]);
    git_ok(&clone, &["commit", "-m", "work that only exists here"]);
    git_ok(&clone, &["checkout", "main"]);

    // Prove the premise: the OLD model really did answer clean on this clone.
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&clone)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "premise broken: the clone's working tree should be clean"
    );
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(&clone)
        .args(["rev-list", "--count", "origin/main..HEAD"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&head.stdout).trim(),
        "0",
        "premise broken: the clone's HEAD should be fully pushed"
    );

    let dirt = inspect_dirt(&parent)
        .expect("a commit living only on another local branch of a nested clone must be seen");
    assert!(
        dirt.reason.contains("self-contained clone"),
        "the reason must say which loss model applied; got: {}",
        dirt.reason
    );
    assert!(
        dirt.unpushed_commits >= 1,
        "the unreachable commit must be counted; got: {}",
        dirt.reason
    );
}

/// The same conflation, via the stash. A self-contained clone's `refs/stash`
/// and its reflog live inside the candidate and die with it — unlike the
/// top-level candidate's stash, which lives in the shared `.base/.git` and
/// survives. The old residual-risk note asserted the top-level truth
/// universally.
#[test]
fn inspect_dirt_reports_self_contained_clone_holding_only_a_stash() {
    let fx = GitWorktreeFixture::new();
    let parent = seed_parent(&fx, "clone-stash");
    let clone = seed_self_contained_clone(&fx, &parent);
    std::fs::write(clone.join("a.txt"), "stash me\n").unwrap();
    git_ok(&clone, &["stash"]);

    // Premise: stashing left the working tree clean and HEAD pushed, so the
    // ONLY thing at risk is the stash itself.
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&clone)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "premise broken: `git stash` should have left a clean tree"
    );

    let dirt = inspect_dirt(&parent).expect("a nested clone's stash dies with it and must be seen");
    assert!(
        dirt.reason.contains("stash"),
        "the reason must name the stash; got: {}",
        dirt.reason
    );
}

/// A clone nobody ever pushed anywhere has no remote-tracking refs at all, so
/// `--not --remotes` excludes nothing and every commit counts. Conservative and
/// correct — this is the common "scratch clone" case.
#[test]
fn inspect_dirt_reports_remote_less_self_contained_clone() {
    let fx = GitWorktreeFixture::new();
    let parent = seed_parent(&fx, "clone-no-remote");
    let clone = parent.join("scratch").join("orphan");
    std::fs::create_dir_all(&clone).unwrap();
    git_ok(&clone, &["init", "--initial-branch=main"]);
    git_ok(&clone, &["config", "user.email", "ci@test.invalid"]);
    git_ok(&clone, &["config", "user.name", "CI"]);
    git_ok(&clone, &["config", "commit.gpgsign", "false"]);
    std::fs::write(clone.join("solo.txt"), "never pushed anywhere\n").unwrap();
    git_ok(&clone, &["add", "solo.txt"]);
    git_ok(&clone, &["commit", "-m", "solo"]);

    let dirt = inspect_dirt(&parent).expect("a remote-less nested clone must read as dirty");
    assert!(
        dirt.reason.contains("self-contained clone"),
        "unexpected reason: {}",
        dirt.reason
    );
}

/// THE COUNTERWEIGHT, and it matters as much as the finding: a self-contained
/// clone with everything pushed and nothing stashed must NOT pin the parent.
/// Without this the new model could "pass" every test above by calling every
/// nested clone dirty, which is a silent leak rather than a fix.
#[test]
fn inspect_dirt_allows_fully_pushed_self_contained_clone() {
    let fx = GitWorktreeFixture::new();
    let parent = seed_parent(&fx, "clone-clean");
    seed_self_contained_clone(&fx, &parent);

    assert!(
        inspect_dirt(&parent).is_none(),
        "a fully-pushed nested clone must not pin the parent; got {:?}",
        inspect_dirt(&parent)
    );
}

/// The discriminator, measured directly on a REGISTERED nested worktree: its
/// object store is the shared `.base/.git`, outside the candidate, and survives
/// removal. This is what keeps a clean nested agent worktree from pinning its
/// parent — behaviour exercised live by the seven nested worktrees under
/// `.base/.worktrees/2eb72dca-…`.
#[test]
fn object_store_dies_with_is_false_for_a_registered_worktree() {
    let fx = GitWorktreeFixture::new();
    let parent = seed_parent(&fx, "discriminator-registered");
    let nested = fx.add_nested_worktree(&parent, ".claude/worktrees", "agentA");
    let canonical = std::fs::canonicalize(&parent).unwrap();

    assert_eq!(
        object_store_dies_with(&nested, &canonical),
        Ok(false),
        "a registered worktree's store lives outside the candidate and survives it"
    );
}

/// The other half of the discriminator: a self-contained clone's store IS
/// inside the candidate, so everything it holds dies with the directory.
#[test]
fn object_store_dies_with_is_true_for_a_self_contained_clone() {
    let fx = GitWorktreeFixture::new();
    let parent = seed_parent(&fx, "discriminator-clone");
    let clone = seed_self_contained_clone(&fx, &parent);
    let canonical = std::fs::canonicalize(&parent).unwrap();

    assert_eq!(
        object_store_dies_with(&clone, &canonical),
        Ok(true),
        "a self-contained clone's store lives inside the candidate and dies with it"
    );
}

/// FAIL-SAFE: when the discriminator cannot answer, the nested root is DIRTY.
/// Not knowing which loss model applies means not knowing what removal costs.
#[test]
fn object_store_dies_with_errors_on_a_non_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let plain = tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&plain).unwrap();

    assert!(
        object_store_dies_with(&plain, tmp.path()).is_err(),
        "a directory git cannot speak for must not yield a loss-model verdict"
    );
}
