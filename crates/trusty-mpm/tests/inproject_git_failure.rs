//! Fail-open regression coverage for the in-project spawn path (#4734).
//!
//! Why: `try_inproject_spawn` decides whether a managed session gets worktree
//! isolation, a protected base clone, and a push guard — or runs directly in
//! the operator's live checkout. It made that call from a bare `Option`, so a
//! git invocation that FAILED on a directory with a `.git` entry was
//! indistinguishable from a repo that simply has no `origin`, and the failure
//! took the unisolated branch. Every test here asserts the failing git call is
//! now an error, so a regression to the collapsed signal turns this file red.
//!
//! These tests deliberately touch only [`try_inproject_spawn`], whose signature
//! the fix did not change: that is what let them be run against the pre-fix
//! commit to prove they fail there.
//!
//! What: two directories that satisfy the function's own precondition
//! (`is_dir()` and `.git` present) but that git cannot answer for — one because
//! `.git/config` is unparseable, one because `git` is not on `PATH` at all.
//! Test: this IS the test file.
//!
//! Both tests are `#[serial]` because the second replaces the process-global
//! `PATH`, which would otherwise break any concurrently-running test that
//! spawns git.

use trusty_mpm::daemon::managed_routes::inproject::try_inproject_spawn;

/// Build a real git repo in `dir` so `.git` genuinely exists.
fn init_repo(dir: &std::path::Path) {
    let out = std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(dir)
        .output()
        .expect("spawn git init");
    assert!(
        out.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// An unreadable `.git/config` must stop the spawn, not downgrade it (#4734).
///
/// Why: this is the ticket's exact scenario. `git config --get` exits 128 on a
/// config it cannot parse; pre-fix that became `Ok(None)`, and `Ok(None)` is
/// the value `lifecycle::spawn_managed_routed` reads as "not an in-project
/// GitHub checkout" before falling through to `spawn_managed_local` — a session
/// in the live tree with no isolation.
/// What: initialises a repo, overwrites `.git/config` with an unterminated
/// section header, and asserts `Err`.
/// Test: this function IS the test.
#[test]
#[serial_test::serial]
fn try_inproject_spawn_errors_when_git_cannot_read_the_remote() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let dir = tmp.path();
    init_repo(dir);
    std::fs::write(dir.join(".git/config"), "[remote \"origin\"\n").expect("corrupt config");

    let result = try_inproject_spawn(dir);
    assert!(
        result.is_err(),
        "a git config git cannot read must fail the spawn, not fall through to an \
         unisolated local spawn; got {result:?}"
    );
}

/// Git missing from `PATH` must stop the spawn too (#4734).
///
/// Why: covers the other half of the collapsed signal — pre-fix the spawn
/// failure was swallowed by `.output().ok()?`, so a broken or absent git
/// binary produced the same silent isolation downgrade as an unreadable
/// config. Exercising it needs the process-global `PATH`, hence `#[ignore]`;
/// rung 5 of the test ladder runs it via
/// `cargo test -p trusty-mpm -- --include-ignored`.
/// What: points `PATH` at an empty directory so `git` cannot be spawned at
/// all, and asserts `Err`. `PATH` is restored before the assertion so a
/// failure does not poison the rest of the binary.
/// Test: this function IS the test.
#[test]
#[ignore = "replaces the process-global PATH; run with --include-ignored"]
#[serial_test::serial]
fn try_inproject_spawn_errors_when_git_cannot_be_executed() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let dir = tmp.path();
    // No `git init` here — git is about to be unavailable, and the function
    // only requires that a `.git` entry exists.
    std::fs::create_dir(dir.join(".git")).expect("mkdir .git");

    let empty = tempfile::TempDir::new().expect("empty PATH dir");
    let previous = std::env::var_os("PATH");

    // SAFETY: `#[serial]` keeps every other test in this binary off the CPU for
    // the duration, and the binary contains only these two tests.
    unsafe { std::env::set_var("PATH", empty.path()) };
    let result = try_inproject_spawn(dir);
    unsafe {
        match previous {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
    }

    assert!(
        result.is_err(),
        "an unspawnable git must fail the spawn, not fall through to an \
         unisolated local spawn; got {result:?}"
    );
}
