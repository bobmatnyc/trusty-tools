//! Tests for `tm repair session-store` (#5007).
//!
//! Why: this command deletes bytes from the operator's only copy of the fleet's
//! state, so the two properties worth pinning are that `--dry-run` writes
//! nothing at all and that a real run leaves a loadable store plus a backup.
//! What: the healthy no-op, the dry run, and the real repair.
//! Test: this file IS the test module.

use super::*;
use tempfile::TempDir;

/// Build a store file that is a complete document followed by a stale tail —
/// the 2026-08-06 shape.
fn corrupt_store(dir: &TempDir) -> (std::path::PathBuf, String) {
    let path = dir.path().join("sessions.json");
    let doc = r#"{"sessions":{"3d1b0c8e-1111-2222-3333-444444444444":{"task":"t"}}}"#;
    let bytes = format!("{doc}{doc}");
    std::fs::write(&path, &bytes).expect("write");
    (path, bytes)
}

/// Why: the default has to be the file the daemon writes, or the command
/// repairs nothing anyone uses.
/// What: asserts the default lands under `session-manager/` and that `--path`
/// overrides it verbatim.
/// Test: this test.
#[test]
fn resolve_store_path_defaults_under_the_framework_root() {
    let default = resolve_store_path(None);
    assert!(
        default.ends_with("session-manager/sessions.json"),
        "{}",
        default.display()
    );
    assert_eq!(
        resolve_store_path(Some("/tmp/x.json".into())),
        std::path::PathBuf::from("/tmp/x.json")
    );
}

/// Why: re-running the command after a successful repair must be a safe no-op,
/// not an error an operator has to interpret.
/// What: asserts a parseable store succeeds and is left byte-identical.
/// Test: this test.
#[test]
fn repair_session_store_reports_a_healthy_store() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("sessions.json");
    let good = r#"{"sessions":{}}"#;
    std::fs::write(&path, good).expect("write");

    repair_session_store(Some(path.display().to_string()), false, false).expect("healthy is Ok");
    assert_eq!(std::fs::read_to_string(&path).expect("re-read"), good);
}

/// Why: `--dry-run` exists so an operator can see the cut before authorising
/// it. A dry run that wrote anything — including a backup — would defeat that.
/// What: asserts the store is unchanged and no backup was created.
/// Test: this test.
#[test]
fn repair_session_store_dry_run_writes_nothing() {
    let dir = TempDir::new().expect("tempdir");
    let (path, before) = corrupt_store(&dir);

    repair_session_store(Some(path.display().to_string()), true, false).expect("dry run");
    assert_eq!(
        std::fs::read_to_string(&path).expect("re-read"),
        before,
        "--dry-run must not modify the store"
    );
    let siblings: Vec<_> = std::fs::read_dir(dir.path())
        .expect("readdir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect();
    assert_eq!(
        siblings,
        vec!["sessions.json".to_string()],
        "--dry-run must not write a backup either"
    );
}

/// Why: the point of the command is that a wedged store becomes a working one
/// without a human editing JSON.
/// What: asserts the file parses afterwards and the original bytes survive in a
/// timestamped backup.
/// Test: this test.
#[test]
fn repair_session_store_truncates_and_backs_up() {
    let dir = TempDir::new().expect("tempdir");
    let (path, before) = corrupt_store(&dir);

    repair_session_store(Some(path.display().to_string()), false, false).expect("repair");
    let after = std::fs::read_to_string(&path).expect("re-read");
    serde_json::from_str::<serde_json::Value>(&after).expect("the repaired store must parse");
    assert!(after.len() < before.len(), "the stale tail was discarded");

    let backup = std::fs::read_dir(dir.path())
        .expect("readdir")
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().contains(".corrupt-backup-"))
        .expect("a timestamped backup must exist");
    assert_eq!(
        std::fs::read_to_string(backup.path()).expect("read backup"),
        before,
        "the backup holds the original bytes verbatim"
    );
}
