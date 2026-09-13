//! `prepare_session` stack-profile `skillOverrides` wiring tests (#7751).
//!
//! Why: the table and merge rules are unit-tested in
//! `core/skill_overrides_tests.rs` against a pinned stack set; these tests prove
//! the launch path detects the stack from real marker files and writes the
//! result into the project's `.claude/settings.json` beside every other key the
//! same launch writes.
//! What: a Rust project and a Svelte project, each seeded with user settings,
//! run through the real `prepare_session` twice; an unmarked project runs once.
//! Test: this module IS the test suite for that wiring.

use super::tests::EnvVarGuard;
use super::*;
use crate::core::skill_overrides::{OFF, SKILL_FAMILY_TABLE, SKILL_OVERRIDES_KEY};
use std::collections::BTreeSet;

/// A tracked, user-edited settings file: a foreign key and one user entry for a
/// skill the table turns off for BOTH stacks under test.
const USER_SETTINGS: &str =
    r#"{"permissions":{"allow":["Bash(ls)"]},"skillOverrides":{"breeze-voice":"on"}}"#;

fn run_prepare(fw: &crate::core::paths::FrameworkPaths, project: &Path, home: &Path) {
    prepare_session_with_memory_reachable(fw, project, Some(home), true).expect("prep succeeds");
}

/// Run the real launch path on a project with `markers`, then assert both
/// directions of the table, the merge rules, and a byte-identical second run.
fn assert_launch_applies_table(label: &str, markers: &[(&str, &str)], stacks: &[&str]) {
    let tmp_home = crate::test_support::hermetic_temp_dir();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = crate::test_support::hermetic_temp_dir();
    let project = tmp.path();
    for (name, body) in markers {
        std::fs::write(project.join(name), body).unwrap();
    }
    let settings_path = project.join(".claude").join("settings.json");
    std::fs::create_dir_all(project.join(".claude")).unwrap();
    std::fs::write(&settings_path, USER_SETTINGS).unwrap();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    run_prepare(&fw, project, tmp_home.path());
    let first = std::fs::read_to_string(&settings_path).unwrap();
    println!("--- {label} settings.json BEFORE ---\n{USER_SETTINGS}\n--- AFTER ---\n{first}");

    let value: serde_json::Value = serde_json::from_str(&first).unwrap();
    let overrides = &value[SKILL_OVERRIDES_KEY];
    assert_eq!(
        value["permissions"]["allow"],
        serde_json::json!(["Bash(ls)"])
    );
    assert_eq!(value["outputStyle"], serde_json::json!("trusty-mpm"));
    assert_eq!(
        overrides["breeze-voice"],
        serde_json::json!("on"),
        "a user entry wins"
    );

    let detected: BTreeSet<&str> = stacks.iter().copied().collect();
    for family in SKILL_FAMILY_TABLE {
        let relevant = family.relevant_stacks.iter().any(|s| detected.contains(s));
        for skill in family.skills.iter().filter(|s| **s != "breeze-voice") {
            if relevant {
                assert!(
                    overrides.get(*skill).is_none(),
                    "{label}: `{}` is relevant, but `{skill}` was turned off",
                    family.name
                );
            } else {
                assert_eq!(
                    overrides[*skill],
                    serde_json::json!(OFF),
                    "{label}: `{}` is irrelevant, but `{skill}` is still listed",
                    family.name
                );
            }
        }
    }

    run_prepare(&fw, project, tmp_home.path());
    assert_eq!(
        std::fs::read_to_string(&settings_path).unwrap(),
        first,
        "{label}: a second prepare_session must leave settings.json byte-identical"
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_writes_stack_profile_skill_overrides_for_a_rust_project() {
    assert_launch_applies_table(
        "rust",
        &[("Cargo.toml", "[package]\nname = \"demo\"\n")],
        &["rust-engineer"],
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_writes_stack_profile_skill_overrides_for_a_svelte_project() {
    assert_launch_applies_table(
        "svelte",
        &[
            (
                "package.json",
                "{\"devDependencies\": {\"svelte\": \"^5.0.0\"}}",
            ),
            ("svelte.config.js", "export default {};\n"),
        ],
        &[
            "javascript-engineer",
            "svelte-engineer",
            "typescript-engineer",
        ],
    );
}

#[test]
#[serial_test::serial]
fn prepare_session_on_an_unknown_stack_writes_no_skill_overrides() {
    // #7751: no marker file, so the detector answers an empty stack set — the
    // same answer it gives when detection fails. Nothing may be turned off.
    let tmp_home = crate::test_support::hermetic_temp_dir();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = crate::test_support::hermetic_temp_dir();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    run_prepare(&fw, project, tmp_home.path());

    let text = std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(
        value.get(SKILL_OVERRIDES_KEY).is_none(),
        "an unknown stack must write no skillOverrides, got:\n{text}"
    );
}
