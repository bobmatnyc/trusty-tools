//! Fixture-based tests for the #7171 maintenance-storm doctor probes.
//!
//! Test: this file IS the test module for `super`.

use super::*;
use crate::core::doctor::CheckStatus;

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    trusty_common::git::command_in(dir)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Build `<root>/owner/repo` as a real git repo with `worktree_count` fake
/// `.worktrees/*` entries (plain directories — this probe only counts
/// entries, it never asks git about them). Returns `None` when `git` is
/// unavailable on the runner, mirroring the established pattern in
/// `session_manager::decommission_worktree_tests`.
fn base_clone_with_worktrees(root: &Path, worktree_count: usize) -> Option<PathBuf> {
    let base = root.join("owner").join("repo");
    std::fs::create_dir_all(&base).ok()?;
    if !git_ok(&base, &["init", "-q", "."]) {
        return None;
    }
    let worktrees_dir = base.join(".worktrees");
    std::fs::create_dir_all(&worktrees_dir).ok()?;
    for i in 0..worktree_count {
        std::fs::create_dir_all(worktrees_dir.join(format!("wt-{i}"))).ok()?;
    }
    Some(base)
}

#[test]
fn ok_with_no_repos_root() {
    let check = check_maintenance_config(None);
    assert_eq!(check.status, CheckStatus::Ok);
}

#[test]
fn ok_when_worktree_count_is_at_or_below_threshold() {
    let Ok(root) = tempfile::tempdir() else {
        return;
    };
    let Some(_base) = base_clone_with_worktrees(root.path(), WORKTREE_WARN_THRESHOLD) else {
        return; // no git on this runner
    };
    let check = check_maintenance_config(Some(root.path()));
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

#[test]
fn ok_when_maintenance_auto_is_pinned_off() {
    let Ok(root) = tempfile::tempdir() else {
        return;
    };
    let Some(base) = base_clone_with_worktrees(root.path(), WORKTREE_WARN_THRESHOLD + 1) else {
        return;
    };
    assert!(git_ok(
        &base,
        &["config", "--local", "maintenance.auto", "false"]
    ));
    let check = check_maintenance_config(Some(root.path()));
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

#[test]
fn warns_when_maintenance_auto_is_unset_above_threshold() {
    let Ok(root) = tempfile::tempdir() else {
        return;
    };
    let Some(base) = base_clone_with_worktrees(root.path(), WORKTREE_WARN_THRESHOLD + 1) else {
        return;
    };
    let check = check_maintenance_config(Some(root.path()));
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(
        check.message.contains(&base.display().to_string()),
        "message must name the exposed base clone: {}",
        check.message
    );
}

#[test]
fn count_maintenance_processes_ignores_unrelated_lines() {
    let ps = "/usr/bin/tmux\n/bin/zsh -l\ngrep git maintenance\n";
    // "grep git maintenance" contains neither exact substring this counter
    // matches, so it must not be counted as a live maintenance process.
    assert_eq!(count_maintenance_processes(ps), 0);
}

#[test]
fn count_maintenance_processes_counts_each_matching_line() {
    let ps = "git maintenance run --auto\nsome-other-proc\ngit maintenance run --task=gc\n";
    assert_eq!(count_maintenance_processes(ps), 2);
}

#[test]
fn count_maintenance_processes_empty_output_is_zero() {
    assert_eq!(count_maintenance_processes(""), 0);
}

#[test]
fn live_maintenance_processes_probe_does_not_panic() {
    // Real `ps` call — asserts only that it returns SOME check, never that
    // this host happens to be storming right now.
    let check = check_live_maintenance_processes();
    assert!(matches!(check.status, CheckStatus::Ok | CheckStatus::Warn));
}

/// #4005 / critic HIGH-1: a `ps` spawn failure must report `Unknown`, never
/// degrade to `Ok` — a sandboxed host with no `ps` must not read as "0
/// processes, no storm" during an actual one.
#[test]
fn check_live_maintenance_processes_reports_unknown_when_ps_is_unavailable() {
    let check = check_live_maintenance_processes_with(|| {
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    });
    assert_eq!(check.status, CheckStatus::Unknown);
    assert!(
        check.message.contains("could not enumerate host processes"),
        "message must name the failure: {}",
        check.message
    );
}

/// A successful spawn with zero matching lines still reports `Ok`, not
/// `Unknown` — only the SPAWN failing is unknown; an empty process table is a
/// known, healthy answer.
#[test]
fn check_live_maintenance_processes_reports_ok_when_ps_succeeds_with_no_matches() {
    let check = check_live_maintenance_processes_with(|| {
        Ok(std::process::Command::new("true")
            .output()
            .expect("spawn `true`"))
    });
    assert_eq!(check.status, CheckStatus::Ok);
}
