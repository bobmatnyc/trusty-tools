//! Unit tests for the stack-profile `skillOverrides` table and merge (#7751).

use super::*;
use std::collections::BTreeSet;

fn stacks(stems: &[&str]) -> BTreeSet<String> {
    stems.iter().map(|s| (*s).to_string()).collect()
}

/// A COMPLETE detection of `stems` — no scan bound tripped (#7781).
fn detection(stems: &[&str]) -> StackDetection {
    StackDetection {
        engineers: stacks(stems),
        truncated: false,
        depth_limited: false,
    }
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
    write_skill_overrides_for(tmp.path(), &detection(project_stacks)).expect("write succeeds");
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
    let outcome = write_skill_overrides_for(tmp.path(), &detection(&[])).unwrap();
    assert_eq!(outcome, SkillOverridesOutcome::NoStack);
    assert!(!settings_path(tmp.path()).exists());
}

#[test]
fn unknown_stack_leaves_an_existing_file_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
    let original = "{\"permissions\": {\"allow\": []}}\n";
    std::fs::write(settings_path(tmp.path()), original).unwrap();
    write_skill_overrides_for(tmp.path(), &detection(&[])).unwrap();
    assert_eq!(
        std::fs::read_to_string(settings_path(tmp.path())).unwrap(),
        original
    );
}

#[test]
fn truncated_detection_writes_no_overrides() {
    // #7751 review round 2 (HIGH-2): a resource cap leaves the engineer set
    // PARTIAL, and the `"off"` this module writes is sticky — the merge only
    // ever ADDS keys, so no later, complete detection re-enables a skill hidden
    // from a partial scan. Fail closed instead (#7781).
    let truncated = StackDetection {
        engineers: stacks(&["rust-engineer"]),
        truncated: true,
        depth_limited: false,
    };
    // Without the guard this stack turns skills off, so neither assertion below
    // can pass vacuously.
    assert!(!irrelevant_skills(&truncated.engineers).is_empty());

    let fresh = tempfile::tempdir().unwrap();
    assert_eq!(
        write_skill_overrides_for(fresh.path(), &truncated).unwrap(),
        SkillOverridesOutcome::Truncated
    );
    assert!(
        !settings_path(fresh.path()).exists(),
        "a truncated detection must not create the settings file"
    );

    let existing = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(existing.path().join(".claude")).unwrap();
    let original = "{\"permissions\": {\"allow\": []}}\n";
    std::fs::write(settings_path(existing.path()), original).unwrap();
    assert_eq!(
        write_skill_overrides_for(existing.path(), &truncated).unwrap(),
        SkillOverridesOutcome::Truncated
    );
    assert_eq!(
        std::fs::read_to_string(settings_path(existing.path())).unwrap(),
        original,
        "a truncated detection must leave an existing file byte-for-byte"
    );
}

