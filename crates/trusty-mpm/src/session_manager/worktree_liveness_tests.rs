//! Tests for [`super`] — the OS-level "is anything standing in here?" gate (#4311).
//!
//! Why: this gate's whole value is its fail direction. A wrong implementation
//! that returned `None` on a probe it could not run would read as "free" and
//! hand a live directory to `git worktree remove --force`, so the arms that
//! must refuse get the coverage, not the happy path.
//! Test: this file.

use std::path::{Path, PathBuf};

use super::{process_holding, run_cwd_probe, scan_probe};

/// One `lsof -F pcn` process set.
fn set(pid: &str, command: &str, cwd: &str) -> String {
    format!("p{pid}\nc{command}\nn{cwd}\n")
}

/// A process whose cwd is the candidate itself is named, with pid and command.
#[test]
fn liveness_reports_a_process_standing_in_the_directory() {
    let root = PathBuf::from("/r/.claude/worktrees/agent-x");
    let listing = format!(
        "{}{}",
        set("100", "zsh", "/Users/bob"),
        set("7762", "trusty-memory", "/r/.claude/worktrees/agent-x")
    );

    let reason = scan_probe(&listing, &root).expect("a process in the tree must refuse removal");

    assert!(reason.contains("7762"), "must name the pid: {reason}");
    assert!(
        reason.contains("trusty-memory"),
        "must name the command: {reason}"
    );
}

/// A cwd nested BELOW the candidate still counts — the 2026-08-15 shape.
#[test]
fn liveness_reports_a_process_nested_below_the_directory() {
    let root = PathBuf::from("/r/.claude/worktrees/agent-x");
    let listing = set(
        "7762",
        "cargo",
        "/r/.claude/worktrees/agent-x/crates/trusty-mpm",
    );

    assert!(
        scan_probe(&listing, &root).is_some(),
        "a process one level down is still inside the tree"
    );
}

/// A sibling worktree is not this worktree, and a prefix that is not a path
/// boundary is not a match either.
///
/// Why: a naive `str::starts_with` would read `…/agent-x2` as inside
/// `…/agent-x` and refuse to reap a directory nothing is using, permanently.
#[test]
fn liveness_ignores_a_sibling_directory() {
    let root = PathBuf::from("/r/.claude/worktrees/agent-x");
    let listing = format!(
        "{}{}",
        set("100", "zsh", "/r/.claude/worktrees/agent-y"),
        set("101", "zsh", "/r/.claude/worktrees/agent-x2")
    );

    assert_eq!(
        scan_probe(&listing, &root),
        None,
        "neither sibling is inside the candidate"
    );
}

/// #4311 REGRESSION: a probe that could not be run reads as IN USE, never free.
///
/// Why (ADR-0045): an unanswerable question on a destructive path is
/// UNDETERMINABLE, not ABSENT. Returning `None` here would let a
/// `git worktree remove --force` proceed on a machine where the check never
/// ran at all — the exact shape of #4470's empty-`lsof` defect, one subsystem
/// over.
#[test]
fn liveness_treats_a_missing_lsof_as_in_use() {
    let err = run_cwd_probe("trusty-mpm-no-such-probe-binary")
        .expect_err("an absent probe binary must not yield a listing");

    assert!(
        err.contains("could not run"),
        "must say which step failed: {err}"
    );
}

/// A listing carrying no working directory at all rules nothing out.
#[test]
fn liveness_treats_an_empty_probe_as_in_use() {
    let root = PathBuf::from("/r/.claude/worktrees/agent-x");

    let reason = scan_probe("", &root).expect("an empty listing observed nothing");

    assert!(reason.contains("ADR-0045"), "{reason}");
}

/// A listing in a shape the parser does not recognise is likewise in use.
#[test]
fn liveness_treats_an_unparsable_probe_as_in_use() {
    let root = PathBuf::from("/r/.claude/worktrees/agent-x");

    assert!(
        scan_probe("COMMAND PID USER FD\nzsh 100 bob cwd\n", &root).is_some(),
        "a human-readable table carries no `n` field — the probe answered in a \
         shape this cannot read, which is not evidence of freedom"
    );
}

/// A path that does not exist cannot be compared against a resolved cwd, so the
/// answer is in use rather than free.
#[test]
fn liveness_treats_an_uncanonicalizable_path_as_in_use() {
    let reason = process_holding(Path::new("/nonexistent/trusty-mpm/agent-x"))
        .expect("an unresolvable path must not read as free");

    assert!(reason.contains("canonicalize"), "{reason}");
}

/// The real probe runs on this machine and clears an empty temp directory.
///
/// Why: the parser tests all run against fixed text, so without this nothing
/// proves the flags actually work against the `lsof` that is installed — the
/// gate could fail-closed forever and every other test would still pass.
#[test]
fn liveness_clears_an_unused_directory_against_the_real_probe() {
    let dir = tempfile::tempdir().expect("tempdir");

    match process_holding(dir.path()) {
        None => {}
        // A machine with no `lsof` refuses by design; that is this gate's
        // stated cost, not a test failure. Assert it refused for THAT reason
        // and not because the probe ran and found something in a fresh
        // directory, which would be a real defect.
        Some(reason) => assert!(
            reason.contains("could not run") || reason.contains("exited"),
            "a fresh temp directory holds no process; the only acceptable \
             refusal is an unavailable probe, got: {reason}"
        ),
    }
}
