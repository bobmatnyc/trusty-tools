//! Unit tests for `commands::hooks` (issue #2940, `tm hooks clean`).
//!
//! Why: split out of `hooks.rs` (test-file budget: 1500 SLOC).
//! What: exercises `resolve_targets` and `clean` end-to-end against a
//! fixture project directory.
//! Test: this module IS the test suite for `super`.

use super::*;

fn write_tm_settings(dir: &std::path::Path) -> std::path::PathBuf {
    let claude_dir = dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let settings = claude_dir.join("settings.json");
    std::fs::write(
        &settings,
        serde_json::to_string_pretty(&serde_json::json!({
            "outputStyle": "trusty-mpm",
            "hooks": {
                "PreToolUse": [{
                    "matcher": "*",
                    "hooks": [{ "type": "command", "command": "/usr/local/bin/tm hook", "timeout": 5 }]
                }]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    settings
}

#[test]
fn resolve_targets_with_explicit_path_returns_both_settings_files() {
    let targets = resolve_targets(Some("/tmp/some-project")).unwrap();
    assert_eq!(
        targets,
        vec![
            std::path::PathBuf::from("/tmp/some-project/.claude/settings.json"),
            std::path::PathBuf::from("/tmp/some-project/.claude/settings.local.json"),
        ]
    );
}

#[test]
fn clean_with_explicit_path_scopes_to_that_project() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("my-project");
    std::fs::create_dir_all(&project).unwrap();
    let settings = write_tm_settings(&project);

    clean(Some(project.to_string_lossy().to_string()), false).unwrap();

    // Dry run — the file must be unchanged.
    let val: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert!(
        val["hooks"]["PreToolUse"].is_array(),
        "dry run must not have removed anything"
    );
}

#[test]
fn clean_dry_run_reports_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("my-project");
    std::fs::create_dir_all(&project).unwrap();
    let settings = write_tm_settings(&project);
    let original = std::fs::read_to_string(&settings).unwrap();

    clean(Some(project.to_string_lossy().to_string()), false).unwrap();

    let after = std::fs::read_to_string(&settings).unwrap();
    assert_eq!(after, original, "dry run must never mutate a file");
}

#[test]
fn clean_force_applies_and_reports_backup() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("my-project");
    std::fs::create_dir_all(&project).unwrap();
    let settings = write_tm_settings(&project);

    clean(Some(project.to_string_lossy().to_string()), true).unwrap();

    let val: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert!(
        val["hooks"].get("PreToolUse").is_none(),
        "force must have stripped the tm-owned PreToolUse group"
    );
    assert_eq!(
        val["outputStyle"], "trusty-mpm",
        "unrelated keys must survive"
    );

    let claude_dir = project.join(".claude");
    let has_backup = std::fs::read_dir(&claude_dir)
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().contains(".bak-"));
    assert!(has_backup, "force must write a backup file alongside");
}

#[test]
fn clean_no_contamination_is_silent_noop() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("clean-project");
    std::fs::create_dir_all(project.join(".claude")).unwrap();
    // No settings files at all — must not error.
    clean(Some(project.to_string_lossy().to_string()), true).unwrap();
    clean(Some(project.to_string_lossy().to_string()), false).unwrap();
}
