//! The audit halves of the spike's `test_broker.py::test_audit_line_per_call`
//! and `test_audit_write_failure_refuses_call`, on the same inputs, plus the
//! rotation and privacy checks the plan adds.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use common::short_tmp;
use serde_json::{Map, Value, json};
use trusty_computer::audit::{Audit, AuditError, summarize_args};

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn lines(audit: &Audit) -> Vec<Value> {
    fs::read_to_string(audit.path())
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or(Value::Null))
        .collect()
}

#[test]
fn audit_line_per_call() {
    let dir = short_tmp();
    let audit = Audit::new(dir.path().join("audit.jsonl"));
    let args =
        obj(json!({"pid": 1, "window_id": 1, "element_token": "tok-field", "text": "secret"}));
    let summary = summarize_args(&args);
    let base = json!({"op": "call", "tool": "type_text", "request_id": "r1", "args": summary});
    let mut forward = obj(base.clone());
    forward.insert("phase".into(), json!("forward"));
    let mut done = obj(base);
    done.extend(obj(json!({"phase": "done", "ok": true, "ms": 3})));
    let refused = obj(json!({"op": "call", "tool": "clipboard_read", "code": "denied_tool"}));
    for record in [&forward, &done, &refused] {
        audit.write(record).expect("audit write");
    }
    let got = lines(&audit);
    let phases: Vec<&str> = got
        .iter()
        .map(|l| l.get("phase").and_then(Value::as_str).unwrap_or("refused"))
        .collect();
    assert_eq!(phases, ["forward", "done", "refused"]);
    assert_eq!(
        got[1]["args"],
        json!({"keys": ["element_token", "pid", "text", "window_id"], "text_len": 6})
    );
    assert!(
        !fs::read_to_string(audit.path())
            .unwrap_or_default()
            .contains("secret")
    );
    let mode = fs::metadata(audit.path()).map(|m| m.permissions().mode() & 0o777);
    assert_eq!(mode.ok(), Some(0o600));
}

#[test]
fn audit_write_failure_refuses_call() {
    let dir = short_tmp();
    let path = dir.path().join("audit.jsonl");
    fs::create_dir(&path).expect("mkdir"); // an append now fails with EISDIR
    let err = Audit::new(&path).write(&obj(json!({"phase": "forward"})));
    assert!(matches!(err, Err(AuditError::Io(_))), "{err:?}");
}

#[test]
fn audit_refuses_a_shared_log() {
    let dir = short_tmp();
    let path = dir.path().join("audit.jsonl");
    fs::write(&path, "").expect("touch");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
    let err = Audit::new(&path).write(&obj(json!({"phase": "forward"})));
    assert!(matches!(err, Err(AuditError::NotPrivate)), "{err:?}");
}

#[test]
fn audit_line_is_sorted_and_carries_ts() {
    let line = Audit::line(&obj(json!({"z": 1, "a": {"y": 2, "b": 3}})), 12.5);
    assert_eq!(line, r#"{"a":{"b":3,"y":2},"ts":12.5,"z":1}"#);
}

#[test]
fn audit_rotates_at_the_size_limit() {
    let dir = short_tmp();
    let audit = Audit::new(dir.path().join("audit.jsonl"));
    for round in 0..3 {
        audit.write(&obj(json!({"round": round}))).expect("write");
        audit.rotate_at(1).expect("rotate");
    }
    assert!(!audit.path().exists());
    let gen1 = fs::read_to_string(audit.generation(1)).unwrap_or_default();
    let gen2 = fs::read_to_string(audit.generation(2)).unwrap_or_default();
    assert!(gen1.contains("\"round\":2") && gen2.contains("\"round\":1"));
    assert!(!audit.generation(3).exists());
}
