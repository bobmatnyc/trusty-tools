//! Tests for the #12127 one-time removal of the four launch keys earlier tm
//! releases wrote into `<ws>/.claude/settings.json`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;

/// A canonical temp root with a workspace under it.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    let ws = root.join("ws");
    std::fs::create_dir_all(&ws).expect("mkdir ws");
    (dir, ws)
}

fn settings_path(ws: &Path) -> PathBuf {
    ws.join(".claude/settings.json")
}

fn marker_path(ws: &Path) -> PathBuf {
    ws.join(".trusty-mpm").join(LAUNCH_KEYS_CLEANUP_MARKER_FILE)
}

fn write_text(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
}

/// What tm wrote: the default style, the Opus pin, two skills off.
fn ours() -> TmLaunchKeys {
    TmLaunchKeys {
        output_style: "trusty-mpm".to_owned(),
        model: Some("opus".to_owned()),
        skills_off: Some(BTreeSet::from(["xlsx".to_owned(), "pdf".to_owned()])),
    }
}

/// The project file as an earlier release left it, plus the user's keys.
const SETTINGS: &str = r#"{
  "theme": "dark",
  "outputStyle": "trusty-mpm",
  "autoMemoryEnabled": false,
  "model": "opus",
  "skillOverrides": {"xlsx": "off", "pdf": "on", "my-skill": "off"},
  "hooks": {"Stop": []}
}"#;

