//! #9832: the shared audit log's batch append and its owner-only mode.
//!
//! Why: a route change is recorded with one durable write before it goes
//! live, and the tm daemon's log must not be readable by other users (A3).
//! gchat's own state files keep their mode (A3 narrowed).
//! What: temp files only; no git, no network.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use crate::audit::{AuditEvent, AuditLog, AuditRecord};
use crate::gchat::state::audit::append_owner_only_with;

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

fn record(reason: &'static str) -> AuditRecord {
    let mut r = AuditRecord::new(AuditEvent::RouteChanged, reason);
    r.project = Some("/work/a".into());
    r
}

fn lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .expect("read log")
        .lines()
        .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
        .collect()
}

#[test]
fn record_all_appends_every_line() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("audit.jsonl");
    let log = AuditLog::new(&path);
    log.record_all(&[record("first_load"), record("routes_changed")])
        .expect("append");
    log.record_all(&[record("routes_changed")]).expect("append");
    let got: Vec<String> = lines(&path)
        .iter()
        .map(|l| format!("{}/{}", l["event"], l["reason"]))
        .collect();
    assert_eq!(
        got,
        [
            "\"route_changed\"/\"first_load\"",
            "\"route_changed\"/\"routes_changed\"",
            "\"route_changed\"/\"routes_changed\"",
        ]
    );
}

#[test]
fn owner_only_log_creates_file_0600() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("audit.jsonl");
    AuditLog::owner_only(&path)
        .record_all(&[record("first_load")])
        .expect("append");
    assert_eq!(mode(&path), 0o600, "created");
    // An existing wider file is narrowed.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    AuditLog::owner_only(&path)
        .record_all(&[record("routes_changed")])
        .expect("append");
    assert_eq!(mode(&path), 0o600, "narrowed");
    assert_eq!(lines(&path).len(), 2);
}

#[test]
fn failed_owner_only_append_leaves_no_torn_line() {
    use std::io::Write as _;
    // #9832: a write that fails partway (ENOSPC) must not leave a fragment
    // the next append joins onto.
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("audit.jsonl");
    let log = AuditLog::owner_only(&path);
    log.record_all(&[record("first_load")]).expect("append");
    let mut line = serde_json::to_vec(&record("routes_changed")).expect("encode");
    line.push(b'\n');
    let failed = append_owner_only_with(&path, &line, |file, bytes| {
        file.write_all(&bytes[..bytes.len() / 2])?;
        Err(std::io::Error::from(std::io::ErrorKind::StorageFull))
    });
    assert!(failed.is_err(), "the partial write reported success");
    log.record_all(&[record("retried")]).expect("retry");
    let text = std::fs::read_to_string(&path).expect("read log");
    assert!(text.ends_with('\n'), "{text}");
    let reasons: Vec<String> = text
        .lines()
        .map(|l| match serde_json::from_str::<serde_json::Value>(l) {
            Ok(v) => v["reason"].to_string(),
            Err(e) => panic!("torn line {l:?}: {e}"),
        })
        .collect();
    assert_eq!(reasons, ["\"first_load\"", "\"retried\""]);
}

#[test]
fn gchat_log_keeps_an_existing_file_mode() {
    // A3 narrowed: gchat's state files are not narrowed by this change.
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("audit.jsonl");
    std::fs::write(&path, b"").expect("create");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let log = AuditLog::new(&path);
    log.record(&record("first_load")).expect("append");
    log.record_all(&[record("routes_changed")]).expect("append");
    assert_eq!(mode(&path), 0o644);
}
