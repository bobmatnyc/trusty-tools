//! Tests for the shared kill-on-timeout subprocess runner (#7965).
//!
//! Why only four: the process-group kill and the hung-child reap are already
//! covered end to end against this exact code by
//! `run_with_timeout_kills_the_whole_process_group` and
//! `run_with_timeout_kills_a_hung_child` in `worktree_reclaim_tests.rs`, which
//! now reach it through the `gh` adapter. What is NEW at this layer is the
//! return shape the adapter throws away — a non-zero exit is `Ok` here, carrying
//! both streams — and the typed timeout the hygiene sweep branches on.

use std::process::Command;
use std::time::{Duration, Instant};

use super::{BoundedError, run_bounded};

/// A command that exits non-zero is `Ok` here, with both streams captured.
///
/// Why this is the assertion: the `gh` adapter treats a non-zero exit as a
/// failure, but the hygiene sweep must tell "git refused the fast-forward" (a
/// decision, logged and survivable) from "git never answered" (the #7965 hang).
/// Folding both into `Err` at this layer would erase that distinction.
#[test]
fn run_bounded_captures_stdout_and_status() {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", "echo out; echo err >&2; exit 3"]);
    let out = run_bounded(cmd, Duration::from_secs(5)).expect("a non-zero exit is not an error");
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(out.stdout.trim(), "out");
    assert_eq!(out.stderr.trim(), "err");
}

/// Output larger than one pipe buffer still comes back, and does not deadlock.
///
/// Why: polling `try_wait` without draining the pipes wedges on any command
/// whose output exceeds the buffer — `git status --porcelain` in a dirty
/// checkout, `gh pr list` with 400 pull requests. This is the drain threads'
/// regression test.
#[test]
fn run_bounded_drains_a_pipe_larger_than_its_buffer() {
    let mut cmd = Command::new("sh");
    cmd.args([
        "-c",
        "i=0; while [ $i -lt 20000 ]; do echo 0123456789; i=$((i+1)); done",
    ]);
    let out = run_bounded(cmd, Duration::from_secs(20)).expect("a chatty child must not deadlock");
    assert!(out.status.success(), "{:?}", out.status);
    assert_eq!(out.stdout.lines().count(), 20_000);
}

/// A child that outlives its budget is killed and reported as a TIMEOUT.
///
/// 🔴 #7965 REGRESSION: the variant matters, not just the failure. The hygiene
/// sweep logs a timeout as "this base was abandoned" and keeps going; a spawn
/// error means git is missing. Collapsing them would hide a wedged fetch behind
/// a misleading message.
#[test]
fn run_bounded_kills_a_hung_child() {
    let mut cmd = Command::new("sleep");
    cmd.arg("30");
    let started = Instant::now();
    let err = run_bounded(cmd, Duration::from_millis(300)).expect_err("a hung child must fail");
    assert!(matches!(err, BoundedError::TimedOut), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the timeout must actually fire; took {:?}",
        started.elapsed()
    );
}

/// A binary that is not on PATH is a spawn failure, not a timeout.
#[test]
fn run_bounded_reports_a_spawn_failure() {
    let cmd = Command::new("definitely-not-a-real-binary-7965");
    let err = run_bounded(cmd, Duration::from_secs(5)).expect_err("an unspawnable command fails");
    assert!(matches!(err, BoundedError::Spawn(_)), "{err}");
    assert!(err.to_string().contains("could not be run"), "{err}");
}
