//! Tests for [`super`]: the Architect's hook registration and the PM session's
//! unchanged one (#10459 PR 2, DOC-86 §5, §8).
//!
//! Why: the Architect's guard must be the exact exec-form argv PR 1's mint
//! check accepts, with `onFailure: "block"`, and nothing else may change —
//! for PM and agent sessions not one byte.
//! What: golden JSON for both `direct_commands` values, a PM golden over every
//! toggle, the strip and doctor readers on exec-form entries, and the real
//! launch writer for both profiles.
//! Test: this module is the test suite for `super`.

use std::path::Path;

use serde_json::{Value, json};
use tempfile::tempdir;

use super::super::project_hooks::{
    project_managed_hook_additions_for_profile, project_managed_hook_additions_with_pm_guard,
};
use super::super::settings::{
    PM_GUARD_SUFFIX, resolve_statusline_binary, write_project_hooks_with,
};
use super::super::tests::EnvVarGuard;
use super::*;
use crate::core::standalone::hooks::hook_entry_command_line;
use trusty_common::install_paths::MPM_DIR_NAME;

/// The installed-looking binary every test pins.
const EXE: &str = "/usr/local/bin/tm";

/// The Architect's additions under the default toggles.
fn architect(direct_commands: bool) -> Value {
    project_managed_hook_additions_for_profile(
        Some(Path::new(EXE)),
        true,
        false,
        false,
        true,
        Some(ArchitectHooks { direct_commands }),
    )
    .expect("a stable exe resolves")
}

/// The exact exec-form guard entry; `on_failure` adds `onFailure`.
fn guard_entry(on_failure: bool) -> Value {
    let mut entry = json!({
        "type": "command",
        "command": EXE,
        "args": ["hook", "--pm-guard"],
        "timeout": 10
    });
    if on_failure {
        entry["onFailure"] = json!("block");
    }
    entry
}

/// Every hook entry under every event of `additions`, with its event.
fn entries(additions: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for (event, groups) in additions["hooks"].as_object().expect("hooks object") {
        for group in groups.as_array().expect("event array") {
            for entry in group["hooks"].as_array().expect("group hooks") {
                out.push((event.clone(), entry.clone()));
            }
        }
    }
    out
}

/// The exact `tm hook --standing-rules` group (#10960).
fn standing_rules_group() -> Value {
    json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": format!("{EXE}{STANDING_RULES_SUFFIX}"),
            "timeout": 10
        }]
    })
}

/// The number of standing-rules entries on `event` (#10960).
fn standing_rules_on(settings: &Value, event: &str) -> usize {
    settings["hooks"][event]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| group["hooks"].as_array().into_iter().flatten())
        .filter(|entry| {
            entry["command"]
                .as_str()
                .is_some_and(is_standing_rules_hook_command)
        })
        .count()
}

/// #10960: an Architect registers `tm hook --standing-rules` once on
/// `SessionStart` and once on `UserPromptSubmit` — with the prompt-context
/// hook off too — and no PM toggle combination registers it at all.
#[test]
fn architect_standing_rules_groups_are_architect_only() {
    for inject_prompt_context in [false, true] {
        let got = project_managed_hook_additions_for_profile(
            Some(Path::new(EXE)),
            inject_prompt_context,
            false,
            false,
            true,
            Some(ArchitectHooks {
                direct_commands: false,
            }),
        )
        .expect("architect additions");
        for event in ["SessionStart", "UserPromptSubmit"] {
            assert_eq!(standing_rules_on(&got, event), 1, "{event}: {got}");
        }
        assert_eq!(
            entries(&got)
                .iter()
                .filter(|(_, e)| e["command"]
                    .as_str()
                    .is_some_and(is_standing_rules_hook_command))
                .count(),
            2,
            "{got}"
        );
    }
    for bits in 0u8..32 {
        let on = |bit: u8| bits & (1 << bit) != 0;
        let pm = project_managed_hook_additions_for_profile(
            Some(Path::new(EXE)),
            on(0),
            on(1),
            on(2),
            on(3),
            None,
        )
        .expect("pm additions");
        assert!(
            !pm.to_string().contains(STANDING_RULES_SUFFIX),
            "bits {bits:05b}: {pm}"
        );
    }
}

