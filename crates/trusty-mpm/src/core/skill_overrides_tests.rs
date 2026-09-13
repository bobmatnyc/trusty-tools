//! Unit tests for the stack-profile `skillOverrides` table and merge (#7751).

use super::*;
use std::collections::BTreeSet;

fn stacks(stems: &[&str]) -> BTreeSet<String> {
    stems.iter().map(|s| (*s).to_string()).collect()
}

fn settings_path(project: &Path) -> PathBuf {
    project.join(".claude").join("settings.json")
}

fn read_overrides(project: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(settings_path(project)).expect("settings.json exists");
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    value[SKILL_OVERRIDES_KEY].clone()
}

/// Assert both directions of the table for `project_stacks`: every skill of a
/// family the table marks irrelevant is `"off"`, and no skill of a family the
/// table marks relevant appears at all.
fn assert_table_applied(project_stacks: &[&str]) {
    let tmp = tempfile::tempdir().unwrap();
    let detected = stacks(project_stacks);
    write_skill_overrides_for(tmp.path(), &detected).expect("write succeeds");
    let overrides = read_overrides(tmp.path());

    let mut saw_relevant = false;
    let mut saw_irrelevant = false;
    for family in SKILL_FAMILY_TABLE {
        let relevant = family.relevant_stacks.iter().any(|s| detected.contains(*s));
        for skill in family.skills {
            if relevant {
                saw_relevant = true;
                assert!(
                    overrides.get(*skill).is_none(),
                    "{project_stacks:?}: family `{}` is relevant, but `{skill}` was turned off",
                    family.name
                );
            } else {
                saw_irrelevant = true;
                assert_eq!(
                    overrides[*skill],
                    serde_json::json!(OFF),
                    "{project_stacks:?}: family `{}` is irrelevant, but `{skill}` is still listed",
                    family.name
                );
            }
        }
    }
    assert!(
        saw_relevant && saw_irrelevant,
        "{project_stacks:?} must exercise both directions of the table"
    );
}

#[test]
fn rust_project_turns_off_every_family_the_table_marks_irrelevant() {
    assert_table_applied(&["rust-engineer"]);
}

#[test]
fn svelte_project_turns_off_every_family_the_table_marks_irrelevant() {
    // What the detector returns for a `package.json` declaring `"svelte"`.
    assert_table_applied(&[
        "javascript-engineer",
        "svelte-engineer",
        "typescript-engineer",
    ]);
}

#[test]
fn rust_and_svelte_disagree_on_the_rust_and_web_families() {
    // Pins the two stacks against each other, so a table that turns the same
    // families off for every stack cannot pass the two tests above by accident.
    let rust = irrelevant_skills(&stacks(&["rust-engineer"]));
    let svelte = irrelevant_skills(&stacks(&["javascript-engineer", "svelte-engineer"]));
    assert!(!rust.contains("rust-build-performance") && svelte.contains("rust-build-performance"));
    assert!(rust.contains("webapp-testing") && !svelte.contains("webapp-testing"));
}

#[test]
fn polyglot_project_keeps_a_family_any_of_its_stacks_uses() {
    let off = irrelevant_skills(&stacks(&["rust-engineer", "javascript-engineer"]));
    assert!(!off.contains("rust-build-performance"));
    assert!(!off.contains("webapp-testing"));
    assert!(off.contains("breeze-voice"));
}

#[test]
fn every_table_stack_is_a_detectable_stem() {
    // A misspelt stem can never match, which would turn its family off for
    // every project — the failure this module must not have.
    let categories = crate::core::manifest::framework::framework_agent_categories()
        .expect("bundled framework manifest is valid");
    let detectable: BTreeSet<&str> = categories
        .language
        .iter()
        .chain(categories.framework.iter())
        .map(|entry| entry.stem.as_str())
        .collect();
    for family in SKILL_FAMILY_TABLE {
        for stem in family.relevant_stacks {
            assert!(
                detectable.contains(stem),
                "family `{}` names `{stem}`, which the detector never returns",
                family.name
            );
        }
    }
}

#[test]
fn unknown_stack_turns_nothing_off() {
    // #7751: the detector answers an EMPTY set both for an unrecognised project
    // and when the bundled manifest is unusable. Either way nothing is hidden,
    // and the settings file is not created.
    let tmp = tempfile::tempdir().unwrap();
    assert!(irrelevant_skills(&BTreeSet::new()).is_empty());
    let outcome = write_skill_overrides_for(tmp.path(), &BTreeSet::new()).unwrap();
    assert_eq!(outcome, SkillOverridesOutcome::NoStack);
    assert!(!settings_path(tmp.path()).exists());
}

#[test]
fn unknown_stack_leaves_an_existing_file_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
    let original = "{\"permissions\": {\"allow\": []}}\n";
    std::fs::write(settings_path(tmp.path()), original).unwrap();
    write_skill_overrides_for(tmp.path(), &BTreeSet::new()).unwrap();
    assert_eq!(
        std::fs::read_to_string(settings_path(tmp.path())).unwrap(),
        original
    );
}

#[test]
fn user_entries_and_foreign_keys_survive_the_merge() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
    std::fs::write(
        settings_path(tmp.path()),
        r#"{"permissions":{"allow":["Bash(ls)"]},"skillOverrides":{"xlsx":"on","my-skill":"off"}}"#,
    )
    .unwrap();

    write_skill_overrides_for(tmp.path(), &stacks(&["rust-engineer"])).unwrap();

    let text = std::fs::read_to_string(settings_path(tmp.path())).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        value["permissions"]["allow"],
        serde_json::json!(["Bash(ls)"])
    );
    assert_eq!(
        value[SKILL_OVERRIDES_KEY]["xlsx"],
        serde_json::json!("on"),
        "user entry wins"
    );
    assert_eq!(
        value[SKILL_OVERRIDES_KEY]["my-skill"],
        serde_json::json!("off")
    );
    assert_eq!(
        value[SKILL_OVERRIDES_KEY]["breeze-voice"],
        serde_json::json!(OFF)
    );
}

#[test]
fn malformed_settings_are_left_untouched() {
    for original in ["{ not json", "[1, 2]", r#"{"skillOverrides": "off"}"#] {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        std::fs::write(settings_path(tmp.path()), original).unwrap();

        let outcome = write_skill_overrides_for(tmp.path(), &stacks(&["rust-engineer"])).unwrap();

        assert!(
            matches!(outcome, SkillOverridesOutcome::Skipped(_)),
            "{original}: {outcome:?}"
        );
        assert_eq!(
            std::fs::read_to_string(settings_path(tmp.path())).unwrap(),
            original,
            "a settings file tm cannot parse must survive byte-for-byte"
        );
    }
}

#[test]
fn second_write_is_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let rust = stacks(&["rust-engineer"]);
    assert!(matches!(
        write_skill_overrides_for(tmp.path(), &rust).unwrap(),
        SkillOverridesOutcome::Written(_)
    ));
    let first = std::fs::read(settings_path(tmp.path())).unwrap();
    assert_eq!(
        write_skill_overrides_for(tmp.path(), &rust).unwrap(),
        SkillOverridesOutcome::Unchanged
    );
    assert_eq!(std::fs::read(settings_path(tmp.path())).unwrap(), first);
}
