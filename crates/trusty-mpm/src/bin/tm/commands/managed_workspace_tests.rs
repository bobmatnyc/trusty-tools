//! #4300 acceptance: the CLI paths honour the #3455 per-project worktree
//! opt-out — no worktree AND no base clone.
//!
//! Why: before #4300 the opt-out was consulted at exactly one decision point,
//! inside the daemon. `tm launch` runs in-process and never asks the daemon;
//! the guided fallback runs precisely BECAUSE the daemon is unreachable. Both
//! provisioned a clone and a worktree for projects registered with `worktree:
//! false` (live for `bobmatnyc/writing` and `bob-duetto/cto`). These tests
//! pin both paths, and the `unwrap_or(true)` default they must not regress.
//! What: per CLI path — one opted-out case asserting the SPECIFIC main-checkout
//! path is returned and that the base-clone directory and the `.worktrees`
//! directory do NOT exist, and one opted-in/unset control asserting the
//! worktree IS created at its exact expected path.
//! Fixture remotes: every origin here uses the non-resolvable `.invalid` TLD
//! (RFC 2606) with fixture-only owner/repo names, following the convention
//! `tests_behavior_c_tests.rs:1570-1580` documents. An earlier revision used
//! the REAL `bobmatnyc/writing` and `bob-duetto/cto` remotes, and the
//! guard-neutralised revert run proved why that is wrong: `ensure_base_clone`
//! genuinely cloned both private repos over the network (88.43s vs 0.28s
//! baseline). The assertion under test is the opt-out DECISION, never that a
//! clone succeeds, so a host that cannot resolve is lossless here and keeps
//! the suite hermetic and fast whether the guard is present or not.
//! Concurrency: the fallback tests read `TRUSTY_MPM_REPOS_ROOT` (a
//! process-global) through `inproject::base_clone_path`, so they are
//! `#[serial_test::serial]` and restore the previous value via an RAII guard
//! that runs on panic-driven unwind too. Nothing here deletes anything: the
//! only filesystem writes are `git init`/`git worktree add` under
//! test-owned `TempDir`s, so a clobbered global can make a test FAIL but can
//! never make it destructive.
//! Test: this file.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use trusty_mpm::daemon::managed_routes::inproject;
use trusty_mpm::project::{Project, ProjectRegistry};
use trusty_mpm::session_manager::ManagedSessionId;

use super::{ManagedWorkspace, provision_for_fallback, provision_for_launch};

/// RAII override of `TRUSTY_MPM_REPOS_ROOT`, restored on drop (incl. unwind).
///
/// Why: `inproject::base_clone_path` reads this global, and a test that
/// panicked mid-assertion must not leave the rest of the binary pointed at a
/// deleted `TempDir`.
/// What: sets the var on construction, restores the prior value (or removes
/// it) on drop. Callers MUST be `#[serial_test::serial]`.
/// Test: used by the fallback tests below.
struct ReposRootGuard {
    prev: Option<std::ffi::OsString>,
}

impl ReposRootGuard {
    fn set(path: &Path) -> Self {
        let prev = std::env::var_os(inproject::REPOS_ROOT_ENV);
        // SAFETY: every caller is `#[serial_test::serial]`, so no other test
        // thread races this set/restore.
        unsafe { std::env::set_var(inproject::REPOS_ROOT_ENV, path) };
        Self { prev }
    }
}

impl Drop for ReposRootGuard {
    fn drop(&mut self) {
        // SAFETY: see `set`.
        match self.prev.take() {
            Some(v) => unsafe { std::env::set_var(inproject::REPOS_ROOT_ENV, v) },
            None => unsafe { std::env::remove_var(inproject::REPOS_ROOT_ENV) },
        }
    }
}

/// Build a registry directory containing one project with the given opt-out.
///
/// Why: both paths resolve the opt-out by reading `projects.json` out of
/// process, so the fixture must be a REAL on-disk registry, not a stub.
/// What: writes `<tmp>/projects.json` via `ProjectRegistry` and returns the
/// owning `TempDir`. Any failure `unwrap`s — a fixture that cannot be built is
/// a test failure, never a skip.
/// Test: used by every test below.
async fn registry_with(repo_url: &str, worktree: Option<bool>) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let registry = ProjectRegistry::load(dir.path()).await.unwrap();
    registry
        .register(Project {
            name: "fixture".into(),
            repo_url: repo_url.into(),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree,
        })
        .await
        .unwrap();
    assert!(
        dir.path().join("projects.json").is_file(),
        "fixture precondition: projects.json must exist at {}",
        dir.path().display()
    );
    dir
}

