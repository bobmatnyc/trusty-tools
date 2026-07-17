//! Unit tests for [`super`] (`tm hooks clean` / doctor hook-hygiene primitives).
//!
//! Why: split out of `cleanup.rs` to keep the production file focused; mirrors
//! the `hooks/{mod.rs,tests.rs}` split already used in this module tree.
//! What: exercises `contains_tm_hooks`, `tm_hook_event_names`,
//! `foreign_hook_event_names`, and `clean_settings_file` (dry-run, `--force`,
//! preservation of unrelated keys, and the missing-file no-op).
//! Test: this module IS the test suite for `super`.

use super::*;
use serde_json::json;
use tempfile::TempDir;

/// A settings JSON value with a tm-owned `PreToolUse` hook group.
fn tm_settings() -> Value {
    json!({
        "outputStyle": "trusty-mpm",
        "hooks": {
            "PreToolUse": [{
                "matcher": "*",
                "hooks": [{ "type": "command", "command": "/usr/local/bin/tm hook", "timeout": 5 }]
            }]
        }
    })
}

/// A settings JSON value with a foreign claude-mpm hook group.
fn claude_mpm_settings() -> Value {
    json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "",
                "hooks": [{ "type": "command", "command": "claude-mpm hooks fire SessionStart", "timeout": 5 }]
            }]
        }
    })
}

#[test]
fn contains_tm_hooks_true_for_tm_entry() {
    assert!(contains_tm_hooks(&tm_settings()));
}

#[test]
fn contains_tm_hooks_false_for_foreign_or_absent() {
    assert!(!contains_tm_hooks(&claude_mpm_settings()));
    assert!(!contains_tm_hooks(&json!({})));
    assert!(!contains_tm_hooks(&json!({ "hooks": {} })));
}

#[test]
fn tm_hook_event_names_lists_only_contaminated_events() {
    let mut val = tm_settings();
    // Add a non-tm event alongside so the matcher must discriminate.
    val["hooks"]["SessionStart"] = claude_mpm_settings()["hooks"]["SessionStart"].clone();
    let names = tm_hook_event_names(&val);
    assert_eq!(names, vec!["PreToolUse".to_string()]);
}

#[test]
fn foreign_hook_event_names_lists_claude_mpm_entries() {
    let mut val = claude_mpm_settings();
    val["hooks"]["PreToolUse"] = tm_settings()["hooks"]["PreToolUse"].clone();
    let names = foreign_hook_event_names(&val);
    assert_eq!(names, vec!["SessionStart".to_string()]);
}

#[test]
fn clean_settings_file_dry_run_reports_without_writing() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.json");
    let original = serde_json::to_string_pretty(&tm_settings()).unwrap();
    std::fs::write(&path, &original).unwrap();

    let outcome = clean_settings_file(&path, false).unwrap().unwrap();
    assert_eq!(outcome.removed_events, vec!["PreToolUse".to_string()]);
    assert!(outcome.backup_path.is_none(), "dry run must not back up");

    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, original, "dry run must never mutate the file");
    assert!(
        !dir.path().join("settings.json.bak").exists(),
        "dry run must never write a backup file"
    );
}

#[test]
fn clean_settings_file_force_writes_backup_and_strips() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.json");
    let original = serde_json::to_string_pretty(&tm_settings()).unwrap();
    std::fs::write(&path, &original).unwrap();

    let outcome = clean_settings_file(&path, true).unwrap().unwrap();
    assert_eq!(outcome.removed_events, vec!["PreToolUse".to_string()]);
    let backup = outcome.backup_path.expect("force must write a backup");
    assert!(backup.exists());
    let backup_contents = std::fs::read_to_string(&backup).unwrap();
    assert_eq!(
        backup_contents, original,
        "backup must be byte-identical to the pre-clean file"
    );

    let cleaned: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        !contains_tm_hooks(&cleaned),
        "cleaned file must no longer contain tm hooks"
    );
}

#[test]
fn clean_settings_file_preserves_non_tm_keys() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.json");
    let mut settings = tm_settings();
    settings["hooks"]["SessionStart"] = claude_mpm_settings()["hooks"]["SessionStart"].clone();
    std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

    clean_settings_file(&path, true).unwrap();

    let cleaned: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        cleaned["outputStyle"], "trusty-mpm",
        "unrelated top-level keys must survive the clean"
    );
    assert!(
        cleaned["hooks"]["SessionStart"].is_array(),
        "the foreign claude-mpm SessionStart group must be preserved untouched"
    );
    assert!(
        cleaned["hooks"].get("PreToolUse").is_none(),
        "the tm-owned PreToolUse group must be removed"
    );
}

#[test]
fn clean_settings_file_missing_file_is_noop() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("does-not-exist.json");
    let result = clean_settings_file(&path, true).unwrap();
    assert!(result.is_none());
    assert!(!path.exists(), "a missing file must never be created");
}

#[test]
fn clean_settings_file_no_tm_hooks_is_noop() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.json");
    let original = serde_json::to_string_pretty(&claude_mpm_settings()).unwrap();
    std::fs::write(&path, &original).unwrap();

    let result = clean_settings_file(&path, true).unwrap();
    assert!(
        result.is_none(),
        "a file with only foreign hooks must never be reported or touched"
    );
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, original);
}
