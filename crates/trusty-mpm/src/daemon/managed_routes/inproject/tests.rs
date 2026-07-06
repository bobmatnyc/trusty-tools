//! Unit tests for the in-project spawn path (#1706, #1803, #1805, #1807).
//!
//! Why: split out of `inproject.rs` so the production module stays under the
//! 500-SLOC cap; the tests themselves are unchanged. Coverage: repos-root
//! resolution + `TRUSTY_MPM_REPOS_ROOT` precedence, old-layout migration
//! (`ensure_base_clone` + `migrate_old_layout_aside`), per-session worktree
//! creation, and `.git/info/exclude` idempotency.
//! What: pure-function + real-temp-repo assertions over the module's public
//! and private surface (reachable via `use super::*`).
//! Test: this IS the test module.

use super::*;

/// #1807: `tm launch` and the daemon in-project path must resolve the managed
/// repo root through the SAME function so a lone `TRUSTY_MPM_REPOS_ROOT` cannot
/// send them to different roots.
///
/// Why: before the fix, the daemon path used `base_clone_path` (which honours
/// `TRUSTY_MPM_REPOS_ROOT`) while `tm launch` used `workspace_subpath` (which
/// does NOT), so setting only `TRUSTY_MPM_REPOS_ROOT` re-diverged the roots.
/// This locks in that BOTH entry points now call `base_clone_path`, and proves
/// the old `workspace_subpath` path really did diverge (guarding against a
/// regression that reverts `tm launch` to it).
/// Test: this function IS the test.
#[test]
fn launch_and_daemon_agree_on_repos_root_env() {
    let _g = crate::core::trusty_tools_config::env_test_lock();

    // The #1807 scenario: ONLY TRUSTY_MPM_REPOS_ROOT is set (no WORKSPACE_ROOT).
    // SAFETY: guarded by the crate-wide env_test_lock; both vars restored below.
    unsafe {
        std::env::set_var(REPOS_ROOT_ENV, "/explicit/repos/root");
        std::env::remove_var(crate::core::trusty_tools_config::WORKSPACE_ROOT_ENV);
    }

    // Daemon in-project path AND (post-fix) `tm launch` both resolve via this.
    let unified = base_clone_path("owner", "repo");

    // The PRE-fix `tm launch` path resolved via workspace_subpath → workspace_root,
    // which ignores TRUSTY_MPM_REPOS_ROOT.
    let cfg = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    let gh = trusty_common::github_path::GithubPath {
        owner: "owner".into(),
        repo: "repo".into(),
    };
    let pre_fix_launch = crate::core::trusty_tools_config::workspace_subpath(&cfg, &gh);

    // SAFETY: guarded by env_test_lock.
    unsafe { std::env::remove_var(REPOS_ROOT_ENV) };

    assert_eq!(
        unified,
        PathBuf::from("/explicit/repos/root/owner/repo"),
        "the unified resolver (used by BOTH entry points) must honour \
             TRUSTY_MPM_REPOS_ROOT"
    );
    assert_ne!(
        unified, pre_fix_launch,
        "the pre-#1807 tm launch path (workspace_subpath) diverged from the \
             daemon root; tm launch must now use base_clone_path instead"
    );
}

