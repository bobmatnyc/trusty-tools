//! Unit tests for `core::worktree_index`.
//!
//! Why: split into a sibling file (declared `#[path = "worktree_index_tests.rs"]
//! mod tests;`), the crate's convention for production modules.
//!
//! What: pins the never-block property of [`super::spawn_detached`] and the
//! worktree vs clone distinction of [`super::is_git_worktree`] against REAL git
//! repositories built in a hermetic temp dir. #10534 removed the creation-time
//! registration these tests used to cover; its regression test is
//! `worktree_creation_registers_no_index` in `base_facet_index_tests.rs`.
//!
//! Test: `cargo test -p trusty-mpm -- core::worktree_index`

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// The spawn helper returns BEFORE its work finishes (regression).
///
/// Why: the base-facet registration behind session launch is a cold walk that
/// measured 35 s on this workspace. If a future edit joined the spawned thread,
/// every session launch would block for that long, and a joined version still
/// produces a correct index, so nothing else in the suite would go red.
/// What: hands `spawn_detached` work that sleeps far longer than the
/// assertion's ceiling, so a `.join()` cannot pass.
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

/// `is_git_worktree` distinguishes the two on-disk shapes.
///
/// Why (regression): relaxing the check from "`.git` is a FILE" to "`.git`
/// EXISTS" would make every plain clone read as a worktree.
/// Test: this test.
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
