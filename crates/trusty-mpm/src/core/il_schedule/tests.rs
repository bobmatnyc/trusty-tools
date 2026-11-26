//! Tests for the IL schedule preset and its writer (#11001).

use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;

use super::fields::{SESSION_KEYS, THRESHOLDS, TOP_LEVEL};
use super::*;

/// A temp Architect dir, its schedule path, and a real project directory.
fn fixture() -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let project = tmp.path().join("proj");
    std::fs::create_dir(&project).expect("project dir");
    let file = path(tmp.path());
    (tmp, file, project)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read schedule")
}

#[test]
fn the_preset_parses_and_has_one_example() {
    let preset = Schedule::preset();
    assert!(
        preset.sessions.is_empty(),
        "readers act on sessions; the preset enrolls none"
    );
    let example = preset
        .session_example
        .expect("the preset carries a session example");
    assert!(!example.name.is_empty() && !example.project.is_empty());
    assert_eq!(preset.jobs.len(), 3);
    assert!(preset.extra.is_empty(), "{:?}", preset.extra);
}

#[test]
fn every_preset_key_is_documented() {
    let raw: serde_json::Value = serde_json::from_str(PRESET).expect("preset JSON");
    let documented = |table: &[(&str, &str)], key: &str| table.iter().any(|(k, _)| *k == key);
    for key in raw.as_object().expect("object").keys() {
        assert!(
            documented(TOP_LEVEL, key),
            "top-level {key} has no docs row"
        );
    }
    for key in raw["thresholds"].as_object().expect("thresholds").keys() {
        assert!(
            documented(THRESHOLDS, key),
            "threshold {key} has no docs row"
        );
    }
    let example = raw["session_example"].as_object().expect("example");
    for key in example.keys() {
        assert!(
            documented(SESSION_KEYS, key),
            "session field {key} has no docs row"
        );
    }
    for key in example["thresholds"].as_object().expect("overrides").keys() {
        assert!(
            documented(THRESHOLDS, key),
            "override {key} has no docs row"
        );
    }
}

#[test]
fn show_of_an_absent_file_is_the_preset() {
    let (_tmp, file, _) = fixture();
    let loaded = load(&file).expect("load");
    assert!(!loaded.from_file);
    assert_eq!(loaded.text, PRESET);
    assert!(!file.exists(), "show writes nothing");
}

#[test]
fn a_malformed_file_is_refused_and_left_unchanged() {
    let (_tmp, file, project) = fixture();
    std::fs::create_dir_all(file.parent().expect("parent")).expect("local/");
    std::fs::write(&file, "{\"sessions\": [{\"name\": \"x\"}]}").expect("write");
    let before = read(&file);
    assert!(matches!(load(&file), Err(IlScheduleError::Parse { .. })));
    let err = update(&file, |s| s.enroll("tm-a", &project, None)).expect_err("refused");
    assert!(err.to_string().contains("reset --force"), "{err}");
    assert_eq!(read(&file), before);
}

#[test]
fn enroll_then_set_then_remove_round_trips() {
    let (_tmp, file, project) = fixture();
    let out = update(&file, |s| s.enroll("tm-a", &project, Some("ta"))).expect("enroll");
    assert!(out.changed, "{out:?}");
    update(&file, |s| s.set(Some("tm-a"), "signals", "s2,S1,S2")).expect("signals");
    update(&file, |s| {
        s.set(Some("tm-a"), "thresholds.floor_ctx", "180000")
    })
    .expect("override");
    update(&file, |s| s.set(None, "thresholds.bar_ctx", "260000")).expect("global");
    let v: serde_json::Value = serde_json::from_str(&read(&file)).expect("JSON");
    let canon = std::fs::canonicalize(&project).expect("canon");
    assert_eq!(
        v["sessions"],
        json!([{"name": "tm-a", "project": canon.to_str().expect("utf8"), "code": "ta",
                "signals": ["S1", "S2"], "thresholds": {"floor_ctx": 180000}}])
    );
    assert_eq!(v["thresholds"]["bar_ctx"], 260_000);
    assert_eq!(
        v["thresholds"]["floor_ctx"], 150_000,
        "the global value is untouched"
    );
    update(&file, |s| s.remove("tm-a")).expect("remove");
    let after: Schedule = serde_json::from_str(&read(&file)).expect("parse");
    assert!(after.sessions.is_empty());
    assert!(matches!(
        update(&file, |s| s.remove("tm-a")),
        Err(IlScheduleError::UnknownSession(_))
    ));
}

#[test]
fn enroll_twice_is_unchanged_and_a_conflict_is_refused() {
    let (tmp, file, project) = fixture();
    update(&file, |s| s.enroll("tm-a", &project, None)).expect("enroll");
    let before = read(&file);
    let again = update(&file, |s| s.enroll("tm-a", &project, None)).expect("again");
    assert!(!again.changed, "{again:?}");
    let other = tmp.path().join("other");
    std::fs::create_dir(&other).expect("other");
    let err = update(&file, |s| s.enroll("tm-a", &other, None)).expect_err("conflict");
    assert!(
        matches!(err, IlScheduleError::AlreadyEnrolled { .. }),
        "{err}"
    );
    assert_eq!(read(&file), before);
}

