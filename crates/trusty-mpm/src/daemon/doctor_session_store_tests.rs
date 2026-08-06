//! Tests for the `session_store` doctor probe (#5007).
//!
//! Why: the probe exists so a corrupt store is visible before someone stumbles
//! into it. Each test pins a distinct VERDICT, not just "a check came back" — a
//! probe that returned `Ok` for a corrupt file would be worse than none,
//! because it would positively assert health.
//! What: the four classifications — healthy, corrupt, absent, unreadable.
//! Test: this file IS the test module.

use super::*;
use tempfile::TempDir;

/// Why: the daemon and the probe must agree on which file they are talking
/// about, or the probe reports on a file nobody writes.
/// What: asserts the path shape.
/// Test: this test.
#[test]
fn store_path_is_under_the_session_manager_dir() {
    let got = store_path(Path::new("/home/x/.trusty-mpm"));
    assert_eq!(
        got,
        PathBuf::from("/home/x/.trusty-mpm/session-manager/sessions.json")
    );
}

/// Why: the probe must not cry wolf on a healthy store, or it gets ignored.
/// What: writes a parseable store and asserts `Ok` with the record count.
/// Test: this test.
#[test]
fn session_store_check_is_ok_for_a_healthy_store() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("session-manager")).expect("mkdir");
    std::fs::write(store_path(dir.path()), r#"{"sessions":{"a":{},"b":{}}}"#).expect("write");

    let check = check_session_store(dir.path());
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("2 session record(s)"),
        "{}",
        check.message
    );
}

/// Why: this is the condition the probe was added for — the 2026-08-06 shape, a
/// complete document followed by a stale tail. It must be `Fail` (every write
/// is failing while it holds), and the message must carry the byte offset a
/// human needs to verify the cut.
/// What: writes that exact shape and asserts the verdict and the offset.
/// Test: this test.
#[test]
fn session_store_check_fails_on_a_trailing_tail_and_names_the_offset() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("session-manager")).expect("mkdir");
    let doc = r#"{"sessions":{"a":{}}}"#;
    std::fs::write(store_path(dir.path()), format!("{doc}{doc}")).expect("write");

    let check = check_session_store(dir.path());
    assert_eq!(check.status, CheckStatus::Fail, "{}", check.message);
    assert!(
        check.message.contains(&format!("byte {}", doc.len())),
        "the verdict must name where the valid document ends: {}",
        check.message
    );
    assert!(
        check.message.contains("tm repair session-store"),
        "the verdict must name the repair: {}",
        check.message
    );
}

/// Why: a machine that has never run a managed session has no store, and that
/// is healthy — flagging it would make the probe noise on every fresh install.
/// What: asserts `Ok` for an absent file.
/// Test: this test.
#[test]
fn session_store_check_is_ok_when_no_store_exists_yet() {
    let dir = TempDir::new().expect("tempdir");
    let check = check_session_store(dir.path());
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(check.message.contains("no managed-session store yet"));
}

/// Why: a probe that could not read the file has learned nothing, and must not
/// report health — the same #4005 rule this repo already applies to timed-out
/// probes.
/// What: puts a directory where the store belongs and asserts `Unknown`.
/// Test: this test.
#[test]
fn session_store_check_is_unknown_when_the_file_cannot_be_read() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(store_path(dir.path())).expect("mkdir over the store path");
    let check = check_session_store(dir.path());
    assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
}
