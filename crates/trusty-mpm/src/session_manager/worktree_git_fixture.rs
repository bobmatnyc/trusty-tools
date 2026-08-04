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
//! `repos_root` (the `<repos_root>/<owner>/<repo>/` layout
//! `find_orphaned_worktrees` is pointed at) plus helpers to add worktrees, park
//! them outside the project, lock them, and dirty them.
//! Test: exercised by every `#4091` test in `worktree_safety_tests` and
//! `prune_orphan_tests`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::record::ManagedSessionId;

/// A throwaway `<repos_root>/owner/repo` checkout with a bare remote (#4091).
///
/// Why: `prune_orphaned_worktrees` enumerates per managed project under
/// `<repos_root>/<owner>/<repo>/`, the `git worktree list` cross-check resolves
/// the owning repo from the candidate itself (#4207 — it used to guess the
/// candidate's GRANDPARENT), and the unpushed-commit check needs real
/// remote-tracking refs. One fixture has to satisfy all three at once.
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

/// Restores a path's permission bits when it drops, pass or panic (#4732).
///
/// Why: the "git cannot read this" tests work by `chmod 000`-ing a real `.git`
/// entry. A mode left behind survives the assertion that unwinds past it and
/// can defeat `TempDir`'s own cleanup, so the restore has to be a `Drop`, not a
/// line at the end of the test.
/// What: captures the current mode on construction and re-applies it on drop.
/// Test: `an_unreadable_git_entry_is_protected`,
/// `remove_refuses_an_unreadable_git_entry`.
pub(crate) struct RestoreMode {
    path: PathBuf,
    mode: u32,
}

impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(self.mode));
    }
}

