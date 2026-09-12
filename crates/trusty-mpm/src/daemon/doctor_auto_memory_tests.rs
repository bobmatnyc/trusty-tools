//! Tests for the `auto_memory` doctor check and its `--fix` repair (#7685).
//!
//! Why: split into a sibling file so `doctor_auto_memory.rs` stays well under
//! the 500-SLOC production cap, the same pattern every other `doctor_*_tests.rs`
//! in this directory uses.
//! What: all three verdicts of [`super::check_auto_memory`] — the same
//! configuration graded against a live and a dead trusty-memory — plus tier
//! precedence, the index half of the directive, and the four repair states.
//! Reachability and the config dir are INJECTED, so no test depends on whether
//! the host happens to be running trusty-memory.
//! Test: this is the test module.

use std::path::{Path, PathBuf};

use super::*;
use crate::core::auto_memory_import::{INDEX_FILE, auto_memory_dir};
use crate::core::doctor::CheckStatus;

/// Write `<root>/<rel>` with `body`, creating parents.
fn write_settings(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Seed a Claude config dir whose auto-memory index for `project` holds `body`.
fn write_index(config_dir: &Path, project: &Path, body: &str) -> PathBuf {
    let memory = auto_memory_dir(config_dir, project);
    std::fs::create_dir_all(&memory).unwrap();
    std::fs::write(memory.join(INDEX_FILE), body).unwrap();
    memory.join(INDEX_FILE)
}

/// A home + project pair with `settings` written into the project tier.
fn fixture(settings: Option<&str>) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    if let Some(body) = settings {
        write_settings(&project, ".claude/settings.json", body);
    }
    (tmp, home, project)
}

const OFF: &str = r#"{"autoMemoryEnabled": false}"#;
const ON: &str = r#"{"autoMemoryEnabled": true}"#;

#[test]
fn auto_memory_fails_when_on_beside_a_healthy_trusty_memory() {
    // 🔴: trusty-memory is up and IS the memory, so auto memory being on is the
    // directive unmet.
    let (_tmp, home, project) = fixture(Some(ON));

    let check = check_auto_memory(Some(&project), &home, None, true);

    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.contains("project tier"),
        "the row must name the tier it resolved from: {}",
        check.message
    );
    assert!(
        check.message.contains(DISABLE_ENV_VAR),
        "the row must name the env-var lever too: {}",
        check.message
    );
}

#[test]
fn auto_memory_fails_when_unset_beside_a_healthy_trusty_memory() {
    // A settings file that exists and is silent on the key is not a pass —
    // Claude Code defaults auto memory ON.
    let (_tmp, home, project) = fixture(Some(r#"{"outputStyle": "x"}"#));

    let check = check_auto_memory(Some(&project), &home, None, true);

    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(check.message.contains("unset"), "{}", check.message);
}

#[test]
fn auto_memory_warns_when_on_while_trusty_memory_is_down() {
    // ⚠️: the SAME configuration as the first test. With trusty-memory down,
    // auto memory carrying the project is the fallback working as designed —
    // reporting it red would train the operator to ignore the row.
    let (_tmp, home, project) = fixture(Some(ON));

    let check = check_auto_memory(Some(&project), &home, None, false);

    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check.message.contains("FALLBACK"),
        "the row must say why it is not a failure: {}",
        check.message
    );
}

#[test]
fn auto_memory_ok_when_off_and_the_index_is_empty() {
    // ✅: off, and nothing stranded in the store it turned off.
    let (_tmp, home, project) = fixture(Some(OFF));
    let config = tempfile::TempDir::new().unwrap();
    write_index(config.path(), &project, "   \n");

    let check = check_auto_memory(Some(&project), &home, Some(config.path()), true);

    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("project tier"),
        "the row must name the tier it resolved from: {}",
        check.message
    );
}

#[test]
fn auto_memory_ok_when_the_project_has_no_auto_memory_store() {
    // An absent store is the same end state as an emptied one.
    let (_tmp, home, project) = fixture(Some(OFF));
    let config = tempfile::TempDir::new().unwrap();

    let check = check_auto_memory(Some(&project), &home, Some(config.path()), true);

    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
}

#[test]
fn auto_memory_fails_when_the_index_still_holds_facts() {
    // 🔴: the key says off, but the facts are in neither memory — they are
    // sitting in a `MEMORY.md` nothing reads any more.
    let (_tmp, home, project) = fixture(Some(OFF));
    let config = tempfile::TempDir::new().unwrap();
    let index = write_index(config.path(), &project, "- [a fact](a-fact.md) — text\n");

    let check = check_auto_memory(Some(&project), &home, Some(config.path()), true);

    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.contains("tm memory import-auto-memory"),
        "the row must name the migration, which --fix does not run: {}",
        check.message
    );
    assert!(
        check.message.contains(&index.display().to_string()),
        "the row must name the index it read: {}",
        check.message
    );
}

#[test]
fn auto_memory_warns_when_the_index_holds_facts_and_memory_is_down() {
    // Same stranded facts, but nothing can migrate them right now.
    let (_tmp, home, project) = fixture(Some(OFF));
    let config = tempfile::TempDir::new().unwrap();
    write_index(config.path(), &project, "- [a fact](a-fact.md) — text\n");

    let check = check_auto_memory(Some(&project), &home, Some(config.path()), false);

    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
}

#[test]
fn auto_memory_project_local_overrides_project() {
    let (_tmp, home, project) = fixture(Some(OFF));
    // The layer Claude Code actually reads first. A project tier tm wrote does
    // not make the setting effective when this one contradicts it.
    write_settings(&project, ".claude/settings.local.json", ON);

    let check = check_auto_memory(Some(&project), &home, None, true);

    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.contains("project-local"),
        "the row must name the winning tier: {}",
        check.message
    );
}

#[test]
fn auto_memory_falls_back_to_the_user_tier() {
    let (_tmp, home, project) = fixture(None);
    write_settings(&home, ".claude/settings.json", OFF);

    let check = check_auto_memory(Some(&project), &home, None, true);

    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("user tier"),
        "the row must name the tier it resolved from: {}",
        check.message
    );
}

#[test]
fn auto_memory_fails_on_malformed_json() {
    let (_tmp, home, project) = fixture(Some("not json{{{"));

    // A settings file Claude Code cannot parse is a finding either way round.
    for reachable in [true, false] {
        let check = check_auto_memory(Some(&project), &home, None, reachable);
        assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
        assert!(
            check.message.contains("not valid JSON"),
            "{}",
            check.message
        );
    }
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
fn auto_memory_repair_never_migrates() {
    // #7685: `--fix` writes ONE boolean key. Moving facts between two stores is
    // an operator-run data migration, so a repair must leave the auto-memory
    // store byte-identical — index included.
    let tmp = tempfile::TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    write_settings(&project, ".claude/settings.json", r#"{"outputStyle": "x"}"#);
    let config = tempfile::TempDir::new().unwrap();
    let body = "- [a fact](a-fact.md) — text\n";
    let index = write_index(config.path(), &project, body);
    let fact = index.parent().unwrap().join("a-fact.md");
    std::fs::write(&fact, "---\nname: a-fact\n---\nbody\n").unwrap();

    let steps = repair_auto_memory(&project, RepairMode::Apply);

    assert_eq!(steps.len(), 1, "the repair writes the key and nothing else");
    assert_eq!(
        std::fs::read_to_string(&index).unwrap(),
        body,
        "--fix must not empty the index"
    );
    assert!(fact.is_file(), "--fix must not move a fact file");
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
    write_settings(project, ".claude/settings.json", OFF);

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
