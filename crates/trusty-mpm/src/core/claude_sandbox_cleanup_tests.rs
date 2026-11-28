//! Tests for the #12077 one-time removal of the sandbox keys tm 1.14.14 and
//! 1.14.15 wrote into `<ws>/.claude/settings.json`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::core::session_launch::{SandboxInputs, SandboxRole, apply_launch_sandbox};

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
    ws.join(".trusty-mpm").join(CLEANUP_MARKER_FILE)
}

fn write_text(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
}

/// The role record 1.14.14 left: one PM policy.
fn record() -> Value {
    json!({"pm": {
        "sandbox": {
            "enabled": true,
            "network": {"allowUnixSockets": ["/tmp/a.sock", "/tmp/b.sock"]}
        },
        "env": {"RUSTC_WRAPPER": ""},
        "permissions": {"deny": ["Edit(/ws/.claude/settings.json)"]}
    }})
}

/// The project file as 1.14.14 left it, plus operator keys and entries.
const SETTINGS: &str = r#"{
  "model": "opus",
  "sandbox": {
    "enabled": true,
    "network": {
      "allowUnixSockets": ["/tmp/a.sock", "/tmp/user.sock", "/tmp/b.sock"]
    }
  },
  "env": {"RUSTC_WRAPPER": "", "KEEP": "1"},
  "permissions": {
    "deny": ["Edit(/ws/.claude/settings.json)"],
    "allow": ["Bash(ls)"]
  },
  "outputStyle": "tm"
}"#;

fn plant(ws: &Path, settings: &str) {
    write_text(
        &ws.join(".trusty-mpm/sandbox-roles.json"),
        &record().to_string(),
    );
    write_text(&settings_path(ws), settings);
}

fn top_keys(value: &Value) -> Vec<String> {
    value.as_object().expect("object").keys().cloned().collect()
}

/// #12077: only the keys and list entries that equal the record go; every
/// operator key and entry stays, list entries in their original order.
#[test]
fn only_matching_keys_and_entries_are_removed_12077() {
    let (_tmp, ws) = fixture();
    plant(&ws, SETTINGS);
    let outcome = clean_tm_sandbox_keys_once(&ws).expect("cleanup");
    let CleanupOutcome::Cleaned(removed) = outcome else {
        panic!("expected Cleaned, got {outcome:?}");
    };
    let mut labels: Vec<String> = removed.iter().map(|k| k.key.clone()).collect();
    labels.sort();
    assert_eq!(
        labels,
        [
            "env.RUSTC_WRAPPER",
            "permissions.deny[Edit(/ws/.claude/settings.json)]",
            "sandbox.enabled",
            "sandbox.network.allowUnixSockets[/tmp/a.sock]",
            "sandbox.network.allowUnixSockets[/tmp/b.sock]",
        ]
    );
    let after = read_json(&settings_path(&ws));
    assert_eq!(
        after,
        json!({
            "model": "opus",
            "sandbox": {"network": {"allowUnixSockets": ["/tmp/user.sock"]}},
            "env": {"KEEP": "1"},
            "permissions": {"allow": ["Bash(ls)"]},
            "outputStyle": "tm"
        })
    );
    // #12077: `preserve_order` is off for this crate, so a parsed `Value`
    // cannot show key order; read it from the written text. The rewrite
    // sorts the keys; this goes red if that ever changes.
    let text = std::fs::read_to_string(settings_path(&ws)).expect("read");
    let mut by_position: Vec<(usize, String)> = top_keys(&after)
        .into_iter()
        .map(|k| (text.find(&format!("\n  \"{k}\":")).expect("top key"), k))
        .collect();
    by_position.sort();
    let written: Vec<String> = by_position.into_iter().map(|(_, k)| k).collect();
    assert_eq!(
        written,
        ["env", "model", "outputStyle", "permissions", "sandbox"],
        "{text}"
    );
    // The detector, the one matching rule, now finds nothing of tm's here.
    let left = detect_tm_sandbox_keys(&ws, &ws.join("no-user-tier")).expect("detect");
    assert!(left.is_empty(), "{left:?}");
    assert!(marker_path(&ws).exists(), "the done marker is written");
}

