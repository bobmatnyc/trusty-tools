//! The per-instance turn log (#11650).
//!
//! Test: this module IS the test surface.

use super::instance;
use crate::ask::{AskError, TURN_LOG_FILE, TurnLog, TurnRecord, replay_window};

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

#[test]
fn the_replay_window_is_the_newest_suffix() {
    let turns: Vec<TurnRecord> = (0..10)
        .map(|i| TurnRecord::now(&format!("q{i}"), "aaaa"))
        .collect();
    // Turn cap: the last three.
    assert_eq!(replay_window(&turns, 3, usize::MAX), &turns[7..]);
    // Char cap: each turn is 6 chars, so 20 chars hold three.
    assert_eq!(replay_window(&turns, 100, 20), &turns[7..]);
    // A newest turn over the budget stops the window empty, never skipping it.
    let mut big = turns.clone();
    big.push(TurnRecord::now("q", &"x".repeat(50)));
    assert!(replay_window(&big, 100, 20).is_empty());
    // Zero limits replay nothing; a short log is replayed whole.
    assert!(replay_window(&turns, 0, usize::MAX).is_empty());
    assert!(replay_window(&turns, 100, 0).is_empty());
    assert_eq!(replay_window(&turns, 100, usize::MAX), &turns[..]);
}

/// #11650 item 2: `load` moves a corrupt last line aside and reports it.
#[test]
fn a_corrupt_last_line_is_quarantined() {
    let tmp = tempfile::tempdir().unwrap();
    let log = TurnLog::for_home(&instance(&tmp));
    log.append(&TurnRecord::now("q1", "a1")).unwrap();
    let good = std::fs::read(log.path()).unwrap();

    let tails: [&[u8]; 4] = [b"{not json\n", b"{\"v\":1", b"\n", b"\xff\xfe\n"];
    for tail in tails {
        let mut body = good.clone();
        body.extend_from_slice(tail);
        std::fs::write(log.path(), &body).unwrap();
        let loaded = log.load().unwrap();
        assert_eq!(loaded.turns.len(), 1, "{tail:?}");
        let recovery = loaded.recovered.expect("a recovery is reported");
        assert_eq!(recovery.line, 2);
        assert_eq!(recovery.bytes, tail.len());
        assert_eq!(std::fs::read(&recovery.quarantine).unwrap(), tail);
        // Truncated to the good prefix; a strict read now passes.
        assert_eq!(std::fs::read(log.path()).unwrap(), good);
        assert_eq!(log.read().unwrap().len(), 1);
        // A second load has nothing to recover.
        assert!(log.load().unwrap().recovered.is_none());
    }
}

/// #11650 item 2: only the last line is recovered; anything else still fails
/// and leaves the file alone.
#[test]
fn mid_file_corruption_is_not_recovered() {
    let tmp = tempfile::tempdir().unwrap();
    let log = TurnLog::for_home(&instance(&tmp));
    log.append(&TurnRecord::now("q1", "a1")).unwrap();
    let good = std::fs::read_to_string(log.path()).unwrap();

    let cases = [
        (format!("{good}not json\n{good}"), 2),
        (good.replace("\"v\":1", "\"v\":2"), 1),
    ];
    for (body, line) in cases {
        std::fs::write(log.path(), &body).unwrap();
        match log.load() {
            Err(AskError::CorruptTurnLog { line: l, .. }) => assert_eq!(l, line, "{body:?}"),
            other => panic!("{body:?}: {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(log.path()).unwrap(), body);
    }
}
