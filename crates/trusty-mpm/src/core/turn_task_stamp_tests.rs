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

/// A dbatg `state.json` shaped like the dbatg MCP's own write (#11398): the
/// `trusty-architect` `State` — `code`, `v`, `config`, `goals`, `items`,
/// `tasks` (no `created`), `next` — with `session.task` free text.
fn dbatg_state(session_task: Option<&str>, tasks: serde_json::Value) -> String {
    let mut session = json!({
        "written_at": "2026-11-19T01:40Z",
        "written_epoch": 1_795_052_400,
        "next": "dolor sit",
    });
    if let Some(t) = session_task {
        session["task"] = json!(t);
    }
    json!({
        "code": "tt", "v": 9, "config": {"mode": "warn"},
        "goals": [], "items": [], "tasks": tasks,
        "next": {"G": "02", "D": "01", "A": "01", "T": "0d"},
        "session": session,
    })
    .to_string()
}

/// A dbatg task record: no `created`.
fn dbatg_task(id: &str, status: &str) -> serde_json::Value {
    json!({ "id": id, "goal": "tt-G01", "title": "t", "status": status, "to": "", "outcome": "" })
}

#[test]
fn a_dbatg_session_task_is_stamped() {
    let tasks = json!([
        dbatg_task("tt-T0b", "open"),
        dbatg_task("tt-T0c", "running")
    ]);
    let cases = [
        ("tt-T0b", "Task: tt-T0b"),
        ("tt-T0c: fix the hook", "Task: tt-T0c"),
        ("  tt-T0b - wiring the stamp", "Task: tt-T0b"),
    ];
    for (line, want) in cases {
        let got = dbatg_stamp_from_state(&dbatg_state(Some(line), tasks.clone()));
        assert_eq!(got.as_deref(), Some(want), "{line}");
    }
}

#[test]
fn each_dbatg_fail_open_case_is_silent() {
    let tasks = json!([dbatg_task("tt-T0b", "open"), dbatg_task("tt-T0c", "done")]);
    let cases = [
        ("no task line", dbatg_state(None, tasks.clone())),
        (
            "free text only",
            dbatg_state(Some("waiting on the owner"), tasks.clone()),
        ),
        (
            "id not leading",
            dbatg_state(Some("after tt-T0b lands"), tasks.clone()),
        ),
        (
            "id with a suffix",
            dbatg_state(Some("tt-T0bx"), tasks.clone()),
        ),
        ("done task", dbatg_state(Some("tt-T0c"), tasks.clone())),
        ("unknown task", dbatg_state(Some("tt-T0d"), tasks.clone())),
        (
            "no tasks array",
            json!({"session": {"task": "tt-T0b"}}).to_string(),
        ),
        ("malformed", "{not json".to_owned()),
    ];
    for (why, text) in cases {
        assert_eq!(dbatg_stamp_from_state(&text), None, "{why}");
    }
}

/// Write `<project>/.trusty-mpm/<root>/<session>/state.json` with `text`.
fn put_text(project: &Path, root: &str, session: &str, text: &str) {
    let dir = project.join(".trusty-mpm").join(root).join(session);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("state.json"), text).unwrap();
}

#[test]
fn the_dbatg_session_resolves_like_dbatg_own_session() {
    let none = || -> Option<String> { None };
    let project = tempdir().unwrap();
    assert_eq!(dbatg_state_path(project.path(), None, none), None);
    put_text(project.path(), "dbatg", "pm-a", "{}");
    put_text(project.path(), "dbatg", "pm-b", "{}");
    let base = project.path().join(".trusty-mpm/dbatg");
    let env = Some(OsStr::new("pm-a"));
    let got = dbatg_state_path(project.path(), env, || Some("pm-b".into()));
    assert_eq!(got, Some(base.join("pm-a/state.json")));
    let got = dbatg_state_path(project.path(), None, || Some("pm-b".into()));
    assert_eq!(got, Some(base.join("pm-b/state.json")));
    let bad = || Some("../pm-a".to_owned());
    assert_eq!(dbatg_state_path(project.path(), None, bad), None);
}

#[test]
fn dbatg_wins_over_datg_when_both_exist() {
    let project = tempdir().unwrap();
    let dbatg = dbatg_state(Some("tt-T0b: now"), json!([dbatg_task("tt-T0b", "open")]));
    let datg = state(
        Some("tt-T0c"),
        json!([task("tt-T0c", "open", Some("2026-11-02"))]),
    );
    put_text(project.path(), "dbatg", "pm-a", &dbatg);
    put_text(project.path(), "datg", "pm-a", &datg);
    let env = Some(OsStr::new("pm-a"));
    let got = resolve_stamp(project.path(), env, env, || None);
    assert_eq!(got.as_deref(), Some("Task: tt-T0b"));
    // The datg store alone still answers (the fallback).
    let only_datg = tempdir().unwrap();
    put_text(only_datg.path(), "datg", "pm-a", &datg);
    let got = resolve_stamp(only_datg.path(), env, env, || None);
    assert_eq!(got.as_deref(), Some("Task: tt-T0c@1102"));
}

#[test]
fn a_dbatg_store_without_a_task_never_falls_back_to_datg() {
    let project = tempdir().unwrap();
    let dbatg = dbatg_state(Some("waiting on the owner"), json!([]));
    let datg = state(
        Some("tt-T0c"),
        json!([task("tt-T0c", "open", Some("2026-11-02"))]),
    );
    put_text(project.path(), "dbatg", "pm-a", &dbatg);
    put_text(project.path(), "datg", "pm-a", &datg);
    let env = Some(OsStr::new("pm-a"));
    assert_eq!(resolve_stamp(project.path(), env, env, || None), None);
    // Neither store: nothing.
    let bare = tempdir().unwrap();
    assert_eq!(resolve_stamp(bare.path(), env, env, || None), None);
}