/// #12077: a key or entry whose value differs from the record is the
/// operator's and stays.
#[test]
fn a_changed_value_is_left_alone_12077() {
    let (_tmp, ws) = fixture();
    let settings = r#"{
  "sandbox": {"enabled": false, "network": {"allowUnixSockets": ["/tmp/c.sock"]}},
  "env": {"RUSTC_WRAPPER": "sccache"},
  "permissions": {"deny": ["Edit(/elsewhere)"]}
}"#;
    plant(&ws, settings);
    let before = std::fs::read(settings_path(&ws)).expect("read");
    let outcome = clean_tm_sandbox_keys_once(&ws).expect("cleanup");
    assert!(
        matches!(&outcome, CleanupOutcome::Cleaned(removed) if removed.is_empty()),
        "{outcome:?}"
    );
    let after = std::fs::read(settings_path(&ws)).expect("read");
    assert_eq!(
        after, before,
        "nothing of tm's, so the file is not rewritten"
    );
    assert!(
        marker_path(&ws).exists(),
        "nothing to do still finishes once"
    );
}

/// #12077: with no role record tm owns nothing, so nothing is written — not
/// the settings file, not the marker.
#[test]
fn no_role_record_writes_nothing_12077() {
    let (_tmp, ws) = fixture();
    write_text(&settings_path(&ws), SETTINGS);
    let file = settings_path(&ws);
    let mtime = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .expect("mtime");
    let outcome = clean_tm_sandbox_keys_once(&ws).expect("cleanup");
    assert!(matches!(outcome, CleanupOutcome::NoRecord), "{outcome:?}");
    assert_eq!(std::fs::read_to_string(&file).expect("read"), SETTINGS);
    let after = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .expect("mtime");
    assert_eq!(after, mtime, "the settings file is not rewritten");
    assert!(!ws.join(".trusty-mpm").exists(), "no marker, no tm dir");
}

/// #12077: the cleanup runs once; a later launch reads the marker and never
/// writes the generic file again, even when matching keys reappear.
#[test]
fn a_second_launch_writes_nothing_12077() {
    let (_tmp, ws) = fixture();
    plant(&ws, SETTINGS);
    clean_tm_sandbox_keys_once(&ws).expect("first cleanup");
    write_text(&settings_path(&ws), SETTINGS);
    let outcome = clean_tm_sandbox_keys_once(&ws).expect("second cleanup");
    assert!(
        matches!(outcome, CleanupOutcome::AlreadyDone),
        "{outcome:?}"
    );
    let text = std::fs::read_to_string(settings_path(&ws)).expect("read");
    assert_eq!(text, SETTINGS, "the second launch did not write");
}

/// #12077 Fail-Open Check: a settings file that cannot be parsed is an error
/// naming the file, the file is untouched, and NO marker is written, so a
/// later launch retries and then succeeds.
#[test]
fn a_parse_failure_writes_no_marker_and_retries_12077() {
    let (_tmp, ws) = fixture();
    plant(&ws, "{ not json");
    let err = clean_tm_sandbox_keys_once(&ws).expect_err("a parse failure is an error");
    let file = settings_path(&ws);
    assert!(
        err.to_string().contains(&file.display().to_string()),
        "{err}"
    );
    assert!(
        !marker_path(&ws).exists(),
        "a failure must not finish the cleanup"
    );
    assert_eq!(std::fs::read_to_string(&file).expect("read"), "{ not json");
    write_text(&file, SETTINGS);
    let outcome = clean_tm_sandbox_keys_once(&ws).expect("the retry runs");
    assert!(matches!(outcome, CleanupOutcome::Cleaned(_)), "{outcome:?}");
    assert!(marker_path(&ws).exists());
}

/// Make `dir` read-only; `false` on a root runner, which ignores the mode, so
/// the arm cannot be exercised there.
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

