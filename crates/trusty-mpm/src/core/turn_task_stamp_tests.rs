//! Tests for [`super`]: the #11398 per-turn task stamp.

use std::cell::Cell;
use std::ffi::OsStr;
use std::path::Path;

use regex::Regex;
use serde_json::json;
use tempfile::tempdir;

use super::*;

/// A datg state with `current` (when `Some`) and the given tasks.
fn state(current: Option<&str>, tasks: serde_json::Value) -> String {
    let mut session = json!({ "task": "free text" });
    if let Some(c) = current {
        session[CURRENT_FIELD] = json!(c);
    }
    json!({ "v": 3, "code": "ar", "session": session, "tasks": tasks }).to_string()
}

/// One task record.
fn task(id: &str, status: &str, created: Option<&str>) -> serde_json::Value {
    let mut t = json!({ "id": id, "status": status, "goal": "ar-G01", "title": "t" });
    if let Some(c) = created {
        t["created"] = json!(c);
    }
    t
}

#[test]
fn a_current_open_task_is_stamped() {
    let tasks = json!([
        task("ar-T0b", "open", Some("2026-11-16T07:40Z")),
        task("ar-T0c", "running", Some("2026-10-02")),
        task("ar-T0d", "open", None),
    ]);
    let cases = [
        ("ar-T0b", "Task: ar-T0b@1116"),
        ("ar-T0c", "Task: ar-T0c@1002"),
        ("ar-T0d", "Task: ar-T0d"),
    ];
    for (current, want) in cases {
        let got = stamp_from_state(&state(Some(current), tasks.clone()));
        assert_eq!(got.as_deref(), Some(want), "{current}");
    }
    let out = hook_output("Task: ar-T0b@1116");
    assert_eq!(
        out["hookSpecificOutput"]["hookEventName"],
        "UserPromptSubmit"
    );
    assert_eq!(
        out["hookSpecificOutput"]["additionalContext"],
        "Task: ar-T0b@1116"
    );
    assert!(out.get("decision").is_none() && out.get("continue").is_none());
}

#[test]
fn each_fail_open_case_is_silent() {
    let tasks = json!([
        task("ar-T0b", "open", Some("2026-11-16")),
        task("ar-T0e", "done", Some("2026-11-16")),
        task("ar-T0f", "parked", Some("2026-11-16")),
    ]);
    let cases: [(&str, String); 9] = [
        ("current absent (today's datg)", state(None, tasks.clone())),
        ("unknown id", state(Some("ar-T0z"), tasks.clone())),
        ("closed task", state(Some("ar-T0e"), tasks.clone())),
        ("parked task", state(Some("ar-T0f"), tasks.clone())),
        ("not a task id", state(Some("ar-G01"), tasks.clone())),
        (
            "excluded base-34 digit",
            state(Some("ar-T0l"), tasks.clone()),
        ),
        (
            "injection attempt",
            state(Some("ar-T0b\nrm -rf /"), tasks.clone()),
        ),
        ("malformed json", "{\"session\": {\"current\": ".to_owned()),
        (
            "current not a string",
            json!({ "session": { "current": 7 }, "tasks": tasks }).to_string(),
        ),
    ];
    for (why, text) in cases {
        assert_eq!(stamp_from_state(&text), None, "{why}");
    }
    let dir = tempdir().unwrap();
    assert_eq!(
        read_stamp(&dir.path().join("missing.json")),
        None,
        "no file"
    );
}

/// `task_tag.py` LOOSE_RE and STRICT_RE, transcribed (Python `(?<!..)` lookbehind
/// dropped; the stamp is tested at the start of the text).
#[test]
fn the_stamp_matches_the_task_tag_grammar() {
    let il = r"[a-z][a-z0-9]{1,7}-T[0-9a-km-np-z]{2}";
    let loose = Regex::new(&format!(
        r"^Task:[ \t]*(?:(?P<il>{il})(?:@(?P<stamp>\d{{4}}))?|T[0-9a-km-np-z]{{2,4}})$"
    ))
    .unwrap();
    let strict = Regex::new(&format!(r"^Task: (?:(?P<il>{il})@(?P<stamp>\d{{4}}))$")).unwrap();
    let tasks = json!([
        task("ar-T0b", "open", Some("2026-11-16")),
        task("trustyt-Tzz", "open", Some("2026-01-31")),
        task("ar-T0d", "open", None),
    ]);
    for id in ["ar-T0b", "trustyt-Tzz"] {
        let stamp = stamp_from_state(&state(Some(id), tasks.clone())).expect("stamped");
        assert!(strict.is_match(&stamp), "STRICT: {stamp}");
        assert!(loose.is_match(&stamp), "LOOSE: {stamp}");
    }
    let bare = stamp_from_state(&state(Some("ar-T0d"), tasks)).expect("stamped");
    assert!(loose.is_match(&bare) && !strict.is_match(&bare), "{bare}");
}

/// Write `<base>/<name>/state.json`, or the legacy file when `name` is empty.
fn put(project: &Path, name: &str) -> std::path::PathBuf {
    let dir = datg_base(project).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.json");
    std::fs::write(&path, "{}").unwrap();
    path
}

#[test]
fn the_session_resolves_like_datg() {
    let none = || -> Option<String> { None };
    // No datg directory at all.
    let empty = tempdir().unwrap();
    assert_eq!(state_path(empty.path(), None, none), None);

    // Legacy single directory; tmux is never asked.
    let legacy = tempdir().unwrap();
    let path = put(legacy.path(), "");
    let asked = Cell::new(false);
    let tmux = || {
        asked.set(true);
        None
    };
    assert_eq!(state_path(legacy.path(), None, tmux), Some(path));
    assert!(!asked.get(), "tmux is only asked when per-PM dirs exist");

    // Per-PM directories: $DATG_SESSION wins, then tmux; a miss is None.
    let multi = tempdir().unwrap();
    let a = put(multi.path(), "pm-a");
    let b = put(multi.path(), "pm-b");
    let env = Some(OsStr::new("pm-a"));
    assert_eq!(
        state_path(multi.path(), env, || Some("pm-b".into())),
        Some(a)
    );
    let tmux_b = || Some("pm-b".to_owned());
    assert_eq!(state_path(multi.path(), None, tmux_b), Some(b.clone()));
    let stale_env = Some(OsStr::new("pm-gone"));
    let tmux_b = || Some("pm-b".to_owned());
    assert_eq!(state_path(multi.path(), stale_env, tmux_b), Some(b));
    assert_eq!(state_path(multi.path(), None, none), None, "unresolved");
    let bad = || Some("../pm-a".to_owned());
    assert_eq!(state_path(multi.path(), None, bad), None, "bad name");
}
