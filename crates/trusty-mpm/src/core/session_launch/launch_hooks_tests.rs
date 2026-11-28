//! Tests for tm's launch-time hooks and the project strip (#12077 part 4c).

use super::*;
use crate::core::standalone::hooks::build_tree::PM_GUARD_SUFFIX;
use crate::core::standalone::hooks::egress::{EgressGuardState, egress_guard_state};
use crate::test_support::STABLE_HOOK_EXE;
use serde_json::json;

/// A scratch framework base and project directory.
struct Fx {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    project: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical");
        let project = root.join("project");
        std::fs::create_dir_all(project.join(".claude")).expect("mkdir .claude");
        Self {
            base: root.join("base"),
            project,
            _tmp: tmp,
        }
    }
    fn fw(&self) -> FrameworkPaths {
        FrameworkPaths::for_managed_workspace_under(&self.base, &self.project)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.project.join(".claude").join(name)
    }
    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.file(name);
        std::fs::write(&path, text).expect("write settings");
        path
    }
}

/// Every command line under `val`'s `hooks`, by event.
fn command_lines(val: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(hooks) = val.get("hooks").and_then(Value::as_object) else {
        return out;
    };
    for (event, groups) in hooks {
        for group in groups.as_array().into_iter().flatten() {
            for entry in group["hooks"].as_array().into_iter().flatten() {
                if let Some(line) = crate::core::standalone::hooks::hook_entry_command_line(entry) {
                    out.push((event.clone(), line.into_owned()));
                }
            }
        }
    }
    out
}

fn tm_group(cmd: &str) -> Value {
    json!({ "matcher": "", "hooks": [{ "type": "command", "command": cmd, "timeout": 5 }] })
}

#[test]
fn the_body_carries_the_pm_guard_and_egress_groups_12077() {
    let fx = Fx::new();
    let additions = launch_hook_additions(
        &fx.fw(),
        &fx.project,
        SessionProfile::Pm,
        Path::new(STABLE_HOOK_EXE),
    )
    .expect("a pinned exe resolves");
    assert_eq!(egress_guard_state(&additions), EgressGuardState::Present);
    let lines = command_lines(&additions);
    assert!(
        lines
            .iter()
            .any(|(event, line)| event == "PreToolUse" && line.ends_with(PM_GUARD_SUFFIX)),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|(event, line)| event == "SessionStart" && line == "/usr/local/bin/tm hook"),
        "{lines:?}"
    );
}

#[test]
fn an_architect_body_carries_the_exec_form_guard_12077() {
    let fx = Fx::new();
    let additions = launch_hook_additions(
        &fx.fw(),
        &fx.project,
        SessionProfile::Architect,
        Path::new(STABLE_HOOK_EXE),
    )
    .expect("a pinned exe resolves");
    let guard = additions["hooks"]["PreToolUse"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
        .find(|e| e["args"] == json!(["hook", "--pm-guard"]))
        .expect("the exec-form guard");
    assert_eq!(guard["command"], STABLE_HOOK_EXE);
    assert_eq!(guard["onFailure"], "block");
    let lines = command_lines(&additions);
    assert!(
        lines
            .iter()
            .any(|(_, line)| super::super::architect_hooks::is_standing_rules_hook_command(line)),
        "the standing-rules groups ride the body: {lines:?}"
    );
}

#[test]
fn the_strip_predicate_claims_every_tm_shape_and_no_memory_prefix_12077() {
    for cmd in [
        "/Users/u/.cargo/bin/tm hook",
        "/Users/u/.local/share/mise/shims/tm hook",
        "/usr/local/bin/tm hook --pm-guard",
        "tm hook --pm-guard",
        "/usr/local/bin/tm hook --egress-guard",
    ] {
        assert!(is_stale_tm_hook_command(cmd), "{cmd}");
    }
    for cmd in [
        "trusty-memory prompt-context",
        "trusty-memory inbox-check",
        "npm run lint",
        "/usr/local/bin/claude-hook PreToolUse",
    ] {
        assert!(!is_stale_tm_hook_command(cmd), "{cmd}");
    }
}

#[test]
fn a_default_launch_leaves_a_hookless_project_file_byte_identical_12077() {
    let fx = Fx::new();
    // Odd spacing and key order: any rewrite would change the bytes.
    let text = "{\"permissions\":{\"allow\":[\"Bash(ls)\"]},  \"hooks\": {\"Stop\": [{\"matcher\":\"\",\"hooks\":[{\"type\":\"command\",\"command\":\"npm run lint\"}]}]}}\n";
    let path = fx.write("settings.json", text);
    let changed = strip_tm_project_hooks(&fx.project).expect("strip");
    assert!(changed.is_empty(), "{changed:?}");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), text);
    assert!(!fx.file("settings.local.json").exists());
}