/// #10960: a relaunch keeps one standing-rules group per event, and a flip to
/// PM strips both.
#[test]
fn a_flip_to_pm_strips_the_standing_rules_groups() {
    let dir = tempdir().unwrap();
    write(dir.path(), architect(false));
    let again = write(dir.path(), architect(false));
    assert_eq!(standing_rules_on(&again, "SessionStart"), 1);
    assert_eq!(standing_rules_on(&again, "UserPromptSubmit"), 1);
    let pm = project_managed_hook_additions_for_profile(
        Some(Path::new(EXE)),
        true,
        false,
        false,
        true,
        None,
    )
    .expect("pm");
    let flipped = write(dir.path(), pm);
    for event in ["SessionStart", "UserPromptSubmit"] {
        assert_eq!(standing_rules_on(&flipped, event), 0, "{event}: {flipped}");
    }
}

/// The golden: the Architect's block is the PM block with the guard group
/// swapped for the exec-form one, with the flag on two prompt groups
/// appended, and (#10960) the standing-rules groups last. Nothing else differs.
#[test]
fn architect_hook_settings_golden() {
    let pm = project_managed_hook_additions_with_pm_guard(
        Some(Path::new(EXE)),
        true,
        false,
        false,
        true,
    )
    .expect("pm additions");
    for direct_commands in [false, true] {
        let mut expected = pm.clone();
        // #11398: the PM's turn-task group (its last `UserPromptSubmit` group)
        // is PM-only; the Architect block is unchanged.
        let pm_submit = expected["hooks"]["UserPromptSubmit"]
            .as_array_mut()
            .expect("UserPromptSubmit");
        let turn_task = pm_submit.pop().expect("the PM turn-task group");
        assert!(
            turn_task.to_string().contains(" hook --turn-task"),
            "{turn_task}"
        );
        // #10575: likewise the PM's completion-evidence group, its last `Stop` group.
        let evidence = expected["hooks"]["Stop"]
            .as_array_mut()
            .expect("Stop")
            .pop()
            .expect("the PM completion-evidence group");
        assert!(
            evidence.to_string().contains(" hook --completion-evidence"),
            "{evidence}"
        );
        expected["hooks"]["PreToolUse"][0] = json!({
            "matcher": "",
            "hooks": [guard_entry(true)]
        });
        if direct_commands {
            let submit = expected["hooks"]["UserPromptSubmit"]
                .as_array_mut()
                .expect("UserPromptSubmit");
            submit.push(json!({ "matcher": "", "hooks": [guard_entry(false)] }));
            expected["hooks"]["UserPromptExpansion"] =
                json!([{ "matcher": "tm-run", "hooks": [guard_entry(false)] }]);
        }
        // #10960: the standing-rules groups close both events, every Architect.
        for event in ["SessionStart", "UserPromptSubmit"] {
            expected["hooks"][event]
                .as_array_mut()
                .expect(event)
                .push(standing_rules_group());
        }
        let got = architect(direct_commands);
        assert_eq!(got, expected, "direct_commands = {direct_commands}");
        // `onFailure` sits on the PreToolUse guard and nowhere else.
        let blocking: Vec<_> = entries(&got)
            .into_iter()
            .filter(|(_, entry)| entry.get("onFailure").is_some())
            .collect();
        assert_eq!(blocking, [("PreToolUse".to_owned(), guard_entry(true))]);
        assert_eq!(
            got["hooks"].get("UserPromptExpansion").is_some(),
            direct_commands
        );
    }
}

/// `[pm_guard] enabled = false` leaves the Architect with no guard and no
/// prompt hooks, whatever `direct_commands` says.
#[test]
fn a_disabled_guard_registers_no_architect_handler() {
    let got = project_managed_hook_additions_for_profile(
        Some(Path::new(EXE)),
        true,
        false,
        false,
        false,
        Some(ArchitectHooks {
            direct_commands: true,
        }),
    )
    .expect("additions");
    assert!(
        entries(&got)
            .iter()
            .all(|(_, entry)| entry.get("args").is_none()),
        "{got}"
    );
}

