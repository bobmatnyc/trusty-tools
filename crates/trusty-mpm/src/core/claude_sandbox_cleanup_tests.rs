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
/// operator key and entry stays, in its original order.
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
    // The writer keeps the file's own key order (serde_json preserve_order).
    let before: Value = serde_json::from_str(SETTINGS).expect("json");
    assert_eq!(top_keys(&after), top_keys(&before));
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

/// #12077 Fail-Open Check: a write that fails is an error naming the file and
/// writes no marker.
#[cfg(unix)]
#[test]
fn a_write_failure_writes_no_marker_12077() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, ws) = fixture();
    plant(&ws, SETTINGS);
    let claude = ws.join(".claude");
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    // A root runner ignores the mode; the arm cannot be exercised there.
    if std::fs::write(claude.join("probe"), b"x").is_ok() {
        eprintln!("skipped: this runner can write a read-only directory");
        return;
    }
    let result = clean_tm_sandbox_keys_once(&ws);
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let err = result.expect_err("a write failure is an error");
    assert!(err.to_string().contains("settings.json"), "{err}");
    assert!(
        !marker_path(&ws).exists(),
        "a failure must not finish the cleanup"
    );
    assert_eq!(
        std::fs::read_to_string(settings_path(&ws)).expect("read"),
        SETTINGS
    );
}

/// #12077: a missing project settings file is never created, and the cleanup
/// is not marked done.
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
    assert!(!marker_path(&ws).exists());
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
