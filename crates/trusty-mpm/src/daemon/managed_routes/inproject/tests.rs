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
    let src = crate::test_support::hermetic_temp_dir();
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
    let parent = crate::test_support::hermetic_temp_dir();
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
    let empty = crate::test_support::hermetic_temp_dir();
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
    let git = crate::test_support::hermetic_temp_dir();
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

/// #4270: a project directory holding a legacy `.base` store must be refused,
/// never renamed aside.
///
/// Why: `.base` matches the old-layout signature exactly — non-empty, no
/// top-level `.git` — but it is a real bare repository owning every
/// `.base/.worktrees/<id>` worktree beneath it, any of which may belong to a
/// live session. Renaming the parent orphans all of them at once. This is the
/// same failure #3605 caused one level down, and the owner ruling makes `.base`
/// cleanup a manual step, so the correct behaviour is a loud refusal that moves
/// nothing.
/// What: pre-seeds `<base>/.base/.worktrees/live/IN-USE.txt`, calls
/// `migrate_old_layout_aside`, and asserts it returns `Err` naming `.base` while
/// the marker file survives byte-for-byte and no backup sibling was created.
/// Test: this function IS the test.
#[test]
fn migrate_old_layout_aside_refuses_a_dir_holding_a_dot_base_store() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let base = tmp.path().join("owner").join("repo");
    let live = base.join(".base").join(".worktrees").join("live");
    std::fs::create_dir_all(&live).expect("create legacy worktree");
    let marker = live.join("IN-USE.txt");
    std::fs::write(&marker, b"live session").expect("write marker");

    let err = migrate_old_layout_aside(&base)
        .expect_err("a dir holding a .base store must be refused, not migrated");
    assert!(
        err.contains(".base"),
        "the refusal must name the legacy store, got: {err}"
    );

    assert!(
        base.is_dir(),
        "the project dir must stay exactly where it is"
    );
    assert_eq!(
        std::fs::read(&marker).expect("the legacy worktree must survive untouched"),
        b"live session",
        "#4270: an existing .base worktree must not be disturbed, relocated, or deleted"
    );
    let siblings: Vec<_> = std::fs::read_dir(base.parent().unwrap())
        .expect("read owner dir")
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    assert_eq!(
        siblings.len(),
        1,
        "no backup sibling may be created, got {siblings:?}"
    );
}

/// #4270: a project directory holding live worktrees must be refused too, not
/// just one holding `.base`.
///
/// Why: the first round of this fix guarded `.base` and left `.worktrees`
/// open — the same hole one name over. A project directory whose `.git` is
/// absent OR merely unreadable still matches the old-layout signature, so it
/// gets renamed whole, orphaning every session worktree beneath it. The guard
/// has to key on the class, not on one member of it.
/// What: pre-seeds `<base>/.worktrees/sess-1/IN-USE.txt` with no `.git` at all,
/// calls `migrate_old_layout_aside`, and asserts it returns `Err` naming
/// `.worktrees` while the marker survives and no backup sibling appears.
/// Test: this function IS the test.
#[test]
fn migrate_old_layout_aside_refuses_a_dir_holding_live_worktrees() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let base = tmp.path().join("owner").join("repo");
    let live = base.join(".worktrees").join("sess-1");
    std::fs::create_dir_all(&live).expect("create live worktree");
    let marker = live.join("IN-USE.txt");
    std::fs::write(&marker, b"live session").expect("write marker");

    let err = migrate_old_layout_aside(&base)
        .expect_err("a dir holding live worktrees must be refused, not migrated");
    assert!(
        err.contains(".worktrees"),
        "the refusal must name what it found, got: {err}"
    );

    assert!(
        base.is_dir(),
        "the project dir must stay exactly where it is"
    );
    assert_eq!(
        std::fs::read(&marker).expect("the live worktree must survive untouched"),
        b"live session",
        "#4270: a live session worktree must not be disturbed, relocated, or deleted"
    );
    let siblings: Vec<_> = std::fs::read_dir(base.parent().unwrap())
        .expect("read owner dir")
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    assert_eq!(
        siblings.len(),
        1,
        "no backup sibling may be created, got {siblings:?}"
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
    // A non-git directory is an ANSWER, not a failure: git-config exits 1 for a
    // key it cannot find, and #4734 maps exactly that code to Ok(None).
    let tmp = std::env::temp_dir();
    let result = get_origin_url(&tmp);
    assert!(
        matches!(result, Ok(None)),
        "non-git dir must be Ok(None), got {result:?}"
    );
}

