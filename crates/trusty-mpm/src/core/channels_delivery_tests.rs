//! Tests for the delivery trace (#9873 Q5).
//!
//! Why: a lost or unconfirmed answer must stay visible; each test goes red
//! against a trace that writes nothing, loses a line, or reads a bad file as
//! clean.
//! What: real files in a temp taint directory.
//! Test: this is the test module.

use std::os::unix::fs::PermissionsExt as _;

use super::*;

fn dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = crate::core::channels_taint::taint_dir(tmp.path());
    (tmp, dir)
}

/// 🔴 #9873: each step is one owner-only line; the reader returns them in
/// order, and no line carries answer text.
#[test]
fn delivery_trace_is_0600_and_one_line_per_step() {
    let (_tmp, dir) = dir();
    let pending = DeliveryLine::new(DeliveryState::Pending, "7", "m-1", None);
    let failed = DeliveryLine::new(DeliveryState::Failed, "7", "m-1", Some("send_failed"));
    record_delivery(&dir, &pending).expect("pending");
    record_delivery(&dir, &failed).expect("failed");
    let path = dir.join(DELIVERY_FILE);
    let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let dir_mode = std::fs::metadata(&dir).expect("stat").permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700);
    let trace = read_deliveries(&dir).expect("read");
    assert_eq!(trace.lines, [pending, failed]);
    assert!(trace.malformed.is_empty());
    let raw = std::fs::read_to_string(&path).expect("raw");
    assert_eq!(raw.lines().count(), 2, "{raw}");
    assert!(!raw.contains("text"), "{raw}");
}

/// 🔴 #9873 (F18, fix round): a terminated line the reader cannot parse is
/// named, never a clean trace, and the read goes on; an unterminated last
/// line is a torn append and is skipped; an absent trace is empty.
#[test]
fn malformed_delivery_line_is_named_and_a_torn_last_line_is_skipped() {
    let (_tmp, dir) = dir();
    assert_eq!(
        read_deliveries(&dir).expect("absent"),
        DeliveryTrace::default()
    );
    let failed = DeliveryLine::new(DeliveryState::Failed, "7", "m-1", Some("send_failed"));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(DELIVERY_FILE), "not json\n").expect("write");
    record_delivery(&dir, &failed).expect("after the bad line");
    let trace = read_deliveries(&dir).expect("read");
    assert_eq!(trace.lines, std::slice::from_ref(&failed));
    assert_eq!(trace.malformed.len(), 1, "{:?}", trace.malformed);
    assert!(trace.malformed[0].starts_with("delivery.jsonl line 1: malformed"));
    std::fs::write(dir.join(DELIVERY_FILE), "{\"v\":1,\"ts").expect("torn");
    assert_eq!(
        read_deliveries(&dir).expect("torn"),
        DeliveryTrace::default()
    );
    record_delivery(&dir, &failed).expect("after the torn line");
    let raw = std::fs::read_to_string(dir.join(DELIVERY_FILE)).expect("raw");
    assert_eq!(
        raw.lines().count(),
        2,
        "a newline ends the torn line: {raw:?}"
    );
    let trace = read_deliveries(&dir).expect("read");
    assert_eq!(trace.lines, [failed]);
    assert!(trace.malformed[0].starts_with("delivery.jsonl line 1: malformed"));
}

/// 🔴 #9873 (AC2, Q4a): an unsettled `pending`, a `failed` line, and a
/// resolved question with no line at all are each a problem naming the
/// question and the asker; a delivered one is not.
#[test]
fn delivery_problems_name_unsettled_failed_and_unrecorded() {
    let lines = [
        DeliveryLine::new(DeliveryState::Pending, "1", "m-a", None),
        DeliveryLine::new(DeliveryState::Pending, "2", "m-b", None),
        DeliveryLine::new(DeliveryState::Delivered, "2", "m-b", None),
        DeliveryLine::new(DeliveryState::Pending, "3", "m-c", None),
        DeliveryLine::new(DeliveryState::Failed, "3", "m-c", Some("send_failed")),
    ];
    let resolved = [
        ("1".to_owned(), "m-a".to_owned()),
        ("2".to_owned(), "m-b".to_owned()),
        ("3".to_owned(), "m-c".to_owned()),
        ("4".to_owned(), "m-d".to_owned()),
    ];
    let problems = delivery_problems(&lines, &resolved);
    assert_eq!(problems.len(), 3, "{problems:?}");
    let all = problems.join("\n");
    for needle in [
        "Q-1 to asker m-a: delivery unconfirmed",
        "Q-3 to asker m-c: delivery failed (send_failed)",
        "Q-4 to asker m-d: resolved with no delivery record",
    ] {
        assert!(all.contains(needle), "{needle} missing: {all}");
    }
    assert!(!all.contains("Q-2"), "a delivered answer is fine: {all}");
}
