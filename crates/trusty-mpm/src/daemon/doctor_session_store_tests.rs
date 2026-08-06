//! Tests for the `session_store` doctor probe (#5007).
//!
//! Why: the probe exists so a corrupt store is visible before someone stumbles
//! into it. Each test pins a distinct VERDICT, not just "a check came back" — a
//! probe that returned `Ok` for a corrupt file would be worse than none,
//! because it would positively assert health.
//! What: the four classifications — healthy, corrupt, absent, unreadable.
//! Test: this file IS the test module.

use super::*;
use crate::session_manager::store::SessionStore;
use crate::session_manager::store_integrity::loadable_store_json;
use tempfile::TempDir;

/// The two ids the fixtures below use. Fixed rather than random so a failure
/// message is the same on every run.
const ID_A: &str = "aaaaaaaa-1111-2222-3333-444444444444";
const ID_B: &str = "bbbbbbbb-1111-2222-3333-444444444444";

/// Write `body` where the probe (and the daemon) look for the store.
fn seed(dir: &TempDir, body: &str) -> PathBuf {
    let path = store_path(dir.path());
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, body).expect("write");
    path
}

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
/// What: writes a store the daemon can genuinely LOAD — asserted here, not
/// assumed — and asserts `Ok` with the record count.
/// Test: this test.
#[tokio::test]
async fn session_store_check_is_ok_for_a_healthy_store() {
    let dir = TempDir::new().expect("tempdir");
    let path = seed(&dir, &loadable_store_json(&[(ID_A, "a"), (ID_B, "b")]));

    assert!(
        SessionStore::load(path.parent().expect("parent"))
            .await
            .is_ok(),
        "fixture invariant: the healthy fixture must be a store the daemon loads"
    );
    let check = check_session_store(dir.path());
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("2 session record(s)"),
        "{}",
        check.message
    );
}

/// Why (#5027 review): this probe parsed a `serde_json::Value` while the store
/// parses `StoredData`, so `{"sessions":{"a":{},"b":{}}}` — this test's own
/// former "healthy" fixture — came back `Ok` with "2 session record(s)" for a
/// file that blocks every write. A probe that positively asserts health for a
/// wedged store is worse than no probe.
/// What: feeds valid JSON that is not a loadable store and asserts the probe and
/// `SessionStore::load` reach the SAME verdict.
/// Test: this test.
#[tokio::test]
async fn session_store_check_agrees_with_the_store_about_what_loads() {
    let dir = TempDir::new().expect("tempdir");
    let path = seed(&dir, r#"{"sessions":{"a":{},"b":{}}}"#);

    let load = SessionStore::load(path.parent().expect("parent")).await;
    assert!(load.is_err(), "the daemon cannot load this file");
    let check = check_session_store(dir.path());
    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "the probe must not report health for a store the daemon rejects: {}",
        check.message
    );
    assert!(
        !check.message.contains("session record(s)"),
        "and it must not report a record count it could not read: {}",
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
    let doc = loadable_store_json(&[(ID_A, "a")]);
    seed(&dir, &format!("{doc}{doc}"));

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
