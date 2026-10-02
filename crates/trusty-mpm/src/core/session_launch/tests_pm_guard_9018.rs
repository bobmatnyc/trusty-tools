//! #9018: `[pm_guard] enabled` decides whether the launch and resume writers
//! register the `hook --pm-guard` `PreToolUse` entry.
//!
//! Why: owner ruling 307 turned the guard off fleet-wide, and a hand-stripped
//! entry came back at the next launch. These prove the key is WIRED into the
//! real launch (`prepare_session`) and the resume merge, in every state of the
//! key — on, off, absent, and unparseable.
//! What: each test runs against a scratch `$HOME` whose
//! `.trusty-mpm/config.toml` holds the state under test, a scratch project, and
//! a pinned hook binary, then reads back `.claude/settings.json`.
//! Test: this is the test module.

use super::tests::EnvVarGuard;
use super::*;
use tempfile::TempDir;

/// The pinned stable hook binary (see `tests_malformed_settings_7780`).
const TEST_EXE: &str = "/usr/local/bin/tm";

/// A guard entry a prior launch wrote, a tm lifecycle entry, and two foreign
/// entries the writers must never touch.
const SEEDED: &str = r#"{
  "permissions": { "allow": ["Bash(ls)"] },
  "hooks": {
    "PreToolUse": [
      { "matcher": "", "hooks": [ { "type": "command", "command": "/usr/local/bin/tm hook --pm-guard", "timeout": 10 } ] },
      { "matcher": "*", "hooks": [ { "type": "command", "command": "/usr/local/bin/tm hook", "timeout": 5 } ] },
      { "matcher": "Bash", "hooks": [ { "type": "command", "command": "/opt/foreign/check-bash" } ] }
    ],
    "Notification": [
      { "matcher": "", "hooks": [ { "type": "command", "command": "/opt/foreign/notify" } ] }
    ]
  }
}"#;

/// A scratch home whose user config is `body` (`None` writes no file).
fn home_with(body: Option<&str>) -> TempDir {
    let home = TempDir::new().unwrap();
    let root = home.path().join(crate::core::paths::FRAMEWORK_DIR_NAME);
    std::fs::create_dir_all(&root).unwrap();
    if let Some(body) = body {
        std::fs::write(root.join("config.toml"), body).unwrap();
    }
    home
}

/// Seed `<project>/.claude/settings.json` with [`SEEDED`].
fn seed(project: &Path) {
    let claude = project.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(claude.join("settings.json"), SEEDED).unwrap();
}

/// Run the real launch with `home` as `$HOME` and return the settings file.
fn launch(home: &Path, project: &Path) -> serde_json::Value {
    let _home = EnvVarGuard::set("HOME", home);
    let mut fw = crate::core::paths::FrameworkPaths::under(home);
    fw.trusty_mpm_root = None;
    prepare_session_for_repair_under(
        &fw,
        project,
        None,
        Some(Path::new(TEST_EXE)),
        Some(false),
        Some(home),
    )
    .expect("the launch prepares");
    read_settings(project)
}

fn read_settings(project: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// Every hook command in `settings`, across every event.
fn all_commands(settings: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    for groups in settings["hooks"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
    {
        for group in groups.as_array().into_iter().flatten() {
            for hook in group["hooks"].as_array().into_iter().flatten() {
                if let Some(cmd) = hook["command"].as_str() {
                    out.push(cmd.to_string());
                }
            }
        }
    }
    out
}

fn has_guard(settings: &serde_json::Value) -> bool {
    all_commands(settings)
        .iter()
        .any(|c| c.ends_with(" hook --pm-guard"))
}

/// Assert the guard is gone and every other hook in [`SEEDED`] survived.
fn assert_guard_removed_others_kept(settings: &serde_json::Value) {
    let commands = all_commands(settings);
    assert!(!has_guard(settings), "guard must be removed: {commands:?}");
    for kept in [
        "/usr/local/bin/tm hook",
        "/opt/foreign/check-bash",
        "/opt/foreign/notify",
    ] {
        assert!(
            commands.iter().any(|c| c == kept),
            "`{kept}` must survive: {commands:?}"
        );
    }
    assert_eq!(settings["permissions"]["allow"][0], "Bash(ls)");
}

#[test]
#[serial_test::serial]
fn launch_writes_the_guard_when_enabled() {
    let home = home_with(Some("[pm_guard]\nenabled = true\n"));
    let project = TempDir::new().unwrap();
    assert!(has_guard(&launch(home.path(), project.path())));
}

#[test]
#[serial_test::serial]
fn launch_writes_the_guard_when_the_key_is_missing() {
    let home = home_with(Some("[hooks]\nprompt_context = true\n"));
    let project = TempDir::new().unwrap();
    assert!(has_guard(&launch(home.path(), project.path())));
    let no_file = home_with(None);
    let project = TempDir::new().unwrap();
    assert!(has_guard(&launch(no_file.path(), project.path())));
}

#[test]
#[serial_test::serial]
fn launch_with_the_guard_disabled_removes_an_existing_entry() {
    let home = home_with(Some("[pm_guard]\nenabled = false\n"));
    let project = TempDir::new().unwrap();
    seed(project.path());
    let settings = launch(home.path(), project.path());
    assert_guard_removed_others_kept(&settings);
}

/// #9018 Fail-Open Check: a config that does not parse keeps the guard.
#[test]
#[serial_test::serial]
fn launch_with_a_malformed_config_keeps_the_guard() {
    let home = home_with(Some("[pm_guard\nenabled = false\n"));
    let project = TempDir::new().unwrap();
    assert!(has_guard(&launch(home.path(), project.path())));
}

#[test]
#[serial_test::serial]
fn resume_with_the_guard_disabled_removes_an_existing_entry() {
    let home = home_with(Some("[pm_guard]\nenabled = false\n"));
    let _home = EnvVarGuard::set("HOME", home.path());
    let project = TempDir::new().unwrap();
    seed(project.path());
    let fw = crate::core::paths::FrameworkPaths::under(home.path());
    ensure_project_hooks_with(&fw, project.path(), Some(Path::new(TEST_EXE)))
        .expect("the resume merge writes");
    assert_guard_removed_others_kept(&read_settings(project.path()));
}

#[test]
#[serial_test::serial]
fn a_disabled_guard_is_left_out_of_the_additions() {
    let project = TempDir::new().unwrap();
    for (body, want_guard) in [
        (Some("[pm_guard]\nenabled = false\n"), false),
        (Some("[pm_guard]\nenabled = true\n"), true),
        (None, true),
    ] {
        let home = home_with(body);
        let _home = EnvVarGuard::set("HOME", home.path());
        let fw = crate::core::paths::FrameworkPaths::under(home.path());
        let additions = hook_group_diff::project_hook_additions_for(
            &fw,
            project.path(),
            Some(Path::new(TEST_EXE)),
        )
        .expect("the pinned binary resolves");
        assert_eq!(has_guard(&additions), want_guard, "{body:?}: {additions}");
        // The lifecycle `tm hook` group is written either way.
        assert!(
            all_commands(&additions)
                .iter()
                .any(|c| c == "/usr/local/bin/tm hook"),
            "{body:?}: {additions}"
        );
    }
}
