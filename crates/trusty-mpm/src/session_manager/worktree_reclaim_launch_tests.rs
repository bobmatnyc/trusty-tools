//! Tests for the worktree-launched-process gate (#7504).
//!
//! Why: the gate's whole value is the containment DIRECTION — a process inside
//! the candidate refuses, a process in an ancestor does not. Getting that
//! backwards either deletes the daemon's own footing or spares every worktree in
//! the repository the daemon was started from, and both read as "the sweep is
//! behaving".
//! What: one test per branch of [`launch_refusal`], plus a liveness pin on the
//! production collector.

use std::path::{Path, PathBuf};

use super::{launch_refusal, process_launch_dirs};

/// A real directory tree to compare against, since the gate canonicalizes.
fn tree() -> tempfile::TempDir {
    tempfile::tempdir().expect("scratch tree")
}

/// A process whose cwd is a subdirectory of the candidate refuses (#7504).
///
/// Fails without the gate: `launch_refusal` answers `None` and the candidate
/// reaches `remove_session_worktree`.
#[test]
fn a_launch_dir_inside_the_candidate_refuses() {
    let dir = tree();
    let candidate = dir.path().join("wt");
    let inside = candidate.join("crates").join("trusty-mpm");
    std::fs::create_dir_all(&inside).expect("create nested dir");

    let reason = launch_refusal(&candidate, std::slice::from_ref(&inside))
        .unwrap_or_else(|| panic!("{} must be spared", candidate.display()));
    assert!(
        reason.contains(&inside.display().to_string()),
        "the refusal must name the launch directory: {reason}"
    );
}

/// The candidate itself as a launch directory refuses (#7504).
#[test]
fn the_candidate_itself_as_a_launch_dir_refuses() {
    let dir = tree();
    let candidate = dir.path().join("wt");
    std::fs::create_dir_all(&candidate).expect("create candidate");

    assert!(
        launch_refusal(&candidate, std::slice::from_ref(&candidate)).is_some(),
        "a process whose cwd IS the candidate must spare it"
    );
}

/// A launch directory that CONTAINS the candidate does not refuse (#7504).
///
/// The inverse of the test above, and the one that matters for usefulness: the
/// daemon is normally started from the repository root, which is an ancestor of
/// every worktree under it. Refusing on an ancestor would spare all of them.
#[test]
fn a_launch_dir_containing_the_candidate_does_not_refuse() {
    let dir = tree();
    let candidate = dir.path().join(".claude").join("worktrees").join("agent-1");
    std::fs::create_dir_all(&candidate).expect("create candidate");

    assert_eq!(
        launch_refusal(&candidate, &[dir.path().to_path_buf()]),
        None,
        "an ancestor launch directory must not spare the worktree"
    );
}

/// An unrelated launch directory does not refuse (#7504).
#[test]
fn an_unrelated_launch_dir_does_not_refuse() {
    let dir = tree();
    let candidate = dir.path().join("wt-a");
    let elsewhere = dir.path().join("wt-b").join("src");
    std::fs::create_dir_all(&candidate).expect("create candidate");
    std::fs::create_dir_all(&elsewhere).expect("create elsewhere");

    assert_eq!(launch_refusal(&candidate, &[elsewhere]), None);
}

/// An empty launch set is a no-op gate (#7504).
///
/// Every pre-#7504 call site passes one, so this pins that the gate changes
/// nothing for them.
#[test]
fn an_empty_launch_set_never_refuses() {
    assert_eq!(launch_refusal(Path::new("/tmp/whatever"), &[]), None);
}

/// A launch directory that cannot be canonicalized still refuses by its literal
/// spelling (#7504).
///
/// Why it matters: the commonest way a launch directory stops resolving is the
/// directory being deleted — which is exactly the state a half-finished sweep
/// leaves. Dropping an unresolvable entry would fail OPEN on the one input whose
/// absence is evidence of trouble.
#[test]
fn an_unresolvable_launch_dir_still_refuses_by_its_literal_spelling() {
    let candidate = PathBuf::from("/nonexistent-7504/wt");
    let inside = candidate.join("gone");

    assert!(
        launch_refusal(&candidate, &[inside]).is_some(),
        "a literal-prefix match must refuse when neither path canonicalizes"
    );
}

/// The production collector reports at least the current working directory.
///
/// A liveness pin, not a discriminating one: it asserts the collector returns
/// real paths rather than an empty vector, which is what would silently disable
/// the gate in production while every pure test above kept passing.
#[test]
fn process_launch_dirs_reports_the_current_directory() {
    let dirs = process_launch_dirs();
    let cwd = std::env::current_dir().expect("a cwd");
    assert!(
        dirs.contains(&cwd),
        "the collector must report the process's own cwd; got {dirs:?}"
    );
}