/// PM and agent sessions: every toggle combination gives the same block as
/// before #10459, the guard stays shell form, and nothing carries `args` or a
/// `UserPromptExpansion` group. #10600: `onFailure: "block"` sits on the PM
/// guard entry and on no other entry.
#[test]
fn pm_session_registers_no_ticket_handler() {
    let shell_guard = json!({
        "type": "command",
        "command": format!("{}{PM_GUARD_SUFFIX}", resolve_statusline_binary()),
        "timeout": 10,
        "onFailure": "block"
    });
    for bits in 0u8..32 {
        let on = |bit: u8| bits & (1 << bit) != 0;
        let got = project_managed_hook_additions_for_profile(
            Some(Path::new(EXE)),
            on(0),
            on(1),
            on(2),
            on(3),
            None,
        )
        .expect("pm additions");
        assert!(got["hooks"].get("UserPromptExpansion").is_none(), "{got}");
        for (event, entry) in entries(&got) {
            assert!(entry.get("args").is_none(), "{event}: {entry}");
            // #10600: only the PM guard fails closed; every other hook keeps its behaviour.
            let is_guard = entry == shell_guard;
            assert_eq!(
                entry.get("onFailure").is_some(),
                is_guard,
                "{event}: {entry}"
            );
        }
        assert_eq!(
            got["hooks"]["PreToolUse"][0]["hooks"][0] == shell_guard,
            on(3),
            "bits {bits:05b}: {got}"
        );
    }
}

/// The literal PM golden: the default-toggle PM block, byte for byte, with
/// the PM guard's host-resolved binary written as `<BIN>` (#10459 fix round).
const PM_GOLDEN: &str = include_str!("architect_hooks_pm_golden.json");

/// A PM session's default hook block equals [`PM_GOLDEN`].
#[test]
fn the_pm_hook_block_matches_its_literal_golden() {
    let got = project_managed_hook_additions_for_profile(
        Some(Path::new(EXE)),
        true,
        false,
        false,
        true,
        None,
    )
    .expect("pm additions");
    let text = serde_json::to_string_pretty(&got).expect("json").replace(
        &format!("\"{}{PM_GUARD_SUFFIX}\"", resolve_statusline_binary()),
        &format!("\"<BIN>{PM_GUARD_SUFFIX}\""),
    );
    assert_eq!(text.trim(), PM_GOLDEN.trim(), "actual PM block:\n{text}");
}

/// An Architect prep on an old Claude Code is a fatal refusal that writes
/// nothing: no `.claude/`, no settings (#10459 fix round).
#[test]
#[serial_test::serial]
fn an_old_claude_code_refuses_the_architect_prep_before_any_write() {
    let project = tempdir().unwrap();
    let home = tempdir().unwrap();
    let root = home.path().join(MPM_DIR_NAME);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("config.toml"),
        format!(
            "[architect]\nprojects = [{:?}]\n",
            project.path().display().to_string()
        ),
    )
    .unwrap();
    std::fs::write(
        project.path().join(".trusty-mpm.toml"),
        "profile = \"supervisor\"\n",
    )
    .unwrap();
    let probe = &crate::core::architect_claude_version::TEST_PROBE;
    probe.with(|p| *p.borrow_mut() = Ok("2.1.294 (Claude Code)".to_owned()));
    let _home = EnvVarGuard::set("HOME", home.path());
    let fw = crate::core::paths::FrameworkPaths::under(home.path());
    let mut host = super::super::HostInputs::with_home(Some(home.path()));
    host.hook_exe = Some(Path::new(EXE));
    let result =
        super::super::prepare_session_inner(&fw, project.path(), None, true, None, None, host);
    probe.with(|p| *p.borrow_mut() = Ok("2.1.312 (Claude Code)".to_owned()));
    let Err(err) = result else {
        panic!("an old Claude Code must refuse the Architect prep");
    };
    assert!(err.is_fatal(), "{err}");
    assert!(err.to_string().contains("older than v2.1.295"), "{err}");
    assert!(
        !project.path().join(".claude").exists(),
        "prep wrote before refusing"
    );
}

