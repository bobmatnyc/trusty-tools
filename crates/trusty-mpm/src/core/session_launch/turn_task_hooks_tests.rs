//! Tests for [`super`]: the PM-only `tm hook --turn-task` registration (#11398).

use std::path::Path;

use serde_json::Value;
use tempfile::tempdir;

use super::super::architect_hooks::ArchitectHooks;
use super::super::project_hooks::project_managed_hook_additions_for_profile;
use super::super::settings::write_project_hooks_with;
use super::*;

/// The installed-looking binary every test pins.
const EXE: &str = "/usr/local/bin/tm";

/// The additions for toggle bits `bits` and `architect`.
fn additions(bits: u8, architect: Option<ArchitectHooks>) -> Value {
    let on = |bit: u8| bits & (1 << bit) != 0;
    project_managed_hook_additions_for_profile(
        Some(Path::new(EXE)),
        on(0),
        on(1),
        on(2),
        on(3),
        architect,
    )
    .expect("additions")
}

/// How many turn-task commands sit anywhere in `settings`, and how many on
/// `UserPromptSubmit` as its last group.
fn turn_task_count(settings: &Value) -> (usize, bool) {
    let total = settings.to_string().matches(TURN_TASK_SUFFIX).count();
    let last = settings["hooks"]["UserPromptSubmit"]
        .as_array()
        .and_then(|g| g.last())
        .is_some_and(|g| *g == turn_task_group(Path::new(EXE)));
    (total, last)
}

#[test]
fn pm_sessions_register_the_turn_task_group_and_the_architect_does_not() {
    for bits in 0u8..16 {
        let pm = additions(bits, None);
        assert_eq!(turn_task_count(&pm), (1, true), "PM bits {bits:04b}: {pm}");
        for direct_commands in [false, true] {
            let arch = additions(bits, Some(ArchitectHooks { direct_commands }));
            assert_eq!(
                turn_task_count(&arch).0,
                0,
                "Architect bits {bits:04b}: {arch}"
            );
        }
    }
    let group = turn_task_group(Path::new(EXE));
    assert_eq!(
        group["hooks"][0]["command"],
        format!("{EXE} hook --turn-task")
    );
    assert!(group["hooks"][0].get("onFailure").is_none(), "fails open");
    assert!(is_turn_task_hook_command("/x/tm hook --turn-task"));
    assert!(!is_turn_task_hook_command("/x/tm hook --standing-rules"));
}

/// A relaunch keeps one group; a flip to the Architect strips it.
#[test]
fn a_relaunch_keeps_one_group_and_a_flip_to_architect_strips_it() {
    let dir = tempdir().unwrap();
    let write = |v: Value| -> Value {
        write_project_hooks_with(dir.path(), Ok(v)).expect("write");
        let text = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    };
    write(additions(1, None));
    let again = write(additions(1, None));
    assert_eq!(turn_task_count(&again).0, 1, "{again}");
    let flipped = write(additions(
        1,
        Some(ArchitectHooks {
            direct_commands: false,
        }),
    ));
    assert_eq!(turn_task_count(&flipped).0, 0, "{flipped}");
}
