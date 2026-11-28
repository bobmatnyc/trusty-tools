//! Tests for the #12127 launch keys: the record, the precedence rules, the
//! prep step and its fail-closed write.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::super::tests::EnvVarGuard;
use super::*;
use crate::core::claude_sandbox::LAUNCH_KEYS_CLEANUP_MARKER_FILE;

fn write_text(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
}

fn hash(path: &Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(path).expect("read")).to_vec()
}

const FOUR: [&str; 4] = [
    "outputStyle",
    "model",
    "autoMemoryEnabled",
    "skillOverrides",
];

/// The skills the table turns off for a Rust project.
fn rust_skills_off() -> BTreeSet<&'static str> {
    crate::core::skill_overrides::irrelevant_skills(&BTreeSet::from(["rust-engineer".to_owned()]))
}

/// #12127: a record written for a dir reads back for it, under the harness
/// dir; a dir with none reads `None`.
#[test]
fn a_record_round_trips_through_its_dir_12127() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let dir = tmp.path().join("ws");
    std::fs::create_dir_all(&dir).expect("mkdir");
    assert_eq!(read_launch_keys(&dir).expect("no record"), None);
    let keys = launch_keys_for(&dir, "trusty-mpm", SessionProfile::Pm, None);
    let path = write_launch_keys(&dir, &keys).expect("the record writes");
    // The record sits under the canonical dir's harness (`/tmp` is a link on macOS).
    let canonical = std::fs::canonicalize(&dir).expect("canonical");
    assert!(path.starts_with(crate::core::harness_root::harness_dir(&canonical)));
    assert_eq!(path, record_path(&dir));
    assert_eq!(read_launch_keys(&dir).expect("readable"), Some(keys));
}

/// #12127 (b): with no user value in either file, each key carries the value
/// the old project-tier write set; only an Architect gets `model`.
#[test]
fn the_record_carries_each_key_with_the_old_value_12127() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let off = BTreeSet::from(["xlsx", "pdf"]);
    let architect = launch_keys_for(tmp.path(), "tm-arch", SessionProfile::Architect, Some(&off));
    assert_eq!(
        Value::Object(architect),
        json!({
            "outputStyle": "tm-arch",
            "autoMemoryEnabled": false,
            "model": ARCHITECT_MODEL,
            "skillOverrides": {"pdf": "off", "xlsx": "off"},
        })
    );
    let pm = launch_keys_for(tmp.path(), "trusty-mpm", SessionProfile::Pm, None);
    assert_eq!(
        Value::Object(pm),
        json!({"outputStyle": "trusty-mpm", "autoMemoryEnabled": false}),
        "a partial detection delivers no skillOverrides; a PM gets no model"
    );
}

/// #12127: where the old write let a user's value win, the flag tier stays
/// out of its way; where the old write overwrote the project file, it does not.
#[test]
fn a_user_value_keeps_winning_where_it_used_to_12127() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let ws = tmp.path();
    let claude = ws.join(".claude");
    let off = BTreeSet::from(["xlsx", "pdf", "docx"]);
    write_text(
        &claude.join("settings.local.json"),
        r#"{"outputStyle": "mine", "autoMemoryEnabled": true, "skillOverrides": {"xlsx": "on"}}"#,
    );
    write_text(
        &claude.join("settings.json"),
        r#"{"model": "sonnet", "skillOverrides": {"pdf": "on"}}"#,
    );
    let keys = launch_keys_for(ws, "trusty-mpm", SessionProfile::Architect, Some(&off));
    assert_eq!(
        Value::Object(keys),
        json!({"skillOverrides": {"docx": "off"}})
    );

    // A project-file style alone was overwritten before; the flag tier wins too.
    write_text(&claude.join("settings.local.json"), "{}");
    write_text(
        &claude.join("settings.json"),
        r#"{"outputStyle": "theirs"}"#,
    );
    let keys = launch_keys_for(ws, "trusty-mpm", SessionProfile::Pm, Some(&off));
    assert_eq!(keys["outputStyle"], json!("trusty-mpm"));

    // A `skillOverrides` that is not an object: the old write skipped the file.
    write_text(
        &claude.join("settings.json"),
        r#"{"skillOverrides": "off"}"#,
    );
    let keys = launch_keys_for(ws, "trusty-mpm", SessionProfile::Pm, Some(&off));
    assert!(keys.get("skillOverrides").is_none(), "{keys:?}");
}