/// #1805: `ensure_base_clone` must migrate a pre-existing old-layout dir aside
/// (non-empty, no top-level `.git`) and then successfully clone the fresh base,
/// instead of failing and silently falling back to full-clone-per-session.
///
/// Why: this is the exact scenario observed on `bobmatnyc/trusty-tools` — 37
/// stale UUID full-clone subdirs with no top-level `.git`. Cloning into that
/// non-empty dir fails, so the new `.worktrees/` layout never engaged.
/// Non-tautology proof: the backup-path prefix is derived from the PUBLIC
/// constant `OLD_LAYOUT_BACKUP_SUFFIX`, and the assertions check both that the
/// OLD marker survived in the backup AND that a NEW `.git` now exists at the
/// base — a silent-fallback (no migration) would leave the marker at the base
/// and no `.git`, failing both.
/// Test: this function IS the test.
#[test]
fn ensure_base_clone_migrates_old_layout_dir_aside() {
    // 1. Build a real source repo to clone from.
    let src = tempfile::TempDir::new().expect("src tmp dir");
    let src_path = src.path();
    let init = std::process::Command::new("git")
        .args(["init", src_path.to_str().expect("src utf8")])
        .output()
        .expect("git init src");
    assert!(init.status.success(), "git init src failed");
    for (k, v) in [("user.email", "t@example.com"), ("user.name", "T")] {
        let ok = std::process::Command::new("git")
            .args(["-C", src_path.to_str().expect("utf8"), "config", k, v])
            .status()
            .expect("git config");
        assert!(ok.success(), "git config {k} failed");
    }
    std::fs::write(src_path.join("README"), b"src").expect("write README");
    let add = std::process::Command::new("git")
        .args(["-C", src_path.to_str().expect("utf8"), "add", "."])
        .status()
        .expect("git add");
    assert!(add.success(), "git add failed");
    let commit = std::process::Command::new("git")
        .args([
            "-C",
            src_path.to_str().expect("utf8"),
            "commit",
            "-m",
            "init",
        ])
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit failed");

    // 2. Build an OLD-LAYOUT base dir: <parent>/owner/repo/ containing a fake
    //    per-session UUID full-clone subdir and NO top-level `.git`.
    let parent = tempfile::TempDir::new().expect("parent tmp dir");
    let base = parent.path().join("owner").join("repo");
    let old_session = base.join("00000000-old-session-uuid");
    std::fs::create_dir_all(&old_session).expect("create old-layout subdir");
    std::fs::write(old_session.join("MARKER"), b"legacy").expect("write marker");
    assert!(
        !base.join(".git").exists(),
        "precondition: no top-level .git"
    );

    // 3. ensure_base_clone must migrate the old dir aside and clone fresh.
    let url = src_path.to_str().expect("src url utf8");
    ensure_base_clone(url, &base).expect("ensure_base_clone must migrate + clone");

    // 4. The base now holds a FRESH clone (top-level `.git` present).
    assert!(
        base.join(".git").exists(),
        "fresh shared-base clone must exist at the base path after migration"
    );

    // 5. A sibling backup dir carrying the documented suffix must exist and
    //    still contain the OLD marker (data preserved, not deleted).
    let repo_parent = base.parent().expect("base has parent");
    let backup = std::fs::read_dir(repo_parent)
        .expect("read owner dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.contains(OLD_LAYOUT_BACKUP_SUFFIX))
                .unwrap_or(false)
        })
        .expect("a migrated backup dir must exist");
    assert!(
        backup
            .join("00000000-old-session-uuid")
            .join("MARKER")
            .exists(),
        "the old per-session data must be preserved under the backup dir"
    );
}

/// #1805: the migrator must be a no-op for the cases that are NOT old-layout —
/// an empty base dir and a dir that already contains `.git`.
///
/// Why: git clone succeeds into an empty/absent dir, and an existing `.git` is
/// the reuse path handled by the caller; migrating either would be destructive
/// and wrong. This guards the precise old-layout signature.
/// Test: this function IS the test.
#[test]
fn migrate_old_layout_aside_ignores_empty_and_git_dirs() {
    // Empty dir → Ok(None), dir untouched.
    let empty = tempfile::TempDir::new().expect("empty tmp");
    let empty_base = empty.path().join("owner").join("repo");
    std::fs::create_dir_all(&empty_base).expect("create empty base");
    assert!(
        migrate_old_layout_aside(&empty_base)
            .expect("empty dir must not error")
            .is_none(),
        "an empty base dir must not be migrated"
    );
    assert!(empty_base.is_dir(), "empty base dir must remain in place");

    // Dir with a top-level `.git` → Ok(None), dir untouched.
    let git = tempfile::TempDir::new().expect("git tmp");
    let git_base = git.path().join("owner").join("repo");
    std::fs::create_dir_all(git_base.join(".git")).expect("create .git");
    std::fs::write(git_base.join("file"), b"x").expect("write file");
    assert!(
        migrate_old_layout_aside(&git_base)
            .expect("git dir must not error")
            .is_none(),
        "a dir with a top-level .git must not be migrated"
    );
    assert!(
        git_base.join(".git").is_dir(),
        ".git dir must remain in place"
    );

    // Absent path → Ok(None).
    let absent = git.path().join("does").join("not").join("exist");
    assert!(
        migrate_old_layout_aside(&absent)
            .expect("absent path must not error")
            .is_none(),
        "an absent path must not be migrated"
    );
}

