//! Tests for [`super`]: the `tm hook --pm-guard-shadow` registration (#11431).

use std::path::Path;

use serde_json::Value;
use tempfile::tempdir;

use super::super::architect_hooks::ArchitectHooks;
use super::super::project_hooks::project_managed_hook_additions_for_profile;
use super::super::settings::write_project_hooks_with;
use crate::core::standalone::hooks::build_tree::EGRESS_GUARD_SUFFIX;

use super::*;

const EXE: &str = "/usr/local/bin/tm";

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

/// (shadow commands anywhere, shadow group directly follows the egress guard).
fn shadow(settings: &Value) -> (usize, bool) {
    let total = settings.to_string().matches(PM_GUARD_SHADOW_SUFFIX).count();
    let groups = settings["hooks"]["PreToolUse"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let egress = groups
        .iter()
        .position(|g| g.to_string().contains(EGRESS_GUARD_SUFFIX));
    let placed = egress
        .and_then(|i| groups.get(i + 1))
        .is_some_and(|g| *g == shadow_guard_group(Path::new(EXE)));
    (total, placed)
}

#[test]
fn every_session_registers_the_shadow_guard_after_the_egress_guard() {
    for bits in 0u8..16 {
        let pm = additions(bits, None);
        assert_eq!(shadow(&pm), (1, true), "PM bits {bits:04b}: {pm}");
        for direct_commands in [false, true] {
            let arch = additions(bits, Some(ArchitectHooks { direct_commands }));
            assert_eq!(
                shadow(&arch),
                (1, true),
                "Architect bits {bits:04b}: {arch}"
            );
        }
    }
    let group = shadow_guard_group(Path::new(EXE));
    assert_eq!(
        group["hooks"][0]["command"],
        format!("{EXE} hook --pm-guard-shadow")
    );
    assert!(group["hooks"][0].get("onFailure").is_none(), "never blocks");
    assert!(is_shadow_guard_hook_command("/x/tm hook --pm-guard-shadow"));
    assert!(!is_shadow_guard_hook_command("/x/tm hook --pm-guard"));
}

#[test]
fn a_relaunch_keeps_one_shadow_group() {
    let dir = tempdir().unwrap();
    let write = |v: Value| -> Value {
        write_project_hooks_with(dir.path(), Ok(v)).expect("write");
        let text = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    };
    write(additions(1, None));
    let again = write(additions(1, None));
    assert_eq!(shadow(&again).0, 1, "{again}");
    let flipped = write(additions(
        1,
        Some(ArchitectHooks {
            direct_commands: false,
        }),
    ));
    assert_eq!(shadow(&flipped).0, 1, "{flipped}");
}