/// A repo with no `origin` remote is `Ok(None)` — the fall-through by design.
///
/// Why: #4734 splits git failures out of this function's `None`, and the split
/// is only correct if the legitimate no-remote case stays on the `Ok` side. If
/// it drifted to `Err`, every fall-through caller (`try_inproject_spawn`,
/// `spawn_managed_local`, `tm launch`) would start failing repos that are
/// merely un-remoted.
/// What: `git init`s a repo, adds no remote, asserts `Ok(None)`.
/// Test: this function IS the test.
#[test]
fn get_origin_url_returns_none_for_repo_without_origin() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let init = std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(tmp.path())
        .output()
        .expect("git init");
    assert!(init.status.success(), "git init failed");

    let result = get_origin_url(tmp.path());
    assert!(
        matches!(result, Ok(None)),
        "repo without origin must be Ok(None), got {result:?}"
    );
}

/// A git invocation that fails outright is `Err`, never `Ok(None)` (#4734).
///
/// Why: this is the fail-open the ticket is about. `try_inproject_spawn` reads
/// `Ok(None)` as "no GitHub remote here" and spawns the session directly in the
/// operator's live checkout — so a config git refuses to parse used to cost the
/// session its worktree isolation, its protected base clone, and its push guard.
/// What: `git init`s a real repo (so `.git` genuinely exists), then overwrites
/// `.git/config` with a line git cannot parse. `git config --get` answers that
/// with exit 128, which must surface as `Err`.
/// Test: this function IS the test.
#[test]
fn get_origin_url_errors_when_git_config_is_unreadable() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let init = std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(tmp.path())
        .output()
        .expect("git init");
    assert!(init.status.success(), "git init failed");

    // An unterminated section header — `fatal: bad config line 1`, exit 128.
    std::fs::write(tmp.path().join(".git/config"), "[remote \"origin\"\n")
        .expect("write broken config");

    let result = get_origin_url(tmp.path());
    assert!(
        result.is_err(),
        "an unreadable git config must be Err, not a silent Ok(None): {result:?}"
    );
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

