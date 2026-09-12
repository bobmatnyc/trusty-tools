//! Tests for the `auto_memory` doctor check and its `--fix` repair (#7685).
//!
//! Why: split into a sibling file so `doctor_auto_memory.rs` stays well under
//! the 500-SLOC production cap, the same pattern every other `doctor_*_tests.rs`
//! in this directory uses.
//! What: precedence and verdict coverage for [`super::check_auto_memory`], plus
//! the four repair states — applied, dry run, no-op, and a write that failed.
//! Test: this is the test module.

use std::path::Path;

use super::*;
use crate::core::doctor::CheckStatus;

/// Write `<root>/<rel>` with `body`, creating parents.
fn write_settings(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

#[test]
fn auto_memory_ok_when_project_disables_it() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    write_settings(
        &project,
        ".claude/settings.json",
        r#"{"autoMemoryEnabled": false}"#,
    );

    let check = check_auto_memory(Some(&project), &home);

    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("project tier"),
        "the row must name the tier it resolved from: {}",
        check.message
    );
}

#[test]
fn auto_memory_warns_when_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    // A settings file that exists and is silent on the key must not read as a
    // pass — Claude Code defaults auto memory ON.
    write_settings(&project, ".claude/settings.json", r#"{"outputStyle": "x"}"#);

    let check = check_auto_memory(Some(&project), &home);

    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check.message.contains(DISABLE_ENV_VAR),
        "the Warn row must name the env-var lever too: {}",
        check.message
    );
}

#[test]
fn auto_memory_fails_when_enabled() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    write_settings(
        &project,
        ".claude/settings.json",
        r#"{"autoMemoryEnabled": true}"#,
    );

    let check = check_auto_memory(Some(&project), &home);

    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
}

#[test]
fn auto_memory_project_local_overrides_project() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    write_settings(
        &project,
        ".claude/settings.json",
        r#"{"autoMemoryEnabled": false}"#,
    );
    // The layer Claude Code actually reads first. A project tier tm wrote does
    // not make the setting effective when this one contradicts it.
    write_settings(
        &project,
        ".claude/settings.local.json",
        r#"{"autoMemoryEnabled": true}"#,
    );

    let check = check_auto_memory(Some(&project), &home);

    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.contains("project-local"),
        "the row must name the winning tier: {}",
        check.message
    );
}

#[test]
fn auto_memory_falls_back_to_the_user_tier() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    write_settings(
        &home,
        ".claude/settings.json",
        r#"{"autoMemoryEnabled": false}"#,
    );

    let check = check_auto_memory(Some(&project), &home);

    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("user tier"),
        "the row must name the tier it resolved from: {}",
        check.message
    );
}

#[test]
fn auto_memory_fails_on_malformed_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    write_settings(&project, ".claude/settings.json", "not json{{{");

    let check = check_auto_memory(Some(&project), &home);

    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.contains("not valid JSON"),
        "{}",
        check.message
    );
}

#[test]
fn auto_memory_repair_applies_when_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    write_settings(project, ".claude/settings.json", r#"{"outputStyle": "x"}"#);

    let steps = repair_auto_memory(project, RepairMode::Apply);

    assert_eq!(steps.len(), 1);
    assert!(steps[0].changed(), "{:?}", steps[0].status);
    let written: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(written[AUTO_MEMORY_KEY], serde_json::Value::Bool(false));
    assert_eq!(
        written["outputStyle"], "x",
        "the repair must preserve every other key"
    );
}

#[test]
fn auto_memory_repair_dry_run_writes_nothing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    let body = r#"{"outputStyle": "x"}"#;
    write_settings(project, ".claude/settings.json", body);

    let steps = repair_auto_memory(project, RepairMode::DryRun);

    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].status, StepStatus::Planned);
    assert_eq!(
        std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
        body,
        "a dry run must leave the file byte-identical"
    );
}

#[test]
fn auto_memory_repair_is_silent_when_already_false() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    write_settings(
        project,
        ".claude/settings.json",
        r#"{"autoMemoryEnabled": false}"#,
    );

    assert!(repair_auto_memory(project, RepairMode::Apply).is_empty());
}

#[test]
fn auto_memory_repair_reports_a_write_failure() {
    // Fail-open guard: a repair that could not write must never come back as a
    // success. A DIRECTORY where `settings.json` belongs makes the write fail
    // deterministically without depending on permission bits.
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path();
    std::fs::create_dir_all(project.join(".claude").join("settings.json")).unwrap();

    let steps = repair_auto_memory(project, RepairMode::Apply);

    assert_eq!(steps.len(), 1);
    assert!(
        matches!(steps[0].status, StepStatus::Failed(_)),
        "a failed write must report Failed, not Applied: {:?}",
        steps[0].status
    );
    assert!(
        !steps[0].changed(),
        "a failed write must never count as a change"
    );
}
