//! Tests for [`super`] — the `.trusty-mpm/` location resolver (#4832).
//!
//! Why: the whole point of the module is that a WORKTREE never accumulates
//! harness state, and that cannot be asserted against a plain temp directory —
//! every test here builds a real git repository and a real linked worktree.
//! What: covers the shapes `harness_root_for` must distinguish (main checkout,
//! linked worktree, subdirectory, `.base` BARE clone vs. an ordinary repo that
//! is merely NAMED `.base`, submodule, `--separate-git-dir` checkout), the
//! non-git refusal, and the `session_scope` resolution order including the
//! path-traversal rejection.
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
fn harness_root_for_a_non_bare_repo_named_base_is_itself() {
    // #4841 review (HIGH): the `.base` rewrite is about trusty-mpm's own BARE
    // provisioning clone. An ORDINARY repository that merely happens to be
    // named `.base` owns its own harness state — rewriting it to the parent
    // would write that state outside the repository, possibly into a different
    // one, which is the exact scatter #4832 exists to stop.
    let tmp = crate::test_support::hermetic_temp_dir();
    let project = tmp.path().join("proj");
    let repo = project.join(".base");
    init_repo(&repo);

    let resolved = harness_root_for(&repo).expect("a checkout named .base resolves");
    assert_eq!(
        canon(&resolved),
        canon(&repo),
        "a non-bare repo named `.base` must resolve to itself, not to its parent"
    );
    assert_ne!(canon(&resolved), canon(&project));
}

#[test]
fn harness_root_for_a_submodule_is_the_submodule_checkout() {
    // #4841 review (MEDIUM): a submodule's git common dir is
    // `<super>/.git/modules/<name>` — outside the working tree entirely. Derived
    // from the git directory, `tm project init` would have scaffolded INTO
    // `.git/`, and the submodule's own `framework/manifest.toml` would never be
    // read.
    let tmp = crate::test_support::hermetic_temp_dir();
    let upstream = tmp.path().join("upstream");
    init_repo(&upstream);
    let sup = tmp.path().join("super");
    init_repo(&sup);
    git(
        &sup,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            upstream.to_str().unwrap(),
            "sm",
        ],
    );
    let sm = sup.join("sm");

    let resolved = harness_root_for(&sm).expect("a submodule resolves");
    assert_eq!(
        canon(&resolved),
        canon(&sm),
        "a submodule must resolve to its own checkout"
    );
    assert!(
        !harness_dir(&sm)
            .components()
            .any(|c| c.as_os_str() == ".git"),
        "harness state must never land inside a git directory: {}",
        harness_dir(&sm).display()
    );
}

#[test]
fn harness_root_for_a_separate_git_dir_checkout_is_the_working_tree() {
    // #4841 review (MEDIUM): `--separate-git-dir` puts the git directory at
    // `<store>.git`, outside the tree. The working tree still owns the state.
    let tmp = crate::test_support::hermetic_temp_dir();
    let work = tmp.path().join("work");
    let store = tmp.path().join("store.git");
    std::fs::create_dir_all(&work).unwrap();
    let out = Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(format!("--separate-git-dir={}", store.display()))
        .arg(&work)
        .output()
        .expect("git init --separate-git-dir spawns");
    assert!(
        out.status.success(),
        "separate-git-dir init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let resolved = harness_root_for(&work).expect("a separate-git-dir checkout resolves");
    assert_eq!(
        canon(&resolved),
        canon(&work),
        "a --separate-git-dir checkout must resolve to its working tree, not its store"
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

/// #4270: the predicate names each protected entry it finds.
///
/// Why: two guards refuse a rename based on this answer, and each wants to say
/// what it found. A predicate that recognised only one of the three names is
/// exactly the hole the first round of #4270 shipped — `.base` was guarded and
/// `.worktrees` was not.
/// What: asserts each of `.git`, `.base`, `.worktrees` is detected on its own.
/// Test: this function IS the test.
#[test]
fn protected_state_in_names_each_protected_entry() {
    for name in [".git", ".base", ".worktrees"] {
        let tmp = crate::test_support::hermetic_temp_dir();
        let dir = tmp.path().join("owner").join("repo");
        std::fs::create_dir_all(dir.join(name)).expect("create protected entry");
        assert_eq!(
            protected_state_in(&dir),
            Some(name),
            "{name} must be recognised as protected state"
        );
    }
}

/// #4270: a directory carrying none of the protected entries is not protected.
///
/// Why: the guards must still let genuine foreign debris through to the
/// quarantine path, or the stale-clone recovery #1937 added stops working.
/// What: a non-empty directory holding only a stray `HEAD` — the crashed-clone
/// shape — reads as unprotected.
/// Test: this function IS the test.
#[test]
fn protected_state_in_is_none_for_a_foreign_directory() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let dir = tmp.path().join("owner").join("repo");
    std::fs::create_dir_all(&dir).expect("create dir");
    std::fs::write(dir.join("HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
    assert_eq!(protected_state_in(&dir), None);
}

/// #4270: an entry that cannot be stat'd counts as PRESENT, not absent.
///
/// Why: this is the whole reason the probe is not `Path::exists`, which maps
/// EACCES to `false` — indistinguishable from "no git dir". Under that
/// spelling a permissions blip on `.git` lets `migrate_old_layout_aside` rename
/// a live project directory. The failure is silent and the data is gone, so the
/// fail-safe direction has to be proven, not assumed.
/// What: chmods the containing directory to 000 so stat'ing a child returns
/// EACCES, then asserts the probe still reports protected. Skips when running
/// as root (root bypasses the permission check) or when the platform lets the
/// stat through anyway.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn protected_state_in_treats_an_unreadable_entry_as_present() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = crate::test_support::hermetic_temp_dir();
    let dir = tmp.path().join("owner").join("repo");
    std::fs::create_dir_all(dir.join(".git")).expect("create .git");

    let restore = std::fs::metadata(&dir).expect("stat dir").permissions();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");

    let unreadable = std::fs::symlink_metadata(dir.join(".git"))
        .err()
        .is_some_and(|e| e.kind() != std::io::ErrorKind::NotFound);
    let observed = protected_state_in(&dir);

    // Restore before asserting so a failure cannot leave an undeletable tempdir.
    std::fs::set_permissions(&dir, restore).expect("restore permissions");

    if !unreadable {
        eprintln!(
            "protected_state_in_treats_an_unreadable_entry_as_present: stat still \
             succeeds (running as root?), skipping"
        );
        return;
    }
    assert_eq!(
        observed,
        Some(".git"),
        "an entry that cannot be stat'd must count as present — Path::exists would \
         report false here and the caller would rename live work"
    );
}
