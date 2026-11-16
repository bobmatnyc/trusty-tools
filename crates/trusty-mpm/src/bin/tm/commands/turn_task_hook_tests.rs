//! Tests for [`super::respond`] (#11398).

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;

use serde_json::json;

use super::*;

/// A PM project with `datg/pm-a/state.json` whose `current` is `current`.
fn project(current: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let sdir = dir.path().join(".trusty-mpm/datg/pm-a");
    std::fs::create_dir_all(&sdir).unwrap();
    let mut session = json!({ "task": "free text" });
    if let Some(c) = current {
        session["current"] = json!(c);
    }
    let state = json!({
        "code": "ar",
        "session": session,
        "tasks": [{ "id": "ar-T0b", "status": "open", "created": "2026-11-16T07:00Z" }],
    });
    std::fs::write(sdir.join("state.json"), state.to_string()).unwrap();
    dir
}

/// Call [`respond`] with `vars` as the environment and `payload` on stdin.
fn call(vars: &[(&str, &Path)], extra: &[(&str, &str)], payload: Option<String>) -> Option<String> {
    let mut map: HashMap<String, OsString> = vars
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.as_os_str().to_owned()))
        .collect();
    map.extend(
        extra
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v))),
    );
    respond(
        |name: &str| map.get(name).cloned(),
        payload,
        MpmConfig::default,
        || Some("pm-a".to_owned()),
    )
}

/// A `UserPromptSubmit` payload, plus `agent_id` when given.
fn payload(event: &str, agent_id: Option<&str>) -> Option<String> {
    let mut p = json!({ "hook_event_name": event, "session_id": "s", "prompt": "go" });
    if let Some(a) = agent_id {
        p["agent_id"] = json!(a);
    }
    Some(p.to_string())
}

#[test]
fn a_pm_payload_is_stamped() {
    let dir = project(Some("ar-T0b"));
    let got = call(
        &[("CLAUDE_PROJECT_DIR", dir.path())],
        &[],
        payload("UserPromptSubmit", None),
    )
    .expect("stamped");
    let out: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert_eq!(
        out["hookSpecificOutput"]["additionalContext"],
        "Task: ar-T0b@1116"
    );
}

#[test]
fn each_fail_open_arm_prints_nothing() {
    let dir = project(Some("ar-T0b"));
    let pd = [("CLAUDE_PROJECT_DIR", dir.path())];
    let ups = || payload("UserPromptSubmit", None);
    assert_eq!(
        call(&pd, &[], payload("UserPromptSubmit", Some("a-1"))),
        None,
        "agent_id"
    );
    assert_eq!(
        call(&pd, &[("CLAUDE_MPM_SUB_AGENT", "1")], ups()),
        None,
        "sub-agent env"
    );
    assert_eq!(
        call(&pd, &[], payload("SessionStart", None)),
        None,
        "other event"
    );
    assert_eq!(call(&pd, &[], Some("not json".into())), None, "bad payload");
    assert_eq!(call(&pd, &[], None), None, "unreadable stdin");
    assert_eq!(call(&[], &[], ups()), None, "no project dir");
    let absent = project(None);
    let pd_absent = [("CLAUDE_PROJECT_DIR", absent.path())];
    assert_eq!(call(&pd_absent, &[], ups()), None, "current absent");
    let bare = tempfile::tempdir().unwrap();
    let pd_bare = [("CLAUDE_PROJECT_DIR", bare.path())];
    assert_eq!(call(&pd_bare, &[], ups()), None, "no state file");
}