/// #12127 (a)+(c): a default launch leaves `settings.local.json`
/// byte-identical, writes none of the four keys into the project file or the
/// home `~/.claude/settings.json`, removes the values an earlier release left
/// and keeps the user's, and records the keys for the launch.
///
/// The project and home files are not hashed whole: the hooks, `enabledPlugins`
/// and `statusLine` writers (#12077 rows 5, 7, 8, 10) still write them until
/// parts 4b to 4d land.
#[test]
#[serial_test::serial]
fn prepare_session_writes_none_of_the_four_keys_12127() {
    let tmp_home = crate::test_support::hermetic_temp_dir();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = crate::test_support::hermetic_temp_dir();
    let project = tmp.path();
    std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"demo\"\n").expect("marker");
    let tm_skill = *rust_skills_off()
        .iter()
        .next()
        .expect("a Rust project turns one off");
    let claude = project.join(".claude");
    write_text(
        &claude.join("settings.json"),
        &json!({
            "theme": "dark",
            "outputStyle": "trusty-mpm",
            "autoMemoryEnabled": false,
            "skillOverrides": {tm_skill: "off", "my-skill": "on"},
        })
        .to_string(),
    );
    let local = claude.join("settings.local.json");
    write_text(&local, r#"{"spinnerTipsEnabled": false}"#);
    let home_settings = tmp_home.path().join(".claude").join("settings.json");
    write_text(&home_settings, r#"{"theme": "light"}"#);
    let local_before = hash(&local);
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    super::super::prepare_session_with_home(&fw, project, None, false, Some(tmp_home.path()))
        .expect("prep succeeds");

    assert_eq!(
        hash(&local),
        local_before,
        "settings.local.json is never written"
    );
    let file = read_json(&claude.join("settings.json"));
    assert_eq!(file["theme"], json!("dark"));
    assert_eq!(file["skillOverrides"], json!({"my-skill": "on"}), "{file}");
    for key in ["outputStyle", "autoMemoryEnabled", "model"] {
        assert!(
            file.get(key).is_none(),
            "{key} left in the project file: {file}"
        );
    }
    let home = read_json(&home_settings);
    for key in FOUR {
        assert!(
            home.get(key).is_none(),
            "{key} written to ~/.claude: {home}"
        );
    }
    let record = read_launch_keys(project)
        .expect("readable")
        .expect("recorded");
    assert_eq!(record["outputStyle"], json!("trusty-mpm"));
    assert_eq!(record["autoMemoryEnabled"], json!(false));
    assert_eq!(record["skillOverrides"][tm_skill], json!("off"));
    assert!(
        project
            .join(".trusty-mpm")
            .join(LAUNCH_KEYS_CLEANUP_MARKER_FILE)
            .exists()
    );
}

/// #12127 (d), Fail-Open Check: a record that cannot be written refuses the
/// launch with a fatal error, never a warning the caller launches past.
#[test]
#[serial_test::serial]
fn an_unwritable_record_refuses_the_prepared_launch_12127() {
    let tmp_home = crate::test_support::hermetic_temp_dir();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let tmp = crate::test_support::hermetic_temp_dir();
    let project = tmp.path();
    // A file where the record directory goes, so the stage cannot create it.
    let blocker = record_path(project)
        .parent()
        .expect("record dir")
        .to_path_buf();
    write_text(&blocker, "not a directory");
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let err =
        super::super::prepare_session_with_home(&fw, project, None, false, Some(tmp_home.path()))
            .expect_err("an unwritable record must refuse the launch");
    // #12077 row 13: the launch reads the current record first, to keep its
    // excludes, so the blocker now refuses at the read.
    assert!(
        matches!(err, PrepError::LaunchKeys(LaunchKeysError::Read { .. })),
        "{err}"
    );
    assert!(err.is_fatal(), "{err}");
    assert!(err.to_string().contains("launch-settings"), "{err}");
}

/// Plant tm's model record for `project`: `model` written into its settings.
fn plant_model_record(project: &Path, model: &str) {
    let key = project
        .join(".claude")
        .join("settings.json")
        .display()
        .to_string();
    write_text(
        &crate::core::harness_root::harness_dir(project).join("tm-written-model.json"),
        &json!({ key: model }).to_string(),
    );
}

/// #12127 (c): `model` goes only when it equals the value tm's record holds;
/// a different value stays.
#[test]
fn a_model_tm_recorded_is_removed_and_any_other_kept_12127() {
    for (on_disk, removed) in [("opus", true), ("sonnet", false)] {
        let tmp = crate::test_support::hermetic_temp_dir();
        let project = tmp.path();
        plant_model_record(project, "opus");
        let settings = project.join(".claude").join("settings.json");
        write_text(&settings, &json!({ "model": on_disk }).to_string());
        assert_eq!(
            super::super::architect_model::recorded_model(project).expect("readable"),
            Some("opus".to_owned())
        );
        deliver_launch_keys(project, "trusty-mpm", SessionProfile::Pm).expect("delivered");
        let file = read_json(&settings);
        assert_eq!(file.get("model").is_none(), removed, "{on_disk}: {file}");
    }
}

/// #12127 (c): with no record, tm owns no `model`, so it stays.
#[test]
fn an_unrecorded_model_is_kept_12127() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let project = tmp.path();
    let settings = project.join(".claude").join("settings.json");
    write_text(&settings, r#"{"model": "opus"}"#);
    deliver_launch_keys(project, "trusty-mpm", SessionProfile::Architect).expect("delivered");
    assert_eq!(read_json(&settings)["model"], json!("opus"));
    let record = read_launch_keys(project)
        .expect("readable")
        .expect("recorded");
    assert!(
        record.get("model").is_none(),
        "the user's model keeps winning"
    );
}

/// #12127: a cleanup failure never refuses the launch: no marker, and the
/// keys are still recorded.
#[test]
fn the_launch_key_pass_never_refuses_12127() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let project = tmp.path();
    let settings = project.join(".claude").join("settings.json");
    write_text(&settings, "{ not json");
    deliver_launch_keys(project, "trusty-mpm", SessionProfile::Pm).expect("never refused");
    assert_eq!(
        std::fs::read_to_string(&settings).expect("read"),
        "{ not json"
    );
    assert!(
        !project
            .join(".trusty-mpm")
            .join(LAUNCH_KEYS_CLEANUP_MARKER_FILE)
            .exists()
    );
    assert!(read_launch_keys(project).expect("readable").is_some());
}

