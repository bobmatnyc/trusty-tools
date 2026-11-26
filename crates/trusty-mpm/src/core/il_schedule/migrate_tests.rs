//! Tests for the IL schedule's preset versions, `diff` and `upgrade` (#11002).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use tempfile::TempDir;

use super::migrate::merge_maps;
use super::*;

/// The version-0 preset: the bytes every pre-#11002 install was seeded with.
const V0: &str = include_str!("presets/v0.json");

fn fixture() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let file = path(tmp.path());
    std::fs::create_dir_all(file.parent().expect("local/")).expect("local dir");
    (tmp, file)
}

fn json_of(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().expect("object").clone()
}

/// A v0 file as an owner left it: one enrolled session, an edited and a
/// removed threshold, an edited pause list and a hand-added key.
fn edited_v0(project: &Path) -> String {
    let mut doc: Value = serde_json::from_str(V0).expect("v0");
    doc["sessions"] = json!([{"name": "tm-a", "project": project, "code": "aa"}]);
    doc["thresholds"]["floor_ctx"] = json!(180_000);
    doc["thresholds"]
        .as_object_mut()
        .expect("thresholds")
        .remove("guard_ctx");
    doc["paused_sessions"] = json!(["tm-b"]);
    doc["owner_note"] = json!("keep me");
    serde_json::to_string_pretty(&doc).expect("encode") + "\n"
}

#[test]
fn every_preset_version_parses_and_names_its_version() {
    let v0: Value = serde_json::from_str(V0).expect("v0 parses");
    assert!(v0.get("schema_version").is_none(), "version 0 has no key");
    let current: Value = serde_json::from_str(PRESET).expect("preset parses");
    assert_eq!(current["schema_version"], json!(SCHEMA_VERSION));
    assert_eq!(Schedule::preset().schema_version, Some(SCHEMA_VERSION));
}

#[test]
fn merge_takes_preset_changes_and_keeps_edits() {
    let base = obj(json!({"a": 1, "b": 1, "c": {"x": 1}, "d": 1, "e": 1, "s": []}));
    let file = obj(json!({"a": 1, "b": 5, "c": {"x": 1}, "d": 1, "s": ["mine"], "u": 1}));
    let new = obj(json!({"a": 2, "b": 2, "c": {"x": 2, "y": 3}, "e": 1, "s": [], "n": 9}));
    let mut deltas = Vec::new();
    let merged = merge_maps("", &base, &file, &new, &mut deltas);
    // Preset changes: a changed, c.x changed, c.y and n added, d removed.
    // Edits kept: b's value, s's entries, u's hand-added key, e's deletion.
    assert_eq!(
        Value::Object(merged),
        json!({"a": 2, "b": 5, "c": {"x": 2, "y": 3}, "s": ["mine"], "u": 1, "n": 9})
    );
    let kinds: Vec<(&str, DeltaKind)> = deltas.iter().map(|d| (d.path.as_str(), d.kind)).collect();
    assert_eq!(
        kinds,
        [
            ("a", DeltaKind::Preset),
            ("b", DeltaKind::Edit),
            ("c.x", DeltaKind::Preset),
            ("c.y", DeltaKind::Preset),
            ("d", DeltaKind::Preset),
            ("s", DeltaKind::Edit),
            ("u", DeltaKind::Edit),
            ("e", DeltaKind::Edit),
            ("n", DeltaKind::Preset),
        ]
    );
}

/// Issue #11002 AC4: an edited file survives an update, and the update still
/// brings in what the new preset added.
#[test]
fn an_edited_v0_file_survives_an_update_and_takes_the_preset_changes() {
    let (tmp, file) = fixture();
    let original = edited_v0(tmp.path());
    std::fs::write(&file, &original).expect("seed v0");

    let outcome = upgrade(&file).expect("update");
    assert!(outcome.changed, "{outcome:?}");
    assert!(
        outcome.message.contains("schema_version 0 -> 1")
            && outcome.message.contains("kept 5 edit(s)"),
        "{}",
        outcome.message
    );

    let got = json_of(&file);
    assert_eq!(got["schema_version"], json!(1));
    // Every edit survives.
    assert_eq!(got["sessions"][0]["name"], json!("tm-a"));
    assert_eq!(got["sessions"][0]["code"], json!("aa"));
    assert_eq!(got["thresholds"]["floor_ctx"], json!(180_000));
    assert!(
        got["thresholds"].get("guard_ctx").is_none(),
        "a deletion is an edit"
    );
    assert_eq!(got["paused_sessions"], json!(["tm-b"]));
    assert_eq!(got["owner_note"], json!("keep me"));
    // The preset change (the v1 session example) arrives; untouched keys stay.
    let preset = Schedule::preset();
    let example = serde_json::to_value(preset.session_example).expect("example");
    assert_eq!(got["session_example"], example);
    assert_eq!(got["thresholds"]["stale_pct"], json!(70));
    assert_eq!(
        got["jobs"],
        serde_json::to_value(&preset.jobs).expect("jobs")
    );
    // The result is a schedule the CLI edits, and the backup is the original.
    load(&file).expect("the updated file loads");
    let bak = file.with_file_name("il-schedule.json.v0.bak");
    assert_eq!(std::fs::read_to_string(bak).expect("backup"), original);
}

