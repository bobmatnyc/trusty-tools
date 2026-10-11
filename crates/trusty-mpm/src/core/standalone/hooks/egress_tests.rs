//! Unit tests for the egress-guard settings entry (#8454 S2): its shape, the
//! user-tier writers, and every strip that must leave it in place.

use super::*;
use crate::core::standalone::hooks::build_tree::repointed_hook_command;
use crate::core::standalone::hooks::cleanup::clean_settings_file;
use crate::core::standalone::hooks::{
    is_mpm_hook_command, strip_mpm_hook_entries, write_project_hooks,
    write_project_hooks_honoring_pm_guard,
};
use serde_json::json;
use std::path::PathBuf;

/// An installed-looking binary the writers accept on any host.
const EXE_A: &str = "/usr/local/bin/tm";
/// The same binary after an exe-path change.
const EXE_B: &str = "/opt/tm/bin/trusty-mpm";
/// The #7244 incident's build-tree executable.
const BUILD_TREE_EXE: &str = "/repo/target-7247/debug/deps/test_session_lifecycle-cd3ba8f03938239b";

/// Every egress-guard command in `settings`, on every event.
fn egress_commands(settings: &Value) -> Vec<String> {
    crate::core::standalone::hooks::cleanup::event_names_matching(settings, |_| true)
        .iter()
        .flat_map(|event| {
            settings["hooks"][event]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|group| group["hooks"].as_array().into_iter().flatten())
                .filter_map(|entry| entry["command"].as_str())
                .filter(|cmd| cmd.ends_with(" hook --egress-guard"))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A settings file in a scratch directory.
fn scratch() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("settings.json");
    (dir, path)
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read settings")).expect("json")
}

/// A settings value holding an installed egress entry and a pm-guard entry.
fn installed_entries() -> Value {
    json!({ "hooks": { "PreToolUse": [
        { "matcher": "", "hooks": [
            { "type": "command", "command": "/usr/local/bin/tm hook --pm-guard", "timeout": 10 }
        ]},
        egress_guard_hook_group(Path::new(EXE_A)),
    ]}})
}

/// 🔴 #8454 S2: the group fires for every tool, names the absolute binary, and
/// allows Claude Code 10 s.
#[test]
fn egress_group_registers_the_guard_command_with_timeout_10() {
    let group = egress_guard_hook_group(Path::new(EXE_A));
    assert_eq!(group["matcher"], "", "{group}");
    let entry = &group["hooks"][0];
    assert_eq!(entry["type"], "command", "{group}");
    assert_eq!(entry["command"], "/usr/local/bin/tm hook --egress-guard");
    assert_eq!(entry["timeout"], 10);
    assert_eq!(EGRESS_GUARD_TIMEOUT_SECS, 10);
    assert!(is_egress_guard_hook_command("/x/tm hook --egress-guard"));
    assert!(!is_egress_guard_hook_command("/x/tm hook --pm-guard"));
}

/// 🔴 #8454 S2: the read side tells a working entry from a missing or a
/// weakened one. Each weakened arm fails against a predicate that only asks
/// whether the command is present.
#[test]
fn egress_state_reads_present_missing_and_weakened() {
    let group = |matcher: Value, timeout: Value| {
        let mut entry = json!({ "type": "command", "command": "/t/tm hook --egress-guard" });
        if !timeout.is_null() {
            entry["timeout"] = timeout;
        }
        let mut group = json!({ "hooks": [entry] });
        if !matcher.is_null() {
            group["matcher"] = matcher;
        }
        json!({ "hooks": { "PreToolUse": [group] } })
    };
    assert_eq!(
        egress_guard_state(&group(json!(""), json!(10))),
        EgressGuardState::Present
    );
    assert_eq!(
        egress_guard_state(&group(json!("*"), json!(30))),
        EgressGuardState::Present
    );
    // No timeout is Claude Code's 60 s default; no matcher is every tool.
    assert_eq!(
        egress_guard_state(&group(Value::Null, Value::Null)),
        EgressGuardState::Present
    );
    for (case, settings) in [
        ("timeout 5", group(json!(""), json!(5))),
        ("timeout as text", group(json!(""), json!("10"))),
        ("matcher narrowed", group(json!("Bash"), json!(10))),
    ] {
        assert!(
            matches!(egress_guard_state(&settings), EgressGuardState::Weakened(_)),
            "{case}: {settings}"
        );
    }
    for settings in [
        json!({}),
        json!({ "hooks": { "PostToolUse": [egress_guard_hook_group(Path::new(EXE_A))] } }),
        json!({ "hooks": { "PreToolUse": "not an array" } }),
        json!({ "hooks": { "PreToolUse": [installed_entries()["hooks"]["PreToolUse"][0].clone()] } }),
    ] {
        assert_eq!(
            egress_guard_state(&settings),
            EgressGuardState::Missing,
            "{settings}"
        );
    }
}

/// 🔴 #8454 S2: `disableAllHooks` counts as set for any value but `false`.
#[test]
fn egress_disable_all_hooks_detects_any_non_false_value() {
    for value in [json!(true), json!("true"), json!(1), json!(null)] {
        assert!(
            disables_all_hooks(&json!({ "disableAllHooks": value })),
            "{value}"
        );
    }
    assert!(!disables_all_hooks(&json!({ "disableAllHooks": false })));
    assert!(!disables_all_hooks(&json!({})));
}

/// 🔴 #8454 S2 (plan §6): the user-tier writers add the egress group whatever
/// `[pm_guard] enabled` says.
#[test]
fn egress_user_tier_writer_adds_the_group_even_when_pm_guard_disabled() {
    for pm_guard in [false, true] {
        let (_dir, path) = scratch();
        write_project_hooks_honoring_pm_guard(&path, Some(Path::new(EXE_A)), pm_guard)
            .expect("write");
        let settings = read(&path);
        assert_eq!(
            egress_guard_state(&settings),
            EgressGuardState::Present,
            "pm_guard={pm_guard}: {settings}"
        );
    }
    let (_dir, path) = scratch();
    write_project_hooks(&path, Some(Path::new(EXE_A))).expect("write");
    assert_eq!(
        egress_commands(&read(&path)),
        vec!["/usr/local/bin/tm hook --egress-guard".to_string()]
    );
}

/// 🔴 #8454 S2: a second write after an exe-path change replaces the entry
/// rather than adding a second one, and a third identical write is a no-op.
#[test]
fn egress_user_tier_writer_is_idempotent_across_an_exe_path_change() {
    let (_dir, path) = scratch();
    write_project_hooks(&path, Some(Path::new(EXE_A))).expect("first write");
    write_project_hooks(&path, Some(Path::new(EXE_B))).expect("second write");
    assert_eq!(
        egress_commands(&read(&path)),
        vec![format!("{EXE_B} hook --egress-guard")]
    );
    let changed = write_project_hooks(&path, Some(Path::new(EXE_B))).expect("third write");
    assert!(!changed, "an identical write must not rewrite the file");
}

/// 🔴 #8454 S2 (plan §6): removing the pm-guard, `tm hooks clean`,
/// `strip_mpm_hook_entries` and the event-scoped strip all leave an installed
/// egress entry in place.
#[test]
fn egress_entry_survives_pm_guard_removal_clean_and_strip() {
    let want = vec!["/usr/local/bin/tm hook --egress-guard".to_string()];

    let (_dir, path) = scratch();
    std::fs::write(&path, installed_entries().to_string()).expect("seed");
    write_project_hooks_honoring_pm_guard(&path, Some(Path::new(EXE_A)), false).expect("write");
    let after = read(&path);
    assert_eq!(egress_commands(&after), want, "pm-guard removal: {after}");
    assert!(
        !after.to_string().contains("--pm-guard"),
        "the pm-guard entry is still removed: {after}"
    );

    let (_dir, path) = scratch();
    std::fs::write(&path, installed_entries().to_string()).expect("seed");
    clean_settings_file(&path, true).expect("clean");
    assert_eq!(egress_commands(&read(&path)), want, "tm hooks clean");

    let mut all = installed_entries();
    strip_mpm_hook_entries(&mut all);
    assert_eq!(egress_commands(&all), want, "strip_mpm_hook_entries");

    let mut scoped = installed_entries();
    super::super::strip_mpm_hook_entries_for_events(
        &mut scoped,
        Some(&["PreToolUse".to_string()][..]),
    );
    assert_eq!(egress_commands(&scoped), want, "event-scoped strip");
}

/// 🔴 #8454 S2 (plan §2): the shared tm-hook predicate claims a build-tree
/// egress command — so the strips replace and the repair repoints it — and
/// never the installed one.
#[test]
fn egress_is_mpm_hook_command_claims_only_the_build_tree_shape() {
    let dead = format!("{BUILD_TREE_EXE} hook --egress-guard");
    assert!(is_mpm_hook_command(&dead), "{dead}");
    assert!(!is_mpm_hook_command(
        "/usr/local/bin/tm hook --egress-guard"
    ));
    assert_eq!(
        repointed_hook_command(&dead, Path::new(EXE_A)).as_deref(),
        Some("/usr/local/bin/tm hook --egress-guard")
    );
}
