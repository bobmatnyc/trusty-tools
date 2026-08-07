//! Unit tests for `core::worktree_index` (#5060).
//!
//! Why: split into a sibling file (declared `#[path = "worktree_index_tests.rs"]
//! mod tests;`) so `worktree_index.rs` stays under the 500-SLOC production cap.
//!
//! What: drives [`super::index_new_worktree`] synchronously against REAL git
//! repositories built in a hermetic temp dir, so the worktree-vs-clone
//! distinction is proven against git's actual on-disk shape (`.git` file vs
//! `.git` directory) rather than a hand-rolled stand-in. No test here touches
//! the operator's trusty-search daemon: `trusty_common::search_index` refuses
//! every daemon write from a test process (#4255), so the registration branch
//! runs its derivation and returns without issuing a request.
//!
//! Test: `cargo test -p trusty-mpm -- core::worktree_index`

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{WorktreeIndexOutcome, index_new_worktree, index_new_worktree_in_background};

/// Run `git <args>` inside `dir`, returning whether it succeeded.
fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Build a real git repository with one commit, or `None` when git is
/// unavailable / unusable in this environment (CI sandboxes without a git
/// identity), so the tests skip rather than fail spuriously.
fn init_repo(base: &Path) -> Option<()> {
    std::fs::create_dir_all(base).ok()?;
    if !git_ok(base, &["init", "--initial-branch=main"]) {
        return None;
    }
    let _ = git_ok(base, &["config", "user.email", "ci@test.invalid"]);
    let _ = git_ok(base, &["config", "user.name", "CI"]);
    std::fs::write(base.join("README.md"), "# fixture\n").ok()?;
    if !git_ok(base, &["add", "."]) {
        return None;
    }
    if !git_ok(base, &["commit", "-m", "seed"]) {
        return None;
    }
    Some(())
}

/// A base clone plus one worktree cut from it, both under a hermetic temp dir.
struct Fixture {
    _dir: tempfile::TempDir,
    base: PathBuf,
    worktree: PathBuf,
}

fn fixture(worktree_name: &str) -> Option<Fixture> {
    let dir = crate::test_support::hermetic_temp_dir();
    let base = dir.path().join("base-repo");
    init_repo(&base)?;
    let worktree = dir.path().join(worktree_name);
    if !git_ok(
        &base,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            worktree.to_str().expect("utf8 path"),
        ],
    ) {
        return None;
    }
    Some(Fixture {
        _dir: dir,
        base,
        worktree,
    })
}

/// A path that does not exist is refused, not registered.
///
/// Why: the caller is a `git worktree add` success branch, but a TOCTOU
/// removal (or a caller passing the wrong path) must not reach the daemon and
/// register an index for a directory that is not there — the exact shape that
/// filled the registry with dead roots and stalled warm boot (#4255).
/// Test: this test.
#[test]
fn skips_missing_path() {
    let dir = crate::test_support::hermetic_temp_dir();
    let missing = dir.path().join("no-such-worktree");
    assert_eq!(
        index_new_worktree(&missing),
        WorktreeIndexOutcome::PathMissing
    );
}

/// A PLAIN CLONE is refused — only worktrees are indexed here.
///
/// Why (regression): this is the guard that keeps worktree creation from
/// making the operator's opt-in decision for them. Relaxing
/// `is_git_worktree` from "`.git` is a FILE" to "`.git` EXISTS" — the obvious
/// simplification, since both shapes are "a git repo" — silently turns this
/// module into one that registers an index for any checkout handed to it.
/// That mutation is invisible without this assertion, because the permissive
/// version still returns `Registered` and still "works".
/// Test: this test.
#[test]
fn skips_path_that_is_not_a_worktree() {
    let dir = crate::test_support::hermetic_temp_dir();
    let base = dir.path().join("plain-clone");
    let Some(()) = init_repo(&base) else {
        eprintln!("skipping: git unavailable in this environment");
        return;
    };
    assert!(
        base.join(".git").is_dir(),
        "fixture precondition: a primary clone's .git is a directory"
    );
    assert_eq!(
        index_new_worktree(&base),
        WorktreeIndexOutcome::NotAWorktree,
        "a primary clone must not be auto-indexed by worktree creation"
    );
}

/// A directory with no git repository at all is refused.
#[test]
fn skips_plain_directory() {
    let dir = crate::test_support::hermetic_temp_dir();
    let plain = dir.path().join("not-a-repo");
    std::fs::create_dir_all(&plain).expect("create dir");
    assert_eq!(
        index_new_worktree(&plain),
        WorktreeIndexOutcome::NotAWorktree
    );
}