/// #12077 row 13: a launch rewrites the record, and keeps the
/// `claudeMdExcludes` `tm doctor --fix` stored on it.
#[test]
fn deliver_keeps_the_recorded_claude_md_excludes_12077() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let project = tmp.path();
    set_launch_key(project, "claudeMdExcludes", json!(["/anc/CLAUDE.md"])).expect("recorded");
    deliver_launch_keys(project, "trusty-mpm", SessionProfile::Pm).expect("delivered");
    let record = read_launch_keys(project)
        .expect("readable")
        .expect("recorded");
    assert_eq!(record["claudeMdExcludes"], json!(["/anc/CLAUDE.md"]));
    assert_eq!(record["outputStyle"], json!("trusty-mpm"));
}

/// #12077 row 13, Fail-Open Check: a record that cannot be read refuses the
/// launch and stays as it is, so the excludes on it are never dropped unseen.
#[test]
fn deliver_refuses_over_an_unreadable_record_12077() {
    let tmp = crate::test_support::hermetic_temp_dir();
    let project = tmp.path();
    let record = record_path(project);
    write_text(&record, "{ not json");
    let err = deliver_launch_keys(project, "trusty-mpm", SessionProfile::Pm)
        .expect_err("an unreadable record refuses the launch");
    assert!(
        err.to_string().contains(&record.display().to_string()),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(&record).expect("read"),
        "{ not json"
    );
}