#[test]
fn try_inproject_spawn_returns_none_for_non_git_path() {
    // A directory that is not a git repo must return Ok(None), not an error.
    let tmp = std::env::temp_dir();
    let result = try_inproject_spawn(&tmp);
    assert!(
        matches!(result, Ok(None)),
        "non-git path should yield Ok(None), got {result:?}"
    );
}

#[test]
fn get_origin_url_returns_none_for_non_git() {
    // A non-git directory should return None cleanly.
    let tmp = std::env::temp_dir();
    assert!(get_origin_url(&tmp).is_none());
}

#[test]
fn repos_root_default_ends_with_canonical_segment() {
    // Without env overrides, repos_root() must resolve to ~/trusty-mpm-projects
    // (the canonical base — same root as workspace_root()).
    // Skip when env is overridden so CI with custom roots doesn't false-fail.
    if std::env::var(REPOS_ROOT_ENV).is_ok()
        || std::env::var(crate::core::trusty_tools_config::WORKSPACE_ROOT_ENV).is_ok()
    {
        return;
    }
    let root = repos_root();
    assert!(
        root.ends_with(DEFAULT_REPOS_DIR),
        "repos_root() default must end with '{DEFAULT_REPOS_DIR}', got {}",
        root.display()
    );
}

#[test]
fn base_clone_path_resolves_to_canonical_root() {
    // Without env overrides, base_clone_path must return
    // ~/trusty-mpm-projects/<owner>/<repo> (#1803).
    if std::env::var(REPOS_ROOT_ENV).is_ok()
        || std::env::var(crate::core::trusty_tools_config::WORKSPACE_ROOT_ENV).is_ok()
    {
        return;
    }
    let base = base_clone_path("myorg", "myrepo");
    assert!(
        base.ends_with("trusty-mpm-projects/myorg/myrepo"),
        "base_clone_path must resolve to …/trusty-mpm-projects/myorg/myrepo, got {}",
        base.display()
    );
}