#[test]
fn an_update_is_idempotent() {
    let (tmp, file) = fixture();
    std::fs::write(&file, edited_v0(tmp.path())).expect("seed v0");
    upgrade(&file).expect("first update");
    let after = std::fs::read(&file).expect("read");
    let again = upgrade(&file).expect("second update");
    assert!(!again.changed, "{again:?}");
    assert_eq!(std::fs::read(&file).expect("read"), after);
}

#[test]
fn an_update_of_an_unedited_v0_file_is_the_preset() {
    let (_tmp, file) = fixture();
    std::fs::write(&file, V0).expect("seed v0");
    let outcome = upgrade(&file).expect("update");
    assert!(
        outcome.message.contains("kept 0 edit(s)"),
        "{}",
        outcome.message
    );
    assert_eq!(load(&file).expect("load").schedule, Schedule::preset());
}

#[test]
fn an_update_of_an_absent_file_writes_the_preset() {
    let (_tmp, file) = fixture();
    assert!(upgrade(&file).expect("update").changed);
    assert_eq!(std::fs::read_to_string(&file).expect("read"), PRESET);
}

#[test]
fn a_failed_backup_aborts_the_update_and_the_file_is_unchanged() {
    let (tmp, file) = fixture();
    let original = edited_v0(tmp.path());
    std::fs::write(&file, &original).expect("seed v0");
    // A directory where the backup goes makes the backup write fail.
    std::fs::create_dir(file.with_file_name("il-schedule.json.v0.bak")).expect("block");
    let err = upgrade(&file).expect_err("the backup must fail");
    assert!(matches!(err, IlScheduleError::Write { .. }), "{err}");
    assert_eq!(std::fs::read_to_string(&file).expect("read"), original);
}

#[test]
fn diff_lists_preset_changes_and_edits() {
    let (tmp, file) = fixture();
    std::fs::write(&file, edited_v0(tmp.path())).expect("seed v0");
    let d = diff(&file).expect("diff");
    assert_eq!(d.file_version, 0);
    let kind = |p: &str| d.deltas.iter().find(|x| x.path == p).map(|x| x.kind);
    assert_eq!(kind("session_example"), Some(DeltaKind::Preset));
    assert_eq!(kind("thresholds.floor_ctx"), Some(DeltaKind::Edit));
    assert_eq!(kind("thresholds.guard_ctx"), Some(DeltaKind::Edit));
    assert_eq!(kind("sessions"), Some(DeltaKind::Edit));
    assert_eq!(
        kind("thresholds.stale_pct"),
        None,
        "an equal key is no delta"
    );
    let text = d.render(&file);
    assert!(
        text.contains("edit  thresholds.floor_ctx: 180000 (preset: 150000)"),
        "{text}"
    );
    assert!(
        text.contains("apply session_example: absent -> {"),
        "{text}"
    );
    assert!(text.contains("schema_version 0 -> 1"), "{text}");
    assert!(
        text.contains("1 preset change(s)") && text.contains("5 edit(s)"),
        "{text}"
    );
}

#[test]
fn diff_of_the_current_preset_is_empty() {
    let (_tmp, file) = fixture();
    let absent = diff(&file).expect("diff of an absent file");
    assert!(!absent.from_file && absent.deltas.is_empty());
    std::fs::write(&file, PRESET).expect("seed preset");
    let d = diff(&file).expect("diff");
    assert!(d.from_file && d.deltas.is_empty(), "{:?}", d.deltas);
    assert!(d.render(&file).contains("no differences"));
}

#[test]
fn a_newer_schema_is_refused_and_the_file_is_unchanged() {
    let (tmp, file) = fixture();
    let newer = PRESET.replacen("\"schema_version\": 1", "\"schema_version\": 2", 1);
    std::fs::write(&file, &newer).expect("seed v2");
    let too_new = |e: IlScheduleError| matches!(e, IlScheduleError::SchemaTooNew { found: 2, .. });
    assert!(too_new(diff(&file).expect_err("diff")));
    assert!(too_new(upgrade(&file).expect_err("update")));
    let project = tmp.path().to_path_buf();
    let edit = update(&file, |s| s.enroll("tm-a", &project, None));
    assert!(too_new(edit.expect_err("enroll")));
    assert_eq!(std::fs::read_to_string(&file).expect("read"), newer);
}

