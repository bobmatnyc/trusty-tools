//! Unit tests for [`super`] (`tm doctor` hook-hygiene probe, issue #2940).
//!
//! Why: split out of `doctor_hooks_hygiene.rs` to mirror the sibling
//! `doctor_fs_checks.rs` test-module convention.
//! What: exercises `check_hooks_hygiene` against fixture project trees —
//! clean, tm-contaminated, foreign-conflicted, and the project_dir /
//! active-workspace dedup path. Every case passes explicit, bounded paths
//! (never a `$HOME`-wide walk) — see `candidate_settings_files`'s doc for why.
//! Test: this module IS the test suite for `super`.

use super::*;
use crate::core::doctor::CheckStatus;

fn write_settings(project: &Path, contents: &serde_json::Value) -> PathBuf {
    let claude_dir = project.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let path = claude_dir.join("settings.json");
    std::fs::write(&path, serde_json::to_string_pretty(contents).unwrap()).unwrap();
    path
}

fn tm_hooks_value() -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "*",
                "hooks": [{ "type": "command", "command": "/usr/local/bin/tm hook", "timeout": 5 }]
            }]
        }
    })
}

fn claude_mpm_hooks_value() -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "",
                "hooks": [{ "type": "command", "command": "claude-mpm hooks fire SessionStart", "timeout": 5 }]
            }]
        }
    })
}

#[test]
fn check_hooks_hygiene_ok_when_clean() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("clean-project");
    write_settings(
        &project,
        &serde_json::json!({ "outputStyle": "trusty-mpm" }),
    );

    let (contamination, foreign) = check_hooks_hygiene(Some(&project), &[]);
    assert_eq!(contamination.status, CheckStatus::Ok);
    assert_eq!(foreign.status, CheckStatus::Ok);
}

#[test]
fn check_hooks_hygiene_warns_on_tm_contamination() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("contaminated-project");
    write_settings(&project, &tm_hooks_value());

    let (contamination, foreign) = check_hooks_hygiene(Some(&project), &[]);
    assert_eq!(contamination.status, CheckStatus::Warn);
    assert!(contamination.message.contains("tm hooks clean"));
    assert_eq!(foreign.status, CheckStatus::Ok);
}

#[test]
fn check_hooks_hygiene_warns_on_foreign_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("foreign-project");
    write_settings(&project, &claude_mpm_hooks_value());

    let (contamination, foreign) = check_hooks_hygiene(Some(&project), &[]);
    assert_eq!(contamination.status, CheckStatus::Ok);
    assert_eq!(foreign.status, CheckStatus::Warn);
    assert!(foreign.message.contains("informational"));
}

#[test]
fn check_hooks_hygiene_never_double_counts_active_workspace_dupes() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("dupe-project");
    write_settings(&project, &tm_hooks_value());

    // The SAME project is passed as BOTH `project_dir` and an active
    // workspace path — it must be counted once, not twice.
    let (contamination, _foreign) =
        check_hooks_hygiene(Some(&project), std::slice::from_ref(&project));
    assert_eq!(contamination.status, CheckStatus::Warn);
    assert_eq!(
        contamination
            .message
            .matches(&*project.to_string_lossy())
            .count(),
        1,
        "the same file must appear exactly once in the report: {}",
        contamination.message
    );
}

#[test]
fn check_hooks_hygiene_covers_active_workspace_paths_beyond_project_dir() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("current-project");
    let other_workspace = dir.path().join("other-managed-workspace");
    std::fs::create_dir_all(&project).unwrap();
    write_settings(&other_workspace, &tm_hooks_value());

    let (contamination, _foreign) =
        check_hooks_hygiene(Some(&project), std::slice::from_ref(&other_workspace));
    assert_eq!(
        contamination.status,
        CheckStatus::Warn,
        "a live workspace outside project_dir must still be scanned"
    );
}
