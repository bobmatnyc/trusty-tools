//! The per-instance turn log (#11650).
//!
//! Test: this module IS the test surface.

use super::instance;
use crate::ask::{AskError, TURN_LOG_FILE, TurnLog, TurnRecord};

#[test]
fn an_absent_log_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let log = TurnLog::for_home(&instance(&tmp));
    assert!(!log.path().exists());
    assert!(log.read().unwrap().is_empty());
}

#[test]
fn appended_turns_read_back_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let log = TurnLog::for_home(&home);
    assert_eq!(log.path(), home.stores_dir().join(TURN_LOG_FILE));

    let first = TurnRecord::now("q1", "a1");
    let second = TurnRecord::now("q2", "line one\nline two");
    log.append(&first).unwrap();
    let after_first = std::fs::read(log.path()).unwrap();
    log.append(&second).unwrap();

    assert_eq!(log.read().unwrap(), vec![first, second]);
    // Append only: the first line's bytes are unchanged.
    let after_second = std::fs::read(log.path()).unwrap();
    assert!(after_second.starts_with(&after_first));
    assert_eq!(
        std::fs::read_to_string(log.path()).unwrap().lines().count(),
        2
    );
}

#[test]
fn a_corrupt_line_is_a_typed_error() {
    let tmp = tempfile::tempdir().unwrap();
    let log = TurnLog::for_home(&instance(&tmp));
    log.append(&TurnRecord::now("q1", "a1")).unwrap();
    let good = std::fs::read_to_string(log.path()).unwrap();

    let cases = [
        (format!("{good}not json\n"), 2),
        (format!("{good}\n"), 2),
        (format!("{good}{}", good.trim_end()), 2),
        (good.replace("\"v\":1", "\"v\":2"), 1),
    ];
    for (body, line) in cases {
        std::fs::write(log.path(), &body).unwrap();
        match log.read() {
            Err(AskError::CorruptTurnLog { line: l, .. }) => assert_eq!(l, line, "{body:?}"),
            other => panic!("{body:?}: {other:?}"),
        }
        // Reading never repairs or truncates the file.
        assert_eq!(std::fs::read_to_string(log.path()).unwrap(), body);
    }
}