/// `chmod 000 <path>`, restored when the returned guard drops (#4732).
pub(crate) fn deny_all(path: &Path) -> RestoreMode {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::symlink_metadata(path)
        .expect("fixture: stat before chmod")
        .permissions()
        .mode();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000))
        .expect("fixture: chmod 000");
    RestoreMode {
        path: path.to_path_buf(),
        mode,
    }
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
        // #4207: canonicalize the root. On macOS `tempfile` hands back a path
        // under `/var`, which is a symlink to `/private/var`; git records the
        // RESOLVED path in its worktree registry, so a fixture that kept the
        // unresolved form would compare git-reported paths against a different
        // spelling of the same directory and fail for a reason unrelated to
        // what any of these tests are about.
        let tmp_root = std::fs::canonicalize(tmp.path()).unwrap_or_else(|_| tmp.path().into());
        let repos_root = tmp_root.join("repos");
        let repo = repos_root.join("owner").join("repo");
        let remote = tmp_root.join("remote.git");
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
    /// Why: `.worktrees/<name>` is where the provisioner actually parks a
    /// per-session worktree, so this is the ordinary shape most tests want, and
    /// `git_worktree_list_agrees` requires the path to be a genuinely
    /// registered worktree — a bare `mkdir` would be rejected before the dirty
    /// gate ever ran. Note the reclaim path is no longer LIMITED to this shape
    /// (#4207); use [`Self::add_worktree_at`] to prove that.
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

    /// Add a real worktree at `<parent>/<name>`, anywhere on disk (#4207).
    ///
    /// Why: [`Self::add_worktree`] parks every worktree at
    /// `<repo>/.worktrees/<name>`, whose grandparent happens to be the owning
    /// checkout — so it cannot distinguish git-derived ownership from the
    /// grandparent inference #4207 replaces, nor prove that enumeration is
    /// independent of location. This helper takes the parent directory
    /// explicitly so a test can park a worktree somewhere no hard-coded shape
    /// covers, or outside the repos root entirely.
    /// What: `git worktree add -b wt/<name> <parent>/<name>` off the current
    /// `HEAD`, creating `<parent>` if needed. Returns the worktree path.
    /// Test: `enumerate_finds_worktree_at_an_unwalked_location`,
    /// `registry_root_for_ignores_where_the_worktree_is_parked`.
    pub(crate) fn add_worktree_at(&self, parent: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(parent).expect("fixture: create worktree parent");
        let wt = parent.join(name);
        git_ok(
            &self.repo,
            &[
                "worktree",
                "add",
                "-b",
                &format!("wt/{name}"),
                wt.to_str().expect("utf8 worktree path"),
            ],
        );
        wt
    }

    /// Mark `wt` as git-`locked` — the operator's explicit "do not remove
    /// this" (#4224 review).
    ///
    /// Why: git itself honours the lock — `git worktree remove --force` exits
    /// 128 rather than removing it. Until #4732
    /// `decommission::remove_session_worktree` treated that non-zero exit as a
    /// git failure and fell back to `std::fs::remove_dir_all`, deleting the
    /// directory regardless; both the enumeration filter and the remover now
    /// honour it. Proving either requires a really-locked worktree; setting the
    /// flag on a parsed record instead would test the parser, not the gate.
    /// What: `git -C <repo> worktree lock <wt>`.
    /// Test: `enumerate_excludes_a_locked_worktree`,
    /// `remove_session_worktree_refuses_a_git_locked_worktree`,
    /// `a_locked_worktree_is_protected`.
    pub(crate) fn lock_worktree(&self, wt: &Path) {
        git_ok(
            &self.repo,
            &["worktree", "lock", wt.to_str().expect("utf8 worktree path")],
        );
    }

    /// Create a bare `.base` clone inside the checkout and register a worktree
    /// in ITS registry (#4207).
    ///
    /// Why: a managed project has two checkouts that can own a worktree
    /// registry, and `enumerate_registered_worktrees` interrogates both. Without
    /// a fixture for the `.base` anchor, only the project-checkout half of that
    /// contract is covered.
    /// What: `git clone --bare` the checkout to `<repo>/.base`, then
    /// `git -C <repo>/.base worktree add` at `<repo>/.base/.worktrees/<name>`.
    /// Returns the worktree path.
    /// Test: `enumerate_finds_worktree_registered_to_base_clone`.
    pub(crate) fn add_base_clone_worktree(&self, name: &str) -> PathBuf {
        let base = self.repo.join(".base");
        git_ok(
            &self.repo,
            &[
                "clone",
                "--bare",
                ".",
                base.to_str().expect("utf8 base clone path"),
            ],
        );
        let wt = base.join(".worktrees").join(name);
        git_ok(
            &base,
            &[
                "worktree",
                "add",
                "-b",
                &format!("session/{name}"),
                wt.to_str().expect("utf8 worktree path"),
            ],
        );
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

    /// Add a nested git worktree of the SAME repo INSIDE `parent`, at
    /// `<parent>/<rel>` (#4118).
    ///
    /// Why: this is the shape that made the guard bypassable — Claude Code's
    /// `isolation: "worktree"` agents create `.claude/worktrees/<name>` inside a
    /// session checkout, this repo's `main` gitignores that path, and seven of
    /// them were live inside `.base/.worktrees/2eb72dca-…` while the parent
    /// reported `dirty_files = 0`.
    /// What: `git worktree add -b agent/<name>` under `parent`. Returns the
    /// nested worktree path.
    /// Test: `inspect_dirt_reports_nested_gitignored_worktree`.
    pub(crate) fn add_nested_worktree(&self, parent: &Path, rel: &str, name: &str) -> PathBuf {
        let nested = parent.join(rel).join(name);
        git_ok(
            &self.repo,
            &[
                "worktree",
                "add",
                "-b",
                &format!("agent/{name}"),
                nested.to_str().expect("utf8 nested worktree path"),
            ],
        );
        git_ok(&nested, &["config", "user.email", "ci@test.invalid"]);
        git_ok(&nested, &["config", "user.name", "CI"]);
        git_ok(&nested, &["config", "commit.gpgsign", "false"]);
        nested
    }

    /// Commit everything in `wt` and push it, so the worktree reads CLEAN.
    ///
    /// Why: several #4118 tests need a parent worktree whose ONLY dirt is the
    /// thing under test (a nested checkout, a `.trusty-mpm/` artefact). Setting
    /// those up requires committing a `.gitignore` first, and an uncommitted or
    /// unpushed `.gitignore` would make the parent dirty for the wrong reason —
    /// the test would pass without exercising anything.
    /// What: stages all, commits, pushes the current branch to `origin` under
    /// its own name, and fetches so the remote-tracking ref exists locally.
    /// Test: `inspect_dirt_reports_nested_gitignored_worktree`.
    pub(crate) fn commit_all_and_push(wt: &Path, msg: &str) {
        git_ok(wt, &["add", "-A"]);
        git_ok(wt, &["commit", "-m", msg]);
        git_ok(wt, &["push", "origin", "HEAD"]);
        git_ok(wt, &["fetch", "origin"]);
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