/// Create a real git repository at `path` with one empty commit.
///
/// Why: `create_session_worktree` runs `git worktree add`, which requires a
/// HEAD commit; the opted-IN control tests must reach that code for real.
/// What: `git init` + identity config + `commit --allow-empty`, asserting each
/// step succeeded. No skip path — a missing `git` fails the test.
/// Test: used by the opted-in control tests.
fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
        assert!(
            status.success(),
            "git {args:?} failed in {}",
            path.display()
        );
    };
    run(&["init"]);
    run(&["config", "user.email", "ci@test.invalid"]);
    run(&["config", "user.name", "CI"]);
    run(&["commit", "--allow-empty", "-m", "init"]);
}

/// Assert `base` shows no trace of provisioning: no clone, no worktrees dir.
///
/// Why: the acceptance bar is "neither a worktree NOR a base clone", and the
/// daemon's ordering exists precisely so the clone is not created either.
/// What: asserts the base-clone directory itself, its `.git`, and its
/// `.worktrees` child are all absent.
/// Test: used by both opted-out tests.
fn assert_nothing_provisioned(base: &Path) {
    assert!(
        !base.exists(),
        "#4300: base clone dir must NOT be created for an opted-out project: {}",
        base.display()
    );
    assert!(
        !base.join(".git").exists(),
        "#4300: base clone must NOT be initialised: {}",
        base.join(".git").display()
    );
    assert!(
        !base.join(".worktrees").exists(),
        "#4300: worktree dir must NOT be created: {}",
        base.join(".worktrees").display()
    );
}

/// Path a UUID-named session worktree lands at under `base`.
///
/// Why: the tests assert on the SPECIFIC path, not on `is_ok()`.
/// What: `<base>/.worktrees/<session-id>`.
/// Test: used by the opted-in control tests.
fn expected_worktree(base: &Path, session_id: &ManagedSessionId) -> PathBuf {
    base.join(".worktrees").join(session_id.to_string())
}

// ── Path (a): `tm launch` ────────────────────────────────────────────────────

/// `tm launch` against a project registered with `worktree: false` must create
/// NEITHER a base clone NOR a worktree, and must run in the live checkout.
///
/// Why (#4300): `tm launch` provisions in-process and never asks the daemon,
/// so before this fix the opt-out was ignored outright on this path.
/// What: registers `worktree: false`, calls the exact function `launch()`
/// calls, and asserts the returned workspace is the live checkout while the
/// caller-supplied base-clone path stays untouched.
/// Test: itself. RED with the guard reverted (a clone + worktree appear).
#[tokio::test]
async fn provision_for_launch_opted_out_creates_no_clone_and_no_worktree() {
    let origin = "git@github.invalid:fixture-owner/optout-repo.git";
    let registry = registry_with(
        "https://github.invalid/fixture-owner/optout-repo",
        Some(false),
    )
    .await;

    let repos_root = tempfile::tempdir().unwrap();
    let base = repos_root.path().join("fixture-owner").join("optout-repo");
    let live = tempfile::tempdir().unwrap();
    let session_id = ManagedSessionId::new();

    let workspace = provision_for_launch(registry.path(), origin, &base, live.path(), &session_id)
        .await
        .expect(
            "#4300: an opted-out project must not attempt a clone at all — a clone \
             error here means the opt-out was never consulted",
        );

    assert_eq!(
        workspace,
        ManagedWorkspace::MainCheckout(live.path().to_path_buf()),
        "#4300: an opted-out project must launch in its own checkout"
    );
    assert_nothing_provisioned(&base);
    assert!(
        !expected_worktree(&base, &session_id).exists(),
        "#4300: the per-session worktree must NOT exist"
    );
}

