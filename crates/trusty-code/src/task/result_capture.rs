//! Turns a finished daemon run into the actionable [`TaskResult`] a caller
//! gets back from `session.status` (#4351).
//!
//! Why: `task::executor` knows the terminal `SessionStatus` and nothing else a
//! caller can act on. The refs it needs — did this run change anything, and
//! where did that land — are on disk, so somebody has to look. Keeping that
//! lookup here rather than inline keeps `executor.rs` under its SLOC cap and
//! makes the mapping testable against real temp repos without spawning an
//! agent loop.
//! What: [`build_task_result`] captures the working root a second time,
//! compares it against the before-snapshot the executor took, and folds the
//! outcome into a `TaskResult`. It reuses `run_task::diff` (the crate's ONE
//! working-tree snapshot implementation, which already stages into a throwaway
//! index rather than touching the user's) and
//! `trusty_common::catchup::git::capture_git_status` for the branch, rather
//! than spawning its own git.
//! Test: `tests::a_run_that_changed_a_file_reports_a_diff_ref_and_branch`,
//! `tests::a_run_that_changed_nothing_reports_none_refs`,
//! `tests::a_non_git_root_reports_no_diff_ref`,
//! `tests::pr_ref_is_never_invented`.

use std::path::Path;

use crate::run_task::diff::{Snapshot, capture_snapshot, diff_snapshots};
use crate::session::{SessionStatus, TaskResult, TaskResultStatus};

/// Fold a finished run's terminal status and on-disk footprint into a
/// [`TaskResult`].
///
/// Why: see the module docs. Called once, immediately before
/// `SessionRegistry::finish`, so the result and the status land together.
/// What: `before` is the snapshot the executor took at run start. A run whose
/// after-snapshot differs reports `diff_ref` — the git tree object SHA naming
/// the post-run state, which is a real ref a caller can `git show`. A non-git
/// root has no such ref, so it reports `None` while still saying in `summary`
/// that something changed. `pr_ref` is always `None`: nothing in this daemon
/// opens a pull request, and inventing one would be worse than admitting it.
/// Every probe is fail-open — a git error degrades a field to `None`, it never
/// fails the run, which has already completed by the time this is called.
/// Test: `tests::a_run_that_changed_a_file_reports_a_diff_ref_and_branch`,
/// `tests::a_run_that_changed_nothing_reports_none_refs`,
/// `tests::a_non_git_root_reports_no_diff_ref`,
/// `tests::pr_ref_is_never_invented`.
pub(super) fn build_task_result(
    status: SessionStatus,
    before: &Snapshot,
    root: &Path,
) -> TaskResult {
    let after = capture_snapshot(root);
    let changed = !diff_snapshots(before, &after).trim().is_empty();
    let diff_ref = if changed { tree_ref(&after) } else { None };
    let branch = trusty_common::catchup::git::capture_git_status(root).branch;
    let result_status = TaskResultStatus::from_session_status(status);
    let summary = summarise(
        result_status,
        changed,
        diff_ref.as_deref(),
        branch.as_deref(),
    );

    TaskResult::new(result_status)
        .with_diff_ref(diff_ref)
        .with_branch(branch)
        .with_pr_ref(None)
        .with_summary(Some(summary))
}

/// The git tree object SHA of a snapshot, when it has one.
fn tree_ref(after: &Snapshot) -> Option<String> {
    match after {
        Snapshot::Git { tree, .. } if !tree.is_empty() => Some(tree.clone()),
        _ => None,
    }
}

/// One line a caller can show a user verbatim.
fn summarise(
    status: TaskResultStatus,
    changed: bool,
    diff_ref: Option<&str>,
    branch: Option<&str>,
) -> String {
    let change = match (changed, diff_ref) {
        (true, Some(r)) => format!("changes captured at {r}"),
        (true, None) => "changes captured".to_string(),
        (false, _) => "no changes".to_string(),
    };
    match branch {
        Some(b) => format!("{}: {change} on branch {b}", status.as_str()),
        None => format!("{}: {change}", status.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Initialise a real git repo with one commit so `capture_snapshot` takes
    /// the `Snapshot::Git` path and a branch name is resolvable.
    fn git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "t@example.invalid"],
            vec!["config", "user.name", "t"],
        ] {
            let ok = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(&args)
                .output()
                .expect("git runs")
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        }
        std::fs::write(root.join("seed.txt"), "seed\n").expect("write seed");
        for args in [
            vec!["add", "-A"],
            vec!["commit", "-m", "seed", "--no-gpg-sign"],
        ] {
            let ok = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(&args)
                .output()
                .expect("git runs")
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        }
        dir
    }

    /// #4351 acceptance: a run that touched a file reports a real ref, the
    /// branch it landed on, and a status derived from the session's own
    /// terminal state.
    #[test]
    fn a_run_that_changed_a_file_reports_a_diff_ref_and_branch() {
        let dir = git_repo();
        let root = dir.path();
        let before = capture_snapshot(root);

        std::fs::write(root.join("written-by-the-run.txt"), "hello\n").expect("write");

        let result = build_task_result(SessionStatus::Finished, &before, root);
        assert_eq!(result.status, TaskResultStatus::Success);
        let diff_ref = result.diff_ref.expect("a changed git tree yields a ref");
        assert_eq!(
            diff_ref.len(),
            40,
            "expected a git tree object SHA, got {diff_ref:?}"
        );
        assert_eq!(result.branch.as_deref(), Some("main"));
        let summary = result.summary.expect("summary");
        assert!(
            summary.contains(&diff_ref) && summary.contains("main"),
            "summary must name the ref and the branch: {summary}"
        );
    }

    /// #4351 acceptance: a run that changed nothing reports `None` refs and
    /// still reports its status.
    #[test]
    fn a_run_that_changed_nothing_reports_none_refs() {
        let dir = git_repo();
        let root = dir.path();
        let before = capture_snapshot(root);

        let result = build_task_result(SessionStatus::TurnCapExceeded, &before, root);
        assert_eq!(result.status, TaskResultStatus::Partial);
        assert_eq!(result.diff_ref, None);
        assert_eq!(result.pr_ref, None);
        assert_eq!(
            result.summary.as_deref(),
            Some("partial: no changes on branch main")
        );
    }

    /// A plain directory has no git ref to report, but the run's status and
    /// the fact that something changed still come through.
    #[test]
    fn a_non_git_root_reports_no_diff_ref() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let before = capture_snapshot(root);
        std::fs::write(root.join("a.txt"), "a\n").expect("write");

        let result = build_task_result(SessionStatus::Finished, &before, root);
        assert_eq!(result.status, TaskResultStatus::Success);
        assert_eq!(result.diff_ref, None);
        assert_eq!(result.branch, None);
        assert_eq!(result.summary.as_deref(), Some("success: changes captured"));
    }

    /// The daemon never opens a pull request, so it never claims one.
    #[test]
    fn pr_ref_is_never_invented() {
        let dir = git_repo();
        let root = dir.path();
        let before = capture_snapshot(root);
        std::fs::write(root.join("b.txt"), "b\n").expect("write");

        for status in [
            SessionStatus::Finished,
            SessionStatus::Failed,
            SessionStatus::Cancelled,
            SessionStatus::DeadlineExceeded,
        ] {
            assert_eq!(build_task_result(status, &before, root).pr_ref, None);
        }
    }
}
