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

use super::super::hook_group_diff::project_hook_group_gaps_with;
use super::super::project_hooks::{
    project_managed_hook_additions_for_profile, project_managed_hook_additions_with_pm_guard,
};
use super::super::settings::{
    PM_GUARD_SUFFIX, resolve_statusline_binary, write_project_hooks_with,
};
use super::super::tests::EnvVarGuard;
use super::*;
use crate::core::standalone::hooks::hook_entry_command_line;

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

/// The golden: the Architect's block is the PM block with the guard group
/// swapped for the exec-form one and, with the flag on, two prompt groups
/// appended. Nothing else differs.
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
/// before #10459, the guard stays shell form, and nothing carries `args`,
/// `onFailure` or a `UserPromptExpansion` group.
#[test]
fn pm_session_registers_no_ticket_handler() {
    let shell_guard = json!({
        "type": "command",
        "command": format!("{}{PM_GUARD_SUFFIX}", resolve_statusline_binary()),
        "timeout": 10
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
            assert!(entry.get("onFailure").is_none(), "{event}: {entry}");
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
    let root = home.path().join(".trusty-mpm");
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

/// `tm doctor`'s gap probe reads exec-form entries: an Architect file
/// matches its own expected block, and a missing prompt group is reported.
#[test]
fn the_gap_probe_reads_exec_form_entries() {
    let dir = tempdir().unwrap();
    let settings = write(dir.path(), architect(true));
    let gaps = project_hook_group_gaps_with(&settings, || Ok(architect(true))).expect("gaps");
    assert!(gaps.is_empty(), "{gaps:?}");
    let settings = write(dir.path(), architect(false));
    let gaps = project_hook_group_gaps_with(&settings, || Ok(architect(true))).expect("gaps");
    assert!(
        gaps.missing
            .iter()
            .any(|(event, _)| event == "UserPromptExpansion"),
        "{gaps:?}"
    );
}

#[test]
fn a_pm_project_resolves_no_architect_hooks() {
    let dir = tempdir().unwrap();
    let config = crate::core::config::MpmConfig::default();
    assert_eq!(ArchitectHooks::for_project(dir.path(), &config), None);
}

/// Launch `project` under `home` through the real writer; return its settings.
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

/// The launch writer: an allow-listed Architect project gets the exec-form
/// guard and, per `[architect] direct_commands`, the prompt hooks; the same
/// project as a PM gets the shell form back.
#[test]
#[serial_test::serial]
fn the_launch_writes_the_architect_hooks_by_profile() {
    for direct_commands in [false, true] {
        let project = tempdir().unwrap();
        let home = tempdir().unwrap();
        let root = home.path().join(".trusty-mpm");
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
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["hooks"][0],
            guard_entry(true)
        );
        assert_eq!(
            guards_on(&settings, "UserPromptExpansion"),
            usize::from(direct_commands)
        );

        std::fs::write(&profile, "profile = \"pm\"\n").unwrap();
        let settings = launch(project.path(), home.path());
        let guard = &settings["hooks"]["PreToolUse"][0]["hooks"][0];
        assert!(guard.get("args").is_none(), "{settings}");
        assert!(guard.get("onFailure").is_none(), "{settings}");
        assert!(
            settings["hooks"].get("UserPromptExpansion").is_none(),
            "{settings}"
        );
    }
}

/// An allow-listed Architect project under `base`: its framework layout, with
/// `[architect] projects` naming `project` and the `supervisor` profile set.
fn architect_fw(base: &Path, project: &Path) -> crate::core::paths::FrameworkPaths {
    let fw = crate::core::paths::FrameworkPaths::for_managed_workspace_under(base, project);
    std::fs::create_dir_all(&fw.root).unwrap();
    std::fs::write(
        fw.config_toml(),
        format!(
            "[hooks]\nprompt_context = true\n[architect]\nprojects = [{:?}]\ndirect_commands = true\n",
            project.display().to_string()
        ),
    )
    .unwrap();
    std::fs::write(
        project.join(".trusty-mpm.toml"),
        "profile = \"supervisor\"\n",
    )
    .unwrap();
    fw
}

/// #10565: `tm doctor --fix`'s `hooks_contamination` repair leaves the hooks
/// an Architect launch wrote in place, in the dry run and when applied. On
/// 1.9.1 it stripped the lifecycle entry from all seven events.
#[test]
fn doctor_fix_keeps_the_architect_hooks() {
    use crate::core::doctor_repair::{RepairMode, repair_hooks_contamination};
    let dir = tempdir().unwrap();
    write(dir.path(), architect(true));
    let path = dir.path().join(".claude/settings.json");
    let before = std::fs::read_to_string(&path).unwrap();
    for mode in [RepairMode::DryRun, RepairMode::Apply] {
        let steps = repair_hooks_contamination(dir.path(), mode);
        assert!(
            steps.is_empty(),
            "{mode:?} planned a strip of the Architect's own hooks: {steps:?}"
        );
        assert_eq!(before, std::fs::read_to_string(&path).unwrap(), "{mode:?}");
    }
}

/// #10565 error arm: when the re-merge that follows the strip cannot run (an
/// Architect on a Claude Code below v2.1.295), the `--fix --yes` sequence
/// still leaves the Architect's hooks in place rather than reporting a
/// failure over a file it already stripped.
#[test]
fn a_refused_re_merge_never_leaves_the_architect_hooks_stripped() {
    use crate::core::doctor_repair::{
        RepairMode, StepStatus, repair_hooks_contamination, repair_missing_hook_group_with,
    };
    let project = tempdir().unwrap();
    let base = tempdir().unwrap();
    let fw = architect_fw(base.path(), project.path());
    write(project.path(), architect(true));
    let path = project.path().join(".claude/settings.json");
    let before = std::fs::read_to_string(&path).unwrap();

    let probe = &crate::core::architect_claude_version::TEST_PROBE;
    probe.with(|p| *p.borrow_mut() = Ok("2.1.294 (Claude Code)".to_owned()));
    let strip = repair_hooks_contamination(project.path(), RepairMode::Apply);
    let merge = repair_missing_hook_group_with(
        &fw,
        project.path(),
        Some(Path::new(EXE)),
        RepairMode::Apply,
    );
    probe.with(|p| *p.borrow_mut() = Ok("2.1.312 (Claude Code)".to_owned()));

    assert!(
        !merge
            .iter()
            .any(|s| matches!(s.status, StepStatus::Failed(_) | StepStatus::Refused(_))),
        "the strip {strip:?} left a gap the refused merge could not close: {merge:?}"
    );
    assert_eq!(before, std::fs::read_to_string(&path).unwrap());
}