/// `tm launch` from a SUBDIRECTORY of an opted-out project must deploy into
/// the repo ROOT, never the subdirectory.
///
/// Why (code-critic MEDIUM on PR #4303): `get_origin_url` succeeds at any
/// depth, so `cd repo/src && tm launch` on an opted-out project passed `cwd`
/// straight through as the workspace — `.claude`, the project hooks, the tmux
/// cwd and the daemon's `project_path` all landed in `repo/src`. That is
/// precisely the "tm furniture somewhere the operator did not intend" failure
/// the opt-out exists to prevent, and it was introduced by this PR: the
/// pre-#4300 code always redirected to a worktree, so `cwd` never became a
/// deploy target. The daemon-unreachable fallback already resolved the root
/// via `classify_cwd_project`; this pins that `tm launch` agrees.
/// What: builds a REAL git repo (so `git rev-parse --show-toplevel` has a root
/// to find), registers it opted-out, calls `provision_for_launch` with a
/// nested subdirectory as `cwd`, and asserts the returned workspace is the
/// canonical repo root and explicitly NOT the subdirectory.
/// Test: itself. RED if `provision_for_launch` passes `cwd` through.
#[tokio::test]
async fn provision_for_launch_from_subdirectory_targets_repo_root() {
    let origin = "https://github.invalid/fixture-owner/optout-repo.git";
    let registry = registry_with(
        "https://github.invalid/fixture-owner/optout-repo",
        Some(false),
    )
    .await;

    let repos_root = tempfile::tempdir().unwrap();
    let base = repos_root.path().join("fixture-owner").join("optout-repo");

    // A real git working tree, with a nested subdirectory to launch from.
    let live = tempfile::tempdir().unwrap();
    init_git_repo(live.path());
    let subdir = live.path().join("crates").join("inner");
    std::fs::create_dir_all(&subdir).unwrap();
    // `git rev-parse --show-toplevel` canonicalises symlinks (macOS `/var` →
    // `/private/var`), so the expectation must be canonical too.
    let repo_root = live.path().canonicalize().unwrap();
    let session_id = ManagedSessionId::new();

    let workspace = provision_for_launch(registry.path(), origin, &base, &subdir, &session_id)
        .await
        .expect(
            "#4300: an opted-out project must not attempt a clone at all — a clone \
             error here means the opt-out was never consulted",
        );

    assert_eq!(
        workspace,
        ManagedWorkspace::MainCheckout(repo_root.clone()),
        "#4300: `tm launch` from a subdirectory must target the repo ROOT"
    );
    assert_ne!(
        workspace.path(),
        subdir.as_path(),
        "#4300: the subdirectory must NEVER become the deploy target"
    );
    assert!(
        !subdir.join(".claude").exists(),
        "nothing may be provisioned inside the subdirectory: {}",
        subdir.display()
    );
    assert_nothing_provisioned(&base);
}

/// A project with NO `worktree` key still gets a worktree — the `unwrap_or(true)`
/// default must not regress.
///
/// Why: 31 of the 33 registered projects carry no `worktree` key; a fix that
/// silently flipped them to main-checkout mode would be far worse than the bug.
/// What: registers the project with `worktree: None`, provisions, and asserts
/// the worktree exists at its exact expected path inside the base clone.
/// Test: itself.
#[tokio::test]
async fn provision_for_launch_unset_creates_worktree() {
    let origin = "https://github.invalid/fixture-owner/isolated-repo";
    let registry = registry_with("https://github.invalid/fixture-owner/isolated-repo", None).await;

    let repos_root = tempfile::tempdir().unwrap();
    let base = repos_root
        .path()
        .join("fixture-owner")
        .join("isolated-repo");
    init_git_repo(&base);
    let live = tempfile::tempdir().unwrap();
    let session_id = ManagedSessionId::new();

    let workspace = provision_for_launch(registry.path(), origin, &base, live.path(), &session_id)
        .await
        .unwrap();

    let expected = expected_worktree(&base, &session_id);
    assert_eq!(
        workspace,
        ManagedWorkspace::Worktree(expected.clone()),
        "a project with no `worktree` key must still get a worktree"
    );
    assert!(
        expected.is_dir(),
        "the worktree directory must exist at {}",
        expected.display()
    );
}

/// An UNREGISTERED origin also keeps worktree isolation — the default applies
/// to projects the registry has never heard of, not just to unset keys.
///
/// Why: the registry holds 33 of the machine's projects; every other repo the
/// operator runs `tm launch` in resolves through the "no match" branch.
/// What: builds a registry holding a DIFFERENT project's opt-out, then
/// provisions for an unrelated origin and asserts the worktree is created.
/// Test: itself.
#[tokio::test]
async fn provision_for_launch_unregistered_origin_creates_worktree() {
    let registry = registry_with(
        "https://github.invalid/fixture-owner/optout-repo",
        Some(false),
    )
    .await;

    let origin = "https://github.invalid/fixture-owner/never-registered";
    let repos_root = tempfile::tempdir().unwrap();
    let base = repos_root
        .path()
        .join("fixture-owner")
        .join("never-registered");
    init_git_repo(&base);
    let live = tempfile::tempdir().unwrap();
    let session_id = ManagedSessionId::new();

    let workspace = provision_for_launch(registry.path(), origin, &base, live.path(), &session_id)
        .await
        .unwrap();

    let expected = expected_worktree(&base, &session_id);
    assert_eq!(
        workspace,
        ManagedWorkspace::Worktree(expected.clone()),
        "another project's opt-out must not leak onto an unrelated origin"
    );
    assert!(
        expected.is_dir(),
        "worktree must exist at {}",
        expected.display()
    );
}

// ── Path (b): the daemon-unreachable bare-`tm` fallback ─────────────────────