#[test]
fn an_exec_form_entry_reads_as_its_joined_argv() {
    assert_eq!(
        hook_entry_command_line(&guard_entry(true)).as_deref(),
        Some("/usr/local/bin/tm hook --pm-guard")
    );
    let shell = json!({ "type": "command", "command": "/x/tm hook --pm-guard" });
    assert_eq!(
        hook_entry_command_line(&shell).as_deref(),
        Some("/x/tm hook --pm-guard")
    );
    assert_eq!(hook_entry_command_line(&json!({ "type": "http" })), None);
}

/// The settings file at `dir` after one write of `additions`.
fn write(dir: &Path, additions: Value) -> Value {
    write_project_hooks_with(dir, Ok(additions)).expect("write");
    let text = std::fs::read_to_string(dir.join(".claude/settings.json")).expect("read");
    serde_json::from_str(&text).expect("json")
}

/// The number of `hook --pm-guard` entries on `event`, either form.
///
/// Read without [`hook_entry_command_line`], so a broken reader cannot hide
/// an entry from the tests that check it.
fn guards_on(settings: &Value, event: &str) -> usize {
    let exec_args = json!(["hook", "--pm-guard"]);
    settings["hooks"][event]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| group["hooks"].as_array().into_iter().flatten())
        .filter(|entry| {
            entry.get("args") == Some(&exec_args)
                || entry["command"]
                    .as_str()
                    .is_some_and(|c| c.ends_with(PM_GUARD_SUFFIX))
        })
        .count()
}

/// A relaunch replaces the exec-form guard (no duplicate), `direct_commands`
/// off removes the prompt groups, and a flip to PM restores the shell form.
#[test]
fn rewrites_replace_exec_form_entries_by_identity() {
    let dir = tempdir().unwrap();
    let on = write(dir.path(), architect(true));
    let again = write(dir.path(), architect(true));
    assert_eq!(on, again);
    assert_eq!(guards_on(&again, "PreToolUse"), 1);
    assert_eq!(guards_on(&again, "UserPromptExpansion"), 1);

    let off = write(dir.path(), architect(false));
    assert!(off["hooks"].get("UserPromptExpansion").is_none(), "{off}");
    assert_eq!(guards_on(&off, "UserPromptSubmit"), 0, "{off}");
    assert_eq!(off["hooks"]["PreToolUse"][0]["hooks"][0], guard_entry(true));

    let pm = project_managed_hook_additions_for_profile(
        Some(Path::new(EXE)),
        true,
        false,
        false,
        true,
        None,
    )
    .expect("pm");
    let flipped = write(dir.path(), pm.clone());
    assert_eq!(guards_on(&flipped, "PreToolUse"), 1);
    assert_eq!(flipped["hooks"], pm["hooks"], "{flipped}");
}

/// `[pm_guard] enabled = false` through the CLI writer removes the exec-form
/// guard too.
#[test]
fn the_guard_off_switch_strips_an_exec_form_guard() {
    let dir = tempdir().unwrap();
    write(dir.path(), architect(true));
    let path = dir.path().join(".claude/settings.json");
    crate::core::standalone::hooks::write_project_hooks_honoring_pm_guard(
        &path,
        Some(Path::new(EXE)),
        false,
    )
    .expect("strip");
    let text = std::fs::read_to_string(&path).unwrap();
    let settings: Value = serde_json::from_str(&text).unwrap();
    for event in ["PreToolUse", "UserPromptSubmit", "UserPromptExpansion"] {
        assert_eq!(guards_on(&settings, event), 0, "{event}: {settings}");
    }
}

#[test]
fn a_pm_project_resolves_no_architect_hooks() {
    let dir = tempdir().unwrap();
    let config = crate::core::config::MpmConfig::default();
    assert_eq!(ArchitectHooks::for_project(dir.path(), &config), None);
}