/// #12077 Fail-Open Check: a lock that cannot be taken (its sidecar cannot be
/// created) is an error naming the file, and writes no marker.
#[cfg(unix)]
#[test]
fn a_lock_failure_writes_no_marker_12077() {
    let (_tmp, ws) = fixture();
    plant(&ws, SETTINGS);
    let claude = ws.join(".claude");
    if !read_only(&claude) {
        return;
    }
    let result = clean_tm_sandbox_keys_once(&ws);
    writable(&claude);
    let err = result.expect_err("a lock failure is an error");
    assert!(err.to_string().contains("lock sidecar"), "{err}");
    assert!(
        !marker_path(&ws).exists(),
        "a failure must not finish the cleanup"
    );
    assert_eq!(
        std::fs::read_to_string(settings_path(&ws)).expect("read"),
        SETTINGS
    );
}

/// #12077 Fail-Open Check: with the lock sidecar already there the lock is
/// taken, so the failure lands on the rewrite (`publish` cannot stage its temp
/// sibling); an error, no marker, the original file intact.
#[cfg(unix)]
#[test]
fn a_publish_failure_writes_no_marker_12077() {
    let (_tmp, ws) = fixture();
    plant(&ws, SETTINGS);
    let claude = ws.join(".claude");
    write_text(
        &crate::core::settings_lock::lock_sidecar(&settings_path(&ws)),
        "",
    );
    if !read_only(&claude) {
        return;
    }
    let result = clean_tm_sandbox_keys_once(&ws);
    writable(&claude);
    let err = result.expect_err("a publish failure is an error");
    let message = err.to_string();
    assert!(
        matches!(err, SandboxCleanupError::Write { .. }),
        "{message}"
    );
    assert!(message.contains("settings.json"), "{message}");
    assert!(
        !message.contains("lock sidecar"),
        "the lock was taken; the publish arm failed: {message}"
    );
    assert!(
        !marker_path(&ws).exists(),
        "a failure must not finish the cleanup"
    );
    assert_eq!(
        std::fs::read_to_string(settings_path(&ws)).expect("read"),
        SETTINGS
    );
}

/// #12077 Fail-Open Check: a role record that cannot be parsed is an error
/// naming the record, not "no record"; no marker, the settings untouched.
#[test]
fn a_record_parse_failure_writes_no_marker_12077() {
    let (_tmp, ws) = fixture();
    plant(&ws, SETTINGS);
    let record = ws.join(".trusty-mpm/sandbox-roles.json");
    write_text(&record, "{ not json");
    let err = clean_tm_sandbox_keys_once(&ws).expect_err("a bad record is an error");
    assert!(
        err.to_string().contains(&record.display().to_string()),
        "{err}"
    );
    assert!(matches!(err, SandboxCleanupError::Read(_)), "{err}");
    assert!(
        !marker_path(&ws).exists(),
        "a failure must not finish the cleanup"
    );
    assert_eq!(
        std::fs::read_to_string(settings_path(&ws)).expect("read"),
        SETTINGS
    );
}

/// #12077 Fail-Open Check: a marker that cannot be written is an error after
/// the prune landed; the retry writes the marker and removes nothing more.
#[cfg(unix)]
#[test]
fn a_marker_write_failure_leaves_the_prune_and_retries_once_12077() {
    let (_tmp, ws) = fixture();
    plant(&ws, SETTINGS);
    let tm_dir = ws.join(".trusty-mpm");
    if !read_only(&tm_dir) {
        return;
    }
    let result = clean_tm_sandbox_keys_once(&ws);
    writable(&tm_dir);
    let err = result.expect_err("a marker write failure is an error");
    assert!(matches!(err, SandboxCleanupError::Marker { .. }), "{err}");
    assert!(!marker_path(&ws).exists());
    let pruned = std::fs::read(settings_path(&ws)).expect("read");
    let value: Value = serde_json::from_slice(&pruned).expect("json");
    assert!(value.pointer("/sandbox/enabled").is_none(), "{value}");
    assert_eq!(value.pointer("/env/KEEP"), Some(&json!("1")));
    let outcome = clean_tm_sandbox_keys_once(&ws).expect("the retry runs");
    assert!(
        matches!(&outcome, CleanupOutcome::Cleaned(removed) if removed.is_empty()),
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read(settings_path(&ws)).expect("read"),
        pruned,
        "the retry removes nothing more"
    );
    assert!(marker_path(&ws).exists());
}

