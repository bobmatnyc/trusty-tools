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

    let (contamination, foreign, _build_tree, _missing) = check_hooks_hygiene(Some(&project), &[]);
    assert_eq!(contamination.status, CheckStatus::Ok);
    assert_eq!(foreign.status, CheckStatus::Ok);
}

#[test]
fn check_hooks_hygiene_warns_on_tm_contamination() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("contaminated-project");
    write_settings(&project, &tm_hooks_value());

    let (contamination, foreign, _build_tree, _missing) = check_hooks_hygiene(Some(&project), &[]);
    assert_eq!(contamination.status, CheckStatus::Warn);
    assert!(contamination.message.contains("tm hooks clean"));
    assert_eq!(foreign.status, CheckStatus::Ok);
}

#[test]
fn check_hooks_hygiene_warns_on_foreign_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("foreign-project");
    write_settings(&project, &claude_mpm_hooks_value());

    let (contamination, foreign, _build_tree, _missing) = check_hooks_hygiene(Some(&project), &[]);
    assert_eq!(contamination.status, CheckStatus::Ok);
    assert_eq!(foreign.status, CheckStatus::Warn);
    assert!(foreign.message.contains("informational"));
}

/// Why (issue #2948): a hand-mixed `PreToolUse` group carrying BOTH a tm
/// entry and a foreign entry must trip BOTH checks — before the `.any()` fix
/// this shape satisfied neither predicate and was invisible to `tm doctor`.
#[test]
fn check_hooks_hygiene_warns_on_mixed_group_both_checks() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("mixed-project");
    write_settings(
        &project,
        &serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "*",
                    "hooks": [
                        { "type": "command", "command": "/usr/local/bin/tm hook", "timeout": 5 },
                        { "type": "command", "command": "claude-mpm hooks fire PreToolUse", "timeout": 5 }
                    ]
                }]
            }
        }),
    );

    let (contamination, foreign, _build_tree, _missing) = check_hooks_hygiene(Some(&project), &[]);
    assert_eq!(
        contamination.status,
        CheckStatus::Warn,
        "the tm entry in the mixed group must be flagged"
    );
    assert_eq!(
        foreign.status,
        CheckStatus::Warn,
        "the foreign entry in the SAME mixed group must also be flagged"
    );
}

#[test]
fn check_hooks_hygiene_never_double_counts_active_workspace_dupes() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("dupe-project");
    write_settings(&project, &tm_hooks_value());

    // The SAME project is passed as BOTH `project_dir` and an active
    // workspace path — it must be counted once, not twice.
    let (contamination, _foreign, _build_tree, _missing) =
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

    let (contamination, _foreign, _build_tree, _missing) =
        check_hooks_hygiene(Some(&project), std::slice::from_ref(&other_workspace));
    assert_eq!(
        contamination.status,
        CheckStatus::Warn,
        "a live workspace outside project_dir must still be scanned"
    );
}

// ---------------------------------------------------------------------------
// #7262: `hooks_build_tree_binary`. These FAIL on 1ba38636c, where no check
// existed and `tm_hook_event_names` could not see the incident shape either.
// ---------------------------------------------------------------------------

use crate::core::standalone::hooks::build_tree::tests::{INCIDENT_EXE, incident_settings};

#[test]
fn check_hooks_hygiene_reports_the_build_tree_incident_shape() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("corrupted-project");
    let settings = write_settings(&project, &incident_settings());

    let (contamination, _foreign, build_tree, _missing) = check_hooks_hygiene(Some(&project), &[]);
    assert_eq!(build_tree.status, CheckStatus::Warn);
    assert!(
        build_tree.message.contains(&*settings.to_string_lossy()),
        "the report must name the file: {}",
        build_tree.message
    );
    for cmd in [
        format!("{INCIDENT_EXE} hook"),
        format!("{INCIDENT_EXE} hook --pm-guard"),
    ] {
        assert!(
            build_tree.message.contains(&cmd),
            "the report must name `{cmd}`: {}",
            build_tree.message
        );
    }
    // The pre-existing check must see it now too, so `--fix` has something to
    // repair rather than a finding with no remedy.
    assert_eq!(contamination.status, CheckStatus::Warn);
}

#[test]
fn check_hooks_hygiene_reports_a_build_tree_statusline() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("statusline-only-project");
    write_settings(
        &project,
        &serde_json::json!({
            "statusLine": {
                "type": "command",
                "command": format!("{INCIDENT_EXE} statusline")
            }
        }),
    );

    let (contamination, _foreign, build_tree, _missing) = check_hooks_hygiene(Some(&project), &[]);
    assert_eq!(build_tree.status, CheckStatus::Warn);
    assert!(
        build_tree
            .message
            .contains(&format!("{INCIDENT_EXE} statusline")),
        "the statusLine command must be named: {}",
        build_tree.message
    );
    assert_eq!(
        contamination.status,
        CheckStatus::Ok,
        "a statusLine command is not a hook entry and must not be reported as one"
    );
}

#[test]
fn check_hooks_hygiene_build_tree_check_is_ok_for_an_installed_binary() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("installed-binary-project");
    write_settings(&project, &tm_hooks_value());

    let (_contamination, _foreign, build_tree, _missing) = check_hooks_hygiene(Some(&project), &[]);
    assert_eq!(
        build_tree.status,
        CheckStatus::Ok,
        "an installed `/usr/local/bin/tm hook` is not build-tree contamination"
    );
}

/// The #7490 incident file: `SessionStart` wired to the memory hook only, with
/// the PM guard present so the file reads as tm-provisioned.
fn missing_sessionstart_value() -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": "/usr/local/bin/tm hook --pm-guard",
                    "timeout": 5
                }]
            }],
            "SessionStart": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": "trusty-memory inbox-check",
                    "timeout": 60
                }]
            }]
        }
    })
}

/// The gap check names the missing event, not just the file (#7490).
#[test]
fn check_hooks_hygiene_reports_a_missing_sessionstart_group() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("stale-project");
    write_settings(&project, &missing_sessionstart_value());

    let (_contamination, _foreign, _build_tree, missing) = check_hooks_hygiene(Some(&project), &[]);
    assert_eq!(missing.status, CheckStatus::Warn);
    assert!(
        missing.message.contains("SessionStart"),
        "the report must name the unwired event: {}",
        missing.message
    );
    assert!(
        missing.message.contains("tm doctor --fix"),
        "the report must name the repair: {}",
        missing.message
    );
}

/// A file carrying every lifecycle group reports Ok (#7490).
#[test]
fn check_hooks_hygiene_missing_group_check_is_ok_for_a_complete_file() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("complete-project");
    write_settings(&project, &missing_sessionstart_value());
    crate::core::session_launch::ensure_project_hooks(
        &project,
        Some(Path::new("/usr/local/bin/tm")),
    )
    .expect("the merge must succeed against a pinned installed binary");

    let (_contamination, _foreign, _build_tree, missing) = check_hooks_hygiene(Some(&project), &[]);
    assert_eq!(
        missing.status,
        CheckStatus::Ok,
        "a merged file has no gap: {}",
        missing.message
    );
}

/// A project tm never provisioned is never told to adopt tm's hooks (#7490).
#[test]
fn check_hooks_hygiene_missing_group_check_ignores_a_foreign_project() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("foreign-project");
    write_settings(&project, &claude_mpm_hooks_value());

    let (_contamination, _foreign, _build_tree, missing) = check_hooks_hygiene(Some(&project), &[]);
    assert_eq!(
        missing.status,
        CheckStatus::Ok,
        "a foreign project owes no tm hook group: {}",
        missing.message
    );
}