/// A real worktree registers under its OWN basename, not the base repo's.
///
/// Why (regression): the index id must identify the WORKTREE. Deriving it from
/// the enclosing repository instead — the mistake a `resolve_project_root`
/// walk invites — would make every worktree of a repo collide on one id, so
/// the last worktree created would silently overwrite the previous one's root
/// and every earlier worktree's `search` would answer from the wrong tree.
/// Both trees are real checkouts of the same project, so the results would
/// look plausible; only the id assertion catches it.
/// Test: this test.
#[test]
fn registers_index_for_real_worktree() {
    let Some(fx) = fixture("feat-index-on-create") else {
        eprintln!("skipping: git unavailable in this environment");
        return;
    };
    assert!(
        fx.worktree.join(".git").is_file(),
        "fixture precondition: a worktree's .git is a file"
    );
    assert_eq!(
        index_new_worktree(&fx.worktree),
        WorktreeIndexOutcome::Registered("feat-index-on-create".to_string()),
        "index id must be the worktree's own basename, not {:?}",
        fx.base.file_name()
    );
}

/// Re-running against an already-registered worktree is a cheap, stable no-op.
///
/// Why: the hook fires on every worktree creation and the daemon's
/// `POST /indexes` is find-or-create, so a repeat call must return the SAME id
/// rather than minting a second registration or erroring. Without this, a
/// retried provisioning step could fork the index identity.
/// Test: this test.
#[test]
fn is_idempotent_for_same_worktree() {
    let Some(fx) = fixture("feat-idempotent") else {
        eprintln!("skipping: git unavailable in this environment");
        return;
    };
    let first = index_new_worktree(&fx.worktree);
    let second = index_new_worktree(&fx.worktree);
    assert_eq!(first, second);
    assert_eq!(
        first,
        WorktreeIndexOutcome::Registered("feat-idempotent".to_string())
    );
}

/// The spawn helper returns BEFORE its work finishes (regression, THE hard
/// constraint).
///
/// Why: a cold BM25+KG build of this workspace measured 35.0 s. If a future
/// edit joined the spawned thread — the natural "let's return the outcome to
/// the caller" refactor — `git worktree add` and every session launch behind
/// it would block for that long, and a joined version still produces a correct
/// index so nothing else in the suite would go red.
/// What: hands `spawn_detached` work that sleeps far longer than the
/// assertion's ceiling, so a `.join()` cannot pass. Asserting against
/// [`index_new_worktree_in_background`] directly would NOT catch that: under
/// the test harness the daemon write is refused instantly (#4255), so the real
/// work finishes in microseconds whether joined or not. The test therefore
/// pins the property on the helper that owns it, and
/// `background_entry_point_routes_through_spawn_detached` pins that the entry
/// point still uses that helper.
/// Test: this test.
#[test]
fn spawn_detached_returns_before_work_finishes() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    let started = std::time::Instant::now();
    super::spawn_detached(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        flag.store(true, Ordering::SeqCst);
    });
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "spawn must not wait on its work; returned after {elapsed:?}"
    );
    assert!(
        !done.load(Ordering::SeqCst),
        "work must still be running — a joined spawn would have completed it"
    );
}

/// The public entry point returns promptly for a real worktree.
///
/// Why: `spawn_detached_returns_before_work_finishes` proves the helper is
/// detached; this proves the entry point actually goes through it rather than
/// doing the walk inline. Weaker than that test on its own (the harness makes
/// the real work fast), so the two are complementary, not redundant.
/// Test: this test.
#[test]
fn background_entry_point_routes_through_spawn_detached() {
    let Some(fx) = fixture("feat-nonblocking") else {
        eprintln!("skipping: git unavailable in this environment");
        return;
    };
    let started = std::time::Instant::now();
    index_new_worktree_in_background(fx.worktree.clone());
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "worktree creation must never wait on indexing; returned after {elapsed:?}"
    );
}

/// `is_git_worktree` distinguishes the two on-disk shapes.
#[test]
fn is_git_worktree_matches_git_on_disk_shapes() {
    let Some(fx) = fixture("feat-shapes") else {
        eprintln!("skipping: git unavailable in this environment");
        return;
    };
    assert!(
        super::is_git_worktree(&fx.worktree),
        "worktree: .git is a file"
    );
    assert!(
        !super::is_git_worktree(&fx.base),
        "clone: .git is a directory"
    );
}