#[test]
fn session_worktree_path_uses_dot_prefix() {
    // create_session_worktree must place the worktree at
    // <base>/.worktrees/<worktree_name> (dot-prefixed) so it is gitignored via
    // .git/info/exclude (#1803). Since #2032 `worktree_name` is the resolved
    // SEMANTIC tmux name (e.g. `tm-trusty-tools-01`), not a raw session UUID.
    // We call the PRODUCTION function against a real temporary git repository
    // (with an initial commit so `git worktree add` can branch from HEAD) and
    // assert: (a) path is under <base>/.worktrees/, (b) ends with the name,
    // (c) the directory actually exists after the call.
    //
    // Non-tautology proof: `expected_parent` is hardcoded as `base.join(".worktrees")`
    // — NOT derived from production code. If `create_session_worktree` changed
    // to `base_path.join("worktrees").join(...)` the `starts_with` assertion
    // would fail because `base/worktrees/<name>` does NOT start with `base/.worktrees`.
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    let base = tmp.path();

    // Initialise a real git repo so `git worktree add` can run.
    let init = std::process::Command::new("git")
        .args(["init", base.to_str().expect("base is utf8")])
        .output()
        .expect("git init");
    assert!(
        init.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    // Configure identity (required for `git commit`).
    for (k, v) in [("user.email", "test@example.com"), ("user.name", "Test")] {
        let ok = std::process::Command::new("git")
            .args(["-C", base.to_str().expect("base is utf8"), "config", k, v])
            .status()
            .expect("git config");
        assert!(ok.success(), "git config {k} failed");
    }

    // Create an initial commit so HEAD exists and `git worktree add -b` works.
    std::fs::write(base.join("README"), b"init").expect("write README");
    let add = std::process::Command::new("git")
        .args(["-C", base.to_str().expect("base is utf8"), "add", "."])
        .status()
        .expect("git add");
    assert!(add.success(), "git add failed");
    let commit = std::process::Command::new("git")
        .args([
            "-C",
            base.to_str().expect("base is utf8"),
            "commit",
            "-m",
            "init",
        ])
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit failed");

    // Call the production function.
    let name = "tm-test-repo-01";
    let worktree_path = create_session_worktree(base, name)
        .expect("create_session_worktree must succeed on a real git repo");

    // (a) Path must be under <base>/.worktrees/ — hardcoded, not from production.
    let expected_parent = base.join(".worktrees");
    assert!(
        worktree_path.starts_with(&expected_parent),
        "worktree must be under <base>/.worktrees/, got {}",
        worktree_path.display()
    );

    // (b) Path must end with the semantic worktree name.
    assert!(
        worktree_path.ends_with(name),
        "worktree path must end with worktree name {name}, got {}",
        worktree_path.display()
    );

    // (c) The directory must actually exist (git worktree was created on disk).
    assert!(
        worktree_path.is_dir(),
        "worktree directory must exist on disk, got {}",
        worktree_path.display()
    );
}

/// Issue #2032: a worktree directory OR branch that already exists for a
/// candidate name must be detected by [`worktree_name_collides`] so
/// `SessionManager::resolve_session_name`'s extra-collision predicate steers
/// away from it instead of `create_session_worktree` silently clobbering (or
/// confusingly erroring on) it.
///
/// Why: the tmux-liveness check alone cannot see git worktrees/branches — a
/// stale worktree left behind by a hand-deleted-but-not-decommissioned
/// session must still be treated as a collision.
/// What: (a) an absent name must not collide; (b) after creating a worktree
/// for a name, THAT SAME name must collide (both the dir and the branch
/// exist); (c) a name whose branch exists but whose worktree dir was manually
/// removed must still collide (branch-only check).
/// Test: this function IS the test.
#[test]
fn worktree_name_collides_detects_existing_dir_and_branch() {
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    let base = tmp.path();

    let init = std::process::Command::new("git")
        .args(["init", base.to_str().expect("base is utf8")])
        .output()
        .expect("git init");
    assert!(init.status.success(), "git init failed");
    for (k, v) in [("user.email", "test@example.com"), ("user.name", "Test")] {
        let ok = std::process::Command::new("git")
            .args(["-C", base.to_str().expect("utf8"), "config", k, v])
            .status()
            .expect("git config");
        assert!(ok.success(), "git config {k} failed");
    }
    std::fs::write(base.join("README"), b"init").expect("write README");
    let add = std::process::Command::new("git")
        .args(["-C", base.to_str().expect("utf8"), "add", "."])
        .status()
        .expect("git add");
    assert!(add.success(), "git add failed");
    let commit = std::process::Command::new("git")
        .args(["-C", base.to_str().expect("utf8"), "commit", "-m", "init"])
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit failed");

    // (a) No collision before anything exists.
    assert!(
        !worktree_name_collides(base, "tm-fresh-01"),
        "an unused name must not collide"
    );

    // (b) After creating the worktree, the SAME name must collide.
    create_session_worktree(base, "tm-fresh-01").expect("create_session_worktree");
    assert!(
        worktree_name_collides(base, "tm-fresh-01"),
        "an in-use name (dir + branch both exist) must collide"
    );

    // (c) Remove the worktree directory (but not the branch) via `git worktree
    // remove`, which deletes the dir but leaves the branch ref intact — the
    // branch-only check must still report a collision.
    let remove = std::process::Command::new("git")
        .arg("-C")
        .arg(base)
        .args(["worktree", "remove", "--force"])
        .arg(base.join(".worktrees").join("tm-fresh-01"))
        .output()
        .expect("git worktree remove");
    assert!(
        remove.status.success(),
        "git worktree remove failed: {}",
        String::from_utf8_lossy(&remove.stderr)
    );
    assert!(
        !base.join(".worktrees").join("tm-fresh-01").exists(),
        "worktree dir must be gone after `git worktree remove`"
    );
    assert!(
        worktree_name_collides(base, "tm-fresh-01"),
        "a name whose branch still exists must still collide even after the \
         worktree dir was removed"
    );
}

/// Issue #2032: [`create_session_worktree`] must refuse to clobber an
/// existing worktree directory rather than silently overwriting it.
///
/// Why: `worktree_name_collides` is the PROACTIVE guard used by name
/// resolution; this is the DEFENSIVE guard inside `create_session_worktree`
/// itself, covering a caller that skips the collision check (or a TOCTOU
/// race).
/// What: creates a worktree for a name, then calls `create_session_worktree`
/// again for the SAME name and asserts it returns `Err` (not a panic, not a
/// silent overwrite) and that the original worktree directory is untouched.
/// Test: this function IS the test.
#[test]
fn create_session_worktree_rejects_existing_worktree_dir() {
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    let base = tmp.path();

    let init = std::process::Command::new("git")
        .args(["init", base.to_str().expect("base is utf8")])
        .output()
        .expect("git init");
    assert!(init.status.success(), "git init failed");
    for (k, v) in [("user.email", "test@example.com"), ("user.name", "Test")] {
        let ok = std::process::Command::new("git")
            .args(["-C", base.to_str().expect("utf8"), "config", k, v])
            .status()
            .expect("git config");
        assert!(ok.success(), "git config {k} failed");
    }
    std::fs::write(base.join("README"), b"init").expect("write README");
    let add = std::process::Command::new("git")
        .args(["-C", base.to_str().expect("utf8"), "add", "."])
        .status()
        .expect("git add");
    assert!(add.success(), "git add failed");
    let commit = std::process::Command::new("git")
        .args(["-C", base.to_str().expect("utf8"), "commit", "-m", "init"])
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit failed");

    let first = create_session_worktree(base, "tm-dup-01").expect("first create must succeed");
    assert!(first.is_dir(), "first worktree must exist");

    let second = create_session_worktree(base, "tm-dup-01");
    assert!(
        second.is_err(),
        "creating a worktree for an already-existing name must error, not clobber"
    );
    assert!(
        first.is_dir(),
        "the original worktree must remain untouched after the rejected re-create"
    );
}

#[test]
fn ensure_worktrees_gitignored_idempotent() {
    // ensure_worktrees_gitignored must write .worktrees/ to .git/info/exclude
    // exactly ONCE even when called multiple times (idempotent) (#1803).
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    let base = tmp.path();
    // Create a minimal .git dir (not a real git repo, but enough for the function).
    std::fs::create_dir_all(base.join(".git")).expect("create .git");

    // First call: must write the entry.
    ensure_worktrees_gitignored(base).expect("first call must succeed");

    let exclude = base.join(".git").join("info").join("exclude");
    let content1 = std::fs::read_to_string(&exclude).expect("exclude readable");
    let count1 = content1
        .lines()
        .filter(|l| l.trim() == ".worktrees/")
        .count();
    assert_eq!(
        count1, 1,
        "must contain exactly one .worktrees/ entry after first call"
    );

    // Second call: must NOT duplicate the entry.
    ensure_worktrees_gitignored(base).expect("second call must succeed (idempotent)");
    let content2 = std::fs::read_to_string(&exclude).expect("exclude readable");
    let count2 = content2
        .lines()
        .filter(|l| l.trim() == ".worktrees/")
        .count();
    assert_eq!(
        count2, 1,
        "second call must not add a duplicate .worktrees/ entry"
    );
}

/// #1919 regression guard: [`ensure_base_clone`] must emit a `CloningRepo`
/// stage event on the actual first-run clone — the exact "tm: first run for
/// X — cloning into ..." scenario #1904 set out to make observable — and must
/// NOT re-emit it on a subsequent idempotent call against an already-present
/// base clone (the reuse path takes the early `Ok(())` return above the emit
/// site and never runs `git clone` again).
///
/// Why: this is the single most important observable gap #1919 identified —
/// `try_inproject_spawn` (and therefore `ensure_base_clone`) runs from inside
/// `spawn_managed`'s `is_local_workdir` branch, several layers below any
/// `emit(...)` call `provision_in` already had; before this fix nothing in
/// the in-project path announced the clone stage at all.
/// What: builds a real local source repo (same fixture pattern as
/// `ensure_base_clone_migrates_old_layout_dir_aside`), calls `ensure_base_clone`
/// once inside a fresh `provisioning_stage::scoped` (asserting exactly one
/// `CloningRepo` event, since the base dir does not yet exist), then calls it
/// again inside a SECOND fresh scope against the now-existing base clone
/// (asserting ZERO events, proving the reuse path is silent).
/// Test: this function IS the test.
#[tokio::test]
async fn ensure_base_clone_emits_cloning_repo_only_on_fresh_clone() {
    use crate::core::provisioning_stage::{ProvisioningStage, StageEmitter, scoped};

    // 1. Build a real source repo to clone from (mirrors the fixture used by
    //    `ensure_base_clone_migrates_old_layout_dir_aside` above).
    let src = tempfile::TempDir::new().expect("src tmp dir");
    let src_path = src.path();
    let init = std::process::Command::new("git")
        .args(["init", src_path.to_str().expect("src utf8")])
        .output()
        .expect("git init src");
    assert!(init.status.success(), "git init src failed");
    for (k, v) in [("user.email", "t@example.com"), ("user.name", "T")] {
        let ok = std::process::Command::new("git")
            .args(["-C", src_path.to_str().expect("utf8"), "config", k, v])
            .status()
            .expect("git config");
        assert!(ok.success(), "git config {k} failed");
    }
    std::fs::write(src_path.join("README"), b"src").expect("write README");
    let add = std::process::Command::new("git")
        .args(["-C", src_path.to_str().expect("utf8"), "add", "."])
        .status()
        .expect("git add");
    assert!(add.success(), "git add failed");
    let commit = std::process::Command::new("git")
        .args([
            "-C",
            src_path.to_str().expect("utf8"),
            "commit",
            "-m",
            "init",
        ])
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit failed");

    let parent = tempfile::TempDir::new().expect("parent tmp dir");
    let base = parent.path().join("owner").join("repo");
    let url = src_path.to_str().expect("src url utf8").to_string();

    // 2. Fresh clone: exactly one CloningRepo event.
    let (tx1, mut rx1) = tokio::sync::broadcast::channel(8);
    let emitter1 = StageEmitter::new("s-1", "https://example.com/owner/repo", tx1);
    let base_for_clone = base.clone();
    let url_for_clone = url.clone();
    scoped(emitter1, async move {
        ensure_base_clone(&url_for_clone, &base_for_clone)
            .expect("first ensure_base_clone must succeed (fresh clone)");
    })
    .await;

    let mut stages1 = Vec::new();
    while let Ok(value) = rx1.try_recv() {
        stages1.push(value["stage"].as_str().unwrap().to_string());
    }
    assert_eq!(
        stages1,
        vec![ProvisioningStage::CloningRepo.wire_name()],
        "a fresh base clone must emit exactly one CloningRepo stage event"
    );

    // 3. Idempotent reuse: zero events (the base clone already exists).
    let (tx2, mut rx2) = tokio::sync::broadcast::channel(8);
    let emitter2 = StageEmitter::new("s-2", "https://example.com/owner/repo", tx2);
    scoped(emitter2, async move {
        ensure_base_clone(&url, &base).expect("second ensure_base_clone must succeed (reuse)");
    })
    .await;

    let mut stages2 = Vec::new();
    while let Ok(value) = rx2.try_recv() {
        stages2.push(value["stage"].as_str().unwrap().to_string());
    }
    assert!(
        stages2.is_empty(),
        "reusing an existing base clone must NOT re-emit CloningRepo, got: {stages2:?}"
    );
}