/// The preset at `schema_version` 2, written to `file`.
fn seed_v2(file: &Path) -> String {
    let newer = PRESET.replacen("\"schema_version\": 1", "\"schema_version\": 2", 1);
    assert_ne!(newer, PRESET, "the preset names schema_version 1");
    std::fs::write(file, &newer).expect("seed v2");
    newer
}

/// #11002: `--force` must not downgrade a newer file to this preset.
#[test]
fn reset_force_refuses_a_newer_schema_and_the_file_is_unchanged() {
    let (_tmp, file) = fixture();
    let newer = seed_v2(&file);
    let err = reset(&file, true).expect_err("newer schema");
    assert!(
        matches!(err, IlScheduleError::SchemaTooNew { found: 2, .. }),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&file).expect("read"), newer);
}

/// #11002: without `--force` the refusal is the version, not an edit.
#[test]
fn reset_refuses_a_newer_schema_without_force() {
    let (_tmp, file) = fixture();
    let newer = seed_v2(&file);
    let err = reset(&file, false).expect_err("newer schema");
    assert!(
        matches!(err, IlScheduleError::SchemaTooNew { found: 2, .. }),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&file).expect("read"), newer);
}

/// #11002: the version check skips text it cannot read, so `--force` repairs it.
#[test]
fn reset_force_still_repairs_a_malformed_file() {
    let (_tmp, file) = fixture();
    for bad in [
        "not json",
        "{\"schema_version\": 1, \"sessions\": 7}",
        "[2]",
    ] {
        std::fs::write(&file, bad).expect("seed malformed");
        assert!(reset(&file, true).expect("force").changed, "{bad}");
        assert_eq!(std::fs::read_to_string(&file).expect("read"), PRESET);
    }
}

/// The CLI edits keep the file's `schema_version`.
#[test]
fn an_edit_keeps_the_schema_version() {
    let (tmp, file) = fixture();
    std::fs::write(&file, PRESET).expect("seed preset");
    let project = tmp.path().to_path_buf();
    update(&file, |s| s.enroll("tm-a", &project, None)).expect("enroll");
    assert_eq!(json_of(&file)["schema_version"], json!(SCHEMA_VERSION));
}

/// The `.v0.bak` path [`upgrade`] backs a v0 `file` up to.
fn backup_of(file: &Path) -> PathBuf {
    file.with_file_name("il-schedule.json.v0.bak")
}

/// #11002: a file `Schedule` refuses is a parse error before any write.
#[test]
fn a_parse_failure_aborts_the_update_and_the_file_is_unchanged() {
    let (_tmp, file) = fixture();
    let original = "{\"sessions\": 7}\n";
    std::fs::write(&file, original).expect("seed unparsable v0");
    let err = upgrade(&file).expect_err("the parse must fail");
    assert!(matches!(err, IlScheduleError::Parse { .. }), "{err}");
    assert_eq!(std::fs::read_to_string(&file).expect("read"), original);
    assert_eq!(json_of(&file).get("schema_version"), None);
    assert!(!backup_of(&file).exists(), "no backup before validation");
}

/// #11002: a failed publish is an error and leaves the file at its version.
#[cfg(unix)] // the symlink below is Unix-only
#[test]
fn a_failed_write_aborts_the_update_and_the_file_is_unchanged() {
    let (tmp, file) = fixture();
    let original = edited_v0(tmp.path());
    // `write_atomic` refuses a symlink target; the read and the backup follow it.
    let real = file.with_file_name("real-il-schedule.json");
    std::fs::write(&real, &original).expect("seed v0");
    std::os::unix::fs::symlink(&real, &file).expect("link");
    let err = upgrade(&file).expect_err("the write must fail");
    assert!(
        matches!(&err, IlScheduleError::Write { path, .. } if *path == file),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&real).expect("read"), original);
    assert_eq!(json_of(&file).get("schema_version"), None);
    assert!(std::fs::symlink_metadata(&file).expect("meta").is_symlink());
}

/// #11002: a lock that cannot be taken is an error before any read or write.
#[test]
fn a_failed_lock_aborts_the_update_and_the_file_is_unchanged() {
    let (tmp, file) = fixture();
    let original = edited_v0(tmp.path());
    std::fs::write(&file, &original).expect("seed v0");
    // A directory at the lock sidecar makes the lock open fail.
    std::fs::create_dir(trusty_common::file_lock::lock_path(&file)).expect("block");
    let err = upgrade(&file).expect_err("the lock must fail");
    assert!(matches!(err, IlScheduleError::Write { .. }), "{err}");
    assert_eq!(std::fs::read_to_string(&file).expect("read"), original);
    assert_eq!(json_of(&file).get("schema_version"), None);
    assert!(!backup_of(&file).exists(), "no backup without the lock");
}
