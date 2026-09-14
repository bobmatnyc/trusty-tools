//! Tests for the gate-6 landing-ref refresh (#7889).
//!
//! Why: the refresh's whole value is that it runs real `git fetch` against a
//! real remote, so a mocked git would test nothing. Both tests use the shared
//! REAL-git fixture, inside a `tempfile::TempDir`.

use super::*;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_safety::git_stdout;

/// The remote-tracking ref this checkout currently holds for `main`.
fn origin_main(dir: &std::path::Path) -> String {
    git_stdout(dir, &["rev-parse", "refs/remotes/origin/main"])
        .expect("origin/main must exist in the fixture")
        .trim()
        .to_string()
}

/// A refresh moves the stale landing ref onto the commit the remote holds.
#[test]
fn a_refresh_updates_the_stale_landing_ref() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("agent-refresh");
    let before = origin_main(&wt);

    fx.land_on_the_remote_only(&wt, "landed.txt");
    assert_eq!(
        origin_main(&wt),
        before,
        "the fixture must leave this checkout's landing ref STALE, or the test \
         proves nothing"
    );

    refresh_landing_refs(&wt).expect("a reachable origin must fetch");
    assert_ne!(
        origin_main(&wt),
        before,
        "the refresh must advance refs/remotes/origin/main"
    );
}

/// 🔴 Fail-closed: a fetch that FAILS reports the failure and leaves every ref
/// exactly as it was — it must never read as a successful refresh.
#[test]
fn a_refresh_against_a_missing_remote_fails_without_touching_the_refs() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("agent-broken-remote");
    let before = origin_main(&wt);

    // Point `origin` at a path that is not a repository. Spelled as a local
    // path so the failure is immediate and this test never touches a network.
    let missing = fx.repos_root.join("no-such-remote.git");
    git_stdout(
        &wt,
        &[
            "remote",
            "set-url",
            "origin",
            missing.to_str().expect("utf8 path"),
        ],
    )
    .expect("set-url must succeed");

    let err = refresh_landing_refs(&wt).expect_err("an unreachable origin must fail");
    assert!(
        err.contains("git fetch --prune origin"),
        "the error names the command that failed: {err}"
    );
    assert_eq!(
        origin_main(&wt),
        before,
        "a failed fetch must advance no ref"
    );
}
