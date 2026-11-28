//! #9018: `[pm_guard] enabled` decides whether a launch registers the
//! `hook --pm-guard` `PreToolUse` entry.
//!
//! Why: owner ruling 307 turned the guard off fleet-wide, and a hand-stripped
//! entry came back at the next launch. These prove the key is WIRED into the
//! hook groups a launch carries, in every state of the key — on, off, absent,
//! and unparseable. #12077 part 4c: those groups ride the `--settings` body
//! (`launch_hooks::launch_hook_additions`), never the project file.
//! What: each test runs against a scratch `$HOME` whose
//! `.trusty-mpm/config.toml` holds the state under test, a scratch project, and
//! a pinned hook binary, then reads the body's hook groups.
//! Test: this is the test module.

use super::tests::EnvVarGuard;
use super::*;
use tempfile::TempDir;

/// The pinned stable hook binary (see `tests_malformed_settings_7780`).
const TEST_EXE: &str = "/usr/local/bin/tm";

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

/// The hook groups a PM launch in `project` carries with `home` as `$HOME`.
fn launch(home: &Path, project: &Path) -> serde_json::Value {
    let _home = EnvVarGuard::set("HOME", home);
    let fw = crate::core::paths::FrameworkPaths::under(home);
    launch_hooks::launch_hook_additions(
        &fw,
        project,
        crate::core::session_profile::SessionProfile::Pm,
        Path::new(TEST_EXE),
    )
    .expect("the pinned binary resolves")
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
fn a_disabled_guard_is_left_out_of_the_additions() {
    let project = TempDir::new().unwrap();
    for (body, want_guard) in [
        (Some("[pm_guard]\nenabled = false\n"), false),
        (Some("[pm_guard]\nenabled = true\n"), true),
        (None, true),
    ] {
        let home = home_with(body);
        let additions = launch(home.path(), project.path());
        assert_eq!(has_guard(&additions), want_guard, "{body:?}: {additions}");
        // The lifecycle `tm hook` group is carried either way.
        assert!(
            all_commands(&additions)
                .iter()
                .any(|c| c == "/usr/local/bin/tm hook"),
            "{body:?}: {additions}"
        );
    }
}
