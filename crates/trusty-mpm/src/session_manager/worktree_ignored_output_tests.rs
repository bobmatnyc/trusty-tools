//! Tests for [`super`] (#8534): the gitignored-output rule and its fail-safe.
//! Test: this file.

use super::{ignored_output_refusal, is_regenerable, kept_ignored_output};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// Ignore `target/` and `results/` for every worktree of `fx`'s repository.
fn ignore_target_and_results(fx: &GitWorktreeFixture) {
    std::fs::write(fx.repo.join(".git/info/exclude"), "target/\nresults/\n")
        .expect("write info/exclude");
}

/// The rule: build and tool output is excused, anything else is kept.
#[test]
fn rule_excuses_build_output_and_keeps_run_output() {
    for (entry, regenerable) in [
        ("target/", true),
        ("crates/x/target/", true),
        ("target-worktree/", true),
        ("web/node_modules/", true),
        ("pkg/__pycache__/", true),
        ("mod.pyc", true),
        (".DS_Store", true),
        ("results/", false),
        ("run-output.json", false),
        (".env.local", false),
        ("targets/", false),
    ] {
        assert_eq!(is_regenerable(entry), regenerable, "{entry}");
    }
}

/// A gitignored results directory is kept output, counted file by file.
#[test]
fn kept_output_counts_files_in_an_ignored_results_dir() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("ignored-results");
    ignore_target_and_results(&fx);
    std::fs::create_dir_all(wt.join("results/batch")).expect("mkdir");
    std::fs::write(wt.join("results/run.json"), "{}").expect("write");
    std::fs::write(wt.join("results/batch/part.json"), "{}").expect("write");

    let kept = kept_ignored_output(&wt)
        .expect("the check completes")
        .expect("results/ is kept output");

    assert_eq!(kept.files, 2);
    assert_eq!(kept.first, "results/");
}

/// Gitignored build output alone is nothing to keep.
#[test]
fn kept_output_is_none_for_build_output_only() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("ignored-target");
    ignore_target_and_results(&fx);
    std::fs::create_dir_all(wt.join("target/debug")).expect("mkdir");
    std::fs::write(wt.join("target/debug/bin"), "elf").expect("write");
    std::fs::create_dir_all(wt.join("results")).expect("an empty ignored dir");

    assert_eq!(kept_ignored_output(&wt), Ok(None));
}

/// A directory git cannot answer for is an error, never "nothing kept".
#[test]
fn kept_output_errors_when_git_cannot_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = kept_ignored_output(dir.path()).expect_err("git status fails outside a repo");
    assert!(err.contains("git"), "{err}");
}

/// Fail-safe: a failed check keeps the tree and says why.
#[test]
fn refusal_keeps_the_tree_when_the_check_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reason = ignored_output_refusal(dir.path()).expect("a failed check must refuse");
    assert!(reason.contains("check failed"), "{reason}");
    assert!(
        reason.contains(&dir.path().display().to_string()),
        "{reason}"
    );
}
