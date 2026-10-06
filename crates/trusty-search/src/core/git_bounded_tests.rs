//! Tests for `core::git::run_git_bounded` (#9029).
//!
//! What: the deadline, the non-zero exit and the stdout cap, each against a
//! real child process.
//! Test: `cargo test -p trusty-search -- bounded_git`.

use super::*;
use std::time::{Duration, Instant};

/// Write an executable shell script standing in for git.
#[cfg(unix)]
fn fake_git(dir: &Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-git.sh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake git");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// #9029: a git that hangs is killed at the deadline, not waited on.
#[cfg(unix)]
#[test]
fn bounded_git_kills_a_run_past_its_deadline() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = fake_git(tmp.path(), "sleep 30");
    let started = Instant::now();
    let got = run_git_bounded(
        tmp.path(),
        &bin.to_string_lossy(),
        &["diff"],
        Some(Duration::from_millis(200)),
        1024,
    );
    assert!(
        matches!(got, Err(GitRunError::TimedOut(_))),
        "a hung git must time out: {got:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the deadline was not enforced: {:?}",
        started.elapsed()
    );
}

/// #9029: a non-zero exit is an error carrying git's stderr, never empty output.
#[test]
fn bounded_git_reports_a_non_zero_exit_with_stderr() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let got = run_git_bounded(
        tmp.path(),
        "git",
        &["--no-pager", "diff", "HEAD"],
        Some(Duration::from_secs(10)),
        1024,
    );
    match got {
        Err(GitRunError::Exit { stderr, .. }) => assert!(!stderr.is_empty(), "stderr kept"),
        other => panic!("a git failure must be an Exit error: {other:?}"),
    }
}

/// #9029: output past the cap is cut and reported as truncated.
#[cfg(unix)]
#[test]
fn bounded_git_caps_stdout_and_says_so() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = fake_git(tmp.path(), "printf 'abcdefghij'");
    let out = run_git_bounded(
        tmp.path(),
        &bin.to_string_lossy(),
        &[],
        Some(Duration::from_secs(10)),
        4,
    )
    .expect("the fake git succeeds");
    assert_eq!(out.stdout, b"abcd");
    assert!(out.truncated);

    let whole = run_git_bounded(tmp.path(), &bin.to_string_lossy(), &[], None, 64)
        .expect("the fake git succeeds");
    assert_eq!(whole.stdout, b"abcdefghij");
    assert!(!whole.truncated);
}
