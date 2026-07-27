//! Shared REAL-git fixtures for the #4091 dirty-worktree-guard tests.
//!
//! Why: the guard's whole value is that it reads real `git status` /
//! `git rev-list` output, so a mocked git would test nothing. Both
//! `worktree_safety_tests` (the checker in isolation) and `prune_orphan_tests`
//! (the checker wired into the reclaim path) need the same non-trivial
//! setup — a checkout with a remote, a pushed `main`, and a per-session
//! worktree carrying an aged #3649 ownership sentinel so the owner gate lets
//! the candidate through to the dirty gate — so it is built once here rather
//! than twice.
//!
//! These fixtures are built entirely inside a fresh `tempfile::TempDir`.
//! Nothing here ever touches a real worktree; deleting a live worktree is the
//! exact failure #4091 exists to prevent.
//!
//! What: [`GitWorktreeFixture`], which owns the temp dir and exposes
//! `repos_root` (the `<repos_root>/<owner>/<repo>/` shape
//! `find_orphaned_worktrees` walks) plus helpers to add worktrees and dirty
//! them.
//! Test: exercised by every `#4091` test in `worktree_safety_tests` and
//! `prune_orphan_tests`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::record::ManagedSessionId;

/// A throwaway `<repos_root>/owner/repo` checkout with a bare remote (#4091).
///
/// Why: `prune_orphaned_worktrees` walks `<repos_root>/<owner>/<repo>/…`, the
/// `git worktree list` cross-check resolves the repo root as the candidate's
/// GRANDPARENT, and the unpushed-commit check needs real remote-tracking refs.
/// One fixture has to satisfy all three at once.
/// What: owns the `TempDir` (dropped = everything cleaned up) and exposes the
/// repos root and the checkout path.
/// Test: used by `inspect_dirt_clean_pushed_worktree_is_none` and friends.
pub(crate) struct GitWorktreeFixture {
    /// Kept alive so the temp tree outlives the test.
    _tmp: tempfile::TempDir,
    /// The `<repos_root>` that `prune_orphaned_worktrees` should be pointed at.
    pub repos_root: PathBuf,
    /// The checkout at `<repos_root>/owner/repo`.
    pub repo: PathBuf,
}

/// Run `git -C <dir> <args>`, panicking with git's own stderr on failure.
///
/// Why: a fixture step that silently no-ops produces a test that passes for
/// the wrong reason — exactly the "green by deleting coverage" failure mode.
/// What: asserts the command spawned AND exited zero.
fn git_ok(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
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

impl GitWorktreeFixture {
    /// Build a checkout on `main`, pushed to a bare remote in the same temp dir.
    ///
    /// Why: "clean and fully pushed" is the state the guard must still allow to
    /// be reclaimed, so the happy path needs a genuine remote-tracking ref to
    /// measure against — without one, every commit is legitimately unpushed and
    /// the guard would (correctly, but uninterestingly) skip everything.
    /// What: `git init` a bare remote and a working checkout, make one commit,
    /// push it, and fetch so `refs/remotes/origin/main` exists locally.
    /// Test: `inspect_dirt_clean_pushed_worktree_is_none`.
    pub(crate) fn new() -> Self {
        let tmp = tempfile::tempdir().expect("fixture: tempdir");
        let repos_root = tmp.path().join("repos");
        let repo = repos_root.join("owner").join("repo");
        let remote = tmp.path().join("remote.git");
        std::fs::create_dir_all(&repo).expect("fixture: create repo dir");
        std::fs::create_dir_all(&remote).expect("fixture: create remote dir");

        git_ok(&remote, &["init", "--bare", "--initial-branch=main"]);
        git_ok(&repo, &["init", "--initial-branch=main"]);
        git_ok(&repo, &["config", "user.email", "ci@test.invalid"]);
        git_ok(&repo, &["config", "user.name", "CI"]);
        git_ok(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("README.md"), "base\n").expect("fixture: write README");
        git_ok(&repo, &["add", "README.md"]);
        git_ok(&repo, &["commit", "-m", "base"]);
        git_ok(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("utf8 remote"),
            ],
        );
        git_ok(&repo, &["push", "origin", "main"]);
        git_ok(&repo, &["fetch", "origin"]);

        Self {
            _tmp: tmp,
            repos_root,
            repo,
        }
    }

    /// Add a real per-session worktree at `<repo>/.worktrees/<name>`.
    ///
    /// Why: the reclaim path only ever considers leaf dirs under a
    /// `.worktrees`-shaped parent, and `git_worktree_list_agrees` requires the
    /// path to be a genuinely registered worktree — a bare `mkdir` would be
    /// rejected before the dirty gate ever ran.
    /// What: `git worktree add -b session/<name>` off the current `HEAD`.
    /// Returns the worktree path.
    /// Test: `inspect_dirt_clean_pushed_worktree_is_none`.
    pub(crate) fn add_worktree(&self, name: &str) -> PathBuf {
        let wt = self.repo.join(".worktrees").join(name);
        git_ok(
            &self.repo,
            &[
                "worktree",
                "add",
                "-b",
                &format!("session/{name}"),
                wt.to_str().expect("utf8 worktree path"),
            ],
        );
        git_ok(&wt, &["config", "user.email", "ci@test.invalid"]);
        git_ok(&wt, &["config", "user.name", "CI"]);
        git_ok(&wt, &["config", "commit.gpgsign", "false"]);
        wt
    }

    /// Stamp `wt` with an ownership sentinel old enough to clear the #3649
    /// grace window, naming an owner that will never resolve to a record.
    ///
    /// Why: without this the #3649 gate classifies the candidate as
    /// owner-unknown and skips it BEFORE the #4091 dirty gate runs, so the
    /// test would pass without exercising anything. The sentinel is what makes
    /// these fixtures genuinely reclaimable, which is the only condition under
    /// which the dirty gate is load-bearing.
    /// What: writes an aged [`super::worktree_ownership::WorktreeSentinel`] for
    /// a fresh, never-registered [`ManagedSessionId`].
    /// Test: `prune_orphaned_worktrees_reclaims_clean_pushed_worktree`.
    pub(crate) fn stamp_reclaimable_sentinel(wt: &Path) {
        let aged = chrono::Utc::now()
            - super::worktree_ownership::OWNERLESS_GRACE
            - chrono::Duration::minutes(1);
        let payload = serde_json::to_vec(&super::worktree_ownership::WorktreeSentinel {
            owner_session_id: ManagedSessionId::new(),
            created_at: aged,
        })
        .expect("fixture: serialize sentinel");
        std::fs::write(
            wt.join(super::decommission::WORKTREE_SENTINEL_FILE),
            payload,
        )
        .expect("fixture: write sentinel");
    }

    /// Commit a new file inside `wt` without pushing it anywhere.
    ///
    /// Why: this is the "committed but unpushed" case — the one that looks
    /// safe (the work IS committed) but is destroyed anyway, because
    /// `decommission::remove_session_worktree` runs
    /// `git branch -D session/<leaf>` after a successful removal and takes the
    /// commit's last reachable ref with it.
    /// What: writes `unpushed.txt`, stages it, and commits.
    /// Test: `inspect_dirt_reports_unpushed_commit`.
    pub(crate) fn commit_unpushed(wt: &Path) {
        std::fs::write(wt.join("unpushed.txt"), "local only\n").expect("fixture: write file");
        git_ok(wt, &["add", "unpushed.txt"]);
        git_ok(wt, &["commit", "-m", "local only"]);
    }
}