/// #12077: a missing project settings file is never created, and the cleanup
/// is marked done as absent.
#[test]
fn a_missing_settings_file_is_not_created_12077() {
    let (_tmp, ws) = fixture();
    write_text(
        &ws.join(".trusty-mpm/sandbox-roles.json"),
        &record().to_string(),
    );
    let outcome = clean_tm_sandbox_keys_once(&ws).expect("cleanup");
    assert!(
        matches!(outcome, CleanupOutcome::NoSettingsFile),
        "{outcome:?}"
    );
    assert!(!ws.join(".claude").exists(), "no `.claude` dir is created");
    assert_eq!(
        read_json(&marker_path(&ws)),
        json!({"issue": 12077, "removed": [], "settings": "absent"})
    );
}

/// #12077: after a run found no settings file, a file the user writes later
/// is never cleaned, even when its values equal the record.
#[test]
fn a_settings_file_written_after_an_absent_run_is_never_cleaned_12077() {
    let (_tmp, ws) = fixture();
    write_text(
        &ws.join(".trusty-mpm/sandbox-roles.json"),
        &record().to_string(),
    );
    clean_tm_sandbox_keys_once(&ws).expect("first launch");
    let user = r#"{"sandbox":{"enabled":true}}"#;
    write_text(&settings_path(&ws), user);
    let outcome = clean_tm_sandbox_keys_once(&ws).expect("second launch");
    assert!(
        matches!(outcome, CleanupOutcome::AlreadyDone),
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(settings_path(&ws)).expect("read"),
        user
    );
}

/// #12077: a symlinked settings file is skipped: the link stays a link, its
/// target is untouched, and no marker is written.
#[cfg(unix)]
#[test]
fn a_symlinked_settings_file_is_skipped_12077() {
    let (_tmp, ws) = fixture();
    plant(&ws, SETTINGS);
    let file = settings_path(&ws);
    let target = ws.parent().expect("root").join("dotfiles-settings.json");
    std::fs::rename(&file, &target).expect("move");
    std::os::unix::fs::symlink(&target, &file).expect("symlink");
    let outcome = clean_tm_sandbox_keys_once(&ws).expect("cleanup");
    assert_eq!(outcome, CleanupOutcome::SymlinkedSettings(file.clone()));
    let meta = std::fs::symlink_metadata(&file).expect("lstat");
    assert!(meta.file_type().is_symlink(), "the link is kept");
    assert_eq!(std::fs::read_to_string(&target).expect("read"), SETTINGS);
    assert!(!marker_path(&ws).exists(), "a skip is not a finish");
}

/// #12077: `settings.local.json` was never written by 1.14.14 or 1.14.15, so
/// the cleanup leaves it byte-for-byte, even when it holds matching keys.
#[test]
fn the_local_settings_file_is_never_touched_12077() {
    let (_tmp, ws) = fixture();
    plant(&ws, SETTINGS);
    let local = ws.join(".claude/settings.local.json");
    write_text(&local, SETTINGS);
    clean_tm_sandbox_keys_once(&ws).expect("cleanup");
    assert_eq!(std::fs::read_to_string(&local).expect("read"), SETTINGS);
}

/// #12077: every launch path reaches the cleanup through the shared sandbox
/// step, opted in or not; a cleanup failure never refuses the launch.
#[test]
#[serial_test::serial]
fn the_shared_launch_step_runs_the_cleanup_and_never_refuses_12077() {
    use crate::secret_source::test_env::EnvVarGuard;
    let _off = EnvVarGuard::unset(SANDBOX_OPT_IN_ENV);
    let (tmp, ws) = fixture();
    let inputs = SandboxInputs::new(&ws, tmp.path().join("home"));
    plant(&ws, "{ not json");
    apply_launch_sandbox(SandboxRole::Pm, &inputs, None).expect("a failure never refuses");
    assert!(
        !marker_path(&ws).exists(),
        "a failure must not finish the cleanup"
    );
    write_text(&settings_path(&ws), SETTINGS);
    apply_launch_sandbox(SandboxRole::Pm, &inputs, None).expect("not refused");
    let after = read_json(&settings_path(&ws));
    assert!(after.pointer("/sandbox/enabled").is_none(), "{after}");
    assert_eq!(after.pointer("/model"), Some(&json!("opus")));
    assert!(marker_path(&ws).exists());
}