/// Why (#5204): `worktree_path_for` is the CREATION site — the one place that
/// decides where a new worktree lands. Before this change it joined the
/// `".worktrees"` literal, so a configured base was honoured nowhere and
/// creation silently disagreed with every detection site. This pins that it now
/// builds its path from the resolved base.
/// What: with `TRUSTY_MPM_WORKTREES_DIRNAME` set, the returned path nests under
/// the CONFIGURED segment and no longer under `.worktrees`; with it cleared, the
/// default is unchanged.
///
/// Non-tautology proof: both expectations are spelled out as literals here
/// (`base/.sessions/<name>` and `base/.worktrees/<name>`), not derived from the
/// production resolver. A `worktree_path_for` that ignored config would produce
/// `base/.worktrees/<name>` in the first case and fail the inequality.
/// Test: itself.
#[test]
fn worktree_path_for_honours_configured_base() {
    let _g = crate::core::trusty_tools_config::env_test_lock();
    let base = std::path::Path::new("/tmp/some-checkout");

    // SAFETY: serialised by the crate-wide env lock; cleared below.
    unsafe { std::env::set_var("TRUSTY_MPM_WORKTREES_DIRNAME", ".sessions") };
    let configured = super::worktree_path_for(base, "tm-widget-01");
    // SAFETY: as above.
    unsafe { std::env::remove_var("TRUSTY_MPM_WORKTREES_DIRNAME") };
    let defaulted = super::worktree_path_for(base, "tm-widget-01");

    assert_eq!(
        configured,
        std::path::PathBuf::from("/tmp/some-checkout/.sessions/tm-widget-01"),
        "creation must nest under the configured base"
    );
    assert!(
        !configured.starts_with("/tmp/some-checkout/.worktrees"),
        "a configured base must actually replace `.worktrees`, got {}",
        configured.display()
    );
    assert_eq!(
        defaulted,
        std::path::PathBuf::from("/tmp/some-checkout/.worktrees/tm-widget-01"),
        "with nothing configured the default must be unchanged"
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
    let tmp = crate::test_support::hermetic_temp_dir();
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
    let worktree_path =
        create_session_worktree(base, name, &crate::session_manager::ManagedSessionId::new())
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
    let tmp = crate::test_support::hermetic_temp_dir();
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
    create_session_worktree(
        base,
        "tm-fresh-01",
        &crate::session_manager::ManagedSessionId::new(),
    )
    .expect("create_session_worktree");
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
    let tmp = crate::test_support::hermetic_temp_dir();
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

    let first = create_session_worktree(
        base,
        "tm-dup-01",
        &crate::session_manager::ManagedSessionId::new(),
    )
    .expect("first create must succeed");
    assert!(first.is_dir(), "first worktree must exist");

    let second = create_session_worktree(
        base,
        "tm-dup-01",
        &crate::session_manager::ManagedSessionId::new(),
    );
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
    let tmp = crate::test_support::hermetic_temp_dir();
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
    let src = crate::test_support::hermetic_temp_dir();
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

    let parent = crate::test_support::hermetic_temp_dir();
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

/// #2189: [`create_session_worktree`] must leave the new session branch with a
/// working `git pull` (upstream tracking `origin/<default>`) WITHOUT letting a
/// bare `git push` target the default branch.
///
/// Why: before this fix the new `session/<name>` branch had no upstream at
/// all, so `git pull` inside a session worktree failed with "There is no
/// tracking information for the current branch." Naively setting the
/// upstream to `origin/<default>` would fix `pull` but ALSO make a bare `git
/// push` try to push session commits onto the shared default branch — this
/// test proves both halves: pull-tracking works AND push is scoped to the
/// worktree's own branch.
/// What: builds a real bare `origin` remote with one commit on `main`, clones
/// it into `base` (mirroring what `ensure_base_clone` produces in
/// production — `origin` wired up, `refs/remotes/origin/HEAD` set), then
/// calls the production `create_session_worktree` and asserts: (a) `git -C
/// <worktree> rev-parse --abbrev-ref --symbolic-full-name @{u}` resolves to
/// `origin/main`; (b) `git -C <worktree> config --worktree push.default` ==
/// `"current"`; (c) `git -C <base> config extensions.worktreeConfig` ==
/// `"true"`.
/// Test: this function IS the test.
#[test]
fn create_session_worktree_sets_pull_upstream_and_worktree_scoped_push() {
    // 1. A real bare `origin` remote, explicitly on `main` so the assertions
    //    below are not at the mercy of the host's `init.defaultBranch`.
    let origin_dir = crate::test_support::hermetic_temp_dir();
    let origin_path = origin_dir.path();
    let init = std::process::Command::new("git")
        .args([
            "init",
            "--bare",
            "-b",
            "main",
            origin_path.to_str().expect("origin utf8"),
        ])
        .output()
        .expect("git init --bare origin");
    assert!(
        init.status.success(),
        "git init --bare origin failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    // 2. Seed the bare origin with one commit via a scratch clone (a bare repo
    //    has no working tree to commit into directly).
    let seed = crate::test_support::hermetic_temp_dir();
    let seed_path = seed.path();
    let clone_seed = std::process::Command::new("git")
        .args([
            "clone",
            origin_path.to_str().expect("origin utf8"),
            seed_path.to_str().expect("seed utf8"),
        ])
        .output()
        .expect("git clone seed");
    assert!(
        clone_seed.status.success(),
        "git clone seed failed: {}",
        String::from_utf8_lossy(&clone_seed.stderr)
    );
    for (k, v) in [("user.email", "t@example.com"), ("user.name", "T")] {
        let ok = std::process::Command::new("git")
            .args(["-C", seed_path.to_str().expect("seed utf8"), "config", k, v])
            .status()
            .expect("git config");
        assert!(ok.success(), "git config {k} failed");
    }
    std::fs::write(seed_path.join("README"), b"seed").expect("write README");
    let add = std::process::Command::new("git")
        .args(["-C", seed_path.to_str().expect("seed utf8"), "add", "."])
        .status()
        .expect("git add");
    assert!(add.success(), "git add failed");
    let commit = std::process::Command::new("git")
        .args([
            "-C",
            seed_path.to_str().expect("seed utf8"),
            "commit",
            "-m",
            "init",
        ])
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit failed");
    let push_seed = std::process::Command::new("git")
        .args([
            "-C",
            seed_path.to_str().expect("seed utf8"),
            "push",
            "origin",
            "main",
        ])
        .status()
        .expect("git push seed");
    assert!(push_seed.success(), "git push seed failed");

    // 3. Clone the now-non-empty bare origin into `base` — this mirrors what
    //    `ensure_base_clone` produces in production: `origin` remote wired up
    //    and `refs/remotes/origin/HEAD` set by the clone itself.
    let base_parent = crate::test_support::hermetic_temp_dir();
    let base = base_parent.path().join("base");
    let clone_base = std::process::Command::new("git")
        .args([
            "clone",
            origin_path.to_str().expect("origin utf8"),
            base.to_str().expect("base utf8"),
        ])
        .output()
        .expect("git clone base");
    assert!(
        clone_base.status.success(),
        "git clone base failed: {}",
        String::from_utf8_lossy(&clone_base.stderr)
    );
    for (k, v) in [("user.email", "t@example.com"), ("user.name", "T")] {
        let ok = std::process::Command::new("git")
            .args(["-C", base.to_str().expect("base utf8"), "config", k, v])
            .status()
            .expect("git config");
        assert!(ok.success(), "git config {k} failed");
    }

    // 4. Call the production function under test.
    let worktree_path = create_session_worktree(
        &base,
        "tm-pull-01",
        &crate::session_manager::ManagedSessionId::new(),
    )
    .expect("create_session_worktree must succeed");

    // (a) `git pull` must work: @{u} resolves to origin/main.
    let upstream = std::process::Command::new("git")
        .arg("-C")
        .arg(&worktree_path)
        .args(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
        .output()
        .expect("git rev-parse @{u}");
    assert!(
        upstream.status.success(),
        "worktree branch must have an upstream set (git pull must work): {}",
        String::from_utf8_lossy(&upstream.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&upstream.stdout).trim(),
        "origin/main",
        "worktree branch upstream must track origin/main"
    );

    // (b) push.default=current must be scoped to THIS worktree only, so a
    // bare `git push` targets `origin/session/tm-pull-01`, never `main`.
    let push_default = std::process::Command::new("git")
        .arg("-C")
        .arg(&worktree_path)
        .args(["config", "--worktree", "push.default"])
        .output()
        .expect("git config --worktree push.default");
    assert!(
        push_default.status.success(),
        "worktree-scoped push.default must be set: {}",
        String::from_utf8_lossy(&push_default.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&push_default.stdout).trim(),
        "current",
        "push.default must be worktree-scoped to 'current' so push never targets main"
    );

    // (c) extensions.worktreeConfig must be enabled on the base clone —
    // required for the worktree-scoped config in (b) to take effect at all.
    let ext = std::process::Command::new("git")
        .arg("-C")
        .arg(&base)
        .args(["config", "extensions.worktreeConfig"])
        .output()
        .expect("git config extensions.worktreeConfig");
    assert!(
        ext.status.success(),
        "extensions.worktreeConfig must be set on the base clone"
    );
    assert_eq!(
        String::from_utf8_lossy(&ext.stdout).trim(),
        "true",
        "extensions.worktreeConfig must be true"
    );

    // (d) #2867: the foreign upstream is only ever armed behind an EFFECTIVE
    // push pin. Assert the pin as `git push` itself resolves it (full config
    // stack), not merely as a `--worktree`-scoped key.
    let effective = std::process::Command::new("git")
        .arg("-C")
        .arg(&worktree_path)
        .args(["config", "--get", "push.default"])
        .output()
        .expect("git config --get push.default");
    assert_eq!(
        String::from_utf8_lossy(&effective.stdout).trim(),
        "current",
        "the EFFECTIVE push.default (not just the --worktree key) must be `current` \
         whenever a foreign upstream is set (#2867)"
    );
}

/// #2867: [`super::push_is_pinned_to_current`] must report the EFFECTIVE
/// `push.default`, because that — not the exit code of the write that set it —
/// is what decides whether writing a foreign upstream is safe.
///
/// Why: the PR #2863 clobber came from a worktree whose branch tracked a ref it
/// did not own. `configure_session_branch_tracking` may only create that
/// tracking state while a bare `git push` is provably confined to the current
/// branch; if the detector returned `true` optimistically the gate would be
/// decorative.
/// What: builds a real repo, asserts the detector is `false` with no
/// `push.default` set and `false` for a non-`current` value, then enables
/// `extensions.worktreeConfig`, pins `push.default=current` in a linked
/// worktree, and asserts it flips to `true` there while a NON-pinned sibling
/// worktree still reports `false` — proving the detector reads worktree-scoped
/// config rather than the shared repo config.
/// Test: this function IS the test.
#[test]
fn push_pin_detection_matches_effective_config() {
    let repo_dir = crate::test_support::hermetic_temp_dir();
    let repo = repo_dir.path().to_path_buf();
    let git = |args: &[&str], cwd: &std::path::Path| -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(&repo)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return; // git unavailable — nothing to assert
    }
    assert!(git(&["config", "user.email", "t@example.com"], &repo));
    assert!(git(&["config", "user.name", "T"], &repo));
    std::fs::write(repo.join("README"), b"seed").expect("write README");
    assert!(git(&["add", "."], &repo));
    assert!(git(&["commit", "-qm", "init"], &repo));

    assert!(
        !super::push_is_pinned_to_current(&repo),
        "an unset push.default must NOT be reported as pinned"
    );
    assert!(git(&["config", "push.default", "simple"], &repo));
    assert!(
        !super::push_is_pinned_to_current(&repo),
        "push.default=simple must NOT be reported as pinned"
    );

    // Two linked worktrees off the same repo: only one gets the pin.
    let wt_parent = crate::test_support::hermetic_temp_dir();
    let pinned = wt_parent.path().join("pinned");
    let unpinned = wt_parent.path().join("unpinned");
    assert!(git(
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt-a",
            pinned.to_str().unwrap()
        ],
        &repo
    ));
    assert!(git(
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt-b",
            unpinned.to_str().unwrap()
        ],
        &repo
    ));
    assert!(git(&["config", "extensions.worktreeConfig", "true"], &repo));
    assert!(git(
        &["config", "--worktree", "push.default", "current"],
        &pinned
    ));

    assert!(
        super::push_is_pinned_to_current(&pinned),
        "a worktree-scoped push.default=current must be reported as pinned"
    );
    assert!(
        !super::push_is_pinned_to_current(&unpinned),
        "a sibling worktree without the pin must NOT be reported as pinned — \
         the detector must read worktree-scoped config, not the shared repo config"
    );
}

// ── #4957: session branches must be cut from origin, not stale local main ──

/// Run a git command in `cwd`, asserting success.
fn g(cwd: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Read trimmed stdout of a git command in `cwd`, asserting success.
fn g_out(cwd: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Add one commit in `cwd` and return its sha.
fn commit(cwd: &std::path::Path, name: &str) -> String {
    std::fs::write(cwd.join(name), name.as_bytes()).expect("write file");
    g(cwd, &["add", "."]);
    g(cwd, &["commit", "-q", "-m", name]);
    g_out(cwd, &["rev-parse", "HEAD"])
}

/// A base clone whose local `main` is DELIBERATELY behind `origin/main`.
///
/// Why: this is the whole point of the #4957 fixture. A test that only checks
/// "a worktree was created" passes against the pre-fix code; only a base whose
/// local ref lags the remote can tell `HEAD` and `origin/main` apart.
/// What: builds a bare `origin` seeded with commit A, clones it into `base`
/// (so `base`'s local `main` AND `refs/remotes/origin/main` both sit at A),
/// then pushes commit B to `origin` from a separate seed clone that `base`
/// never sees. Returns `(base_path, sha_a, sha_b)` plus the TempDir guards
/// that must stay alive for the duration of the test.
struct StaleBase {
    base: std::path::PathBuf,
    origin: std::path::PathBuf,
    sha_a: String,
    sha_b: String,
    _guards: Vec<tempfile::TempDir>,
}

fn stale_base_fixture() -> StaleBase {
    let origin_dir = crate::test_support::hermetic_temp_dir();
    let origin = origin_dir.path().to_path_buf();
    let init = std::process::Command::new("git")
        .args(["init", "--bare", "-q", "-b", "main"])
        .arg(&origin)
        .output()
        .expect("git init --bare");
    assert!(
        init.status.success(),
        "git init --bare failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    // Seed clone: the only checkout that ever pushes to `origin`.
    let seed_parent = crate::test_support::hermetic_temp_dir();
    let seed = seed_parent.path().join("seed");
    let clone = std::process::Command::new("git")
        .args(["clone", "-q"])
        .arg(&origin)
        .arg(&seed)
        .output()
        .expect("git clone seed");
    assert!(
        clone.status.success(),
        "git clone seed failed: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    g(&seed, &["config", "user.email", "t@example.com"]);
    g(&seed, &["config", "user.name", "T"]);
    let sha_a = commit(&seed, "A");
    g(&seed, &["push", "-q", "origin", "main"]);

    // Base clone: takes A, then never hears about B.
    let base_parent = crate::test_support::hermetic_temp_dir();
    let base = base_parent.path().join("base");
    let clone_base = std::process::Command::new("git")
        .args(["clone", "-q"])
        .arg(&origin)
        .arg(&base)
        .output()
        .expect("git clone base");
    assert!(
        clone_base.status.success(),
        "git clone base failed: {}",
        String::from_utf8_lossy(&clone_base.stderr)
    );
    g(&base, &["config", "user.email", "t@example.com"]);
    g(&base, &["config", "user.name", "T"]);

    // Advance `origin/main` behind the base clone's back.
    let sha_b = commit(&seed, "B");
    g(&seed, &["push", "-q", "origin", "main"]);

    assert_eq!(
        g_out(&base, &["rev-parse", "HEAD"]),
        sha_a,
        "fixture precondition: the base clone's local main must be STALE (at A)"
    );
    assert_ne!(sha_a, sha_b, "fixture precondition: A and B must differ");

    StaleBase {
        base,
        origin,
        sha_a,
        sha_b,
        _guards: vec![origin_dir, seed_parent, base_parent],
    }
}

/// #4957: a new session worktree must start at the FETCHED `origin/<default>`
/// tip, not at the base checkout's stale local `main`.
///
/// Why: the reported failure was eight `session/*` branches and local `main`
/// all pointing at the same three-week-old commit, 667 behind `origin/main`,
/// because `git worktree add -b` was given no start-point and no fetch
/// preceded it. Asserting only "a worktree exists" would have passed against
/// that code; the fixture's deliberately-stale local `main` is what makes this
/// test fail before the fix.
/// What: builds a base clone at commit A while `origin/main` has moved to B,
/// calls the production `create_session_worktree`, and asserts the new
/// worktree's `HEAD` is B — and explicitly not A.
/// Test: this function IS the test.
#[test]
fn session_worktree_branches_from_fetched_origin_not_stale_local_main() {
    let fx = stale_base_fixture();

    let worktree = create_session_worktree(
        &fx.base,
        "tm-stale-4957",
        &crate::session_manager::ManagedSessionId::new(),
    )
    .expect("create_session_worktree must succeed against a real base clone");

    let head = g_out(&worktree, &["rev-parse", "HEAD"]);
    assert_ne!(
        head, fx.sha_a,
        "the session worktree was cut from the STALE local main ({}) — #4957",
        fx.sha_a
    );
    assert_eq!(
        head, fx.sha_b,
        "the session worktree must start at the freshly-fetched origin/main tip"
    );
}

/// #4957: when the fetch fails, the session branch must still prefer the
/// last-known `origin/<default>` over the base checkout's local `HEAD`, and
/// the degradation must not read as a clean success.
///
/// Why: falling straight back to local `HEAD` on a failed fetch reintroduces
/// the exact defect for every offline spawn — the fail-open shape this repo
/// keeps getting bitten by. The base is given a LOCAL commit C that `origin`
/// never saw, so `HEAD` and `refs/remotes/origin/main` are distinguishable.
/// What: removes the `origin` repo so the fetch cannot succeed, adds local
/// commit C to the base, then asserts the worktree starts at A (the stale
/// remote-tracking ref) rather than C, and that
/// `inproject_start_point::resolve` reports a warning rather than `Fresh`.
/// Test: this function IS the test.
#[test]
fn session_worktree_falls_back_to_remote_tracking_ref_when_fetch_fails() {
    let fx = stale_base_fixture();
    let sha_c = commit(&fx.base, "C");
    std::fs::remove_dir_all(&fx.origin).expect("remove origin to break the fetch");

    let resolved = super::super::inproject_start_point::resolve(&fx.base);
    assert_eq!(
        resolved.git_ref(),
        Some("origin/main"),
        "a failed fetch must still hand back the last-known remote-tracking ref"
    );
    let warning = resolved
        .warning()
        .expect("a failed fetch must warn — it must never read as a clean success");
    assert!(
        warning.contains("git fetch origin main failed"),
        "the warning must name the failure: {warning}"
    );

    let worktree = create_session_worktree(
        &fx.base,
        "tm-offline-4957",
        &crate::session_manager::ManagedSessionId::new(),
    )
    .expect("an unreachable remote must not fail worktree creation");

    let head = g_out(&worktree, &["rev-parse", "HEAD"]);
    assert_ne!(
        head, sha_c,
        "an offline spawn must not silently fall back to the base checkout's local HEAD"
    );
    assert_eq!(
        head, fx.sha_a,
        "an offline spawn must start at the last fetched origin/main"
    );
}

/// #4957: a repo with no `origin` remote must keep working, branching from
/// `HEAD` without a spurious staleness warning.
///
/// Why: the fix must not turn "purely local repo" into an error or a noisy
/// warning — there `HEAD` is simply the correct start point.
/// What: builds a remote-less repo with one commit, asserts
/// `inproject_start_point::resolve` reports `LocalOnly` with no warning and no
/// start-point ref, and that the created worktree starts at that commit.
/// Test: this function IS the test.
#[test]
fn session_worktree_without_a_remote_still_branches_from_head() {
    let dir = crate::test_support::hermetic_temp_dir();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    let init = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(&repo)
        .output()
        .expect("git init");
    assert!(
        init.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    g(&repo, &["config", "user.email", "t@example.com"]);
    g(&repo, &["config", "user.name", "T"]);
    let sha = commit(&repo, "only");

    let resolved = super::super::inproject_start_point::resolve(&repo);
    assert_eq!(
        resolved.git_ref(),
        None,
        "a remote-less repo must let git use HEAD"
    );
    assert_eq!(
        resolved.warning(),
        None,
        "a remote-less repo is not a degradation and must not warn"
    );

    let worktree = create_session_worktree(
        &repo,
        "tm-local-4957",
        &crate::session_manager::ManagedSessionId::new(),
    )
    .expect("a repo with no remote must still get a worktree");
    assert_eq!(
        g_out(&worktree, &["rev-parse", "HEAD"]),
        sha,
        "a remote-less repo's worktree must start at the local HEAD commit"
    );
}
