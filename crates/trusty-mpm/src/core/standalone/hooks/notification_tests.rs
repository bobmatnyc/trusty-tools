//! Tests for the opt-in `Notification` hook entry and its push target (#8392).

use super::*;
use crate::core::config::MpmConfig;

/// Installed-looking binary the resolver accepts without a PATH lookup.
const EXE: &str = "/usr/local/bin/tm";

fn exe() -> Option<&'static Path> {
    Some(Path::new(EXE))
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// A settings file holding the six lifecycle groups plus the user's own hooks:
/// a foreign `Notification` group and a foreign `PreToolUse` group.
fn seeded_settings(path: &Path) -> Value {
    let lifecycle = super::super::mpm_hook_additions_with_exe(exe()).unwrap();
    let mut val = serde_json::json!({
        "outputStyle": "trusty-mpm",
        "hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "/opt/me/audit"}]}],
            "Notification": [{"matcher": "", "hooks": [{"type": "command", "command": "/opt/me/notify-me"}]}]
        }
    });
    val = trusty_common::claude_config::merge_hook_entries(&val, &lifecycle);
    std::fs::write(path, serde_json::to_string_pretty(&val).unwrap()).unwrap();
    val
}

#[test]
fn the_opt_in_and_inbox_parse() {
    let cfg: MpmConfig =
        toml::from_str("[notification_hook]\nenabled = true\ninbox = \"/srv/inbox\"\n").unwrap();
    assert!(cfg.notification_hook.enabled);
    assert_eq!(
        cfg.notification_hook.inbox.as_deref(),
        Some(Path::new("/srv/inbox"))
    );
    assert!(!MpmConfig::default().notification_hook.enabled);
}

/// Fail-Open Check: an unparseable opt-in is OFF, and it does not cost the
/// rest of the file (a strict `bool` would fail the whole parse).
#[test]
fn an_unparseable_opt_in_is_off_and_keeps_the_rest_of_the_config() {
    for raw in ["\"yes\"", "1", "\"true\"", "[true]"] {
        let text =
            format!("[hooks]\nprompt_context = false\n[notification_hook]\nenabled = {raw}\n");
        let cfg: MpmConfig = toml::from_str(&text).unwrap_or_else(|e| panic!("{raw}: {e}"));
        assert!(!cfg.notification_hook.enabled, "{raw} must read as off");
        assert!(!cfg.hooks.prompt_context, "{raw} must not drop [hooks]");
    }
}

#[test]
fn the_env_var_outranks_the_config_key() {
    let cfg = NotificationHookConfig {
        enabled: false,
        inbox: Some(PathBuf::from("/from/config")),
    };
    assert_eq!(
        resolve_push_target(Some("/from/env".into()), &cfg),
        PushTarget::Inbox(PathBuf::from("/from/env"))
    );
    assert_eq!(
        resolve_push_target(Some("".into()), &cfg),
        PushTarget::Inbox(PathBuf::from("/from/config"))
    );
}

#[test]
fn an_unset_target_is_unset_and_a_relative_one_is_refused() {
    let cfg = NotificationHookConfig::default();
    assert_eq!(resolve_push_target(None, &cfg), PushTarget::Unset);
    assert_eq!(
        resolve_push_target(Some("inbox".into()), &cfg),
        PushTarget::NotAbsolute
    );
}

