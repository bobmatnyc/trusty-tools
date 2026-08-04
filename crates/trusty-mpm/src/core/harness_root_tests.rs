//! Tests for [`super`] — the `.trusty-mpm/` location resolver (#4832).
//!
//! Why: the whole point of the module is that a WORKTREE never accumulates
//! harness state, and that cannot be asserted against a plain temp directory —
//! every test here builds a real git repository and a real linked worktree.
//! What: covers the four shapes `harness_root_for` must distinguish (main
//! checkout, linked worktree, subdirectory, `.base` bare clone), the non-git
//! refusal, and the `session_scope` resolution order including the path-
//! traversal rejection.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

/// Run `git` in `dir`, asserting success.
///
/// Why: a silently-failing fixture step would make a passing assertion
/// meaningless — every `git` call here is load-bearing setup.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {} failed to spawn: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Initialise a committed git repository at `dir`.
fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@example.com"]);
    git(dir, &["config", "user.name", "T"]);
    std::fs::write(dir.join("README.md"), "# t\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "init"]);
}

/// `std::fs::canonicalize`, which every git-reported path has already been
/// through (macOS `/var` → `/private/var`).
fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

#[test]
fn harness_root_is_the_main_checkout_for_the_main_checkout() {
    // Baseline: the resolver must be idempotent on a plain checkout, or every
    // other case below would be measuring the wrong thing.
    let tmp = crate::test_support::hermetic_temp_dir();
    let repo = tmp.path().join("proj");
    init_repo(&repo);

    let resolved = harness_root_for(&repo).expect("a checkout resolves");
    assert_eq!(canon(&resolved), canon(&repo));
}

#[test]
fn harness_root_is_the_main_checkout_for_a_worktree() {
    // #4832: THE regression. A linked worktree must resolve to the checkout it
    // is a worktree of, so no `.trusty-mpm/` is ever written inside it.
    let tmp = crate::test_support::hermetic_temp_dir();
    let repo = tmp.path().join("proj");
    init_repo(&repo);
    let wt = tmp.path().join("wt");
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
    );

    let resolved = harness_root_for(&wt).expect("a worktree resolves");
    assert_eq!(
        canon(&resolved),
        canon(&repo),
        "a worktree must resolve to its main checkout, not to itself"
    );
    assert_ne!(canon(&resolved), canon(&wt));
    assert!(
        !harness_dir(&wt).starts_with(canon(&wt)),
        "the harness dir must not land inside the worktree: {}",
        harness_dir(&wt).display()
    );
}

#[test]
fn harness_root_is_the_repo_root_from_a_subdirectory() {
    // A tracked subdirectory must not grow its own `.trusty-mpm/` either —
    // this is the shape that seeded one under `crates/<name>/` (#4752).
    let tmp = crate::test_support::hermetic_temp_dir();
    let repo = tmp.path().join("proj");
    init_repo(&repo);
    let sub = repo.join("crates").join("thing");
    std::fs::create_dir_all(&sub).unwrap();

    let resolved = harness_root_for(&sub).expect("a subdirectory resolves");
    assert_eq!(canon(&resolved), canon(&repo));
}

#[test]
fn harness_root_maps_a_base_clone_back_to_the_project() {
    // trusty-mpm's own provisioning shape: worktrees added from the bare
    // `<project>/.base` clone answer `--git-common-dir` with `.base` itself.
    // Without the mapping, harness state would land in `<project>/.base/`.
    let tmp = crate::test_support::hermetic_temp_dir();
    let repo = tmp.path().join("proj");
    init_repo(&repo);
    let base = repo.join(".base");
    let out = Command::new("git")
        .args(["clone", "--bare", "-q"])
        .arg(&repo)
        .arg(&base)
        .output()
        .expect("git clone --bare spawns");
    assert!(
        out.status.success(),
        "bare clone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let wt = base.join(".worktrees").join("s1");
    git(
        &base,
        &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
    );

    let resolved = harness_root_for(&wt).expect("a base-clone worktree resolves");
    assert_eq!(
        canon(&resolved),
        canon(&repo),
        "a `.base`-registered worktree must resolve to the project, not the bare clone"
    );
}

#[test]
fn harness_root_for_is_none_outside_a_git_repo() {
    // The refusal signal `tm session start` gates on (#4832 defect 5).
    let tmp = crate::test_support::hermetic_temp_dir();
    let plain = tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&plain).unwrap();

    assert!(
        harness_root_for(&plain).is_none(),
        "a non-git directory must not resolve to a harness root"
    );
}

#[test]
fn harness_root_falls_back_to_the_given_dir_outside_git() {
    // The documented last-resort default: project-local, never global.
    let tmp = crate::test_support::hermetic_temp_dir();
    let plain = tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&plain).unwrap();

    assert_eq!(harness_root(&plain), plain);
}

#[test]
fn harness_dir_is_under_the_harness_root() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let plain = tmp.path().join("p");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(harness_dir(&plain), plain.join(HARNESS_DIR));
}

#[test]
fn framework_dir_is_under_the_harness_dir() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let plain = tmp.path().join("p");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(
        framework_dir(&plain),
        plain.join(HARNESS_DIR).join(FRAMEWORK_DIR)
    );
}

#[test]
fn session_dir_is_per_session_under_the_harness_root() {
    // #4832: per-session, so two concurrent sessions cannot overwrite each
    // other's compiled prompt — the collision the shared per-project file had.
    let tmp = crate::test_support::hermetic_temp_dir();
    let plain = tmp.path().join("p");
    std::fs::create_dir_all(&plain).unwrap();

    let a = session_dir(&plain, "sess-a");
    let b = session_dir(&plain, "sess-b");
    assert_eq!(
        a,
        plain.join(HARNESS_DIR).join(SESSIONS_DIR).join("sess-a"),
        "the layout is `.trusty-mpm/sessions/<id>/`"
    );
    assert_ne!(a, b, "two sessions must not share a directory");
}

#[test]
fn session_scope_prefers_the_explicit_id() {
    assert_eq!(
        session_scope_from(Some("abc-123"), Some("ambient-id")),
        "abc-123",
        "an explicit managed id outranks the ambient one"
    );
}

#[test]
fn session_scope_reads_the_managed_env_var() {
    // The in-place relaunch has no explicit id but runs inside a pane that
    // exports one — it must land in that session's directory, not `local`.
    assert_eq!(session_scope_from(None, Some("ambient-id")), "ambient-id");
}

#[test]
fn session_scope_falls_back_to_the_unmanaged_bucket() {
    // No explicit id and no managed env var: the documented `local` bucket,
    // never an invented per-call id no later reader could match.
    assert_eq!(session_scope_from(None, None), UNMANAGED_SESSION_SCOPE);
    assert_eq!(
        session_scope_from(Some("   "), None),
        UNMANAGED_SESSION_SCOPE
    );
}

#[test]
fn session_scope_rejects_a_path_traversal_segment() {
    // The env var is operator-writable and becomes a path component; a `../`
    // escape must never reach `session_dir`.
    for hostile in ["..", ".", "../../etc", "a/b", "a\\b", ""] {
        assert_eq!(
            session_scope_from(Some(hostile), None),
            UNMANAGED_SESSION_SCOPE,
            "{hostile:?} must not become a path component as an explicit id"
        );
        assert_eq!(
            session_scope_from(None, Some(hostile)),
            UNMANAGED_SESSION_SCOPE,
            "{hostile:?} must not become a path component as an ambient id"
        );
    }
}