#[test]
fn depth_limited_detection_still_writes() {
    // #7781 round-3: the depth bound is the walk's declared SCOPE, not a
    // resource cap, and it trips on most real repositories — blocking on it
    // would block nearly always. It writes like an unbounded detection.
    let tmp = tempfile::tempdir().unwrap();
    let deep = StackDetection {
        engineers: stacks(&["rust-engineer"]),
        truncated: false,
        depth_limited: true,
    };

    let outcome = write_skill_overrides_for(tmp.path(), &deep).unwrap();

    assert!(
        matches!(outcome, SkillOverridesOutcome::Written(_)),
        "{outcome:?}"
    );
    assert_eq!(
        read_overrides(tmp.path())["breeze-voice"],
        serde_json::json!(OFF)
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

    write_skill_overrides_for(tmp.path(), &detection(&["rust-engineer"])).unwrap();

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

        let outcome =
            write_skill_overrides_for(tmp.path(), &detection(&["rust-engineer"])).unwrap();

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
    let rust = detection(&["rust-engineer"]);
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

#[test]
fn every_table_skill_is_bundled_or_vouched_for() {
    // #7751 review round 1: nothing checked the SKILL names against a real
    // inventory, so four of the eight could be renamed to garbage with the whole
    // suite still green. A misspelt key turns nothing off and reports success.
    let roster: BTreeSet<String> = crate::core::manifest::framework::framework_skill_categories()
        .expect("bundled framework manifest is valid")
        .universal
        .into_iter()
        .collect();
    let vouched: BTreeSet<&str> = UNBUNDLED_TABLE_SKILLS.iter().copied().collect();

    for family in SKILL_FAMILY_TABLE {
        for skill in family.skills {
            assert!(
                roster.contains(*skill) || vouched.contains(*skill),
                "family `{}` names `{skill}`, which is neither a bundled skill nor listed in \
                 UNBUNDLED_TABLE_SKILLS — a name no inventory knows turns nothing off",
                family.name
            );
        }
    }

    // A vouched-for name that no family uses is a stale exemption: it would keep
    // vouching for a skill the table no longer names, and hide the next typo.
    let named: BTreeSet<&str> = SKILL_FAMILY_TABLE
        .iter()
        .flat_map(|family| family.skills.iter().copied())
        .collect();
    for skill in UNBUNDLED_TABLE_SKILLS {
        assert!(
            named.contains(*skill),
            "`{skill}` is vouched for in UNBUNDLED_TABLE_SKILLS but no family names it"
        );
    }
}

#[test]
fn no_skill_appears_in_two_families() {
    // #7751 review round 1: relevance is decided per family and the skills are
    // then flattened, so a skill listed twice would be turned off whenever ANY
    // of its families is irrelevant, even with another family keeping it on. No
    // duplicate exists today; this is where the next table edit lands.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for family in SKILL_FAMILY_TABLE {
        for skill in family.skills {
            assert!(
                seen.insert(*skill),
                "`{skill}` is listed by more than one family (`{}` is the second) — \
                 the flatten in `irrelevant_skills` would turn it off for both",
                family.name
            );
        }
    }
}

/// Fresh-file trials the concurrent-writer regression runs.
///
/// Why: the lost update only shows when two read-modify-write cycles actually
/// interleave, and each trial has to be one-shot. Looping the two writers over
/// one file self-heals a clobber — both are idempotent, so the next round
/// re-adds what the last one dropped — which is how this defect stayed invisible
/// to every other test here. 40 fresh files is what #7751 review round 1
/// measured: 40 losses in 40 trials without the guard, 0 in 40 with it.
const RACE_TRIALS: usize = 40;

#[test]
fn a_concurrent_statusline_write_loses_no_skill_overrides() {
    // THE RACE (#7751 review round 1, HIGH): `prepare_session` writes
    // `skillOverrides` and then `statusLine` into ONE `.claude/settings.json`,
    // and the daemon runs that concurrently for two sessions. Both writers
    // publish atomically, which stops a torn file but not a lost update: the
    // writer that read first stores its own complete pre-read snapshot last and
    // silently drops the other's keys. Only a real two-thread race proves the
    // guard — a single-threaded test of either writer passes with or without it.
    let rust = detection(&["rust-engineer"]);
    let planned = irrelevant_skills(&rust.engineers);
    let mut lost: Vec<String> = Vec::new();

    for trial in 0..RACE_TRIALS {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path();
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        let path = settings_path(project);
        let start = std::sync::Barrier::new(2);

        std::thread::scope(|scope| {
            scope.spawn(|| {
                start.wait();
                write_skill_overrides_for(project, &rust).expect("the merge must never fail");
            });
            scope.spawn(|| {
                start.wait();
                crate::core::statusline_settings::ensure_statusline_entry_in(&path);
            });
        });

        let text = std::fs::read_to_string(&path).expect("both writers create the file");
        let value: serde_json::Value =
            serde_json::from_str(&text).expect("a published settings.json is always valid JSON");
        let mut missing: Vec<String> = Vec::new();
        if value.get("statusLine").is_none() {
            missing.push("statusLine".to_string());
        }
        for skill in &planned {
            if value[SKILL_OVERRIDES_KEY].get(*skill).is_none() {
                missing.push((*skill).to_string());
            }
        }
        if !missing.is_empty() {
            lost.push(format!("trial {trial} lost {missing:?}"));
        }
    }

    assert!(
        lost.is_empty(),
        "{} of {RACE_TRIALS} concurrent trials lost an update — the read → mutate → write \
         cycle stored a snapshot taken before a sibling writer's store (#7751, #4072). {lost:?}",
        lost.len()
    );
}