#[test]
fn a_launch_removes_only_tms_entries_12077() {
    let fx = Fx::new();
    let foreign = json!({ "type": "command", "command": "npm run lint" });
    let settings = json!({
        "permissions": { "allow": ["Bash(ls)"] },
        "statusLine": { "type": "command", "command": "/usr/local/bin/tm statusline" },
        "hooks": {
            "PreToolUse": [
                { "matcher": "", "hooks": [
                    { "type": "command", "command": "/Users/u/.cargo/bin/tm hook --pm-guard", "timeout": 10 },
                    foreign.clone()
                ]},
                { "matcher": "", "hooks": [
                    { "type": "command", "command": "/opt/tm", "args": ["hook", "--pm-guard"] }
                ]},
                tm_group("/Users/u/.local/share/mise/shims/tm hook --egress-guard")
            ],
            "SessionStart": [
                tm_group("trusty-memory inbox-check"),
                tm_group("/Users/u/.cargo/bin/tm hook")
            ]
        }
    });
    let path = fx.write("settings.json", &format!("{settings:#}"));
    let local = fx.write(
        "settings.local.json",
        &json!({ "hooks": { "Stop": [tm_group("/usr/local/bin/tm hook")] } }).to_string(),
    );
    let changed = strip_tm_project_hooks(&fx.project).expect("strip");
    assert_eq!(changed, vec![path.clone(), local.clone()]);
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(after["permissions"], settings["permissions"]);
    assert_eq!(after["statusLine"], settings["statusLine"]);
    assert_eq!(
        after["hooks"],
        json!({ "PreToolUse": [{ "matcher": "", "hooks": [foreign] }] })
    );
    let local_after: Value =
        serde_json::from_str(&std::fs::read_to_string(&local).unwrap()).unwrap();
    assert_eq!(local_after, json!({}), "an emptied hooks key goes too");
}

#[test]
fn a_memory_hook_alone_is_the_operators_and_stays_12077() {
    let fx = Fx::new();
    let text =
        json!({ "hooks": { "SessionStart": [tm_group("trusty-memory inbox-check")] } }).to_string();
    let path = fx.write("settings.json", &text);
    assert!(
        strip_tm_project_hooks(&fx.project)
            .expect("strip")
            .is_empty()
    );
    assert_eq!(std::fs::read_to_string(path).expect("read"), text);
}

#[test]
fn stale_event_names_match_what_the_strip_removes_12077() {
    let mut val = json!({ "hooks": {
        "SessionStart": [tm_group("trusty-memory inbox-check")],
        "Stop": [tm_group("/usr/local/bin/tm hook")],
        "Notification": [tm_group("say done")]
    }});
    let mut names = stale_tm_hook_event_names(&val);
    names.sort();
    assert_eq!(names, vec!["SessionStart".to_owned(), "Stop".to_owned()]);
    assert!(strip_tm_entries(&mut val));
    assert!(stale_tm_hook_event_names(&val).is_empty());
    assert_eq!(
        val["hooks"],
        json!({ "Notification": [tm_group("say done")] })
    );
}

/// Fail-Open Check: a project file that exists and cannot be read refuses.
#[test]
fn an_unreadable_project_file_refuses_the_strip_12077() {
    let fx = Fx::new();
    std::fs::create_dir_all(fx.file("settings.json")).expect("a directory in its place");
    let err = strip_tm_project_hooks(&fx.project).expect_err("an unreadable file refuses");
    assert_eq!(err.path, fx.file("settings.json"));
}

/// Fail-Open Check: a strip that cannot publish refuses and leaves the file.
#[cfg(unix)]
#[test]
fn an_unwritable_project_file_refuses_the_strip_12077() {
    use std::os::unix::fs::PermissionsExt as _;
    let fx = Fx::new();
    let text = json!({ "hooks": { "Stop": [tm_group("/usr/local/bin/tm hook")] } }).to_string();
    let path = fx.write("settings.json", &text);
    let dir = fx.project.join(".claude");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = strip_tm_project_hooks(&fx.project);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    result.expect_err("an unwritable directory refuses");
    assert_eq!(std::fs::read_to_string(path).expect("read"), text);
}
