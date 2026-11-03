//! Unit tests for the retired egress-guard settings entry (#8454, #10735): its
//! identity and read side, and the writers and strips that now remove it.

use super::*;
use crate::core::standalone::hooks::build_tree::repointed_hook_command;
use crate::core::standalone::hooks::cleanup::clean_settings_file;
use crate::core::standalone::hooks::{
    is_mpm_hook_command, strip_mpm_hook_entries, write_project_hooks,
    write_project_hooks_honoring_pm_guard,
};
use serde_json::json;
use std::path::{Path, PathBuf};

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

/// The group the retired writer produced (#8454), for seeding a stale file.
fn egress_guard_hook_group(exe: &Path) -> Value {
    json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{}{EGRESS_GUARD_SUFFIX}", exe.display()),
            "timeout": EGRESS_GUARD_TIMEOUT_SECS,
        }]
    })
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

/// The identity predicate claims the egress suffix of any binary.
#[test]
fn egress_group_is_recognised_by_its_command() {
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

/// 🔴 #10735: no writer adds the egress group any more, and each one strips a
/// stale entry (an upgrade from a release that wrote it), whatever
/// `[pm_guard] enabled` says. The pm-guard entry is unaffected when on.
#[test]
fn egress_entry_is_stripped_by_the_user_tier_writers() {
    for pm_guard in [false, true] {
        let (_dir, path) = scratch();
        std::fs::write(&path, installed_entries().to_string()).expect("seed");
        write_project_hooks_honoring_pm_guard(&path, Some(Path::new(EXE_A)), pm_guard)
            .expect("write");
        let settings = read(&path);
        assert!(
            egress_commands(&settings).is_empty(),
            "pm_guard={pm_guard}: {settings}"
        );
        assert_eq!(
            settings.to_string().contains("--pm-guard"),
            pm_guard,
            "pm_guard={pm_guard}: {settings}"
        );
    }
    let (_dir, path) = scratch();
    write_project_hooks(&path, Some(Path::new(EXE_A))).expect("fresh write");
    assert!(egress_commands(&read(&path)).is_empty());
    std::fs::write(&path, installed_entries().to_string()).expect("seed");
    write_project_hooks(&path, Some(Path::new(EXE_B))).expect("upgrade write");
    assert!(egress_commands(&read(&path)).is_empty());
}

/// 🔴 #10735: `tm hooks clean` and the strips remove an installed egress entry
/// too, so an uninstalled `tm` leaves no hook that cannot run.
#[test]
fn egress_entry_is_removed_by_clean_and_strip() {
    // A user-tier file (no project-tier marker such as the pm-guard entry).
    let (_dir, path) = scratch();
    let user_tier =
        json!({ "hooks": { "PreToolUse": [egress_guard_hook_group(Path::new(EXE_A))] } });
    std::fs::write(&path, user_tier.to_string()).expect("seed");
    clean_settings_file(&path, true).expect("clean");
    assert!(egress_commands(&read(&path)).is_empty(), "tm hooks clean");

    let mut all = installed_entries();
    strip_mpm_hook_entries(&mut all);
    assert!(egress_commands(&all).is_empty(), "strip_mpm_hook_entries");

    assert!(is_mpm_hook_command("/usr/local/bin/tm hook --egress-guard"));
    let dead = format!("{BUILD_TREE_EXE} hook --egress-guard");
    assert!(is_mpm_hook_command(&dead), "{dead}");
    assert_eq!(
        repointed_hook_command(&dead, Path::new(EXE_A)).as_deref(),
        Some("/usr/local/bin/tm hook --egress-guard")
    );
}