/// Launch `project` under `home` through the real prep; return its settings.
fn launch(project: &Path, home: &Path) -> Value {
    let _home = EnvVarGuard::set("HOME", home);
    let fw = crate::core::paths::FrameworkPaths::under(home);
    let mut host = super::super::HostInputs::with_home(Some(home));
    host.hook_exe = Some(Path::new(EXE));
    super::super::prepare_session_inner(&fw, project, None, true, None, None, host)
        .expect("the launch prepares");
    let text = std::fs::read_to_string(project.join(".claude/settings.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// #12077 part 4c: the launch writes no hooks into the project file for either
/// profile; the body carries the Architect's exec-form guard and, per
/// `[architect] direct_commands`, the prompt hooks, and a PM's shell form.
#[test]
#[serial_test::serial]
fn the_launch_carries_the_architect_hooks_by_profile_on_the_body_12077() {
    use crate::core::session_profile::SessionProfile;
    for direct_commands in [false, true] {
        let project = tempdir().unwrap();
        let home = tempdir().unwrap();
        let root = home.path().join(MPM_DIR_NAME);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("config.toml"),
            format!(
                "[architect]\nprojects = [{:?}]\ndirect_commands = {direct_commands}\n",
                project.path().display().to_string()
            ),
        )
        .unwrap();
        let profile = project.path().join(".trusty-mpm.toml");
        std::fs::write(&profile, "profile = \"supervisor\"\n").unwrap();
        let settings = launch(project.path(), home.path());
        assert!(settings.get("hooks").is_none(), "{settings}");
        let fw = crate::core::paths::FrameworkPaths::under(home.path());
        let body = super::super::launch_hooks::launch_hook_additions(
            &fw,
            project.path(),
            SessionProfile::Architect,
            Path::new(EXE),
        )
        .expect("body");
        assert_eq!(
            body["hooks"]["PreToolUse"][0]["hooks"][0],
            guard_entry(true)
        );
        assert_eq!(
            guards_on(&body, "UserPromptExpansion"),
            usize::from(direct_commands)
        );

        std::fs::write(&profile, "profile = \"pm\"\n").unwrap();
        let settings = launch(project.path(), home.path());
        assert!(settings.get("hooks").is_none(), "{settings}");
        let body = super::super::launch_hooks::launch_hook_additions(
            &fw,
            project.path(),
            SessionProfile::Pm,
            Path::new(EXE),
        )
        .expect("body");
        let guard = &body["hooks"]["PreToolUse"][0]["hooks"][0];
        assert!(guard.get("args").is_none(), "{body}");
        // #10600: the PM guard fails closed too, in shell form.
        assert_eq!(guard["onFailure"], "block", "{body}");
        assert!(body["hooks"].get("UserPromptExpansion").is_none(), "{body}");
    }
}

/// #12077 part 4c: `tm doctor --fix`'s `hooks_contamination` repair removes
/// the hooks an older Architect launch wrote into the project file — the
/// #10565 exemption is gone, because the body carries them now.
#[test]
fn doctor_fix_strips_an_architect_projects_old_hooks_12077() {
    use crate::core::doctor_repair::{RepairMode, repair_hooks_contamination};
    let dir = tempdir().unwrap();
    write(dir.path(), architect(true));
    let path = dir.path().join(".claude/settings.json");
    let before = std::fs::read_to_string(&path).unwrap();
    let planned = repair_hooks_contamination(dir.path(), RepairMode::DryRun);
    assert_eq!(planned.len(), 1, "{planned:?}");
    assert_eq!(before, std::fs::read_to_string(&path).unwrap(), "dry run");
    let applied = repair_hooks_contamination(dir.path(), RepairMode::Apply);
    assert_eq!(applied.len(), 1, "{applied:?}");
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(after.get("hooks").is_none(), "{after}");
}

// #12077 part 4c: the #10612 project-file write refusals moved to the carrier
// (`carrier_hooks_tests::an_unwritable_hook_body_refuses_the_launch_12077`).