/// With the opt-in off, the writer leaves the lifecycle-only file exactly as
/// it is and creates no file where there was none.
#[test]
fn opt_in_off_installs_exactly_the_six_lifecycle_events() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    assert!(!apply_notification_hook(&path, exe(), false).unwrap());
    assert!(!path.exists(), "off must not create a settings file");

    super::super::write_project_hooks(&path, exe()).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(!apply_notification_hook(&path, exe(), false).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let events: Vec<String> = read(&path)["hooks"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let mut expected: Vec<String> = super::super::MPM_LIFECYCLE_HOOK_EVENTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut got = events;
    expected.sort();
    got.sort();
    assert_eq!(got, expected);
}

/// On writes one tm entry after the user's own, keeps every other hook in
/// place, and a second run is a byte-identical no-op.
#[test]
fn opt_in_on_writes_one_entry_and_rerunning_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    let seeded = seeded_settings(&path);

    assert!(apply_notification_hook(&path, exe(), true).unwrap());
    let after = std::fs::read(&path).unwrap();
    assert!(!apply_notification_hook(&path, exe(), true).unwrap());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        after,
        "second run rewrote the file"
    );

    let val = read(&path);
    let groups = val["hooks"]["Notification"].as_array().unwrap();
    assert_eq!(
        groups.len(),
        2,
        "the user's group plus exactly one tm group"
    );
    assert_eq!(
        groups[0], seeded["hooks"]["Notification"][0],
        "user's group moved"
    );
    assert_eq!(groups[1]["hooks"][0]["command"], format!("{EXE} hook"));
    for (event, value) in seeded["hooks"].as_object().unwrap() {
        if event != NOTIFICATION_EVENT {
            assert_eq!(&val["hooks"][event], value, "{event} changed");
        }
    }
    assert_eq!(val["outputStyle"], "trusty-mpm");
}

/// Turning the opt-in off removes tm's entry and nothing else: the file goes
/// back to the exact value it had before the opt-in was turned on.
#[test]
fn opt_in_off_removes_only_the_entry_tm_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    let seeded = seeded_settings(&path);
    apply_notification_hook(&path, exe(), true).unwrap();

    assert!(apply_notification_hook(&path, exe(), false).unwrap());
    assert_eq!(read(&path), seeded);
}

/// Fail-Open Check: a malformed settings file is refused, left byte-identical,
/// and no sibling copy is written.
#[test]
fn a_malformed_settings_file_is_refused_and_left_byte_identical() {
    for body in [
        "{\"hooks\": ",
        "[1, 2]",
        "{\"hooks\": \"x\"}",
        "{\"hooks\": {\"Notification\": {}}}",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, body).unwrap();
        for enabled in [true, false] {
            let err = apply_notification_hook(&path, exe(), enabled).unwrap_err();
            assert!(err.to_string().contains("left unchanged"), "{body}: {err}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        }
        let names: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            // The lock sidecar carries a pid, never a copy of the settings.
            .filter(|n| !n.to_string_lossy().ends_with(".lock"))
            .collect();
        assert_eq!(
            names,
            vec![std::ffi::OsString::from("settings.json")],
            "{body}"
        );
    }
}

#[test]
fn the_doctor_step_plans_applies_and_then_goes_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    let seeded = seeded_settings(&path);
    let before = std::fs::read(&path).unwrap();

    let plan = repair_notification_hook(&path, exe(), true, RepairMode::DryRun);
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].check, NOTIFICATION_HOOK_CHECK);
    assert_eq!(plan[0].status, StepStatus::Planned);
    assert_eq!(std::fs::read(&path).unwrap(), before, "a dry run wrote");

    let applied = repair_notification_hook(&path, exe(), true, RepairMode::Apply);
    assert!(applied[0].changed());
    assert!(repair_notification_hook(&path, exe(), true, RepairMode::Apply).is_empty());

    let removed = repair_notification_hook(&path, exe(), false, RepairMode::Apply);
    assert!(removed[0].changed());
    assert_eq!(read(&path), seeded);
}

#[test]
fn the_doctor_step_refuses_a_malformed_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("settings.json");
    std::fs::write(&path, "not json").unwrap();
    let steps = repair_notification_hook(&path, exe(), true, RepairMode::Apply);
    assert_eq!(steps.len(), 1);
    assert!(
        matches!(steps[0].status, StepStatus::Refused(_)),
        "{steps:?}"
    );
    assert!(!steps[0].changed());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
}