/// The daemon-unreachable fallback must ALSO honour the opt-out: no base
/// clone, no worktree, launch in the repo root.
///
/// Why (#4300): this path runs precisely because the daemon is down, so it can
/// never be served by the daemon's own check — the setting was silently
/// conditional on daemon liveness.
/// What: points `TRUSTY_MPM_REPOS_ROOT` at an empty temp dir, registers
/// `worktree: false`, and asserts the fallback returns the repo ROOT while the
/// repos root stays completely empty.
/// Test: itself. RED with the guard reverted (a clone + worktree appear).
#[tokio::test]
#[serial_test::serial]
async fn provision_for_fallback_opted_out_creates_no_clone_and_no_worktree() {
    let origin = "https://github.invalid/fixture-owner/fallback-repo.git";
    let registry = registry_with(
        "https://github.invalid/fixture-owner/fallback-repo",
        Some(false),
    )
    .await;

    let repos_root = tempfile::tempdir().unwrap();
    let _guard = ReposRootGuard::set(repos_root.path());
    let git_root = tempfile::tempdir().unwrap();
    let session_id = ManagedSessionId::new();

    let workspace = provision_for_fallback(registry.path(), origin, git_root.path(), &session_id)
        .await
        .expect(
            "#4300: an opted-out project must not attempt a clone at all — a clone \
             error here means the opt-out was never consulted",
        );

    assert_eq!(
        workspace,
        ManagedWorkspace::MainCheckout(git_root.path().to_path_buf()),
        "#4300: the daemon-unreachable fallback must launch in the repo root"
    );
    assert_nothing_provisioned(
        &repos_root
            .path()
            .join("fixture-owner")
            .join("fallback-repo"),
    );
    assert_eq!(
        std::fs::read_dir(repos_root.path()).unwrap().count(),
        0,
        "#4300: the managed repos root must stay empty for an opted-out project"
    );
}

/// The fallback's default path is unchanged: an unset `worktree` still gets a
/// per-session worktree inside the protected base clone, never the live
/// checkout (#1724 invariant preserved).
///
/// Why: this is the control that proves the new guard did not turn the whole
/// fallback into a live-checkout launcher.
/// What: pre-creates a real base clone at the path `base_clone_path` resolves
/// to, registers the project with no `worktree` key, and asserts the returned
/// workspace is the worktree at its exact expected path.
/// Test: itself.
#[tokio::test]
#[serial_test::serial]
async fn provision_for_fallback_unset_creates_worktree_not_live_checkout() {
    let origin = "https://github.invalid/fixture-owner/fallback-repo.git";
    let registry = registry_with("https://github.invalid/fixture-owner/fallback-repo", None).await;

    let repos_root = tempfile::tempdir().unwrap();
    let _guard = ReposRootGuard::set(repos_root.path());
    let base = repos_root
        .path()
        .join("fixture-owner")
        .join("fallback-repo");
    init_git_repo(&base);
    let git_root = tempfile::tempdir().unwrap();
    let session_id = ManagedSessionId::new();

    let workspace = provision_for_fallback(registry.path(), origin, git_root.path(), &session_id)
        .await
        .unwrap();

    let expected = expected_worktree(&base, &session_id);
    assert_eq!(
        workspace,
        ManagedWorkspace::Worktree(expected.clone()),
        "an unset `worktree` key must still redirect to a protected worktree"
    );
    assert!(
        expected.is_dir(),
        "worktree must exist at {}",
        expected.display()
    );
    assert!(
        !git_root.path().join(".worktrees").exists(),
        "#1724: nothing may be provisioned inside the live checkout"
    );
}

/// Another project's opt-out must not leak onto an unrelated origin on the
/// fallback path either — the registry lookup is keyed by `repo_url`.
///
/// Why: the fallback reads the SAME 33-entry registry, only two of whose
/// entries carry `worktree: false`; a lookup that matched too eagerly would
/// strip isolation from every other project when the daemon happens to be down.
/// What: registers only `bobmatnyc/writing` as opted out, then provisions for
/// `bob-duetto/cto` and asserts the worktree IS created.
/// Test: itself.
#[tokio::test]
#[serial_test::serial]
async fn provision_for_fallback_other_projects_optout_does_not_leak() {
    let registry = registry_with(
        "https://github.invalid/fixture-owner/optout-repo",
        Some(false),
    )
    .await;

    let repos_root = tempfile::tempdir().unwrap();
    let _guard = ReposRootGuard::set(repos_root.path());
    let base = repos_root
        .path()
        .join("fixture-owner")
        .join("fallback-repo");
    init_git_repo(&base);
    let git_root = tempfile::tempdir().unwrap();
    let session_id = ManagedSessionId::new();

    let workspace = provision_for_fallback(
        registry.path(),
        "https://github.invalid/fixture-owner/fallback-repo.git",
        git_root.path(),
        &session_id,
    )
    .await
    .unwrap();

    let expected = expected_worktree(&base, &session_id);
    assert_eq!(
        workspace,
        ManagedWorkspace::Worktree(expected.clone()),
        "an unrelated project's opt-out must not disable isolation here"
    );
    assert!(
        expected.is_dir(),
        "worktree must exist at {}",
        expected.display()
    );
}