#[test]
fn enroll_refuses_a_missing_project() {
    let (tmp, file, _) = fixture();
    let missing = tmp.path().join("nope");
    let err = update(&file, |s| s.enroll("tm-a", &missing, None)).expect_err("missing");
    assert!(
        matches!(err, IlScheduleError::BadValue { ref key, .. } if key == "project"),
        "{err}"
    );
    assert!(!file.exists(), "a refused first edit creates no file");
}

#[test]
fn set_validates_keys_and_values() {
    let (_tmp, _file, project) = fixture();
    let mut s = Schedule::preset();
    s.enroll("tm-a", &project, None).expect("enroll");
    let refused = [
        (None, "code", "ab"),
        (None, "thresholds.nope", "1"),
        (None, "thresholds.floor_ctx", "-1"),
        (None, "thresholds.stale_pct", "101"),
        (Some("tm-a"), "enabled", "true"),
        (Some("tm-a"), "signals", "S1,S9"),
        (Some("tm-a"), "code", "x"),
        (Some("tm-a"), "state_path", "relative/state.json"),
        (Some("tm-a"), "project", "/definitely/not/here/11001"),
        (Some("tm-b"), "code", "ab"),
    ];
    let before = s.clone();
    for (session, key, value) in refused {
        assert!(
            s.set(session, key, value).is_err(),
            "{session:?} {key}={value} accepted"
        );
    }
    assert_eq!(s, before, "a refused set changes nothing");
    let same = s
        .set(None, "thresholds.floor_ctx", "150000")
        .expect("same value");
    assert!(!same.changed);
}

#[test]
fn a_refused_edit_leaves_the_file_byte_identical() {
    let (_tmp, file, project) = fixture();
    update(&file, |s| s.enroll("tm-a", &project, None)).expect("enroll");
    let before = read(&file);
    assert!(update(&file, |s| s.set(Some("tm-a"), "thresholds.x", "1")).is_err());
    assert_eq!(read(&file), before);
}

#[cfg(unix)]
#[test]
fn a_write_failure_is_an_error_and_the_file_is_unchanged() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, file, project) = fixture();
    update(&file, |s| s.enroll("tm-a", &project, None)).expect("enroll");
    let before = read(&file);
    let local = file.parent().expect("local/").to_path_buf();
    std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let result = update(&file, |s| s.set(None, "thresholds.bar_ctx", "1"));
    std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
    let err = result.expect_err("a read-only local/ cannot be written");
    assert!(matches!(err, IlScheduleError::Write { .. }), "{err}");
    assert_eq!(read(&file), before);
}

#[test]
fn an_edit_keeps_unknown_keys_and_the_jobs() {
    let (_tmp, file, project) = fixture();
    std::fs::create_dir_all(file.parent().expect("parent")).expect("local/");
    let mut v: serde_json::Value = serde_json::from_str(PRESET).expect("preset");
    v["schema_version"] = json!(1);
    v["sessions"] = json!([{"name": "tm-old", "project": "/x", "enabled": false}]);
    std::fs::write(&file, v.to_string()).expect("write");
    update(&file, |s| s.enroll("tm-a", &project, None)).expect("enroll");
    let after: serde_json::Value = serde_json::from_str(&read(&file)).expect("JSON");
    assert_eq!(after["schema_version"], 1);
    assert_eq!(after["sessions"][0]["enabled"], false);
    assert_eq!(after["jobs"], v["jobs"]);
    assert_eq!(after["session_example"], v["session_example"]);
}

#[test]
fn reset_refuses_an_edited_file_without_force() {
    let (_tmp, file, project) = fixture();
    update(&file, |s| s.enroll("tm-a", &project, None)).expect("enroll");
    let before = read(&file);
    let err = reset(&file, false).expect_err("edited");
    assert!(
        matches!(err, IlScheduleError::Edited { sessions: 1, .. }),
        "{err}"
    );
    assert_eq!(read(&file), before);
}

#[test]
fn reset_force_restores_the_preset_bytes() {
    let (_tmp, file, _) = fixture();
    assert!(
        reset(&file, false).expect("absent").changed,
        "absent file: written"
    );
    assert!(!reset(&file, false).expect("preset").changed);
    std::fs::write(&file, "not json").expect("corrupt");
    assert!(reset(&file, true).expect("force").changed);
    assert_eq!(read(&file), PRESET);
}

#[test]
fn the_documented_commands_rebuild_the_example() {
    let (_tmp, _file, project) = fixture();
    let example = Schedule::preset().session_example.expect("example");
    let mut s = Schedule::preset();
    for line in example_commands(&example) {
        let words: Vec<&str> = line.split_whitespace().collect();
        let (verb, rest) = (words[3], &words[4..]);
        let flag = |name: &str| rest.iter().position(|w| *w == name).map(|i| rest[i + 1]);
        match verb {
            // The example's project is illustrative; replay it on a real dir.
            "enroll" => s.enroll(rest[0], &project, flag("--code")),
            "set" => s.set(flag("--session"), rest[2], rest[3]),
            other => panic!("unexpected verb {other} in {line}"),
        }
        .unwrap_or_else(|e| panic!("{line}: {e}"));
    }
    let mut built = s.sessions.pop().expect("one session");
    built.project.clone_from(&example.project);
    assert_eq!(built, example);
}

#[test]
fn the_rendered_docs_name_every_threshold_and_the_example() {
    let docs = render_docs();
    for (key, _) in THRESHOLDS {
        assert!(docs.contains(&format!("| `{key}` |")), "{key}");
    }
    assert!(docs.contains("tm il config enroll tm-my-project"), "{docs}");
}