/// #12127 (c): each value equal to tm's goes, every other value stays and is
/// reported, and the user's keys and entries are kept.
#[test]
fn matching_launch_keys_are_removed_and_others_kept_12127() {
    let (_tmp, ws) = fixture();
    write_text(&settings_path(&ws), SETTINGS);
    let outcome = clean_tm_launch_keys_once(&ws, &ours()).expect("cleanup");
    let LaunchKeysCleanupOutcome::Cleaned { removed, kept } = outcome else {
        panic!("{outcome:?}");
    };
    let removed: BTreeSet<String> = removed.into_iter().collect();
    let expected: BTreeSet<String> = [
        "autoMemoryEnabled",
        "model",
        "outputStyle",
        "skillOverrides.xlsx",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(removed, expected);
    assert_eq!(kept, vec!["skillOverrides.pdf".to_owned()]);
    assert_eq!(
        read_json(&settings_path(&ws)),
        json!({
            "theme": "dark",
            "skillOverrides": {"pdf": "on", "my-skill": "off"},
            "hooks": {"Stop": []}
        })
    );
    let marker = read_json(&marker_path(&ws));
    assert_eq!(marker["tier"], json!("launch-keys"));
    assert_eq!(marker["kept"], json!(["skillOverrides.pdf"]));

    // A different value at each scalar key stays and is reported.
    let (_tmp, ws) = fixture();
    write_text(
        &settings_path(&ws),
        r#"{"outputStyle": "mine", "autoMemoryEnabled": true, "model": "sonnet"}"#,
    );
    let outcome = clean_tm_launch_keys_once(&ws, &ours()).expect("cleanup");
    let LaunchKeysCleanupOutcome::Cleaned { removed, mut kept } = outcome else {
        panic!("{outcome:?}");
    };
    kept.sort();
    assert!(removed.is_empty(), "{removed:?}");
    assert_eq!(kept, vec!["autoMemoryEnabled", "model", "outputStyle"]);
}

/// #12127 (c): a file holding none of tm's values is never rewritten.
#[test]
fn a_file_with_no_tm_launch_key_is_byte_identical_12127() {
    let (_tmp, ws) = fixture();
    let text = "{\"theme\":\"dark\",\n \"skillOverrides\": {\"pdf\": \"on\"}}";
    write_text(&settings_path(&ws), text);
    clean_tm_launch_keys_once(&ws, &ours()).expect("cleanup");
    assert_eq!(
        std::fs::read_to_string(settings_path(&ws)).expect("read"),
        text
    );
    assert!(marker_path(&ws).exists());
}

/// #12127 (c): the second launch reads nothing and writes nothing, even when
/// a matching value comes back.
#[test]
fn a_second_launch_key_pass_writes_nothing_12127() {
    let (_tmp, ws) = fixture();
    write_text(&settings_path(&ws), SETTINGS);
    clean_tm_launch_keys_once(&ws, &ours()).expect("first");
    write_text(&settings_path(&ws), SETTINGS);
    let outcome = clean_tm_launch_keys_once(&ws, &ours()).expect("second");
    assert_eq!(outcome, LaunchKeysCleanupOutcome::AlreadyDone);
    assert_eq!(
        std::fs::read_to_string(settings_path(&ws)).expect("read"),
        SETTINGS
    );
}

/// #12127 (c), Fail-Open Check: a parse failure is an error naming the file,
/// leaves the file and no marker, and the next launch retries.
#[test]
fn a_launch_key_parse_failure_writes_no_marker_12127() {
    let (_tmp, ws) = fixture();
    write_text(&settings_path(&ws), "{ not json");
    let err = clean_tm_launch_keys_once(&ws, &ours()).expect_err("a parse failure is an error");
    assert!(
        err.to_string()
            .contains(&settings_path(&ws).display().to_string()),
        "{err}"
    );
    assert!(!marker_path(&ws).exists());
    write_text(&settings_path(&ws), SETTINGS);
    let outcome = clean_tm_launch_keys_once(&ws, &ours()).expect("the retry runs");
    assert!(
        matches!(outcome, LaunchKeysCleanupOutcome::Cleaned { .. }),
        "{outcome:?}"
    );
    assert!(marker_path(&ws).exists());
}

/// Make `dir` read-only; `false` on a root runner, which ignores the mode.
#[cfg(unix)]
fn read_only(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    if std::fs::write(dir.join("probe"), b"x").is_ok() {
        eprintln!("skipped: this runner can write a read-only directory");
        return false;
    }
    true
}

#[cfg(unix)]
fn writable(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// #12127 (c), Fail-Open Check: with the lock taken, a rewrite that cannot be
/// published is an error, the file is intact and no marker is written.
#[cfg(unix)]
#[test]
fn a_launch_key_publish_failure_writes_no_marker_12127() {
    let (_tmp, ws) = fixture();
    write_text(&settings_path(&ws), SETTINGS);
    write_text(
        &crate::core::settings_lock::lock_sidecar(&settings_path(&ws)),
        "",
    );
    let claude = ws.join(".claude");
    if !read_only(&claude) {
        return;
    }
    let result = clean_tm_launch_keys_once(&ws, &ours());
    writable(&claude);
    let err = result.expect_err("a publish failure is an error");
    assert!(matches!(err, SandboxCleanupError::Write { .. }), "{err}");
    assert!(!marker_path(&ws).exists());
    assert_eq!(
        std::fs::read_to_string(settings_path(&ws)).expect("read"),
        SETTINGS
    );
}

/// #12127 (c), Fail-Open Check: a marker that cannot be written is an error
/// and leaves no marker; the retry finds nothing left of tm's and finishes.
#[test]
fn a_launch_key_marker_failure_writes_no_marker_12127() {
    let (_tmp, ws) = fixture();
    write_text(&settings_path(&ws), SETTINGS);
    // A file where `.trusty-mpm/` goes, so the marker cannot be staged.
    write_text(&ws.join(".trusty-mpm"), "not a directory");
    let err = clean_tm_launch_keys_once(&ws, &ours()).expect_err("a marker failure is an error");
    assert!(matches!(err, SandboxCleanupError::Marker { .. }), "{err}");
    std::fs::remove_file(ws.join(".trusty-mpm")).expect("unblock");
    assert!(!marker_path(&ws).exists());
    let outcome = clean_tm_launch_keys_once(&ws, &ours()).expect("the retry runs");
    let LaunchKeysCleanupOutcome::Cleaned { removed, .. } = outcome else {
        panic!("{outcome:?}");
    };
    assert!(removed.is_empty(), "{removed:?}");
    assert!(marker_path(&ws).exists());
}

/// #12127 (c): a symlinked settings file is skipped, link and target intact,
/// and no marker is written.
#[cfg(unix)]
#[test]
fn a_symlinked_settings_file_keeps_its_launch_keys_12127() {
    let (_tmp, ws) = fixture();
    write_text(&settings_path(&ws), SETTINGS);
    let file = settings_path(&ws);
    let target = ws.parent().expect("root").join("dotfiles-settings.json");
    std::fs::rename(&file, &target).expect("move");
    std::os::unix::fs::symlink(&target, &file).expect("symlink");
    let outcome = clean_tm_launch_keys_once(&ws, &ours()).expect("cleanup");
    assert_eq!(
        outcome,
        LaunchKeysCleanupOutcome::SymlinkedSettings(file.clone())
    );
    assert!(
        std::fs::symlink_metadata(&file)
            .expect("lstat")
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&target).expect("read"), SETTINGS);
    assert!(!marker_path(&ws).exists());
}

/// #12127: no settings file is not created; the marker records it absent.
#[test]
fn a_missing_settings_file_is_marked_absent_12127() {
    let (_tmp, ws) = fixture();
    let outcome = clean_tm_launch_keys_once(&ws, &ours()).expect("cleanup");
    assert_eq!(outcome, LaunchKeysCleanupOutcome::NoSettingsFile);
    assert!(!ws.join(".claude").exists());
    assert_eq!(read_json(&marker_path(&ws))["settings"], json!("absent"));
}

/// #12127: with a partial stack detection tm's skill entries cannot be known,
/// so none is removed and the marker says so; with no model record no `model`
/// is tm's.
#[test]
fn a_partial_detection_removes_no_skill_entry_12127() {
    let (_tmp, ws) = fixture();
    write_text(&settings_path(&ws), SETTINGS);
    let unknown = TmLaunchKeys {
        model: None,
        skills_off: None,
        ..ours()
    };
    clean_tm_launch_keys_once(&ws, &unknown).expect("cleanup");
    let file = read_json(&settings_path(&ws));
    assert_eq!(file["model"], json!("opus"), "{file}");
    assert_eq!(file["skillOverrides"]["xlsx"], json!("off"), "{file}");
    assert!(file.get("outputStyle").is_none(), "{file}");
    assert_eq!(
        read_json(&marker_path(&ws))["skillOverrides"],
        json!("unknown")
    );
}
